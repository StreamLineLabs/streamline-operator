//! Error types for the Streamline Kubernetes Operator

use std::fmt;

/// Result type alias for operator operations
pub type Result<T> = std::result::Result<T, OperatorError>;

/// Errors that can occur during operator operations
#[derive(Debug)]
pub enum OperatorError {
    /// Kubernetes API error
    KubeApi(String),
    /// Configuration error
    Configuration(String),
    /// Reconciliation error
    Reconciliation(String),
    /// HTTP client error
    Http(String),
    /// Serialization error
    Serialization(String),
    /// Resource not found
    NotFound(String),
    /// Invalid resource state
    InvalidState(String),
}

impl fmt::Display for OperatorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OperatorError::KubeApi(msg) => write!(f, "Kubernetes API error: {}", msg),
            OperatorError::Configuration(msg) => write!(f, "Configuration error: {}", msg),
            OperatorError::Reconciliation(msg) => write!(f, "Reconciliation error: {}", msg),
            OperatorError::Http(msg) => write!(f, "HTTP error: {}", msg),
            OperatorError::Serialization(msg) => write!(f, "Serialization error: {}", msg),
            OperatorError::NotFound(msg) => write!(f, "Resource not found: {}", msg),
            OperatorError::InvalidState(msg) => write!(f, "Invalid state: {}", msg),
        }
    }
}

impl std::error::Error for OperatorError {}

impl From<kube::Error> for OperatorError {
    fn from(err: kube::Error) -> Self {
        OperatorError::KubeApi(err.to_string())
    }
}

impl From<serde_json::Error> for OperatorError {
    fn from(err: serde_json::Error) -> Self {
        OperatorError::Serialization(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = OperatorError::KubeApi("test error".to_string());
        assert!(err.to_string().contains("Kubernetes API error"));
    }

    #[test]
    fn test_error_variants() {
        let errors = vec![
            OperatorError::KubeApi("api".to_string()),
            OperatorError::Configuration("config".to_string()),
            OperatorError::Reconciliation("reconcile".to_string()),
            OperatorError::Http("http".to_string()),
            OperatorError::Serialization("serde".to_string()),
            OperatorError::NotFound("resource".to_string()),
            OperatorError::InvalidState("state".to_string()),
        ];

        for err in errors {
            // Ensure Display is implemented
            let _ = format!("{}", err);
        }
    }
}
