// SPDX-License-Identifier: GPL-3.0-or-later

//! D-Bus surface (workspace layer **L4**) of `hpd`.
//!
//! Exposes the [`service::PowerDaemonInterface`] under the well-known
//! interface name `dev.cirodev.hpd.PowerDaemon1` and gates every
//! privileged setter through [`polkit::check`]. The action
//! identifiers live in one place — the [`actions::PolkitAction`] enum —
//! so adding a new privileged operation is a single source-of-truth edit.

pub mod actions;
pub mod conflicts;
pub mod polkit;
pub mod service;
