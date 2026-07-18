# hpd-dbus

> zbus interface + polkit authorization for the `hpd` daemon.

| Field    | Value                                                                        |
|----------|------------------------------------------------------------------------------|
| Layer    | **L4** — interface                                                           |
| Stable   | since `1.0.0`                                                                |
| Crate    | `hpd-dbus`                                                                   |
| Features | `simulator` (short-circuits polkit; auto-enabled by the daemon's `simulator`)|

## Purpose

Exposes the daemon over D-Bus as `dev.cirodev.hpd.PowerDaemon1` at
object path `/dev/cirodev/hpd/PowerDaemon1`. Every privileged
setter is gated through `polkit::check` *before* a `Transition` is
enqueued.

| D-Bus member                          | Kind     | Polkit action                              | Since  |
|----------------------------------------|----------|---------------------------------------------|--------|
| `set_spl(u32)`                        | method   | `dev.cirodev.hpd.set-tdp`                   | —      |
| `set_preset(s)`                       | method   | `dev.cirodev.hpd.set-tdp`                   | —      |
| `set_charge_threshold(y)`             | method   | `dev.cirodev.hpd.set-charge`                | —      |
| `set_profile(s)`                      | method   | `dev.cirodev.hpd.set-profile`               | —      |
| `set_cooling_level(s)`                | method   | `dev.cirodev.hpd.set-profile`               | —      |
| `set_fan_auto()`                      | method   | `dev.cirodev.hpd.set-profile`               | —      |
| `reset_fan_curve()`                   | method   | `dev.cirodev.hpd.set-profile`               | —      |
| `set_ac_max_performance(b)`           | method   | `dev.cirodev.hpd.set-profile`               | 2.7.0  |
| `set_fan_curve(cpu: a(yy), gpu: a(yy))` | method | `dev.cirodev.hpd.set-profile`             | 2.9.0  |
| `enable_gpu_auto_follow()`            | method   | `dev.cirodev.hpd.set-profile`               | 2.12.0 |
| `reset_gpu_clocks()`                  | method   | `dev.cirodev.hpd.set-profile`               | 2.12.0 |
| `restore_defaults()`                  | method   | `set-tdp` **and** `set-charge` **and** `set-profile` (all three) | 2.14.0 |
| `get_hardware_limits()` / `get_version()` / `get_thermal_status()` / `get_fan_curve()` | method | — | — |
| `is_ac_connected()`                   | method   | —                                            | —      |
| `get_diagnostics()`                   | method   | —                                            | 2.1.0  |
| `get_power_conflicts()`               | method   | —                                            | 2.2.0  |
| `get_advisory_daemons()`              | method   | —                                            | 2.3.0  |
| `get_telemetry()`                     | method   | —                                            | 2.8.0  |
| `get_fan_curve_constraints()`         | method   | —                                            | 2.9.0  |
| `get_ppd_shim_active()`               | method   | — (whether the `net.hadess.PowerProfiles` compat shim claimed its bus name) | 2.10.0 |
| `get_gpu_clock_constraints()`         | method   | —                                            | 2.12.0 |
| `get_gpu_clock_range()`               | method   | —                                            | 2.12.0 |
| `current_spl`                         | property | —                                            | —      |
| `active_profile`                      | property | —                                            | —      |
| `charge_end_threshold`                | property | —                                            | —      |
| `auto_cooling`                        | property | —                                            | —      |
| `fan_curve`                           | property | —                                            | —      |
| `ac_connected`                        | property | —                                            | 2.4.0  |
| `ac_locked` / `ac_max_performance`    | property | —                                            | 2.7.0  |
| `gpu_clock_range` / `gpu_follows_tdp` | property | —                                            | 2.12.0 |

While `ac_locked` is `true` (on AC with the lock preference on), every
power/cooling setter (`set_spl`, `set_preset`, `set_profile`,
`set_cooling_level`, `set_fan_auto`, `reset_fan_curve`, `set_fan_curve`,
`enable_gpu_auto_follow`, `reset_gpu_clocks`, `restore_defaults`)
rejects with a "locked on AC" error; `set_charge_threshold` and
`set_ac_max_performance` are exempt.

Property changes emit `PropertiesChanged` signals — the daemon's
`spawn_properties_changed_emitter` watches the executor's
`watch::Receiver<ProfileState>` and pushes a notifier for each field
that flipped.

## Polkit contract

- **Action IDs live in `actions.rs`** as the `PolkitAction` enum.
  Adding a privileged operation = add a variant, get a compile error
  at the `as_id` arm, update `package/polkit/dev.cirodev.hpd.policy`.
- **`wheel` passwordless grant:** the `auth_admin` defaults in the
  policy apply to non-administrators only. `package/polkit/49-hpd.rules`
  grants every `dev.cirodev.hpd.*` action to `wheel`-group members
  without a prompt (matched by action-ID prefix, so new actions are
  covered automatically), keyed on group membership rather than the
  session's local/active classification.
- **Fail-closed:** every error path in `polkit::check` (proxy
  failure, method-call timeout, malformed reply, missing sender
  header) returns `false`. Refusing a legitimate request beats
  allowing an unauthenticated one.
- **Simulator bypass:** under `cfg(feature = "simulator")` the check
  unconditionally returns `true` — session-bus runs on macOS / dev
  hosts have no polkit authority to talk to.

## Dependencies

| Dep                | Purpose                                          |
|--------------------|--------------------------------------------------|
| `hpd-core`         | Sends `Transition`s into the executor.           |
| `hpd-capabilities` | Reads value types from `ProfileState`.           |
| `zbus`             | D-Bus server, `#[interface]` macro.              |
| `tokio` (`sync`)   | `mpsc::Sender` + `watch::Receiver` plumbing.     |
| `tracing`          | Per-call debug logs.                             |

## Example (daemon-side wiring)

```rust
use hpd_dbus::service::PowerDaemonInterface;
let iface = PowerDaemonInterface::new(tx, state_rx, limits);
zbus::ConnectionBuilder::system()?
    .name("dev.cirodev.hpd.PowerDaemon1")?
    .serve_at("/dev/cirodev/hpd/PowerDaemon1", iface)?
    .build().await?;
```

## Docs

```bash
cargo doc -p hpd-dbus --no-deps --open
```

## License

GPL-3.0-or-later. See the workspace `Cargo.toml`.
