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
    NotFound {
        /// Absolute sysfs path that returned `ENOENT`.
        path: PathBuf,
    },

    /// The process cannot read/write the path (typically not running as root).
    #[error("Permission denied at sysfs path: {path}")]
    PermissionDenied {
        /// Absolute sysfs path that returned `EACCES`.
        path: PathBuf,
    },

    /// The file existed but its content could not be parsed as expected.
    #[error("Parse error at {path}: expected {expected}, got '{found}'")]
    ParseError {
        /// Absolute sysfs path whose contents failed to parse.
        path: PathBuf,
        /// Human-readable description of the expected format
        /// (e.g. `"integer 0-100"`).
        expected: &'static str,
        /// Trimmed raw contents the backend read off disk.
        found: String,
    },

    /// Any other I/O error (EIO, ENOSPC, etc.) with path context attached.
    #[error("I/O error at {path}: {source}")]
    Io {
        /// Absolute sysfs path the failing I/O was targeting.
        path: PathBuf,
        /// Underlying [`std::io::Error`] preserved for downcasting.
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

#[cfg(test)]
mod tests {
    // Test code may use `.unwrap()` / `.expect()` / `panic!` freely;
    // the strict bar in `[workspace.lints.clippy]` applies to
    // production code only.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use std::io;

    #[test]
    fn from_io_maps_notfound_kind_to_notfound_variant() {
        // ENOENT on a sysfs path almost always means the kernel does
        // not expose that attribute (different model, missing driver).
        // The categorised variant lets callers handle "feature absent"
        // differently from "permission denied" without inspecting
        // strings.
        let err = io::Error::new(io::ErrorKind::NotFound, "no such file");
        let sysfs = SysfsError::from_io("/sys/class/firmware-attributes/x", err);
        assert!(matches!(
            sysfs,
            SysfsError::NotFound { ref path }
                if path == &PathBuf::from("/sys/class/firmware-attributes/x")
        ));
    }

    #[test]
    fn from_io_maps_permissiondenied_kind_to_permissiondenied_variant() {
        // EACCES on a sysfs setter typically means the daemon is
        // running unprivileged. Same as above: surfaced as its own
        // variant so polkit-style handling stays structured.
        let err = io::Error::new(io::ErrorKind::PermissionDenied, "EACCES");
        let sysfs = SysfsError::from_io("/sys/firmware/acpi/platform_profile", err);
        assert!(matches!(
            sysfs,
            SysfsError::PermissionDenied { ref path }
                if path == &PathBuf::from("/sys/firmware/acpi/platform_profile")
        ));
    }

    #[test]
    fn from_io_falls_back_to_io_variant_for_other_kinds() {
        // Anything that is neither NotFound nor PermissionDenied
        // lands in the catch-all `Io` variant with the original error
        // preserved via thiserror's `#[source]` so callers can
        // downcast / inspect if they want.
        let original = io::Error::new(io::ErrorKind::InvalidData, "bad bytes");
        let sysfs = SysfsError::from_io("/sys/foo", original);
        match sysfs {
            SysfsError::Io { path, source } => {
                assert_eq!(path, PathBuf::from("/sys/foo"));
                assert_eq!(source.kind(), io::ErrorKind::InvalidData);
            }
            other => panic!("expected SysfsError::Io, got {:?}", other),
        }
    }

    #[test]
    fn hpderror_display_renders_sysfs_transparently() {
        // `HpdError::Sysfs` is annotated `#[error(transparent)]`, so
        // its Display must be byte-identical to the underlying
        // SysfsError. Locks the public Display contract for the
        // many call sites that just `println!("{e}")` an HpdError.
        let inner = SysfsError::NotFound {
            path: PathBuf::from("/sys/x"),
        };
        let inner_str = inner.to_string();
        let outer: HpdError = inner.into();
        assert_eq!(outer.to_string(), inner_str);
    }

    #[test]
    fn backenderror_parsefailed_display_includes_field_and_raw() {
        // The structured ParseFailed variant must surface enough
        // context in Display form to debug from a log line alone.
        let err = BackendError::ParseFailed {
            field: "watts",
            raw: "0xZZ".to_string(),
            reason: "expected integer".to_string(),
        };
        let s = err.to_string();
        assert!(s.contains("watts"), "missing field name: {s}");
        assert!(s.contains("0xZZ"), "missing raw value: {s}");
        assert!(s.contains("expected integer"), "missing reason: {s}");
    }
}
