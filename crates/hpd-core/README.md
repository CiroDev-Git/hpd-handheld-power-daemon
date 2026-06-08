# hpd-core

> Domain layer: pure reducer + Executor + state machine + persistence.

| Field   | Value                                                  |
|---------|--------------------------------------------------------|
| Layer   | **L3** — domain logic                                  |
| Stable  | since `1.0.0`                                          |
| Crate   | `hpd-core`                                             |

## Purpose

The brain of `hpd`. Every mutation flows through this pipeline:

```text
external event → Transition → reduce(state, t, limits, cfg) → (state', Effect[])
                                                                   │
                                                  Executor.handle_effect ─→ backend
```

Three principles enforced here:

1. **`reduce()` is pure.** No I/O, no async, no globals, no
   `println!`. Logging is via structured `tracing` fields only.
2. **All side-effects go through `Effect`.** `ApplyPowerEnvelope`,
   `ApplyPlatformProfile`, `ApplyChargeThreshold`, `PersistState`.
   The Executor is the only thing that dispatches them.
3. **`ConfigReload` is intercepted before `reduce`.** The Executor
   atomically swaps its `RuntimeConfig`; the next transition uses
   the new values. The reducer treats `ConfigReload` as a no-op.

State is persisted via `persistence::save_atomic` (`tempfile +
rename`). `ProfileState::is_ac_connected` and the derived
`ac_locked` are `#[serde(skip)]` — re-queried/recomputed at boot,
never trusted from disk; the persisted fields include `last_dc_state`
(battery snapshot for the AC restore) and the `ac_max_performance`
lock preference.

## Rollback contract

If a hardware-write Effect fails, the Executor re-reads the live
state from the backend and re-injects a `Sync*` transition so the
in-memory state matches reality. Lote 38 made this uniform across
`Apply{PowerEnvelope, PlatformProfile, ChargeThreshold}`.

## Auto-profile inference

When `RuntimeConfig::fan_follows_tdp == true`, a TDP change in
`reduce()` also emits a `ApplyPlatformProfile` in the same batch.
The mapping lives in `inference.rs` (`infer_profile_from_target`)
and is the single source of truth for the auto-cooling behaviour.

## Dependencies

| Dep                | Purpose                                                    |
|--------------------|------------------------------------------------------------|
| `hpd-error`        | Error type returned from every Effect dispatch.            |
| `hpd-capabilities` | Trait surface the Executor calls into; value types.        |
| `tracing`          | Structured logs from inside the reducer / executor.        |
| `tokio` (`sync`)   | `mpsc::Sender<Transition>` and `watch::Sender<ProfileState>`. |
| `serde` + `toml`   | On-disk persistence at `/var/lib/hpd/state.toml`.          |

Dev-deps add `hpd-capabilities/testing` so executor tests can drive
`MockBackend`.

## Example

```rust
use hpd_core::{reducer::reduce, state::ProfileState, transition::Transition};
use hpd_capabilities::{power::PowerEnvelopeLimits, RuntimeConfig};

let limits = PowerEnvelopeLimits { /* … */ };
let cfg    = RuntimeConfig::default();
let state  = ProfileState::default();

let (next, effects) = reduce(&state, Transition::SetSpl(15), &limits, &cfg);
assert_eq!(next.power_target.spl.as_watts(), 15);
```

## Docs

```bash
cargo doc -p hpd-core --no-deps --open
```

## License

GPL-3.0-or-later. See the workspace `Cargo.toml`.
