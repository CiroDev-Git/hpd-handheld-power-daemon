#[cfg(feature = "mock")]
pub mod testing {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use tempfile::TempDir;
    use crate::io::SysfsIo;
    use crate::error::SysfsError;

    #[derive(Clone)]
    pub struct MockSysfs {
        root: Arc<TempDir>,
    }

    impl MockSysfs {
        pub fn new() -> Self {
            Self {
                root: Arc::new(tempfile::tempdir().expect("Failed to create mock sysfs root")),
            }
        }

        /// Helpers to init state of mock before emit to backend
        pub fn create_file(&self, rel_path: impl AsRef<Path>, content: &str) {
            let full_path = self.root.path().join(rel_path);
            if let Some(parent) = full_path.parent() {
                fs::create_dir_all(parent).unwrap(); // Just for setup allow unwrap
            }
            fs::write(full_path, content).unwrap();
        }

        /// Resolve abs path assuming that root is `/`
        fn resolve(&self, path: impl AsRef<Path>) -> PathBuf {
            let p = path.as_ref();
            let stripped = p.strip_prefix("/").unwrap_or(p);
            self.root.path().join(stripped)
        }
    }

    impl SysfsIo for MockSysfs {
        fn read_string(&self, path: impl AsRef<Path>) -> Result<String, SysfsError> {
            let real_path = self.resolve(&path);
            fs::read_to_string(&real_path)
                .map(|s| s.trim().to_string())
                .map_err(|e| SysfsError::from_io(path.as_ref(), e))
        }

        fn write_string(&self, path: impl AsRef<Path>, val: &str) -> Result<(), SysfsError> {
            let real_path = self.resolve(&path);
            fs::write(&real_path, val)
                .map_err(|e| SysfsError::from_io(path.as_ref(), e))
        }

        fn exists(&self, path: impl AsRef<Path>) -> bool {
            self.resolve(path).exists()
        }
    }
}