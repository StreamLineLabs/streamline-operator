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

use anyhow::Context;
use clap::Parser;
use kube::Client;
use std::sync::Arc;
use streamline_operator::controllers::WatchScope;
use streamline_operator::crd::generate as crd_generate;
use streamline_operator::health::ReadinessState;
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
    #[arg(long, alias = "leader-elect", default_value = "false")]
    leader_election: bool,

    /// Namespace for the leader election Lease (auto-detected if empty).
    ///
    /// This is the namespace the operator *runs in*, which is not necessarily
    /// the namespace it watches: `--namespace` scopes the custom resource
    /// watches and has no effect on where the Lease lives.
    #[arg(long, default_value = "")]
    leader_election_namespace: String,

    /// Namespace to watch for custom resources (empty for all namespaces).
    ///
    /// `deploy/` is namespaced; `overlays/cloud/` supplies the opt-in
    /// cluster-wide RBAC and an explicit empty value.
    #[arg(long, default_value = "")]
    namespace: String,

    /// Metrics bind address
    #[arg(long, default_value = "0.0.0.0:8080")]
    metrics_bind_address: String,

    /// Health probe bind address
    #[arg(long, default_value = "0.0.0.0:8081")]
    health_probe_bind_address: String,

    /// Print the installed CustomResourceDefinition manifests to stdout and exit.
    ///
    /// Output is deterministic and is the source used by the release pipeline.
    #[arg(long, default_value = "false")]
    generate_crds: bool,

    /// Write one CustomResourceDefinition manifest per kind into DIR and exit.
    #[arg(long, value_name = "DIR")]
    generate_crds_dir: Option<std::path::PathBuf>,
}

