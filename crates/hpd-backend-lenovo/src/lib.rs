// SPDX-License-Identifier: GPL-3.0-or-later

#![cfg_attr(
    not(test),
    warn(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

pub mod detect;
pub mod power;
