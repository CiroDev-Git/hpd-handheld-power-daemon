# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working
with code in this repository.

## Project overview

`hpd` (Handheld Power Daemon) is a Linux system daemon — written in Rust
as a Cargo workspace — that manages TDP/power envelope, platform profile
(cooling) + EC-mediated fan curves, battery charge thresholds, and
fan / temperature / power reporting on handheld PCs
(currently ASUS ROG Ally / Ally X / Xbox Ally X). It ships two binaries:

- `hpd-daemon` — long-running root service, exposes D-Bus interface
  `dev.cirodev.hpd.PowerDaemon1` on the system bus.
- `hpdctl` (from crate `hpd-cli`) — user-facing CLI that talks to the
  daemon over D-Bus.

Current release: **`2.0.0`** (see `CHANGELOG.md`). The public
surface (D-Bus interface, CLI subcommands, on-disk state, polkit action
IDs) is stable and follows SemVer.

## Common commands

Build the full workspace (debug): `cargo build`
Release build (what `install.sh` produces): `cargo build --release`
Run all tests across the workspace: `cargo test`
Run tests for a single crate: `cargo test -p hpd-core` (replace crate name)
Run a single test by name: `cargo test -p hpd-core test_profile_inference`
Lint: `cargo clippy --workspace --all-targets -- -D warnings`
Generate docs: `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`

### Running the daemon locally

Production install (Linux, ASUS handheld): `./install.sh` — builds
release, copies binaries to `/usr/local/bin`, installs the systemd unit
(`package/hpd.service`), the D-Bus policy (`package/dev.cirodev.hpd.conf`),
the polkit policy (`package/polkit/dev.cirodev.hpd.policy`) and rule
(`package/polkit/49-hpd.rules`), and the
example config (`/etc/hpd/config.toml.example`); then enables and starts
`hpd.service`. Live logs: `journalctl -fu hpd`. Uninstall:
`./uninstall.sh` (pass `--purge` to also wipe `/var/lib/hpd` and `/etc/hpd`).

Simulator mode (macOS / dev hosts without sysfs):

```bash
HPD_SIMULATOR=1 cargo run -p hpd-daemon --features hpd-daemon/simulator
# in another shell:
HPD_SIMULATOR=1 cargo run -p hpd-cli -- status
```

The simulator (a) returns a fake ROG Ally X DMI, (b) injects a `MockSysfs`
pre-populated with the expected ASUS firmware-attribute files, (c)
switches the daemon and CLI to the **session bus** instead of the system
bus, and (d) short-circuits the polkit authorization check (no polkit
authority exists on macOS / a dev session bus).

The `simulator` Cargo feature on `hpd-daemon` is what compiles the
`MockSysfs` path in; default release builds (`vendor-asus` only)
intentionally exclude it so production binaries never carry mock or
polkit-bypass code. `simulator` implies `vendor-asus` and
`hpd-dbus/simulator` because the simulator currently only models ASUS
firmware and needs the polkit bypass enabled in `hpd-dbus`.

## Architecture

The workspace is organized in numbered layers (L-1 → L4). The dependency
direction is strictly upward: lower layers must not depend on higher ones.

```
L-1 hpd-error        Cross-cutting error types (HpdError, SysfsError,
                     BackendError). Only dep: thiserror. Consumed by
                     every other crate so the whole tree shares one
                     error hierarchy.
L0  hpd-sysfs        Sysfs read/write trait (SysfsIo) + RealSysfs +
                     optional MockSysfs (behind the `mock` feature).
    hpd-netlink      udev (tokio-udev) AC/DC event monitor; no-op on
                     non-Linux. (Lenovo + Valve placeholder crates were
                     removed in 1.0; reintroduce as real backends in
                     1.x minors when implementations land.)
L2  hpd-capabilities Hardware-agnostic traits + value types (mW, RPM,
                     profiles, limits). Defines HwBackend =
                     PowerEnvelope + ChargeControl + PlatformProfile +
                     FanControl + FanCurveControl + ThermalSensors, plus
                     the hot-swappable RuntimeConfig
                     the executor swaps on `Transition::ConfigReload`.
L1  hpd-backend-asus Vendor backend. Implements L2 traits using L0
                     sysfs paths. Only L1 crate in 1.0.
L3  hpd-core         Domain logic. Pure reducer (`reduce`) + side-
                     effecting Executor + state types + TOML
                     persistence + invariants + auto-profile inference.
L4  hpd-dbus         zbus interface + polkit authorization helper +
                     PolkitAction enum (single source of truth for
                     action IDs).
    hpd-cli (hpdctl) D-Bus client.
    hpd-daemon       Composition root: detect hardware, pick L1
                     backend, load config, wire channels, spawn
                     monitors, host D-Bus service, drive shutdown
                     drain.
```

