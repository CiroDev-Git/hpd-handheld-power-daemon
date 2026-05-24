// SPDX-License-Identifier: GPL-3.0-or-later

//! Re-exports of the canonical error types defined in `hpd-error`.
//!
//! Kept as a `pub use` shim so that existing callers can keep importing
//! `hpd_capabilities::error::HpdError` without churn. New code should
//! prefer importing from `hpd_error` directly.

pub use hpd_error::{BackendError, HpdError, SysfsError};
