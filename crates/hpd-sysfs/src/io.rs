use std::path::Path;
use crate::error::SysfsError;

pub trait SysfsIo: Send + Sync {
    /// Remove the \n at the end
    fn read_string(&self, path: impl AsRef<Path>) -> Result<String, SysfsError>;
    
    /// Write a string to sysfs file
    fn write_string(&self, path: impl AsRef<Path>, val: &str) -> Result<(), SysfsError>;
    
    /// Usefull for `probe` in detection phase
    fn exists(&self, path: impl AsRef<Path>) -> bool;

    fn is_writable(&self, path: impl AsRef<Path>) -> bool;
}