L2 is numbered before L1 deliberately: backends depend on the capability
traits, not the other way around. The workspace `Cargo.toml` lists
members in dependency order (L-1 first, L4 last).

### State machine (this is the central abstraction)

All mutations flow through a Transition → reducer → Effect pipeline.
**Don't bypass it** by calling backend methods directly from D-Bus
handlers or monitors.

1. External events become `Transition` variants
   (`hpd-core/src/transition.rs`):
   `SetSpl`, `SetEnvelope`, `SetPreset`, `SetProfile`,
   `SetCoolingLevel`, `SetFanCurve`, `ResetFanCurve`,
   `ChargeThresholdChanged`, `AcPowerChanged`, `SystemResumed`,
   `SyncPowerTarget`, `EnableFanAuto`, `ConfigReload(RuntimeConfig)`,
   `Shutdown`.
2. Transitions are sent over an `mpsc::Sender<Transition>` to the
   `Executor` (`hpd-core/src/executor.rs`). Producers:
   * the D-Bus interface (user commands, polkit-gated),
   * the netlink monitor (AC plug events),
   * the suspend monitor (logind `PrepareForSleep` signal on resume),
   * the SIGHUP handler in `hpd-daemon` (config reload),
   * the SIGINT/SIGTERM handler in `hpd-daemon` (graceful shutdown),
   * the executor itself (rollback via `SyncPowerTarget`).
3. The pure `reduce()` function (`hpd-core/src/reducer.rs`) takes the
   current `ProfileState` + a `Transition` + hardware
   `PowerEnvelopeLimits` + the current `RuntimeConfig`, validates
   invariants (e.g. FPPT ≥ SPPT ≥ SPL, SPL within hw range), and
   returns a new state + a list of `Effect`s. **It must stay pure —
   no I/O, no async, no globals, no `println!`.** Logging from inside
   the reducer goes through `tracing::info!` only (structured fields).
4. The `Executor` applies the new state to a
   `tokio::sync::watch::Sender<ProfileState>` (D-Bus readers observe
   via the receiver) and dispatches each `Effect`
   (`ApplyPowerEnvelope`, `ApplyPlatformProfile`,
   `ApplyChargeThreshold`, `PersistState`) to the backend.
5. **Auto-profile-follow.** When `fan_follows_tdp` is on and a TDP
   change comes through, the reducer's `apply_target_and_profile`
   infers the matching platform profile and emits a single
   `ApplyPlatformProfile` effect *in the same batch* as the
   `ApplyPowerEnvelope`. The executor does **not** re-inject any
   transition for this — the inference lives entirely inside the
   reducer, single source of truth (see `hpd-core/src/inference.rs`).
6. **Rollback on hardware-write failure.** Today only
   `ApplyPowerEnvelope` rolls back: on failure the executor reads the
   real hardware state and re-injects `SyncPowerTarget`. The other two
   `Apply*` effects log on failure but do not roll back — Lote 38 of
   the V2 remediation plan will unify this.
7. **`ConfigReload` interception.** The executor intercepts
   `Transition::ConfigReload(new_config)` *before* `reduce()` is
   called and atomically swaps its own `RuntimeConfig`. The next
   transition uses the new values. The reducer treats `ConfigReload`
   as a no-op so calling `reduce()` with it in isolation (unit tests)
   is harmless.
8. **`Shutdown` drains and exits.** The executor processes
   `Transition::Shutdown` through the normal reducer path (which
   emits a final `PersistState`), then breaks its `run()` loop. The
   daemon awaits the executor join (5s timeout, well under systemd's
   90s `TimeoutStopSec`) before closing the D-Bus connection and
   returning.

State is persisted to **`/var/lib/hpd/state.toml`** via atomic
temp-file + rename. Under systemd the path is resolved from the
`STATE_DIRECTORY` environment variable injected by `StateDirectory=hpd`
in the unit; outside systemd it falls back to the config file's
`state_path`. The persisted state intentionally skips
`is_ac_connected` (`#[serde(skip)]`) — that is re-queried from
hardware at boot.

