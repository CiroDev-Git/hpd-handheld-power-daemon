# hpd-netlink

> udev `power_supply` event monitor → AC plug / unplug transitions.

| Field    | Value                                                       |
|----------|-------------------------------------------------------------|
| Layer    | **L0** — kernel I/O                                         |
| Stable   | since `1.0.0`                                               |
| Crate    | `hpd-netlink`                                               |
| Platform | Linux only (no-op stub on every other target)               |

## Purpose

Subscribes to the udev `power_supply` subsystem and emits a
[`hpd_core::transition::Transition::AcPowerChanged`] every time the
charger is plugged or unplugged. The reducer uses that signal to
snapshot the user's battery (DC) state on plug and restore it on unplug
(`last_dc_state`), and — with the `ac_max_performance` preference on
(default) — to force / release the maximum-performance lock.

On non-Linux targets the crate compiles to a single `async fn` that
awaits a never-resolving future, so the daemon binary stays buildable
on macOS / dev hosts without `tokio-udev`.

## Concurrency caveat

`tokio_udev::AsyncMonitorSocket` is **`!Send`**. The daemon hosts
`spawn_power_monitor` on a dedicated `std::thread` with its own
current-thread runtime + `LocalSet` — do **not** try to call this
from the main multi-threaded tokio runtime. See
`hpd-daemon/src/main.rs` for the wiring.

## Dependencies

| Dep            | Purpose                                                 |
|----------------|---------------------------------------------------------|
| `tokio`        | `mpsc::Sender` over which Transitions are emitted.      |
| `futures-util` | `StreamExt::next()` on the udev socket.                 |
| `tracing`      | Logs every detected AC edge.                            |
| `hpd-core`     | `Transition::AcPowerChanged` is defined there.          |
| `tokio-udev`   | (`cfg(target_os = "linux")` only) udev event stream.    |

## Example

```rust
use tokio::sync::mpsc;
use hpd_core::transition::Transition;
use hpd_netlink::spawn_power_monitor;

let (tx, _rx) = mpsc::channel::<Transition>(16);
// Run on its own current-thread runtime in a dedicated std::thread.
std::thread::spawn(move || {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all().build().unwrap();
    let local = tokio::task::LocalSet::new();
    rt.block_on(local.run_until(spawn_power_monitor(tx)));
});
```

## Docs

```bash
cargo doc -p hpd-netlink --no-deps --open
```

## License

GPL-3.0-or-later. See the workspace `Cargo.toml`.
