//! Kubernetes Operator Hub for discovering, installing, and managing
//! companion operators that integrate with Streamline.
//!
//! The hub acts as a local registry of operators that can be installed
//! alongside the Streamline operator to provide CDC sources, search sinks,
//! tiered storage, and observability integrations.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Bundled operators shipped with every Streamline operator release.
pub const BUNDLED_OPERATORS: &[(&str, &str, &str)] = &[
    (
        "postgresql",
        "5.0.0",
        "PostgreSQL operator for CDC source integration",
    ),
    (
        "redis",
        "7.0.0",
        "Redis operator for cache-aside streaming patterns",
    ),
    (
        "elasticsearch",
        "8.0.0",
        "Elasticsearch operator for search sink integration",
    ),
    (
        "minio",
        "5.0.0",
        "MinIO operator for S3-compatible tiered storage",
    ),
    (
        "prometheus",
        "0.70.0",
        "Prometheus operator for metrics collection",
    ),
];

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

fn default_registry_url() -> String {
    "https://hub.streamline.dev/operators".to_string()
}
fn default_max_installed() -> usize {
    50
}

/// Configuration for the operator hub.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubConfig {
    #[serde(default = "default_registry_url")]
    pub registry_url: String,

    #[serde(default)]
    pub auto_update: bool,

    #[serde(default = "default_max_installed")]
    pub max_installed: usize,

    #[serde(default)]
    pub require_verified: bool,
}

impl Default for HubConfig {
    fn default() -> Self {
        Self {
            registry_url: default_registry_url(),
            auto_update: false,
            max_installed: 50,
            require_verified: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Operator metadata
// ---------------------------------------------------------------------------

/// Category of an operator in the hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperatorCategory {
    Database,
    Cache,
    MessageQueue,
    Monitoring,
    Storage,
    Networking,
    Security,
    Custom(String),
}

/// How the operator integrates with Streamline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IntegrationType {
    Source,
    Sink,
    Sidecar,
    Standalone,
}

/// An operator available in the hub registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubOperator {
    pub name: String,
    pub version: String,
    pub description: String,
    pub category: OperatorCategory,
    pub author: String,
    pub verified: bool,
    pub downloads: u64,
    pub rating: f64,
    pub crds: Vec<String>,
    pub streamline_integration: IntegrationType,
    pub min_k8s_version: String,
    pub published_at: String,
}

// ---------------------------------------------------------------------------
// Installed operator tracking
// ---------------------------------------------------------------------------

/// Installation status of an operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstallStatus {
    Installing,
    Running,
    Failed(String),
    Upgrading,
    Removing,
}

/// An operator that has been installed in the cluster.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledOperator {
    pub name: String,
    pub version: String,
    pub namespace: String,
    pub status: InstallStatus,
    pub installed_at: String,
    pub last_reconcile_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Hub statistics
// ---------------------------------------------------------------------------

/// Summary statistics for the operator hub.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubStats {
    pub available_count: usize,
    pub installed_count: usize,
    pub updates_available: usize,
}

// ---------------------------------------------------------------------------
// OperatorHub
// ---------------------------------------------------------------------------

/// Central registry for discovering, installing, and managing companion
/// operators that integrate with Streamline.
pub struct OperatorHub {
    operators: Arc<RwLock<HashMap<String, HubOperator>>>,
    installed: Arc<RwLock<HashMap<String, InstalledOperator>>>,
    config: HubConfig,
}

