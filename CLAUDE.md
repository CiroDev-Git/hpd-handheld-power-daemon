# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project overview

`hpd` (Handheld Power Daemon) is a Linux system daemon — written in Rust as a Cargo workspace — that manages TDP/power envelope, platform profile (cooling), battery charge thresholds, and fan reporting on handheld PCs (currently ASUS ROG Ally / Ally X / Xbox Ally X). It ships two binaries:

- `hpd-daemon` — long-running root service, exposes D-Bus interface `dev.cirodev.hpd.PowerDaemon1` on the system bus.
- `hpdctl` (from crate `hpd-cli`) — user-facing CLI that talks to the daemon over D-Bus.

## Common commands

Build the full workspace (debug): `cargo build`
Release build (what `install.sh` produces): `cargo build --release`
Run all tests across the workspace: `cargo test`
Run tests for a single crate: `cargo test -p hpd-core` (replace crate name)
Run a single test by name: `cargo test -p hpd-core test_profile_inference`
Lint: `cargo clippy --all-targets`

### Running the daemon locally

Production install (Linux, ASUS handheld): `./install.sh` — builds release, copies binaries to `/usr/local/bin`, installs the systemd unit (`package/hpd.service`) and D-Bus policy (`package/dev.cirodev.hpd.conf`), then enables and starts `hpd.service`. Live logs: `journalctl -fu hpd`.

Simulator mode (macOS / dev hosts without sysfs): `HPD_SIMULATOR=1 cargo run -p hpd-daemon`. The simulator (a) returns a fake ROG Ally X DMI, (b) injects a `MockSysfs` pre-populated with the expected ASUS firmware-attribute files, and (c) switches the daemon and CLI to the **session bus** instead of the system bus. To exercise the CLI against a simulator, run `HPD_SIMULATOR=1 cargo run -p hpd-cli -- <subcommand>`.

`hpd-sysfs` exposes `MockSysfs` only under the `mock` Cargo feature. `hpd-daemon` enables it (`features = ["mock"]`), so simulator mode is built into the production binary — it is not a separate build.

## Architecture

The workspace is organized in numbered layers (L0–L4). The dependency direction is strictly upward: lower layers must not depend on higher ones.

```
L0  hpd-sysfs        Sysfs read/write trait (SysfsIo) + RealSysfs + MockSysfs
    hpd-netlink      udev (tokio-udev) AC/DC event monitor; no-op on non-Linux
L2  hpd-capabilities Hardware-agnostic traits + value types (mW, RPM, profiles,
                     limits, errors). Defines HwBackend = PowerEnvelope +
                     ChargeControl + PlatformProfile + FanControl.
L1  hpd-backend-asus Vendor backends. Implement L2 traits using L0 sysfs paths.
    hpd-backend-lenovo / hpd-backend-valve  (stubs / WIP)
L3  hpd-core         Domain logic. Pure reducer + side-effecting Executor +
                     state types + TOML persistence.
L4  hpd-dbus         zbus interface exposing the daemon over D-Bus.
    hpd-cli (hpdctl) D-Bus client.
    hpd-daemon       Composition root: detect hardware, pick L1 backend,
                     wire channels, spawn monitors, host D-Bus service.
```

L2 is numbered before L1 deliberately: backends depend on the capability traits, not the other way around.

### State machine (this is the central abstraction)

All mutations flow through a Transition → reducer → Effect pipeline. **Don't bypass it** by calling backend methods directly from D-Bus handlers or monitors.

