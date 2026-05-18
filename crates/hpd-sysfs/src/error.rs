use thiserror::Error;
use std::path::PathBuf;

#[derive(Error, Debug)]
pub enum SysfsError {
    #[error("Sysfs path not found: {path}")]
    NotFound { path: PathBuf },
    
    #[error("Permission denied at sysfs path: {path}")]
    PermissionDenied { path: PathBuf },
    
    #[error("Parse error at {path}: expected {expected}, got '{found}'")]
    ParseError { 
        path: PathBuf, 
        expected: &'static str, 
        found: String 
    },
    
    #[error("I/O error at {path}: {source}")]
    Io { 
        path: PathBuf, 
        source: std::io::Error 
    },
}

// Helper for map std::io::Error to SysfsError with path context
impl SysfsError {
    pub fn from_io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        let path = path.into();
        match source.kind() {
            std::io::ErrorKind::NotFound => SysfsError::NotFound { path },
            std::io::ErrorKind::PermissionDenied => SysfsError::PermissionDenied { path },
            _ => SysfsError::Io { path, source },
        }
    }
}