### Authorization

Every privileged D-Bus setter (`set_spl`, `set_preset`,
`set_charge_threshold`, `set_profile`, `set_fan_auto`) calls
`hpd_dbus::polkit::check(...)` *before* enqueuing its `Transition`.
The check talks to `org.freedesktop.PolicyKit1.Authority` directly
(no extra crate dep) and asks for one of:

- `dev.cirodev.hpd.set-tdp` — TDP / preset changes (`auth_admin`).
- `dev.cirodev.hpd.set-charge` — charge threshold (`auth_admin`).
- `dev.cirodev.hpd.set-profile` — cooling level / platform profile +
  fan-auto (`auth_admin_keep` — 5-minute cache).
- `dev.cirodev.hpd.set-fan-curve` — fan-curve set / reset
  (`auth_admin_keep` — 5-minute cache).

These `<defaults>` in `package/polkit/dev.cirodev.hpd.policy` are the
baseline for **non-administrator** callers. **`wheel`-group members
(the device owner) are granted every `dev.cirodev.hpd.*` action without
a prompt** by the companion JS rule
`package/polkit/49-hpd.rules` (`polkit.Result.YES` for
`subject.isInGroup("wheel")`). The rule keys on **group membership, not
the `allow_active`/`allow_inactive`/`allow_any` session tiers**, on
purpose: on handheld desktop sessions a physically-local terminal can
register as `Remote=yes` (e.g. driven over SSH, or a DM that doesn't
attach the session to the seat), which would otherwise drop the owner
into `allow_any` and force a password prompt. Non-`wheel` callers fall
through to the `auth_admin` defaults. The rule needs a polkit build with
the JS engine (>= 0.106), standard on modern distros.

Action IDs live in one place: the `PolkitAction` enum in
`hpd-dbus/src/actions.rs`. Adding a new privileged setter means
adding a variant there + matching `<action>` block in
`package/polkit/dev.cirodev.hpd.policy` (the `49-hpd.rules` grant
already covers any `dev.cirodev.hpd.*` action by prefix).

**Fail-closed:** any error talking to polkit (proxy creation
failure, method-call timeout, malformed reply, missing sender header)
is logged as a warning and the check returns `false`. Refusing a
legitimate request is preferable to allowing an unauthenticated one.

**Simulator bypass:** under `#[cfg(feature = "simulator")]` the check
unconditionally returns `true` — session-bus runs on macOS / dev
hosts have no polkit authority to talk to and gating every setter
would make the simulator unusable.

**Registration self-check.** A partial install (binary copied without
`package/polkit/*`) leaves the action IDs unregistered, so polkit answers
every `CheckAuthorization` with "action is not registered" and the daemon
fail-closes — surfacing only as an opaque `AuthFailed` on every setter.
To make the root cause obvious, `hpd_dbus::polkit::missing_actions`
queries polkit's `EnumerateActions` and returns the subset of
`PolkitAction::ALL` it does not know. The daemon runs this once at
startup (loud warning naming the missing files + fix, then keeps running)
and exposes it live over D-Bus via `get_diagnostics() -> (polkit_ok,
missing_action_ids)`, which `hpdctl status` and the Decky plugin render.
`install.sh` step 5 verifies the same thing post-install with `pkaction`.
`PolkitAction::ALL` must list every variant — an exhaustiveness test in
`hpd-dbus/src/actions.rs` flags drift.

**One-command fix.** `hpdctl fix-polkit` (`hpd-cli/src/fix.rs`) installs
the policy + rules and reloads polkit. The two files are embedded into
`hpdctl` with `include_str!("../../../package/polkit/…")` so the fix needs
no source tree; an unprivileged run re-execs `pkexec hpdctl fix-polkit
--apply` (falling back to `sudo`) — both use polkit's core
`org.freedesktop.policykit.exec` action, which is registered even when
ours are not. `hpdctl status` offers to run it interactively. The daemon
**cannot** self-heal here: `package/hpd.service` sets `ProtectSystem=strict`,
so `/usr` is read-only to the daemon — the privileged write has to come
from the user-side CLI.

### Competing power daemons

