//! Streamline Kubernetes Operator
//!
//! This operator manages Streamline clusters, topics, and users on Kubernetes.
//!
//! ## Usage
//!
//! ```bash
//! # Run the operator (requires kubeconfig)
//! streamline-operator
//!
//! # Run with custom log level
//! RUST_LOG=debug streamline-operator
//! ```

use clap::Parser;
use kube::Client;
use std::sync::Arc;
use streamline_operator::leader_election::{self, LeaderElector};
use streamline_operator::{ClusterController, TopicController, UserController};
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Streamline Kubernetes Operator
#[derive(Parser, Debug)]
#[command(name = "streamline-operator")]
#[command(version, about = "Kubernetes Operator for Streamline clusters")]
struct Args {
    /// Enable leader election for HA deployments
    #[arg(long, default_value = "false")]
    leader_election: bool,

    /// Namespace for the leader election Lease (auto-detected if empty)
    #[arg(long, default_value = "")]
    leader_election_namespace: String,

    /// Namespace to watch (empty for all namespaces)
    #[arg(long, default_value = "")]
    namespace: String,

    /// Metrics bind address
    #[arg(long, default_value = "0.0.0.0:8080")]
    metrics_bind_address: String,

    /// Health probe bind address
    #[arg(long, default_value = "0.0.0.0:8081")]
    health_probe_bind_address: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer().json())
        .init();

    let args = Args::parse();

    info!("Starting Streamline Kubernetes Operator");
    info!("Leader election: {}", args.leader_election);
    info!(
        "Watching namespace: {}",
        if args.namespace.is_empty() {
            "all"
        } else {
            &args.namespace
        }
    );

    // Create Kubernetes client
    let client = Client::try_default().await?;
    info!("Connected to Kubernetes API server");

    // Leader election — acquire lease before starting controllers
    let elector = if args.leader_election {
        let ns = leader_election::detect_namespace(&args.leader_election_namespace);
        info!("Leader election namespace: {}", ns);
        let elector = LeaderElector::new(client.clone(), &ns);
        elector.acquire().await?;
        Some(Arc::new(elector))
    } else {
        None
    };

    // Create controllers
    let cluster_controller = Arc::new(ClusterController::new(client.clone()));
    let topic_controller = Arc::new(TopicController::new(client.clone()));
    let user_controller = Arc::new(UserController::new(client.clone()));

    // Run controllers concurrently
    let cluster_handle = {
        let controller = Arc::clone(&cluster_controller);
        tokio::spawn(async move {
            if let Err(e) = controller.run().await {
                error!("Cluster controller error: {}", e);
            }
        })
    };

    let topic_handle = {
        let controller = Arc::clone(&topic_controller);
        tokio::spawn(async move {
            if let Err(e) = controller.run().await {
                error!("Topic controller error: {}", e);
            }
        })
    };

    let user_handle = {
        let controller = Arc::clone(&user_controller);
        tokio::spawn(async move {
            if let Err(e) = controller.run().await {
                error!("User controller error: {}", e);
            }
        })
    };

    // Periodic lease renewal (no-op future when leader election is disabled)
    let elector_for_renew = elector.clone();
    let renew_handle = tokio::spawn(async move {
        match elector_for_renew {
            Some(e) => loop {
                tokio::time::sleep(e.renew_interval()).await;
                match e.renew().await {
                    Ok(true) => {}
                    Ok(false) => {
                        error!("Lost leader lease");
                        break;
                    }
                    Err(err) => {
                        error!("Failed to renew leader lease: {}", err);
                        break;
                    }
                }
            },
            None => std::future::pending::<()>().await,
        }
    });

    // Wait for shutdown signal
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("Received shutdown signal");
        }
        result = cluster_handle => {
            if let Err(e) = result {
                error!("Cluster controller task failed: {}", e);
            }
        }
        result = topic_handle => {
            if let Err(e) = result {
                error!("Topic controller task failed: {}", e);
            }
        }
        result = user_handle => {
            if let Err(e) = result {
                error!("User controller task failed: {}", e);
            }
        }
        _ = renew_handle => {
            error!("Leader lease lost, initiating shutdown");
        }
    }

    // Release the lease before exiting so a standby replica can take over immediately
    if let Some(e) = &elector {
        e.release().await;
    }

    info!("Streamline Operator shutting down");
    Ok(())
}