1. External events become `Transition` variants (`hpd-core/src/transition.rs`): `SetSpl`, `SetEnvelope`, `SetPreset`, `SetProfile`, `ChargeThresholdChanged`, `AcPowerChanged`, `SystemResumed`, `SyncPowerTarget`, `EnableFanAuto`, `ConfigReload`.
2. Transitions are sent over an `mpsc::Sender<Transition>` to the `Executor` (`hpd-core/src/executor.rs`). Producers: the D-Bus interface (user commands), the netlink monitor (AC plug events), the suspend monitor (logind resume signal), and the executor itself (rollback + auto-profile-follow).
3. The pure `reduce()` function (`hpd-core/src/reducer.rs`) takes the current `ProfileState` + a `Transition` + hardware `PowerEnvelopeLimits` + `ProfileThresholds`, validates invariants (e.g. FPPT ≥ SPPT ≥ SPL, SPL within hw range), and returns a new state + a list of `Effect`s. **It must stay pure — no I/O, no async, no globals.**
4. The `Executor` applies the new state to a `tokio::sync::watch::Sender<ProfileState>` (D-Bus readers observe via the receiver) and dispatches each `Effect` (`ApplyPowerEnvelope`, `ApplyPlatformProfile`, `ApplyChargeThreshold`, `PersistState`, `EmitDbusPropertiesChanged`) to the backend.
5. If a hardware write fails, the executor reads the real hardware state and re-injects a `SyncPowerTarget` transition to roll the in-memory state back to reality.
6. After a successful TDP change, if `fan_follows_tdp` is on, the executor re-injects a `SetProfile` transition to keep the cooling profile in sync with the new wattage.

State is persisted to `/var/tmp/hpd_state.toml` (TODO: move to `/var/lib/hpd/` for production) via atomic temp-file + rename. The persisted state intentionally skips `is_ac_connected` — that is re-queried from hardware at boot.

### Concurrency layout (in `hpd-daemon/src/main.rs`)

- Main thread: `#[tokio::main]` multi-threaded runtime running the Executor and the zbus server.
- `tokio-udev`'s `AsyncMonitorSocket` is `!Send`, so the netlink monitor runs on a **dedicated std::thread** with its own current-thread tokio runtime + `LocalSet`. Don't try to spawn it on the main tokio runtime — that's why the manual thread exists.
- Suspend/resume detection runs as a normal `tokio::spawn` task subscribing to logind's `PrepareForSleep` signal on the system bus.
- D-Bus binds to the **system bus** in production and the **session bus** when `HPD_SIMULATOR` is set. The CLI mirrors this.

### Adding a new vendor backend

1. Create `crates/hpd-backend-<vendor>/` (model on `hpd-backend-asus`).
2. Implement `PowerEnvelope`, `ChargeControl`, `PlatformProfile`, and `FanControl` from `hpd-capabilities`, then blanket-impl `HwBackend`.
3. Add a `detect.rs` returning `Option<Model>` from a `DmiInfo`.
4. Register it in the root `Cargo.toml` workspace members.
5. Wire detection in `hpd-daemon/src/main.rs::main` (the current code only matches ASUS).

### Adding a new D-Bus / CLI command

1. Add a `Transition` variant in `hpd-core/src/transition.rs`.
2. Handle it in `reduce()` in `hpd-core/src/reducer.rs` (return the new state and any effects; no I/O here).
3. If it produces a new kind of side-effect, add an `Effect` variant in `hpd-core/src/effect.rs` and handle it in `Executor::handle_effect`.
4. Expose it via `#[interface]` method in `hpd-dbus/src/service.rs`.
5. Add the matching subcommand in `hpd-cli/src/main.rs` and the proxy method in `hpd-cli/src/dbus.rs`.

## Hard rules (enforced by `.cargo/config.toml`)

- `unsafe_code` is **forbidden** workspace-wide. The single exception is `hpd-netlink`, which must opt in locally with `#[allow(unsafe_code)]`.
- `clippy::unwrap_used`, `clippy::expect_used`, and `clippy::panic` are warnings — treat them as errors during review. Use `?` with `thiserror`-typed errors (`HpdError` in `hpd-capabilities/src/error.rs`) instead. The few existing `.unwrap()` calls in test code and the simulator setup are tolerated; do not add new ones in non-test code.
- Validate at boundaries (`hpd-dbus` rejects bad input, reducer enforces invariants); trust internal types past that.

## Things that look weird but are intentional

- `hpd-capabilities` is numbered L2 but listed before L1 backends in `Cargo.toml` because L1 depends on L2.
- The netlink monitor spawning a raw `std::thread` with its own tokio runtime — required because `tokio-udev`'s socket future is `!Send`.
- `is_ac_connected` is `#[serde(skip)]` in `ProfileState` — re-read from hardware on every boot rather than trusting stale state.
- `Transition::SetSpl` derives SPPT and FPPT from SPL via fixed multipliers (1.15× and 1.25×), capped at hw limits. `Transition::SetEnvelope` is the manual path that takes all three explicitly.
