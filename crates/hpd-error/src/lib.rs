// SPDX-License-Identifier: GPL-3.0-or-later

//! Cross-crate error types for the `hpd` workspace.
//!
//! This crate sits at layer L-1: it depends on nothing internal and is
//! depended on by every other crate. Centralising the error hierarchy here
//! keeps capability boundaries clean — backends can use `?` to bubble up
//! sysfs failures and parsing failures without bespoke `map_err` plumbing.

use std::path::PathBuf;
use thiserror::Error;

/// Top-level error returned across capability boundaries.
#[derive(Error, Debug)]
pub enum HpdError {
    /// A low-level sysfs read/write failed.
    #[error(transparent)]
    Sysfs(#[from] SysfsError),

    /// A backend produced a logical error (parse failure, unsupported state).
    #[error("Backend error: {0}")]
    Backend(#[from] BackendError),

    /// The hardware does not expose the capability the caller asked for.
    #[error("Feature not supported on this hardware")]
    FeatureUnsupported,

    /// A domain invariant was violated by an external command (e.g. SPL > max).
    #[error("State invariant violated: {0}")]
    InvariantViolation(String),
}

/// Lower-level filesystem error from sysfs reads/writes.
#[derive(Error, Debug)]
pub enum SysfsError {
    /// The sysfs path does not exist (kernel doesn't expose this attribute).
    #[error("Sysfs path not found: {path}")]
    NotFound { path: PathBuf },

    /// The process cannot read/write the path (typically not running as root).
    #[error("Permission denied at sysfs path: {path}")]
    PermissionDenied { path: PathBuf },

    /// The file existed but its content could not be parsed as expected.
    #[error("Parse error at {path}: expected {expected}, got '{found}'")]
    ParseError {
        path: PathBuf,
        expected: &'static str,
        found: String,
    },

    /// Any other I/O error (EIO, ENOSPC, etc.) with path context attached.
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl SysfsError {
    /// Map a [`std::io::Error`] into a categorised `SysfsError` with the
    /// originating path attached as context.
    pub fn from_io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        let path = path.into();
        match source.kind() {
            std::io::ErrorKind::NotFound => SysfsError::NotFound { path },
            std::io::ErrorKind::PermissionDenied => SysfsError::PermissionDenied { path },
            _ => SysfsError::Io { path, source },
        }
    }
}

/// Higher-level backend logic error (parse failure, unexpected value, etc.).
///
/// Backends use this when the failure is not a raw sysfs I/O issue.
#[derive(Error, Debug)]
pub enum BackendError {
    /// A sysfs value was read but its content could not be parsed into the
    /// expected type.
    #[error("failed to parse {field} from '{raw}': {reason}")]
    ParseFailed {
        /// Logical name of the field (e.g. "watts", "fan_rpm").
        field: &'static str,
        /// Raw value read from sysfs.
        raw: String,
        /// Free-form explanation of what was expected.
        reason: String,
    },

    /// Catch-all for backend logic errors that don't fit a structured variant.
    #[error("{0}")]
    Other(String),
}
