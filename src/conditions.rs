//! Kubernetes-standard status condition helpers
//!
//! Provides constants and builder functions for managing status conditions
//! across all CRDs following the Kubernetes API conventions.

use chrono::Utc;

// Condition status values
pub const CONDITION_TRUE: &str = "True";
pub const CONDITION_FALSE: &str = "False";
pub const CONDITION_UNKNOWN: &str = "Unknown";

// StreamlineCluster condition types
pub const CLUSTER_CONDITION_READY: &str = "Ready";
pub const CLUSTER_CONDITION_PROGRESSING: &str = "Progressing";
pub const CLUSTER_CONDITION_DEGRADED: &str = "Degraded";
pub const CLUSTER_CONDITION_AVAILABLE: &str = "Available";

// StreamlineTopic condition types
pub const TOPIC_CONDITION_READY: &str = "Ready";
pub const TOPIC_CONDITION_SYNCED: &str = "Synced";

// StreamlineUser condition types
pub const USER_CONDITION_READY: &str = "Ready";
pub const USER_CONDITION_CREDENTIALS_READY: &str = "CredentialsReady";

// Finalizer names
pub const CLUSTER_FINALIZER: &str = "streamline.io/cluster-cleanup";
pub const TOPIC_FINALIZER: &str = "streamline.io/topic-cleanup";
pub const USER_FINALIZER: &str = "streamline.io/user-cleanup";

/// Build a condition with the current timestamp.
pub fn build_condition(
    condition_type: &str,
    status: &str,
    reason: &str,
    message: &str,
) -> ConditionFields {
    ConditionFields {
        condition_type: condition_type.to_string(),
        status: status.to_string(),
        last_transition_time: Some(Utc::now().to_rfc3339()),
        reason: Some(reason.to_string()),
        message: Some(message.to_string()),
    }
}

/// Generic condition fields that can be converted into any CRD-specific condition type.
#[derive(Debug, Clone)]
pub struct ConditionFields {
    pub condition_type: String,
    pub status: String,
    pub last_transition_time: Option<String>,
    pub reason: Option<String>,
    pub message: Option<String>,
}

impl ConditionFields {
    pub fn into_cluster_condition(self) -> crate::crd::ClusterCondition {
        crate::crd::ClusterCondition {
            r#type: self.condition_type,
            status: self.status,
            last_transition_time: self.last_transition_time,
            reason: self.reason,
            message: self.message,
        }
    }

    pub fn into_topic_condition(self) -> crate::crd::TopicCondition {
        crate::crd::TopicCondition {
            r#type: self.condition_type,
            status: self.status,
            last_transition_time: self.last_transition_time,
            reason: self.reason,
            message: self.message,
        }
    }

    pub fn into_user_condition(self) -> crate::crd::UserCondition {
        crate::crd::UserCondition {
            r#type: self.condition_type,
            status: self.status,
            last_transition_time: self.last_transition_time,
            reason: self.reason,
            message: self.message,
        }
    }
}

/// Set or update a condition in a list, preserving lastTransitionTime when status hasn't changed.
pub fn set_condition(conditions: &mut Vec<ConditionFields>, new: ConditionFields) {
    if let Some(existing) = conditions
        .iter_mut()
        .find(|c| c.condition_type == new.condition_type)
    {
        if existing.status != new.status {
            *existing = new;
        } else {
            existing.reason = new.reason;
            existing.message = new.message;
        }
    } else {
        conditions.push(new);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_condition() {
        let cond = build_condition(CLUSTER_CONDITION_READY, CONDITION_TRUE, "AllReady", "All brokers ready");
        assert_eq!(cond.condition_type, "Ready");
        assert_eq!(cond.status, "True");
        assert!(cond.last_transition_time.is_some());
        assert_eq!(cond.reason.as_deref(), Some("AllReady"));
        assert_eq!(cond.message.as_deref(), Some("All brokers ready"));
    }

    #[test]
    fn test_set_condition_adds_new() {
        let mut conditions = Vec::new();
        let cond = build_condition("Ready", CONDITION_TRUE, "OK", "ok");
        set_condition(&mut conditions, cond);
        assert_eq!(conditions.len(), 1);
    }

    #[test]
    fn test_set_condition_preserves_transition_time_on_same_status() {
        let mut conditions = Vec::new();
        let cond1 = ConditionFields {
            condition_type: "Ready".to_string(),
            status: CONDITION_TRUE.to_string(),
            last_transition_time: Some("2024-01-01T00:00:00Z".to_string()),
            reason: Some("First".to_string()),
            message: Some("first".to_string()),
        };
        conditions.push(cond1);

        let cond2 = build_condition("Ready", CONDITION_TRUE, "Second", "second");
        set_condition(&mut conditions, cond2);

        assert_eq!(conditions.len(), 1);
        // Transition time preserved because status didn't change
        assert_eq!(
            conditions[0].last_transition_time.as_deref(),
            Some("2024-01-01T00:00:00Z")
        );
        assert_eq!(conditions[0].reason.as_deref(), Some("Second"));
    }

    #[test]
    fn test_set_condition_updates_transition_time_on_status_change() {
        let mut conditions = Vec::new();
        let cond1 = ConditionFields {
            condition_type: "Ready".to_string(),
            status: CONDITION_FALSE.to_string(),
            last_transition_time: Some("2024-01-01T00:00:00Z".to_string()),
            reason: Some("NotReady".to_string()),
            message: Some("not ready".to_string()),
        };
        conditions.push(cond1);

        let cond2 = build_condition("Ready", CONDITION_TRUE, "AllReady", "all ready");
        set_condition(&mut conditions, cond2);

        assert_eq!(conditions.len(), 1);
        // Transition time updated because status changed
        assert_ne!(
            conditions[0].last_transition_time.as_deref(),
            Some("2024-01-01T00:00:00Z")
        );
    }

    #[test]
    fn test_into_cluster_condition() {
        let cond = build_condition("Ready", CONDITION_TRUE, "OK", "ok");
        let cc = cond.into_cluster_condition();
        assert_eq!(cc.r#type, "Ready");
        assert_eq!(cc.status, "True");
    }

    #[test]
    fn test_into_topic_condition() {
        let cond = build_condition("Synced", CONDITION_TRUE, "OK", "ok");
        let tc = cond.into_topic_condition();
        assert_eq!(tc.r#type, "Synced");
    }

    #[test]
    fn test_into_user_condition() {
        let cond = build_condition("CredentialsReady", CONDITION_TRUE, "OK", "ok");
        let uc = cond.into_user_condition();
        assert_eq!(uc.r#type, "CredentialsReady");
    }
}