hpd expects to be the **sole** manager of the platform power knobs. Two
system daemons seen on handheld images write the *same* surfaces and so
fight it: `power-profiles-daemon` (owns `platform_profile` + EPP) and
Valve's `steamos-manager` (the TDP / charge / fan backend behind Steam
Game Mode's performance panel). Co-running either makes the last writer
win and the effective state flap.

Same split as polkit — **the daemon detects, the CLI repairs, the package
only informs:**

- **Detect.** `hpd_dbus::conflicts::power_conflicts` (single source of
  truth for the rival list in `hpd-dbus/src/conflicts.rs`) asks the bus
  `NameHasOwner` for each rival's well-known name — which does **not**
  D-Bus-activate, so checking never revives a masked rival. The daemon
  runs this at startup (loud warning) and exposes it live over D-Bus via
  `get_power_conflicts() -> Vec<String>`.
- **Repair.** `hpdctl doctor` (`hpd-cli/src/doctor.rs`) reports both the
  polkit and conflict health; `hpdctl doctor --fix` `disable --now` +
  `mask`s the rival system units and installs the polkit policy (reusing
  `fix.rs`) in one elevated step — a superset of `fix-polkit`. The
  per-user `steamos-manager` proxy is masked as the invoking user before
  elevating (a root `pkexec` child can't target `systemctl --user`
  cleanly). The daemon **cannot** do this itself (`ProtectSystem=strict`).
- **Inform.** `package/hpd.service` declares
  `Conflicts=power-profiles-daemon.service` (starting hpd stops PPD; the
  D-Bus-activated `steamos-manager` is left to `doctor --fix`), and the
  AUR `post_install` points at `hpdctl doctor --fix`.

### Configuration

`DaemonConfig` (`hpd-daemon/src/config.rs`) is the on-disk
configuration loaded from `/etc/hpd/config.toml` at startup
(resolved via `CONFIGURATION_DIRECTORY` injected by the unit's
`ConfigurationDirectory=hpd`). Schema is intentionally minimal —
`serde + toml`, no `figment`, no filesystem watcher.

Survival invariant: a missing or corrupt config file is **never**
fatal. The daemon falls back to `DaemonConfig::default()` and keeps
running. Every field uses `#[serde(default)]` so partial TOML files
also work — adding a new field never breaks an existing config.

Hot reload: `systemctl reload hpd` sends SIGHUP, which the daemon
catches and uses to re-read the file and push a
`Transition::ConfigReload(new_config.to_runtime())` to the executor.
Startup-only fields (`state_path`, `channel_capacity`,
`default_charge_threshold`) are logged as "requires restart" if they
appear to have changed. Runtime-tunable fields (`sppt_factor`,
`fppt_factor`, `profile_thresholds`) take effect on the next
transition.

The example config shipped to operators lives at
`package/hpd-example.toml` and is installed as
`/etc/hpd/config.toml.example` (operator copies it to `config.toml`
to override defaults; existing `config.toml` is never overwritten by
`install.sh`).

### Lifecycle / signals

| Signal     | Source                       | Daemon response                                                                 |
|------------|------------------------------|---------------------------------------------------------------------------------|
| SIGINT     | Ctrl+C in a terminal         | `Transition::Shutdown` → reducer emits `PersistState` → executor drains and exits → daemon closes D-Bus → process returns. |
| SIGTERM    | systemd `stop` / `restart`   | Same as SIGINT.                                                                 |
| SIGHUP     | `systemctl reload` / manual  | Reload `/etc/hpd/config.toml`; push `ConfigReload(new.to_runtime())`. Daemon keeps running. |
| Resume     | logind `PrepareForSleep`     | Push `Transition::SystemResumed`; reducer re-applies envelope + profile + charge threshold (kernel may have lost them across suspend). |
| AC plug    | udev `power_supply` event    | Push `Transition::AcPowerChanged(true/false)`; reducer snapshots `last_dc_target` on plug, restores it on unplug. |

The daemon awaits the executor join after sending `Shutdown` with a
5s timeout cap (`tokio::time::timeout`), well below systemd's
default 90s `TimeoutStopSec`. If persistence hangs, the daemon logs
and exits cleanly rather than letting systemd `SIGKILL` it mid-write.

### Concurrency layout (in `hpd-daemon/src/main.rs`)