/// Emit the generated CRD manifests and exit.
///
/// Failures propagate as errors so callers (notably the release workflow) fail
/// closed instead of shipping a release without manifests.
fn generate_crds(args: &Args) -> anyhow::Result<()> {
    use std::io::Write;

    if let Some(dir) = &args.generate_crds_dir {
        let written = crd_generate::write_crds(dir)
            .with_context(|| format!("failed to write CRD manifests to {}", dir.display()))?;
        for path in written {
            eprintln!("wrote {}", path.display());
        }
    }

    if args.generate_crds {
        let manifest =
            crd_generate::render_installed_manifest().context("failed to render CRD manifests")?;
        let mut stdout = std::io::stdout().lock();
        stdout
            .write_all(manifest.as_bytes())
            .context("failed to write CRD manifests to stdout")?;
        stdout.flush().context("failed to flush stdout")?;
    }

    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // CRD generation is a pure, offline operation: handle it before any logging
    // is installed (stdout must stay a clean YAML stream) and before a
    // Kubernetes client is required.
    if args.generate_crds || args.generate_crds_dir.is_some() {
        return generate_crds(&args);
    }

    // Initialize tracing
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer().json())
        .init();

    let scope = WatchScope::from_flag(&args.namespace);

    info!("Starting Streamline Kubernetes Operator");
    info!("Leader election: {}", args.leader_election);
    info!("Watching: {}", scope.describe());
    if scope == WatchScope::AllNamespaces {
        info!(
            "Cluster-wide watch requested: install with overlays/cloud/ or supply equivalent \
             ClusterRole/ClusterRoleBinding permissions"
        );
    }

    // Create Kubernetes client
    let client = Client::try_default().await?;
    info!("Connected to Kubernetes API server");

    // Readiness and leadership start false and are shared with the probe
    // server. The probe listener must be available before leader election:
    // standby replicas can wait indefinitely for the Lease, and they must
    // answer /healthz and /readyz while they do.
    let readiness = ReadinessState::new();
    let health_addr = args.health_probe_bind_address.clone();
    let health_state = readiness.clone();
    let health_handle = tokio::spawn(async move {
        let app = streamline_operator::health::router(health_state);
        let listener = match tokio::net::TcpListener::bind(&health_addr).await {
            Ok(l) => l,
            Err(e) => {
                error!(
                    "Failed to bind health probe server on {}: {}",
                    health_addr, e
                );
                return;
            }
        };
        info!("Health probe server listening on {}", health_addr);
        if let Err(e) = axum::serve(listener, app).await {
            error!("Health probe server error: {}", e);
        }
    });

    // The process is now a healthy operator: it has a Kubernetes client and a
    // probe server. Mark it ready *before* blocking on the lease.
    //
    // Readiness previously meant "controllers are running", i.e. leadership. A
    // standby therefore failed /readyz forever, and a rolling update of an HA
    // Deployment deadlocked: the new pod waited for the lease the old pod
    // still held, while the rollout waited for the new pod to become ready.
    // Leadership is reported separately, on /leaderz and the
    // `streamline_operator_leader` gauge.
    readiness.mark_ready();

    // Leader election — only the lease holder starts controllers. Standby
    // replicas keep serving liveness/readiness while they wait here.
    let elector = if args.leader_election {
        // The Lease lives in the namespace the operator *runs in*, which is
        // independent of the namespaces it watches.
        let ns = leader_election::detect_namespace(&args.leader_election_namespace);
        info!("Leader election namespace: {}", ns);
        let elector = LeaderElector::new(client.clone(), &ns);
        elector.acquire().await?;
        readiness.mark_leader();
        streamline_operator::metrics::get().inc_leader_transition();
        Some(Arc::new(elector))
    } else {
        // Without leader election this replica is unconditionally the active
        // one, so it reports itself as the leader.
        readiness.mark_leader();
        None
    };

    // Shared HTTP client for Streamline API calls (connection pooling)
    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .pool_max_idle_per_host(4)
        .build()
        .context("Failed to create HTTP client")?;

    // Create controllers.
    //
    // Exactly the three kinds the Streamline server can back. StreamlineBranch,
    // StreamlineContract, and StreamlineMemory deliberately have no controller
    // (see `streamline_operator::crd::generate::Reconciliation` for the
    // per-kind reason); their CRDs are schema-only, are not installed, and are
    // not in the operator's RBAC, so nothing here can spin a reconcile loop
    // against an API that does not exist.
    let cluster_controller = Arc::new(ClusterController::new(client.clone(), scope.clone()));
    let topic_controller = Arc::new(TopicController::new(
        client.clone(),
        http_client.clone(),
        scope.clone(),
    ));
    let user_controller = Arc::new(UserController::new(client.clone(), http_client, scope));

    // Run controllers concurrently
    macro_rules! spawn_controller {
        ($controller:expr, $label:literal) => {{
            let controller = Arc::clone(&$controller);
            let readiness = readiness.clone();
            tokio::spawn(async move {
                if let Err(e) = controller.run().await {
                    error!("{} controller error: {}", $label, e);
                }
                // Reaching here means the controller stopped reconciling.
                readiness.mark_unready();
            })
        }};
    }

    let cluster_handle = spawn_controller!(cluster_controller, "Cluster");
    let topic_handle = spawn_controller!(topic_controller, "Topic");
    let user_handle = spawn_controller!(user_controller, "User");

    // Periodic lease renewal (no-op future when leader election is disabled)
    let elector_for_renew = elector.clone();
    let renew_readiness = readiness.clone();
    let renew_handle = tokio::spawn(async move {
        match elector_for_renew {
            Some(e) => loop {
                tokio::time::sleep(e.renew_interval()).await;
                match e.renew().await {
                    Ok(true) => {}
                    Ok(false) => {
                        error!("Lost leader lease");
                        // Leadership is lost immediately; readiness is handled
                        // by the shutdown path below, so the probe never claims
                        // this replica is still the active operator.
                        renew_readiness.mark_standby();
                        break;
                    }
                    Err(err) => {
                        error!("Failed to renew leader lease: {}", err);
                        renew_readiness.mark_standby();
                        break;
                    }
                }
            },
            None => std::future::pending::<()>().await,
        }
    });

    // Metrics server with actual Prometheus counters
    let metrics_addr = args.metrics_bind_address.clone();
    let metrics_handle = tokio::spawn(async move {
        use axum::{routing::get, Router};
        let app = Router::new().route(
            "/metrics",
            get(|| async { streamline_operator::metrics::get().render() }),
        );
        let listener = match tokio::net::TcpListener::bind(&metrics_addr).await {
            Ok(l) => l,
            Err(e) => {
                error!("Failed to bind metrics server on {}: {}", metrics_addr, e);
                return;
            }
        };
        info!("Metrics server listening on {}", metrics_addr);
        if let Err(e) = axum::serve(listener, app).await {
            error!("Metrics server error: {}", e);
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

    // Any exit from the select above means this replica is shutting down and
    // is no longer reconciling: drop both leadership and readiness before
    // tearing things down.
    readiness.mark_unready();

    // Stop the auxiliary HTTP servers so they release their listening sockets
    health_handle.abort();
    metrics_handle.abort();

    // Release the lease before exiting so a standby replica can take over immediately
    if let Some(e) = &elector {
        e.release().await;
    }

    info!("Streamline Operator shutting down");
    Ok(())
}