impl OperatorHub {
    /// Create a new hub and seed it with the bundled operators.
    pub fn new(config: HubConfig) -> Self {
        let mut operators = HashMap::new();

        for &(name, version, description) in BUNDLED_OPERATORS {
            let category = match name {
                "postgresql" => OperatorCategory::Database,
                "redis" => OperatorCategory::Cache,
                "elasticsearch" => OperatorCategory::Storage,
                "minio" => OperatorCategory::Storage,
                "prometheus" => OperatorCategory::Monitoring,
                _ => OperatorCategory::Custom(name.to_string()),
            };

            let integration = match name {
                "postgresql" => IntegrationType::Source,
                "redis" => IntegrationType::Sidecar,
                "elasticsearch" => IntegrationType::Sink,
                "minio" => IntegrationType::Standalone,
                "prometheus" => IntegrationType::Sidecar,
                _ => IntegrationType::Standalone,
            };

            operators.insert(
                name.to_string(),
                HubOperator {
                    name: name.to_string(),
                    version: version.to_string(),
                    description: description.to_string(),
                    category,
                    author: "streamline".to_string(),
                    verified: true,
                    downloads: 0,
                    rating: 0.0,
                    crds: vec![format!("{}s.streamline.dev", name)],
                    streamline_integration: integration,
                    min_k8s_version: "1.26".to_string(),
                    published_at: "2025-01-01T00:00:00Z".to_string(),
                },
            );
        }

        info!(
            count = operators.len(),
            "Operator hub initialised with bundled operators"
        );

        Self {
            operators: Arc::new(RwLock::new(operators)),
            installed: Arc::new(RwLock::new(HashMap::new())),
            config,
        }
    }

    /// List all operators available in the hub.
    pub async fn list_available(&self) -> Vec<HubOperator> {
        let ops = self.operators.read().await;
        ops.values().cloned().collect()
    }

    /// Search for operators whose name or description matches `query`.
    pub async fn search(&self, query: &str) -> Vec<HubOperator> {
        let q = query.to_lowercase();
        let ops = self.operators.read().await;
        ops.values()
            .filter(|op| {
                op.name.to_lowercase().contains(&q) || op.description.to_lowercase().contains(&q)
            })
            .cloned()
            .collect()
    }

    /// Get detailed information about a specific operator.
    pub async fn get_operator_info(&self, name: &str) -> Option<HubOperator> {
        let ops = self.operators.read().await;
        ops.get(name).cloned()
    }

    /// Install an operator into the given namespace.
    pub async fn install(&self, name: &str) -> Result<InstalledOperator, String> {
        // Check the operator exists.
        let op = {
            let ops = self.operators.read().await;
            ops.get(name).cloned()
        };

        let op = match op {
            Some(o) => o,
            None => {
                warn!(name, "Attempted to install unknown operator");
                return Err(format!("operator '{}' not found in hub", name));
            }
        };

        if self.config.require_verified && !op.verified {
            return Err(format!("operator '{}' is not verified", name));
        }

        // Check capacity.
        {
            let inst = self.installed.read().await;
            if inst.len() >= self.config.max_installed {
                return Err(format!(
                    "maximum installed operators ({}) reached",
                    self.config.max_installed
                ));
            }
            if inst.contains_key(name) {
                return Err(format!("operator '{}' is already installed", name));
            }
        }

        let installed_op = InstalledOperator {
            name: name.to_string(),
            version: op.version.clone(),
            namespace: "streamline-system".to_string(),
            status: InstallStatus::Running,
            installed_at: chrono::Utc::now().to_rfc3339(),
            last_reconcile_at: None,
        };

        self.installed
            .write()
            .await
            .insert(name.to_string(), installed_op.clone());

        info!(name, version = %op.version, "Operator installed");
        Ok(installed_op)
    }

    /// Uninstall an operator.
    pub async fn uninstall(&self, name: &str) -> Result<(), String> {
        let mut inst = self.installed.write().await;
        match inst.remove(name) {
            Some(_) => {
                info!(name, "Operator uninstalled");
                Ok(())
            }
            None => {
                debug!(name, "Attempted to uninstall operator that is not installed");
                Err(format!("operator '{}' is not installed", name))
            }
        }
    }

    /// Upgrade an installed operator to the latest available version.
    pub async fn upgrade(&self, name: &str) -> Result<InstalledOperator, String> {
        let latest_version = {
            let ops = self.operators.read().await;
            match ops.get(name) {
                Some(o) => o.version.clone(),
                None => return Err(format!("operator '{}' not found in hub", name)),
            }
        };

        let mut inst = self.installed.write().await;
        match inst.get_mut(name) {
            Some(op) => {
                if op.version == latest_version {
                    return Err(format!(
                        "operator '{}' is already at latest version {}",
                        name, latest_version
                    ));
                }
                info!(
                    name,
                    from = %op.version,
                    to = %latest_version,
                    "Upgrading operator"
                );
                op.version = latest_version;
                op.status = InstallStatus::Running;
                op.last_reconcile_at = Some(chrono::Utc::now().to_rfc3339());
                Ok(op.clone())
            }
            None => Err(format!("operator '{}' is not installed", name)),
        }
    }

