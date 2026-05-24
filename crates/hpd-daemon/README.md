# hpd-daemon

> Long-running root service. Composition root of the workspace.

| Field    | Value                                                                  |
|----------|------------------------------------------------------------------------|
| Layer    | **L4** — interface / composition root                                  |
| Stable   | since `1.0.0`                                                          |
| Crate    | `hpd-daemon`                                                           |
| Binary   | `hpd-daemon`                                                           |
| Features | `vendor-asus` (default), `simulator` (implies `vendor-asus` + polkit bypass) |

## Purpose

Picks the L1 backend from the live DMI, loads `/etc/hpd/config.toml`,
wires the [`Executor`](../hpd-core/README.md) to the D-Bus interface,
spawns the netlink / suspend monitors, and drives the lifecycle
(`SIGHUP` reload, `SIGINT`/`SIGTERM` graceful drain).

Publishes `dev.cirodev.hpd.PowerDaemon1` on the **system bus** in
production and on the **session bus** when built with
`--features simulator`.

## Architecture

```text
                                 ┌──────────────────┐
                                 │   D-Bus clients  │ (hpdctl, KDE, GNOME…)
                                 └────────┬─────────┘
                                          │
                  ┌───────── system bus ───┴───┐
                  ▼                            │
       ┌─────────────────────┐                 │
       │ PowerDaemonInterface│ (hpd-dbus)      │
       └───────┬─────────────┘                 │
               │ mpsc::Sender<Transition>      │ watch::Receiver
               ▼                               │ <ProfileState>
       ┌─────────────────────┐                 │
       │     Executor        │ (hpd-core) ─────┘
       │  ┌──────────────┐   │
       │  │ reduce()     │   │ ─→ Effect[] ─→ backend (hpd-backend-*)
       │  └──────────────┘   │ ─→ persist (/var/lib/hpd/state.toml)
       └────┬────────────┬───┘
            │            │
   ┌────────┴──────┐  ┌──┴───────────┐
   │ netlink (AC)  │  │ suspend mon. │
   │ hpd-netlink   │  │ (logind)     │
   └───────────────┘  └──────────────┘
```

## Lifecycle / signals

| Signal     | Source                       | Daemon response                                                                 |
|------------|------------------------------|---------------------------------------------------------------------------------|
| `SIGINT`   | Ctrl+C in a terminal         | `Transition::Shutdown` → reducer emits `PersistState` → executor drains → exits.|
| `SIGTERM`  | `systemctl stop`             | Same as SIGINT.                                                                 |
| `SIGHUP`   | `systemctl reload`           | Re-read `/etc/hpd/config.toml`; push `ConfigReload(new.to_runtime())`.          |
| Resume     | logind `PrepareForSleep`     | Push `SystemResumed`; reducer re-applies envelope + profile + charge.           |
| AC plug    | udev `power_supply` event    | Push `AcPowerChanged(b)`; reducer snapshots/restores `last_dc_target`.          |

Graceful shutdown drains the executor with a 5s timeout, well under
systemd's default 90s `TimeoutStopSec`.

## Filesystem layout (production)

| Path                              | Purpose                                            |
|-----------------------------------|----------------------------------------------------|
| `/usr/local/bin/hpd-daemon`       | Binary installed by `install.sh`.                  |
| `/usr/local/bin/hpdctl`           | CLI binary.                                        |
| `/etc/systemd/system/hpd.service` | systemd unit (sandboxed, `StateDirectory=hpd`).    |
| `/etc/dbus-1/system.d/dev.cirodev.hpd.conf` | D-Bus bus-level policy.                  |
| `/usr/share/polkit-1/actions/dev.cirodev.hpd.policy` | polkit action policy.           |
| `/etc/hpd/config.toml`            | Operator configuration (optional, all fields default). |
| `/etc/hpd/config.toml.example`    | Reference config shipped by `install.sh`.          |
| `/var/lib/hpd/state.toml`         | Persistent state (atomic `tempfile + rename`).     |

## Features

- `vendor-asus` *(default)* — compiles in `hpd-backend-asus` and the
  ASUS DMI detector. Without any vendor feature the binary builds
  but exits at startup with "no backend matched DMI".
- `simulator` — pulls `hpd-sysfs/mock`, `vendor-asus`, and
  `hpd-dbus/simulator`. Produces a binary that uses the in-memory
  `MockSysfs` ASUS firmware tree, binds to the session bus, and
  bypasses polkit. macOS / dev-host friendly.

## Dependencies

| Dep                  | Purpose                                          |
|----------------------|--------------------------------------------------|
| `hpd-capabilities`   | Reads `RuntimeConfig`, value types.              |
| `hpd-core`           | Hosts the `Executor`.                            |
| `hpd-sysfs`          | Wraps `/sys` (real or mock).                     |
| `hpd-backend-asus`   | (optional, behind `vendor-asus`).                |
| `hpd-dbus`           | Exposes `PowerDaemonInterface`.                  |
| `hpd-netlink`        | udev AC/DC event source.                         |
| `tokio` + `zbus`     | Async runtime + D-Bus server.                    |
| `tracing` + `tracing-subscriber` | Structured logging to journald.      |
| `serde` + `toml`     | Config parsing.                                  |

## Running locally

Linux (production-shaped install):

```bash
./install.sh                # builds release, installs, enables hpd.service
journalctl -fu hpd          # live logs
./uninstall.sh --purge      # remove + wipe /var/lib/hpd, /etc/hpd
```

macOS / dev host (simulator):

```bash
HPD_SIMULATOR=1 cargo run -p hpd-daemon --features hpd-daemon/simulator
# in another shell:
HPD_SIMULATOR=1 cargo run -p hpd-cli -- status
```

## Docs

```bash
cargo doc -p hpd-daemon --features simulator --no-deps --open
```

## License

GPL-3.0-or-later. See the workspace `Cargo.toml`.
