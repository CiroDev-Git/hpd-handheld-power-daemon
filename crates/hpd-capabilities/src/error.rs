use thiserror::Error;

#[derive(Error, Debug)]
pub enum HpdError {
    #[error("Sysfs operation failed: {0}")]
    Sysfs(#[from] SysfsError),
    
    #[error("Backend error: {reason}")]
    Backend { reason: String },

    #[error("Feature not supported on this hardware")]
    FeatureUnsupported,
    
    #[error("State invariant violated: {0}")]
    InvariantViolation(String),
}

#[derive(Error, Debug)]
pub enum SysfsError {
    #[error("File not found: {path}")]
    NotFound { path: String },
    #[error("Permission denied reading/writing: {path}")]
    PermissionDenied { path: String },
    #[error("I/O error at {path}: {source}")]
    Io { path: String, source: std::io::Error },
}