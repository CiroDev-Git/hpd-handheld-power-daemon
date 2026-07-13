# hpd-capabilities

> Hardware-agnostic capability traits and value types.

| Field   | Value                                                  |
|---------|--------------------------------------------------------|
| Layer   | **L2** — capability contracts                          |
| Stable  | since `1.0.0`                                          |
| Crate   | `hpd-capabilities`                                     |
| Features| `testing` (exposes `MockBackend` for integration tests)|

## Purpose

Defines the contract every L1 vendor backend must implement and the
value types those traits speak in. The aggregator [`HwBackend`]
returns each capability via an accessor:

```rust
pub trait HwBackend: Send + Sync {
    fn power(&self)      -> &dyn PowerEnvelope;
    fn charge(&self)     -> Option<&dyn ChargeControl>        { None }
    fn profile(&self)    -> Option<&dyn PlatformProfile>      { None }
    fn fan(&self)        -> Option<&dyn FanControl>           { None }
    fn fan_curve(&self)  -> Option<&dyn FanCurveControl>      { None }
    fn thermal(&self)    -> Option<&dyn ThermalSensors>       { None }
    fn telemetry(&self)  -> Option<&dyn SystemTelemetry>      { None }
    fn gpu_clock(&self)  -> Option<&dyn GpuClockRangeControl> { None }
}
```

`PowerEnvelope` is mandatory; the rest are `Option<_>` so partial
hardware (e.g. a future backend that only models TDP) can still
participate. The ASUS backend returns `Some(...)` for all seven
optional accessors (eight capability traits in total, including the
mandatory `power()`).

Value types live in `units.rs` (`PowerMilliwatts`, `Rpm`) and
domain enums in `profile.rs` (`ProfileName`, `TdpPreset`,
`ProfileThresholds`). The hot-swappable `RuntimeConfig` shipped here
is what the executor swaps on `Transition::ConfigReload` (see
`hpd-core`'s `Executor`).

## Layer ordering note

L2 is numbered *before* L1 in the workspace tree because every L1
backend depends on these traits, not the other way around. The
manifest lists L-1 → L0 → L2 → L1 → L3 → L4 in dependency order.

## Dependencies

| Dep         | Purpose                                                       |
|-------------|---------------------------------------------------------------|
| `hpd-error` | Trait methods return `Result<_, HpdError>`.                   |
| `serde`     | `ProfileName`, `TdpPreset` etc. serialize for persistence.    |

No async runtime, no I/O — pure data + traits.

## Example

Implementing a new backend's `PowerEnvelope`:

```rust
use hpd_capabilities::power::{PowerEnvelope, PowerEnvelopeLimits, PowerEnvelopeTarget};
use hpd_error::HpdError;

struct MyVendorPower { /* sysfs handle */ }

impl PowerEnvelope for MyVendorPower {
    fn get_limits(&self) -> Result<PowerEnvelopeLimits, HpdError> { /* ... */ }
    fn get_target(&self) -> Result<PowerEnvelopeTarget, HpdError> { /* ... */ }
    fn set_target(&self, t: &PowerEnvelopeTarget) -> Result<(), HpdError> { /* ... */ }
}
```

## Docs

```bash
cargo doc -p hpd-capabilities --features testing --no-deps --open
```

## License

GPL-3.0-or-later. See the workspace `Cargo.toml`.
