// SPDX-License-Identifier: GPL-3.0-or-later

//! Hardware-agnostic capability traits and value types for `hpd`.
//!
//! This crate sits at workspace layer **L2**. It owns:
//!
//! * The four capability traits an L1 backend must implement
//!   ([`charge::ChargeControl`], [`fan::FanControl`],
//!   [`platform_profile::PlatformProfile`], [`power::PowerEnvelope`])
//!   plus the blanket [`backend::HwBackend`] that requires all of them.
//! * The strongly-typed value types they exchange ([`units::PowerMilliwatts`],
//!   [`units::Rpm`], [`power::PowerEnvelopeTarget`],
//!   [`power::PowerEnvelopeLimits`], [`profile::ProfileName`], etc.).
//! * The [`profile::RuntimeConfig`] hot-swappable config the reducer
//!   consumes on every transition.
//!
//! Nothing here performs I/O — backends in `hpd-backend-*` plug in
//! at L1, and `hpd-core` orchestrates them at L3.
//!
//! ## Features
//!
//! * `testing` — exposes the in-process `testing::MockBackend` used
//!   by integration tests of higher layers. (Off by default; the module
//!   is invisible in rustdoc output without the feature.)

#![warn(missing_docs)]
pub mod backend;
pub mod charge;
pub mod error;
pub mod fan;
pub mod platform_profile;
pub mod power;
pub mod probe;
pub mod profile;
pub mod units;

#[cfg(any(test, feature = "testing"))]
pub mod testing;
