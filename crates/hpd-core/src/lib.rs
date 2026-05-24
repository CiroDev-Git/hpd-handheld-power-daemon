// SPDX-License-Identifier: GPL-3.0-or-later

//! Domain layer (workspace **L3**) of `hpd`.
//!
//! Owns the Transition → reducer → Effect → Executor state machine
//! that mediates every hardware change. The reducer ([`reducer::reduce`])
//! is a pure function — all I/O happens inside [`executor::Executor`]
//! and the persistence layer.

pub mod effect;
pub mod executor;
pub mod inference;
pub mod invariants;
pub mod persistence;
pub mod reducer;
pub mod state;
pub mod transition;
