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

| D-Bus member            | Kind     | Polkit action                       |
|-------------------------|----------|-------------------------------------|
| `set_spl(u32)`          | method   | `dev.cirodev.hpd.set-tdp`           |
| `set_preset(s)`         | method   | `dev.cirodev.hpd.set-tdp`           |
| `set_charge_threshold(y)`| method  | `dev.cirodev.hpd.set-charge`        |
| `set_profile(s)`        | method   | `dev.cirodev.hpd.set-profile`       |
| `set_fan_auto()`        | method   | `dev.cirodev.hpd.set-profile`       |
| `get_hardware_limits()` | method   | —                                   |
| `is_ac_connected()`     | method   | —                                   |
| `current_spl`           | property | —                                   |
| `active_profile`        | property | —                                   |
| `charge_end_threshold`  | property | —                                   |
| `auto_cooling`          | property | —                                   |

Property changes emit `PropertiesChanged` signals — the daemon's
`spawn_properties_changed_emitter` watches the executor's
`watch::Receiver<ProfileState>` and pushes a notifier for each field
that flipped.

## Polkit contract

- **Action IDs live in `actions.rs`** as the `PolkitAction` enum.
  Adding a privileged operation = add a variant, get a compile error
  at the `as_id` arm, update `package/polkit/dev.cirodev.hpd.policy`.
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
