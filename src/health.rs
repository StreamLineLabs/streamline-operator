//! Liveness, readiness, and leadership state for the operator's probe server.
//!
//! The three endpoints answer three different questions, and conflating them
//! breaks HA rollouts:
//!
//! * `/healthz` — is the process alive? Answers `200` as long as it runs.
//! * `/readyz`  — is *this* process a healthy operator, able to reconcile the
//!   moment it holds the lease? A leader-election **standby** is ready: it has
//!   a working Kubernetes client and serves its probes, it is simply waiting
//!   for the current leader to go away.
//! * `/leaderz` — does this replica currently hold the leader Lease?
//!
//! `/readyz` previously meant "controllers are running", which is leadership,
//! not readiness. Under `maxUnavailable: 1` a standby replica never became
//! ready, so a rolling update of an HA Deployment could not make progress: the
//! new pod waited for the lease the old pod still held, and the rollout waited
//! for the new pod to become ready. Deployment readiness therefore stays on
//! `/readyz`, and anything that genuinely needs the active replica (an
//! operator-only Service, an alert, a dashboard) uses `/leaderz`.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Shared readiness and leadership flags for the probe server.
#[derive(Clone, Debug, Default)]
pub struct ReadinessState {
    ready: Arc<AtomicBool>,
    leader: Arc<AtomicBool>,
}

impl ReadinessState {
    /// Create a state that is neither ready nor leader yet.
    pub fn new() -> Self {
        Self {
            ready: Arc::new(AtomicBool::new(false)),
            leader: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Mark the operator process ready: the Kubernetes client is built and the
    /// probe server is serving. This says nothing about leadership.
    pub fn mark_ready(&self) {
        self.ready.store(true, Ordering::SeqCst);
    }

    /// Mark the operator process not ready: it can no longer do its job, even
    /// if it were handed the lease right now.
    ///
    /// Losing readiness also loses leadership: a process that cannot reconcile
    /// must not advertise itself as the active replica.
    pub fn mark_unready(&self) {
        self.ready.store(false, Ordering::SeqCst);
        self.leader.store(false, Ordering::SeqCst);
    }

    /// Record that this replica acquired the leader Lease.
    pub fn mark_leader(&self) {
        self.leader.store(true, Ordering::SeqCst);
        crate::metrics::get().set_leader(true);
    }

    /// Record that this replica lost (or released) the leader Lease.
    ///
    /// The replica stays *ready*: a standby is a healthy operator process.
    pub fn mark_standby(&self) {
        self.leader.store(false, Ordering::SeqCst);
        crate::metrics::get().set_leader(false);
    }

    /// Whether the operator process currently considers itself ready.
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    /// Whether this replica currently holds the leader Lease.
    pub fn is_leader(&self) -> bool {
        self.leader.load(Ordering::SeqCst)
    }
}

/// Liveness handler: the process is up.
pub async fn healthz() -> &'static str {
    "ok"
}

/// Readiness handler: `503` until the operator process is initialised.
///
/// A leader-election standby answers `200`: it is a healthy operator waiting
/// for the lease, and failing it here would deadlock rolling updates.
pub async fn readyz(State(state): State<ReadinessState>) -> impl IntoResponse {
    if state.is_ready() {
        (StatusCode::OK, "ok")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "operator not initialised")
    }
}

/// Leadership handler: `200` only while this replica holds the leader Lease.
pub async fn leaderz(State(state): State<ReadinessState>) -> impl IntoResponse {
    if state.is_leader() {
        (StatusCode::OK, "leader")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "standby")
    }
}

/// Build the probe router.
pub fn router(state: ReadinessState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/leaderz", get(leaderz))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    // unwrap/expect are acceptable in tests; the crate-wide lint targets production code.
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn state_starts_unready_and_not_leader() {
        let state = ReadinessState::new();
        assert!(!state.is_ready());
        assert!(!state.is_leader());
    }

    #[test]
    fn readiness_and_leadership_are_independent() {
        let state = ReadinessState::new();

        // A standby: ready, but not the leader.
        state.mark_ready();
        assert!(state.is_ready());
        assert!(!state.is_leader());

        // Acquiring the lease does not change readiness.
        state.mark_leader();
        assert!(state.is_ready());
        assert!(state.is_leader());

        // Losing the lease demotes to standby but keeps the process ready, so
        // the replica can take the lease again without a restart.
        state.mark_standby();
        assert!(state.is_ready());
        assert!(!state.is_leader());
    }

    #[test]
    fn losing_readiness_also_drops_leadership() {
        let state = ReadinessState::new();
        state.mark_ready();
        state.mark_leader();

        state.mark_unready();
        assert!(!state.is_ready());
        assert!(
            !state.is_leader(),
            "a process that cannot reconcile must not claim leadership"
        );
    }

    #[test]
    fn clones_share_the_same_flags() {
        let state = ReadinessState::new();
        let clone = state.clone();

        state.mark_ready();
        assert!(clone.is_ready());

        clone.mark_leader();
        assert!(state.is_leader());

        clone.mark_unready();
        assert!(!state.is_ready());
    }

    #[tokio::test]
    async fn readyz_reports_service_unavailable_until_the_process_is_initialised() {
        let state = ReadinessState::new();

        let response = readyz(State(state.clone())).await.into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        state.mark_ready();
        let response = readyz(State(state.clone())).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);

        state.mark_unready();
        let response = readyz(State(state)).await.into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// The regression this split exists for: a standby replica must pass the
    /// readiness probe so a rolling update of an HA Deployment can finish.
    #[tokio::test]
    async fn a_leader_election_standby_is_ready_but_not_leader() {
        let state = ReadinessState::new();
        state.mark_ready();

        assert_eq!(
            readyz(State(state.clone())).await.into_response().status(),
            StatusCode::OK,
            "a standby must pass /readyz or rolling updates deadlock"
        );
        assert_eq!(
            leaderz(State(state)).await.into_response().status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "a standby must not answer /leaderz"
        );
    }

    #[tokio::test]
    async fn leaderz_tracks_lease_acquisition_and_loss() {
        let state = ReadinessState::new();
        state.mark_ready();

        state.mark_leader();
        assert_eq!(
            leaderz(State(state.clone())).await.into_response().status(),
            StatusCode::OK
        );

        state.mark_standby();
        assert_eq!(
            leaderz(State(state.clone())).await.into_response().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            readyz(State(state)).await.into_response().status(),
            StatusCode::OK,
            "losing the lease must not make a healthy process unready"
        );
    }

    #[tokio::test]
    async fn healthz_is_independent_of_readiness_and_leadership() {
        assert_eq!(healthz().await, "ok");
    }

    #[test]
    fn router_builds_with_the_shared_state() {
        // Router construction panics on duplicate or malformed routes, so this
        // pins the probe surface the deployment and docs reference by name.
        // `tests/static_manifests.rs` asserts the individual paths.
        let _router: Router = router(ReadinessState::new());
    }
}