    /// List all installed operators.
    pub async fn get_installed(&self) -> Vec<InstalledOperator> {
        let inst = self.installed.read().await;
        inst.values().cloned().collect()
    }

    /// Check which installed operators have newer versions available.
    pub async fn check_updates(&self) -> Vec<(String, String, String)> {
        let ops = self.operators.read().await;
        let inst = self.installed.read().await;

        let mut updates = Vec::new();
        for (name, installed) in inst.iter() {
            if let Some(available) = ops.get(name) {
                if available.version != installed.version {
                    updates.push((
                        name.clone(),
                        installed.version.clone(),
                        available.version.clone(),
                    ));
                }
            }
        }

        if !updates.is_empty() {
            info!(count = updates.len(), "Updates available for installed operators");
        }

        updates
    }

    /// Return summary statistics for the hub.
    pub async fn stats(&self) -> HubStats {
        let ops = self.operators.read().await;
        let inst = self.installed.read().await;

        let updates_available = {
            let mut count = 0;
            for (name, installed) in inst.iter() {
                if let Some(available) = ops.get(name) {
                    if available.version != installed.version {
                        count += 1;
                    }
                }
            }
            count
        };

        HubStats {
            available_count: ops.len(),
            installed_count: inst.len(),
            updates_available,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn default_hub() -> OperatorHub {
        OperatorHub::new(HubConfig::default())
    }

    // -- Config tests --------------------------------------------------------

    #[test]
    fn test_hub_config_defaults() {
        let cfg = HubConfig::default();
        assert_eq!(cfg.registry_url, "https://hub.streamline.dev/operators");
        assert!(!cfg.auto_update);
        assert_eq!(cfg.max_installed, 50);
        assert!(!cfg.require_verified);
    }

    #[test]
    fn test_hub_config_serde_defaults() {
        let cfg: HubConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.max_installed, 50);
        assert!(!cfg.auto_update);
    }

    #[test]
    fn test_hub_config_serde_override() {
        let json = r#"{"autoUpdate": true, "maxInstalled": 10}"#;
        let cfg: HubConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.auto_update);
        assert_eq!(cfg.max_installed, 10);
    }

    // -- Bundled operators ---------------------------------------------------

    #[test]
    fn test_bundled_operators_count() {
        assert_eq!(BUNDLED_OPERATORS.len(), 5);
    }

    #[tokio::test]
    async fn test_bundled_operators_seeded() {
        let hub = default_hub();
        let available = hub.list_available().await;
        assert_eq!(available.len(), 5);
    }

    #[tokio::test]
    async fn test_bundled_operators_verified() {
        let hub = default_hub();
        let available = hub.list_available().await;
        assert!(available.iter().all(|op| op.verified));
    }

    // -- Search --------------------------------------------------------------

