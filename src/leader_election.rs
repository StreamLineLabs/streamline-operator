//! Kubernetes Lease-based leader election for HA operator deployments.
//!
//! When multiple operator replicas run simultaneously, only the leader
//! (the holder of the Lease) runs the reconciliation controllers. Other
//! replicas block in [`LeaderElector::acquire`] until the current leader's
//! lease expires.

use chrono::Utc;
use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{MicroTime, ObjectMeta};
use kube::api::{Api, PostParams};
use kube::Client;
use std::time::Duration;
use tracing::{debug, info, warn};

const LEASE_NAME: &str = "streamline-operator-leader";
const LEASE_DURATION_SECS: i32 = 15;
const RENEW_INTERVAL: Duration = Duration::from_secs(10);
const RETRY_INTERVAL: Duration = Duration::from_secs(5);

/// Detects the namespace for leader election.
///
/// Priority: explicit argument → service account file → `"default"`.
pub fn detect_namespace(explicit: &str) -> String {
    if !explicit.is_empty() {
        return explicit.to_string();
    }
    std::fs::read_to_string("/var/run/secrets/kubernetes.io/serviceaccount/namespace")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "default".to_string())
}

/// Lease-based leader election for Kubernetes operator HA deployments.
///
/// Uses a Kubernetes `coordination.k8s.io/v1` Lease object with optimistic
/// concurrency (via `resourceVersion`) to ensure only one operator instance
/// runs the controllers at a time.
pub struct LeaderElector {
    lease_api: Api<Lease>,
    identity: String,
}

impl LeaderElector {
    pub fn new(client: Client, namespace: &str) -> Self {
        let lease_api = Api::<Lease>::namespaced(client, namespace);
        let identity = std::env::var("POD_NAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| format!("operator-{:08x}", rand::random::<u32>()));
        info!(identity = %identity, namespace = %namespace, "Initialized leader elector");
        Self { lease_api, identity }
    }

    /// Blocks until the lease is successfully acquired.
    pub async fn acquire(&self) -> anyhow::Result<()> {
        info!(identity = %self.identity, "Waiting to acquire leader lease '{}'", LEASE_NAME);
        loop {
            match self.try_acquire().await {
                Ok(true) => {
                    info!(identity = %self.identity, "Acquired leader lease");
                    return Ok(());
                }
                Ok(false) => {
                    debug!("Lease held by another instance, retrying in {:?}", RETRY_INTERVAL);
                    tokio::time::sleep(RETRY_INTERVAL).await;
                }
                Err(e) => {
                    warn!("Lease acquisition error: {}, retrying in {:?}", e, RETRY_INTERVAL);
                    tokio::time::sleep(RETRY_INTERVAL).await;
                }
            }
        }
    }

    /// Renews the lease. Returns `Ok(true)` if still leader, `Ok(false)` if lost.
    pub async fn renew(&self) -> anyhow::Result<bool> {
        let lease = self.lease_api.get(LEASE_NAME).await?;
        let holder = lease
            .spec
            .as_ref()
            .and_then(|s| s.holder_identity.as_deref());
        if holder != Some(self.identity.as_str()) {
            return Ok(false);
        }

        let mut updated = lease.clone();
        if let Some(ref mut spec) = updated.spec {
            spec.renew_time = Some(MicroTime(Utc::now()));
        }

        match self
            .lease_api
            .replace(LEASE_NAME, &PostParams::default(), &updated)
            .await
        {
            Ok(_) => {
                debug!("Renewed leader lease");
                Ok(true)
            }
            Err(kube::Error::Api(ae)) if ae.code == 409 => {
                warn!("Lease conflict during renewal — lost leadership");
                Ok(false)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Releases the lease by clearing the holder identity.
    pub async fn release(&self) {
        info!(identity = %self.identity, "Releasing leader lease");
        let lease = match self.lease_api.get(LEASE_NAME).await {
            Ok(l) => l,
            Err(e) => {
                warn!("Failed to read lease for release: {}", e);
                return;
            }
        };

        let holder = lease
            .spec
            .as_ref()
            .and_then(|s| s.holder_identity.as_deref());
        if holder != Some(self.identity.as_str()) {
            debug!("Lease not held by us, skipping release");
            return;
        }

        let mut updated = lease.clone();
        if let Some(ref mut spec) = updated.spec {
            spec.holder_identity = None;
        }
        match self
            .lease_api
            .replace(LEASE_NAME, &PostParams::default(), &updated)
            .await
        {
            Ok(_) => info!("Released leader lease"),
            Err(e) => warn!("Failed to release leader lease: {}", e),
        }
    }

    /// Returns the recommended interval between lease renewals.
    pub fn renew_interval(&self) -> Duration {
        RENEW_INTERVAL
    }

    async fn try_acquire(&self) -> anyhow::Result<bool> {
        let now = MicroTime(Utc::now());

        match self.lease_api.get(LEASE_NAME).await {
            Ok(existing) => {
                let spec = existing.spec.as_ref();
                let holder = spec.and_then(|s| s.holder_identity.as_deref());

                if holder == Some(self.identity.as_str()) {
                    // Already ours — renew
                    self.update_lease(&existing, &now, false).await
                } else if Self::is_expired(spec) {
                    // Expired — attempt takeover
                    self.update_lease(&existing, &now, true).await
                } else {
                    Ok(false)
                }
            }
            Err(kube::Error::Api(ae)) if ae.code == 404 => self.create_lease(&now).await,
            Err(e) => Err(e.into()),
        }
    }

    fn is_expired(spec: Option<&LeaseSpec>) -> bool {
        let renew_time = spec.and_then(|s| s.renew_time.as_ref());
        let duration_secs = spec
            .and_then(|s| s.lease_duration_seconds)
            .unwrap_or(LEASE_DURATION_SECS) as i64;

        match renew_time {
            Some(MicroTime(t)) => Utc::now().signed_duration_since(*t).num_seconds() > duration_secs,
            None => true,
        }
    }

    async fn create_lease(&self, now: &MicroTime) -> anyhow::Result<bool> {
        let lease = Lease {
            metadata: ObjectMeta {
                name: Some(LEASE_NAME.to_string()),
                ..Default::default()
            },
            spec: Some(LeaseSpec {
                holder_identity: Some(self.identity.clone()),
                lease_duration_seconds: Some(LEASE_DURATION_SECS),
                acquire_time: Some(now.clone()),
                renew_time: Some(now.clone()),
                lease_transitions: Some(0),
            }),
        };
        match self
            .lease_api
            .create(&PostParams::default(), &lease)
            .await
        {
            Ok(_) => Ok(true),
            Err(kube::Error::Api(ae)) if ae.code == 409 => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    async fn update_lease(
        &self,
        existing: &Lease,
        now: &MicroTime,
        takeover: bool,
    ) -> anyhow::Result<bool> {
        let prev = existing.spec.as_ref();
        let transitions = prev.and_then(|s| s.lease_transitions).unwrap_or(0);

        let mut updated = existing.clone();
        updated.spec = Some(LeaseSpec {
            holder_identity: Some(self.identity.clone()),
            lease_duration_seconds: Some(LEASE_DURATION_SECS),
            acquire_time: if takeover {
                Some(now.clone())
            } else {
                prev.and_then(|s| s.acquire_time.clone())
            },
            renew_time: Some(now.clone()),
            lease_transitions: Some(if takeover { transitions + 1 } else { transitions }),
        });

        match self
            .lease_api
            .replace(LEASE_NAME, &PostParams::default(), &updated)
            .await
        {
            Ok(_) => Ok(true),
            Err(kube::Error::Api(ae)) if ae.code == 409 => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}
