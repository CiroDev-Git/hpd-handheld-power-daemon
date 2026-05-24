// SPDX-License-Identifier: GPL-3.0-or-later

//! Domain layer (workspace **L3**) of `hpd`.
//!
//! Owns the Transition → reducer → Effect → Executor state machine
//! that mediates every hardware change. The reducer ([`reducer::reduce`])
//! is a pure function — all I/O happens inside [`executor::Executor`]
//! and the persistence layer.

/// [`effect::Effect`] — side-effects emitted by the reducer and
/// applied by the [`executor::Executor`] (the only side-effecting
/// component in the domain layer).
pub mod effect;
/// [`executor::Executor`] — drives the `Transition → reduce → Effect`
/// loop, owns the `RuntimeConfig`, dispatches effects to the backend,
/// and handles uniform rollback on hardware-write failure.
pub mod executor;
/// Inference of the cooling profile from the active TDP envelope
/// (`fan_follows_tdp` auto-mode).
pub mod inference;
pub mod invariants;
pub mod persistence;
pub mod reducer;
pub mod state;
pub mod transition;
