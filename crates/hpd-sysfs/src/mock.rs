// SPDX-License-Identifier: GPL-3.0-or-later

//! Test-only sysfs fixture (gated by the `mock` feature).
//!
//! Lint policy: `unwrap` / `expect` are intentional throughout — a
//! poisoned mutex or failing temp-dir creation here would indicate a
//! broken test harness, not a runtime condition the daemon should
//! recover from. The `#![allow]` at module level scopes the opt-out
//! to this file only; production code stays on the strict
//! `[workspace.lints.clippy]` bar.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::io::SysfsIo;
use hpd_error::SysfsError;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;

/// In-memory sysfs fixture used by integration tests of higher layers.
/// `Clone` keeps the underlying `TempDir` shared via `Arc`, so the
/// fixture survives as long as any clone of it does.
#[derive(Clone)]
pub struct MockSysfs {
    root: Arc<TempDir>,
}

impl Default for MockSysfs {
    fn default() -> Self {
        Self::new()
    }
}

impl MockSysfs {
    /// Build a fresh in-memory sysfs rooted in a new `TempDir`.
    pub fn new() -> Self {
        Self {
            root: Arc::new(tempfile::tempdir().expect("Failed to create mock sysfs root")),
        }
    }

    /// Pre-populate the mock with a file at `rel_path` containing `content`.
    /// Creates intermediate directories as needed. Intended to be called
    /// during test setup, before the mock is handed to the backend.
    pub fn create_file(&self, rel_path: impl AsRef<Path>, content: &str) {
        let full_path = self.root.path().join(rel_path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(full_path, content).unwrap();
    }

    /// Map an absolute "sysfs" path to its real location under the
    /// fixture's `TempDir`. Strips the leading `/` so callers can pass
    /// canonical sysfs paths verbatim (`/sys/class/...`).
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
        fs::write(&real_path, val).map_err(|e| SysfsError::from_io(path.as_ref(), e))
    }

    fn exists(&self, path: impl AsRef<Path>) -> bool {
        self.resolve(path).exists()
    }
}