- Main thread: `#[tokio::main]` multi-threaded runtime running the
  Executor, the zbus server, the suspend monitor, the SIGHUP handler,
  the `PropertiesChanged` emitter task, and the SIGINT/SIGTERM
  `select!`.
- `tokio-udev`'s `AsyncMonitorSocket` is `!Send`, so the netlink
  monitor runs on a **dedicated std::thread** with its own
  current-thread tokio runtime + `LocalSet`. Don't try to spawn it on
  the main tokio runtime — that's why the manual thread exists.
- The `spawn_properties_changed_emitter` task watches the executor's
  `watch::Receiver<ProfileState>` and emits zbus-generated
  `<prop>_changed` notifiers when the underlying field of each D-Bus
  property changed. This is the actual wiring behind D-Bus
  `org.freedesktop.DBus.Properties.PropertiesChanged` — there is no
  `Effect::EmitDbusPropertiesChanged` (it was removed in Lote 10).
- D-Bus binds to the **system bus** in production and the **session
  bus** when `HPD_SIMULATOR` is set *and* the binary was compiled
  with `--features simulator`. The CLI mirrors the same convention.

### Adding a new vendor backend

1. Create `crates/hpd-backend-<vendor>/` (model on `hpd-backend-asus`).
2. Implement `PowerEnvelope`, `ChargeControl`, `PlatformProfile`,
   `FanControl` (and optionally `FanCurveControl`, `ThermalSensors`)
   from `hpd-capabilities`, then blanket-impl `HwBackend`.
3. Add a `detect.rs` returning `Option<Model>` from a `DmiInfo`.
4. Register the crate in the root `Cargo.toml` `workspace.members`
   list (preserves dependency order).
5. Add a `vendor-<name>` feature in `hpd-daemon/Cargo.toml` gating
   the optional dep, and wire detection in
   `hpd-daemon/src/main.rs::main` (today only ASUS — the cascade
   pattern is intentional while there is one vendor; a detector
   registry will replace it cleanly in 1.x once a second backend
   lands).
6. Add the matching SPDX header on every new `.rs` file
   (`// SPDX-License-Identifier: GPL-3.0-or-later`).
7. Update `package/hpd-example.toml` and the README hardware matrix.

### Adding a new D-Bus / CLI command

1. Add a `Transition` variant in `hpd-core/src/transition.rs`.
2. Handle it in `reduce()` in `hpd-core/src/reducer.rs` (return the
   new state and any effects; no I/O here).
3. If it produces a new kind of side-effect, add an `Effect` variant
   in `hpd-core/src/effect.rs` and handle it in
   `Executor::handle_effect`.
4. If it changes any field that backs a D-Bus property, extend
   `spawn_properties_changed_emitter` in `hpd-daemon/src/main.rs` with
   a matching diff arm.
5. If it is privileged, add a `PolkitAction` variant in
   `hpd-dbus/src/actions.rs` and a matching `<action>` block in
   `package/polkit/dev.cirodev.hpd.policy`.
6. Expose the method via `#[interface]` in `hpd-dbus/src/service.rs`;
   call `polkit::check(...)` before enqueuing the transition.
7. Add the proxy method in `hpd-cli/src/dbus.rs` and the matching
   subcommand in `hpd-cli/src/main.rs`.
8. Add a `### Added` / `### Changed` entry in `CHANGELOG.md`.

## Hard rules

- **`unsafe_code` is forbidden** workspace-wide via
  `.cargo/config.toml`'s `rustflags = ["-F", "unsafe_code"]`. The
  single exception is `hpd-netlink`, which opts in locally with
  `#[allow(unsafe_code)]` if it needs to (today it does not — the
  `tokio-udev` crate carries the unsafe and `hpd-netlink` only
  consumes its safe API).
- `clippy::unwrap_used`, `clippy::expect_used`, and `clippy::panic`
  are **warned** in production code via per-crate
  `#![cfg_attr(not(test), warn(clippy::unwrap_used, clippy::expect_used, clippy::panic))]`
  attributes on each crate root (`lib.rs` / `main.rs`). CI runs
  `cargo clippy --workspace --all-targets -- -D warnings`, so these
  effectively become errors in CI. Test modules (`#[cfg(test)] mod tests`)
  and the two test-fixture modules (`hpd-capabilities::testing`,
  `hpd-sysfs::mock::testing`) carry inner `#![allow(...)]` opting out.
  Use `?` with `HpdError` (re-exported through `hpd_error`) instead.
