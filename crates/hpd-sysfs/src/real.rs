use std::fs;
use std::path::Path;
use crate::io::SysfsIo;
use crate::error::SysfsError;

#[derive(Clone, Default)]
pub struct RealSysfs;

impl RealSysfs {
    pub fn new() -> Self {
        Self
    }
}

impl SysfsIo for RealSysfs {
    fn read_string(&self, path: impl AsRef<Path>) -> Result<String, SysfsError> {
        let path_ref = path.as_ref();
        fs::read_to_string(path_ref)
            .map(|s| s.trim().to_string())
            .map_err(|e| SysfsError::from_io(path_ref, e))
    }

    fn write_string(&self, path: impl AsRef<Path>, val: &str) -> Result<(), SysfsError> {
        let path_ref = path.as_ref();
        tracing::debug!(path = %path_ref.display(), val, "Writing to sysfs");
        fs::write(path_ref, val)
            .map_err(|e| SysfsError::from_io(path_ref, e))
    }

    fn exists(&self, path: impl AsRef<Path>) -> bool {
        path.as_ref().exists()
    }
}