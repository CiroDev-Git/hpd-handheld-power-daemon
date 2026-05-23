//! Sysfs I/O abstraction (workspace layer **L0**).
//!
//! Exposes the [`SysfsIo`] trait and two implementors:
//!
//! * [`RealSysfs`] — production implementation backed by `std::fs`.
//! * `MockSysfs` (behind the `mock` feature) — in-memory tree rooted
//!   in a `tempfile::TempDir` used by integration tests of higher
//!   layers.

pub mod io;
pub mod real;

#[cfg(feature = "mock")]
pub mod mock;

pub use hpd_error::SysfsError;
pub use io::SysfsIo;
pub use real::RealSysfs;

#[cfg(feature = "mock")]
pub use mock::testing::MockSysfs;