- Validate at boundaries (`hpd-dbus` rejects bad input, reducer
  enforces invariants); trust internal types past that.
- Every new `.rs` file starts with `// SPDX-License-Identifier: GPL-3.0-or-later`
  followed by a blank line, then attributes / doc comments.

## Things that look weird but are intentional

- `hpd-capabilities` is numbered L2 but listed before L1 backends in
  `Cargo.toml` because L1 depends on L2.
- The netlink monitor spawning a raw `std::thread` with its own
  tokio runtime — required because `tokio-udev`'s socket future is
  `!Send`.
- `is_ac_connected` is `#[serde(skip)]` in `ProfileState` — re-read
  from hardware on every boot rather than trusting stale state.
- `Transition::SetSpl` derives SPPT and FPPT from SPL via fixed
  multipliers (1.15× and 1.25× by default, tunable through
  `RuntimeConfig::sppt_factor`/`fppt_factor`), capped at hw limits.
  `Transition::SetEnvelope` is the manual path that takes all three
  explicitly.
- The `hpd-capabilities::error` module is a one-line `pub use` shim
  re-exporting `hpd_error::*`. It exists for backwards compat across
  the workspace's own callers; new code should prefer importing from
  `hpd_error` directly.
- `Transition::ConfigReload` is intercepted *before* `reduce()` runs
  — the reducer must stay pure so the executor owns the runtime
  config swap.
- `MockSysfs` lives inside `hpd_sysfs::mock::testing` (extra module
  layer) because the inner module scopes `#![allow(clippy::unwrap_used, ...)]`.
- The `simulator` feature on `hpd-daemon` implies `vendor-asus`
  *and* `hpd-dbus/simulator` simultaneously: the simulator needs the
  ASUS firmware model and the polkit bypass in the same build.

## Where to look for things

| You want…                                          | Look in                                              |
|---------------------------------------------------|------------------------------------------------------|
| The state machine (transitions / reducer / effects) | `hpd-core/src/{transition,reducer,effect,executor}.rs` |
| Hardware-write contracts                          | `hpd-capabilities/src/{power,charge,fan,fan_curve,thermal,platform_profile}.rs` |
| ASUS firmware-attribute paths                     | `hpd-backend-asus/src/{power,charge,fan,fan_curve,thermal,profile}.rs`  |
| D-Bus method / property surface                   | `hpd-dbus/src/service.rs`                            |
| Polkit action IDs                                 | `hpd-dbus/src/actions.rs`                            |
| Polkit fail-closed contract                       | `hpd-dbus/src/polkit.rs`                             |
| Polkit registration self-check                    | `hpd-dbus/src/polkit.rs::missing_actions` + `hpd-daemon/src/main.rs` startup check + `install.sh` step 5 |
| Polkit one-command repair (`hpdctl fix-polkit`)   | `hpd-cli/src/fix.rs`                                 |
| Competing power-daemon detection                  | `hpd-dbus/src/conflicts.rs` + `hpd-daemon/src/main.rs` startup check + `get_power_conflicts` in `hpd-dbus/src/service.rs` |
| Power-ownership repair (`hpdctl doctor`)          | `hpd-cli/src/doctor.rs`                              |
| Config schema + reload behaviour                  | `hpd-daemon/src/config.rs`                           |
| Composition root / signal wiring                  | `hpd-daemon/src/main.rs`                             |
| Suspend/resume                                    | `hpd-daemon/src/suspend.rs`                          |
| DMI detection                                     | `hpd-daemon/src/probe.rs` + `hpd-backend-asus/src/detect.rs` |
| Atomic state persistence                          | `hpd-core/src/persistence.rs`                        |
| Per-property D-Bus signal emission                | `hpd-daemon/src/main.rs::spawn_properties_changed_emitter` |
| systemd unit + sandboxing                         | `package/hpd.service`                                |
| polkit policy file (non-admin `auth_admin` defaults) | `package/polkit/dev.cirodev.hpd.policy`           |
| polkit rule (`wheel` passwordless grant)          | `package/polkit/49-hpd.rules`                        |
| D-Bus bus-level policy                            | `package/dev.cirodev.hpd.conf`                       |
| Example config                                    | `package/hpd-example.toml`                           |
