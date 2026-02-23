//! StreamlineBackup CRD
//!
//! Manages backup and restore operations for Streamline clusters.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Spec for a StreamlineBackup resource
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "streamline.io",
    version = "v1beta1",
    kind = "StreamlineBackup",
    namespaced,
    status = "BackupStatus",
    printcolumn = r#"{"name":"Cluster","type":"string","jsonPath":".spec.clusterRef"}"#,
    printcolumn = r#"{"name":"Type","type":"string","jsonPath":".spec.backupType"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct BackupSpec {
    /// Reference to the StreamlineCluster to back up
    pub cluster_ref: String,

    /// Type of backup
    #[serde(default = "default_backup_type")]
    pub backup_type: BackupType,

    /// Storage destination for the backup
    pub storage: BackupStorage,

    /// Schedule (cron expression) for recurring backups (empty = one-shot)
    #[serde(default)]
    pub schedule: Option<String>,

    /// Maximum number of backups to retain
    #[serde(default = "default_retention_count")]
    pub retention_count: u32,

    /// Topics to back up (empty = all topics)
    #[serde(default)]
    pub topics: Vec<String>,

    /// Include consumer group offsets in backup
    #[serde(default = "default_true")]
    pub include_offsets: bool,

    /// Include ACLs and user credentials in backup
    #[serde(default = "default_true")]
    pub include_acls: bool,

    /// Include topic configurations
    #[serde(default = "default_true")]
    pub include_configs: bool,

    /// Compression algorithm for backup data
    #[serde(default = "default_compression")]
    pub compression: String,
}

fn default_backup_type() -> BackupType {
    BackupType::Full
}
fn default_retention_count() -> u32 {
    5
}
fn default_true() -> bool {
    true
}
fn default_compression() -> String {
    "zstd".to_string()
}

/// Type of backup operation
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub enum BackupType {
    /// Full backup of all data and metadata
    Full,
    /// Incremental backup since last full/incremental backup
    Incremental,
    /// Metadata-only backup (topics, configs, ACLs, offsets — no message data)
    MetadataOnly,
}

impl Default for BackupType {
    fn default() -> Self {
        Self::Full
    }
}

/// Backup storage destination
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackupStorage {
    /// Storage type
    #[serde(rename = "type")]
    pub storage_type: BackupStorageType,

    /// S3-compatible bucket name
    #[serde(default)]
    pub bucket: Option<String>,

    /// S3-compatible endpoint URL
    #[serde(default)]
    pub endpoint: Option<String>,

    /// Storage prefix/path
    #[serde(default)]
    pub prefix: Option<String>,

    /// Secret reference for storage credentials
    #[serde(default)]
    pub credentials_secret: Option<String>,

    /// AWS/GCS region
    #[serde(default)]
    pub region: Option<String>,

    /// PVC name for local storage
    #[serde(default)]
    pub pvc_name: Option<String>,
}

/// Storage types for backups
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub enum BackupStorageType {
    /// Amazon S3 or S3-compatible storage
    S3,
    /// Google Cloud Storage
    GCS,
    /// Azure Blob Storage
    AzureBlob,
    /// Persistent Volume Claim (local K8s storage)
    PVC,
}

/// Status of a StreamlineBackup resource
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct BackupStatus {
    /// Current phase of the backup
    pub phase: BackupPhase,

    /// Timestamp when the last backup started
    #[serde(default)]
    pub last_backup_time: Option<String>,

    /// Timestamp when the last backup completed
    #[serde(default)]
    pub last_successful_time: Option<String>,

    /// Number of completed backups
    #[serde(default)]
    pub completed_backups: u32,

    /// Size of the last backup in bytes
    #[serde(default)]
    pub last_backup_size_bytes: Option<u64>,

    /// Duration of the last backup in seconds
    #[serde(default)]
    pub last_backup_duration_secs: Option<u64>,

    /// Number of topics backed up
    #[serde(default)]
    pub topics_backed_up: u32,

    /// Error message (if phase is Failed)
    #[serde(default)]
    pub error: Option<String>,

    /// Conditions
    #[serde(default)]
    pub conditions: Vec<BackupCondition>,
}

/// Phase of a backup operation
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
pub enum BackupPhase {
    /// Backup is pending scheduling
    #[default]
    Pending,
    /// Backup is in progress
    Running,
    /// Backup completed successfully
    Completed,
    /// Backup failed
    Failed,
    /// Backup was deleted/expired by retention policy
    Expired,
}

/// Condition for backup status
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackupCondition {
    /// Type of condition
    pub condition_type: String,
    /// Whether the condition is met
    pub status: String,
    /// Human-readable reason
    pub reason: String,
    /// Detailed message
    pub message: String,
    /// Last transition time
    pub last_transition_time: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backup_spec_defaults() {
        let spec = BackupSpec {
            cluster_ref: "my-cluster".to_string(),
            backup_type: BackupType::Full,
            storage: BackupStorage {
                storage_type: BackupStorageType::S3,
                bucket: Some("my-backups".to_string()),
                endpoint: None,
                prefix: Some("streamline/".to_string()),
                credentials_secret: Some("backup-creds".to_string()),
                region: Some("us-east-1".to_string()),
                pvc_name: None,
            },
            schedule: Some("0 2 * * *".to_string()),
            retention_count: 7,
            topics: vec![],
            include_offsets: true,
            include_acls: true,
            include_configs: true,
            compression: "zstd".to_string(),
        };

        assert_eq!(spec.cluster_ref, "my-cluster");
        assert_eq!(spec.retention_count, 7);
        assert!(spec.include_offsets);
    }

    #[test]
    fn test_backup_phase_default() {
        let status = BackupStatus::default();
        assert_eq!(status.phase, BackupPhase::Pending);
        assert_eq!(status.completed_backups, 0);
    }
}
