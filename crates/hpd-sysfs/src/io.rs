// SPDX-License-Identifier: GPL-3.0-or-later

//! `SysfsIo` trait — the minimum surface L1 backends need from sysfs.

use hpd_error::SysfsError;
use std::path::Path;

/// Read / write / probe individual sysfs files.
///
/// The trait is deliberately small: every higher layer that talks to
/// sysfs goes through it, which keeps the mock surface tractable and
/// the real implementation a thin wrapper around `std::fs`.
pub trait SysfsIo: Send + Sync {
    /// Reads a sysfs file as UTF-8 with the trailing newline stripped.
    fn read_string(&self, path: impl AsRef<Path>) -> Result<String, SysfsError>;

    /// Writes a string to a sysfs file. The kernel typically ignores
    /// trailing whitespace, so callers should pass the value verbatim
    /// without adding their own newline.
    fn write_string(&self, path: impl AsRef<Path>, val: &str) -> Result<(), SysfsError>;

    /// Returns whether the given sysfs path exists, used by L1 probe
    /// code to detect whether a feature is available on the running
    /// hardware.
    fn exists(&self, path: impl AsRef<Path>) -> bool;
}
