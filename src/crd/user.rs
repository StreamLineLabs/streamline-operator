//! StreamlineUser Custom Resource Definition
//!
//! Defines the specification for creating and managing users within a Streamline cluster.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// StreamlineUser is the Schema for the streamlineusers API
#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "streamline.io",
    version = "v1alpha1",
    kind = "StreamlineUser",
    namespaced,
    status = "UserStatus",
    shortname = "slu",
    printcolumn = r#"{"name":"Cluster","type":"string","jsonPath":".spec.clusterRef"}"#,
    printcolumn = r#"{"name":"Auth","type":"string","jsonPath":".spec.authentication.type"}"#,
    printcolumn = r#"{"name":"Ready","type":"boolean","jsonPath":".status.ready"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct UserSpec {
    /// Reference to the StreamlineCluster
    pub cluster_ref: String,

    /// Authentication configuration
    pub authentication: UserAuthentication,

    /// Authorization/permissions configuration
    #[serde(default)]
    pub authorization: UserAuthorization,

    /// Quotas for the user
    #[serde(default)]
    pub quotas: Option<UserQuotas>,
}

/// User authentication configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UserAuthentication {
    /// Authentication type (scram-sha-256, scram-sha-512, plain)
    pub r#type: AuthenticationType,

    /// Password configuration
    #[serde(default)]
    pub credentials: Option<UserCredentials>,
}

/// Authentication type
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AuthenticationType {
    /// SCRAM-SHA-256 authentication
    ScramSha256,
    /// SCRAM-SHA-512 authentication
    ScramSha512,
    /// Plain authentication (not recommended for production)
    Plain,
    /// TLS client certificate authentication
    TlsClientAuth,
}

/// User credentials configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UserCredentials {
    /// Reference to a secret containing the password
    #[serde(default)]
    pub secret_ref: Option<SecretReference>,
    /// Password value (for simple setups, prefer secretRef)
    #[serde(default)]
    pub value: Option<String>,
}

/// Reference to a Kubernetes secret
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SecretReference {
    /// Name of the secret
    pub name: String,
    /// Key within the secret containing the password
    #[serde(default = "default_password_key")]
    pub key: String,
}

/// User authorization configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct UserAuthorization {
    /// Authorization type
    #[serde(default)]
    pub r#type: AuthorizationType,

    /// ACL rules for the user
    #[serde(default)]
    pub acls: Vec<UserAcl>,

    /// Role bindings for RBAC
    #[serde(default)]
    pub roles: Vec<String>,
}

/// Authorization type
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AuthorizationType {
    /// Simple ACL-based authorization
    #[default]
    Simple,
    /// Role-based access control
    Rbac,
}

/// ACL rule for the user
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UserAcl {
    /// Resource type (topic, group, cluster, transactional-id)
    pub resource_type: ResourceType,

    /// Resource name (exact or prefixed)
    pub resource_name: String,

    /// Pattern type (literal, prefixed)
    #[serde(default)]
    pub pattern_type: PatternType,

    /// Operations to allow
    pub operations: Vec<UserPermission>,

    /// Permission type (allow or deny)
    #[serde(default)]
    pub permission: PermissionType,
}

/// Resource type for ACL
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ResourceType {
    /// Topic resource
    Topic,
    /// Consumer group resource
    Group,
    /// Cluster resource
    Cluster,
    /// Transactional ID resource
    TransactionalId,
}

/// Pattern type for resource matching
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PatternType {
    /// Exact match
    #[default]
    Literal,
    /// Prefix match
    Prefixed,
}

/// Permission/operation type
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum UserPermission {
    /// Read from topic or describe
    Read,
    /// Write to topic
    Write,
    /// Create topics or groups
    Create,
    /// Delete topics or groups
    Delete,
    /// Alter topic configuration
    Alter,
    /// Describe topic/group/cluster
    Describe,
    /// Describe configuration
    DescribeConfigs,
    /// Alter configuration
    AlterConfigs,
    /// Idempotent write
    IdempotentWrite,
    /// All operations
    All,
}

/// Permission type (allow or deny)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PermissionType {
    /// Allow the operation
    #[default]
    Allow,
    /// Deny the operation
    Deny,
}

/// User quotas
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UserQuotas {
    /// Producer byte rate limit (bytes/sec)
    #[serde(default)]
    pub producer_byte_rate: Option<i64>,
    /// Consumer byte rate limit (bytes/sec)
    #[serde(default)]
    pub consumer_byte_rate: Option<i64>,
    /// Request rate limit (requests/sec)
    #[serde(default)]
    pub request_rate: Option<i32>,
    /// Connection rate limit (connections/sec)
    #[serde(default)]
    pub connection_rate: Option<i32>,
}

/// Status of the StreamlineUser
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct UserStatus {
    /// Whether the user is ready
    #[serde(default)]
    pub ready: bool,

    /// Current phase of the user
    #[serde(default)]
    pub phase: UserPhase,

    /// Username in the cluster
    #[serde(default)]
    pub username: Option<String>,

    /// Secret containing generated credentials
    #[serde(default)]
    pub credentials_secret: Option<String>,

    /// Conditions representing user state
    #[serde(default)]
    pub conditions: Vec<UserCondition>,

    /// Last observed generation
    #[serde(default)]
    pub observed_generation: Option<i64>,

    /// Last update timestamp
    #[serde(default)]
    pub last_updated: Option<String>,

    /// Error message if creation failed
    #[serde(default)]
    pub error_message: Option<String>,
}

/// Phase of the user lifecycle
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default, PartialEq, Eq)]
pub enum UserPhase {
    /// User is being created
    #[default]
    Pending,
    /// User is ready
    Ready,
    /// User is being updated
    Updating,
    /// User creation/update failed
    Failed,
    /// User is being deleted
    Terminating,
}

/// Condition of the user
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UserCondition {
    /// Type of condition
    pub r#type: String,
    /// Status of the condition (True, False, Unknown)
    pub status: String,
    /// Last time the condition transitioned
    #[serde(default)]
    pub last_transition_time: Option<String>,
    /// Reason for the condition
    #[serde(default)]
    pub reason: Option<String>,
    /// Human-readable message
    #[serde(default)]
    pub message: Option<String>,
}

fn default_password_key() -> String {
    "password".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_spec_parsing() {
        let json = r#"{
            "clusterRef": "my-cluster",
            "authentication": {
                "type": "scram-sha256"
            }
        }"#;
        let spec: UserSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.cluster_ref, "my-cluster");
        assert_eq!(spec.authentication.r#type, AuthenticationType::ScramSha256);
    }

    #[test]
    fn test_acl_parsing() {
        let json = r#"{
            "resourceType": "topic",
            "resourceName": "my-topic",
            "patternType": "literal",
            "operations": ["read", "write"],
            "permission": "allow"
        }"#;
        let acl: UserAcl = serde_json::from_str(json).unwrap();
        assert_eq!(acl.resource_type, ResourceType::Topic);
        assert_eq!(acl.resource_name, "my-topic");
        assert_eq!(acl.operations.len(), 2);
    }

    #[test]
    fn test_user_phase_default() {
        let phase = UserPhase::default();
        assert_eq!(phase, UserPhase::Pending);
    }
}