    #[tokio::test]
    async fn test_search_by_name() {
        let hub = default_hub();
        let results = hub.search("redis").await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "redis");
    }

    #[tokio::test]
    async fn test_search_by_description() {
        let hub = default_hub();
        let results = hub.search("CDC").await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "postgresql");
    }

    #[tokio::test]
    async fn test_search_case_insensitive() {
        let hub = default_hub();
        let results = hub.search("REDIS").await;
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn test_search_no_results() {
        let hub = default_hub();
        let results = hub.search("nonexistent").await;
        assert!(results.is_empty());
    }

    // -- Install / uninstall -------------------------------------------------

    #[tokio::test]
    async fn test_install_success() {
        let hub = default_hub();
        let result = hub.install("redis").await;
        assert!(result.is_ok());
        let installed = result.unwrap();
        assert_eq!(installed.name, "redis");
        assert_eq!(installed.status, InstallStatus::Running);
    }

    #[tokio::test]
    async fn test_install_unknown_operator() {
        let hub = default_hub();
        let result = hub.install("unknown").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[tokio::test]
    async fn test_install_duplicate() {
        let hub = default_hub();
        hub.install("redis").await.unwrap();
        let result = hub.install("redis").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already installed"));
    }

    #[tokio::test]
    async fn test_install_max_capacity() {
        let cfg = HubConfig {
            max_installed: 1,
            ..Default::default()
        };
        let hub = OperatorHub::new(cfg);
        hub.install("redis").await.unwrap();
        let result = hub.install("minio").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("maximum"));
    }

    #[tokio::test]
    async fn test_install_require_verified() {
        let cfg = HubConfig {
            require_verified: true,
            ..Default::default()
        };
        let hub = OperatorHub::new(cfg);
        // Bundled operators are verified, so this should succeed.
        let result = hub.install("redis").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_uninstall_success() {
        let hub = default_hub();
        hub.install("redis").await.unwrap();
        let result = hub.uninstall("redis").await;
        assert!(result.is_ok());
        assert!(hub.get_installed().await.is_empty());
    }

    #[tokio::test]
    async fn test_uninstall_not_installed() {
        let hub = default_hub();
        let result = hub.uninstall("redis").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not installed"));
    }

    // -- Upgrade -------------------------------------------------------------

    #[tokio::test]
    async fn test_upgrade_not_installed() {
        let hub = default_hub();
        let result = hub.upgrade("redis").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not installed"));
    }

    #[tokio::test]
    async fn test_upgrade_already_latest() {
        let hub = default_hub();
        hub.install("redis").await.unwrap();
        let result = hub.upgrade("redis").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already at latest"));
    }

    #[tokio::test]
    async fn test_upgrade_unknown_operator() {
        let hub = default_hub();
        let result = hub.upgrade("unknown").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    // -- Get installed / info ------------------------------------------------

    #[tokio::test]
    async fn test_get_installed_empty() {
        let hub = default_hub();
        assert!(hub.get_installed().await.is_empty());
    }

    #[tokio::test]
    async fn test_get_installed_after_install() {
        let hub = default_hub();
        hub.install("redis").await.unwrap();
        hub.install("minio").await.unwrap();
        let installed = hub.get_installed().await;
        assert_eq!(installed.len(), 2);
    }

    #[tokio::test]
    async fn test_get_operator_info_exists() {
        let hub = default_hub();
        let info = hub.get_operator_info("postgresql").await;
        assert!(info.is_some());
        assert_eq!(info.unwrap().name, "postgresql");
    }

    #[tokio::test]
    async fn test_get_operator_info_missing() {
        let hub = default_hub();
        let info = hub.get_operator_info("nonexistent").await;
        assert!(info.is_none());
    }

    // -- Check updates -------------------------------------------------------

    #[tokio::test]
    async fn test_check_updates_none() {
        let hub = default_hub();
        hub.install("redis").await.unwrap();
        let updates = hub.check_updates().await;
        assert!(updates.is_empty());
    }

    // -- Stats ---------------------------------------------------------------

    #[tokio::test]
    async fn test_stats_initial() {
        let hub = default_hub();
        let stats = hub.stats().await;
        assert_eq!(stats.available_count, 5);
        assert_eq!(stats.installed_count, 0);
        assert_eq!(stats.updates_available, 0);
    }

    #[tokio::test]
    async fn test_stats_after_installs() {
        let hub = default_hub();
        hub.install("redis").await.unwrap();
        hub.install("minio").await.unwrap();
        let stats = hub.stats().await;
        assert_eq!(stats.installed_count, 2);
    }

    // -- Serde round-trips ---------------------------------------------------

    #[test]
    fn test_install_status_serialisation() {
        let status = InstallStatus::Failed("timeout".to_string());
        let json = serde_json::to_string(&status).unwrap();
        let back: InstallStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, status);
    }

    #[test]
    fn test_operator_category_serialisation() {
        let cat = OperatorCategory::Custom("ml-pipeline".to_string());
        let json = serde_json::to_string(&cat).unwrap();
        let back: OperatorCategory = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cat);
    }

    #[test]
    fn test_integration_type_serialisation() {
        let it = IntegrationType::Sink;
        let json = serde_json::to_string(&it).unwrap();
        let back: IntegrationType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, it);
    }
}
