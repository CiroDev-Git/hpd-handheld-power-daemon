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

Current release: **`2.7.3`** (see `CHANGELOG.md`). The public
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
5. **Auto-fan-curve-follow (power/cooling decoupled).** Power and
   cooling are independent levers. When `fan_follows_tdp` is on and a
   TDP change comes through, the reducer's `apply_target_and_profile`
   infers the matching **fan-curve preset** (not the platform profile)
   via `infer_fan_curve_from_spl` and emits a single `ApplyFanCurve`
   effect *in the same batch* as the `ApplyPowerEnvelope`. The ACPI
   `platform_profile` is **never** inferred from TDP — it is a separate
   power lever programmed at boot from `DaemonConfig::default_platform_profile`
   (default `Performance`, so the SPL you set is the real usable limit;
   a `PowerSaver` EPP would otherwise clamp the APU well below it). The
   inference lives entirely inside the reducer (see
   `hpd-core/src/inference.rs`). `cool set` (`SetCoolingLevel`) likewise
   programs the fan curve only; `set_profile` is the manual power-profile
   lever and stays decoupled from cooling.
6. **Rollback on hardware-write failure.** All four `Apply*` effects
   roll back through the shared `Executor::rollback` helper: on a backend
   write failure the executor reads the **real** hardware state and
   re-injects the matching `Sync*` transition so `ProfileState` converges
   to what the device actually has — `ApplyPowerEnvelope`→`SyncPowerTarget`,
   `ApplyPlatformProfile`→`SyncPlatformProfile`,
   `ApplyChargeThreshold`→`SyncChargeThreshold`, and
   `ApplyFanCurve`/`ResetFanCurve`→`SyncFanCurve` (via
   `FanCurveControl::active_selection`, which reads the EC and matches the
   live points back to a preset / `custom` / firmware-`auto`). This is the
   invariant that keeps the reported knob state from ever claiming a value
   the hardware refused. (`SetSpl`/etc. additionally `PersistState`; the
   `Sync*` rollbacks deliberately do **not** persist — the executor read
   the authoritative value from hardware, so a reboot re-reads + re-asserts
   the same thing.)
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

### AC = maximum performance (the lock)

The **`ProfileState::ac_max_performance`** preference (persisted, default on,
toggleable at runtime) decides whether wall power means "run flat out, hands
off." It is seeded on first boot from `DaemonConfig::default_ac_max_performance`,
then lives in `state.toml` and is flipped via `set_ac_max_performance`
(`hpdctl ac-lock on|off`, or the plugin toggle) — **not** a config file edit.

- **Force on plug (lock on).** `Transition::AcPowerChanged(true)` snapshots the
  user's battery state into `ProfileState::last_dc_state` (a `DcSnapshot` of
  TDP + power mode + cooling + auto-cooling), then `force_ac_max_performance`
  pins **Performance / Max TDP / Aggressive** (`fan_follows_tdp` off — the
  curve is pinned, not inferred). Effects order: power → profile → fan curve
  **last** (writing `platform_profile` can drop the EC curve).
- **Lock off = fully manual.** With the preference off, `AcPowerChanged` (both
  edges) is a **no-op** — plugging/unplugging changes nothing and everything
  stays editable.
- **Restore on unplug (lock on).** `restore_dc_state` re-applies the
  `last_dc_state` snapshot (diff-only effects) and **clears it**
  (`last_dc_state = None`) so a later manual-mode unplug can't replay a stale
  snapshot. **With no snapshot** — the first unplug after a device *installed /
  booted while plugged in* — it synthesizes **quiet battery defaults**: the
  **Balanced** TDP preset *and* re-engages auto-cooling (`fan_follows_tdp =
  true`) so the curve drops from the forced `Aggressive`, rather than leaving
  the fans roaring on battery. The power mode is left at `Performance` (the
  daemon's always-on default). Both unplug paths reduce on an `is_ac_connected
  = false` view so the lock doesn't gate their own internal `SetPreset`.
- **The lock.** While `is_ac_connected && ac_max_performance`, the reducer
  treats the seven user power/cooling writes (`SetSpl`, `SetPreset`,
  `SetEnvelope`, `SetProfile`, `SetCoolingLevel`, `EnableFanAuto`,
  `ResetFanCurve`) as **no-ops** (`is_locked_write`). The D-Bus setters also
  `reject_if_locked()` up-front. `ChargeThresholdChanged` and
  `SetAcMaxPerformance` are **never** gated — the battery limit stays editable,
  and the toggle is how you *release* the lock. `Sync*` rollbacks,
  AC/suspend/boot and `ConfigReload` are never gated.
- **The toggle (`SetAcMaxPerformance`).** Applied immediately: turning it
  **on** while plugged snapshots the current state (if `last_dc_state` is
  `None`) and forces max; turning it **off** while plugged restores the
  snapshot (so you are not stranded at max) and unlocks. On battery it just
  stores the preference (applies on the next plug).
- **Boot/resume reconciles against the real AC state.** The executor
  **re-reads `is_ac_connected` from the backend** before reducing
  `SystemResumed` (it can be stale across a suspend if the charger was
  (un)plugged while asleep). The arm then: **on AC** re-asserts the forced-max
  lock (no plug edge fires at boot); **on battery with a `last_dc_state`
  snapshot** restores it (the persisted levers are the stale forced-max from
  the prior AC session — this is what stops a "shut down on AC, unplug, boot
  on battery" device from coming up at max); **on battery with no snapshot**
  re-applies the persisted (genuine battery) state.
- **Reported state.** `ProfileState::ac_locked` is **derived, never persisted**
  (`#[serde(skip)]`): the executor recomputes it (`is_ac_connected &&
  ac_max_performance`) on every state publish, and `hpd-dbus` exposes both
  `AcMaxPerformance` (the preference) and `AcLocked` (the live lock) as
  properties so the plugin can render its toggle and disable controls live.

State is persisted to **`/var/lib/hpd/state.toml`** via atomic
temp-file + rename. Under systemd the path is resolved from the
`STATE_DIRECTORY` environment variable injected by `StateDirectory=hpd`
in the unit; outside systemd it falls back to the config file's
`state_path`. The persisted state intentionally skips
`is_ac_connected` (`#[serde(skip)]`) — that is re-queried from
hardware at boot.

### Authorization

Every privileged D-Bus setter (`set_spl`, `set_preset`,
`set_charge_threshold`, `set_profile`, `set_cooling_level`, `set_fan_auto`,
`reset_fan_curve`, `set_ac_max_performance`) calls
`hpd_dbus::polkit::check(...)` *before* enqueuing its `Transition`.
The check talks to `org.freedesktop.PolicyKit1.Authority` directly
(no extra crate dep) and asks for one of:

- `dev.cirodev.hpd.set-tdp` — TDP / preset changes (`auth_admin`).
- `dev.cirodev.hpd.set-charge` — charge threshold (`auth_admin`).
- `dev.cirodev.hpd.set-profile` — cooling level / platform profile +
  fan-auto + fan-curve **reset** (`auth_admin_keep` — 5-minute cache).
  (The separate `set-fan-curve` action and the unused raw `set_fan_curve`
  D-Bus method were retired in 2.5.0; `set_cooling_level` covers the fan
  curve and `reset_fan_curve` moved onto this action.)

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

hpd expects to be the **sole** manager of the platform power knobs.
Several daemons seen on handheld images write the *same* surfaces and so
fight it; co-running any makes the last writer win and the effective state
flap. They split two ways:

- **Hard rivals** (must not co-run — `doctor --fix` masks them):
  `power-profiles-daemon` (`platform_profile` + EPP), Valve's
  `steamos-manager` (TDP / charge / fan behind Game Mode), `tuned`
  (Fedora/Bazzite's default tuner; its `tuned-ppd` shim also claims the PPD
  name), and `hhd` (Handheld Daemon — Bazzite's Ally default).
- **Advisory** (wanted, so reported but *never* masked): Feral `gamemoded`
  (governor boost during games), `asusd` (also drives platform profile /
  fan / charge on ASUS, **but** owns keyboard RGB / Aura so masking it is
  the wrong call), and `auto-cpufreq` (governor / EPP only).

Same split as polkit — **the daemon detects, the CLI repairs, the package
only informs:**

- **Detect.** `hpd_dbus::conflicts` is the single source of truth, with two
  detection mechanisms because not every rival owns a bus name:
  - **By D-Bus name** (`RIVAL_POWER_DAEMONS`, `ADVISORY_POWER_DAEMONS`):
    `NameHasOwner`, which does **not** D-Bus-activate, so checking never
    revives a masked rival.
  - **By active systemd unit** (`RIVAL_UNITS`, `ADVISORY_UNITS`): for
    daemons with no bus name (`hhd`, `auto-cpufreq`), a read-only
    `org.freedesktop.systemd1` `ListUnitsByPatterns` query (allowed under
    `ProtectSystem=strict`; it inspects, never starts, a unit).

  `power_conflicts()` (hard rivals) and `advisory_daemons()` each union both
  mechanisms. The daemon runs `power_conflicts` at startup (loud warning)
  and exposes both live over D-Bus via `get_power_conflicts() -> Vec<String>`
  and `get_advisory_daemons() -> Vec<String>`. The rival and advisory lists
  are kept disjoint across **both** axes by a regression test, so
  `doctor --fix` never masks a daemon it only meant to report.

  **Undetectable by design:** a tool that writes TDP from inside another
  process — a Decky plugin (SimpleDeckyTDP, PowerControl) in the plugin
  loader, or a manual `ryzenadj` — owns no service or bus name, so neither
  mechanism sees it.
- **Repair.** `hpdctl doctor` (`hpd-cli/src/doctor.rs`) reports the polkit,
  conflict, advisory (GameMode) and gamescope-session health via the shared
  `doctor::print_health` renderer; `hpdctl doctor --fix` `disable --now` +
  `mask`s the rival system units and installs the polkit policy (reusing
  `fix.rs`) in one elevated step — a superset of `fix-polkit`. The
  per-user `steamos-manager` proxy is masked as the invoking user before
  elevating (a root `pkexec` child can't target `systemctl --user`
  cleanly). The daemon **cannot** do this itself (`ProtectSystem=strict`).
  `hpdctl status` ends with the **same** `print_health` block (wrapped in
  the dashboard frame) so the everyday status command answers "is anything
  overriding hpd?" with an explicit all-clear. The gamescope-session hint
  is detected client-side in the CLI (the daemon, a root system service,
  does not see the user's session env).
- **Inform.** `package/hpd.service` declares
  `Conflicts=power-profiles-daemon.service` (starting hpd stops PPD; the
  D-Bus-activated `steamos-manager` is left to `doctor --fix`), and the
  AUR `post_install` points at `hpdctl doctor --fix`. The unit-level
  `Conflicts=`/`After=` and the `post_install` mask are **PPD-only by
  design** — they are *not* extended to the newer hard rivals `tuned` and
  `hhd`. `Conflicts=` is symmetric, so it only helps against an
  already-masked rival; a D-Bus-activatable one like `tuned` would
  otherwise be revived by the bus and kill *hpd* (the v2.2.2 regression),
  and `hhd`'s templated `hhd@<user>.service` cannot be named at package
  time at all. PPD uniquely earns automatic neutralization (a headless,
  ubiquitous, boot-race-proven service safe to mask silently); `tuned` and
  `hhd` are user-chosen stacks, so their neutralization stays opt-in via
  `hpdctl doctor --fix`. The header comment in `package/hpd.service`
  records this so it is not "helpfully" re-added.

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
| Boot       | daemon startup               | Build the initial state (re-reading AC + platform profile from hardware, overriding `active_profile` to the configured default), then push `Transition::SystemResumed` to **re-assert the full intended state** (envelope + profile + charge + fan curve) onto the hardware — so the reported state matches the device even after a cold boot reset firmware knobs (profile → balanced, charge → 100 %, …) to their defaults. |
| SIGINT     | Ctrl+C in a terminal         | `Transition::Shutdown` → reducer emits `PersistState` → executor drains and exits → daemon closes D-Bus → process returns. |
| SIGTERM    | systemd `stop` / `restart`   | Same as SIGINT.                                                                 |
| SIGHUP     | `systemctl reload` / manual  | Reload `/etc/hpd/config.toml`; push `ConfigReload(new.to_runtime())`. Daemon keeps running. |
| Resume     | logind `PrepareForSleep`     | Push `Transition::SystemResumed`. The executor **re-reads the real AC state from hardware** first (it can be stale if the charger was (un)plugged while suspended), then the reducer re-applies the right policy: force max on AC, restore the battery snapshot (`last_dc_state`) on battery, else re-apply persisted (kernel may have lost levers across suspend). |
| AC plug    | udev `power_supply` event    | Push `Transition::AcPowerChanged(true/false)`. **When the `ac_max_performance` preference is on (default):** on plug, snapshot the battery (DC) state into `last_dc_state` and force **Performance / Max TDP / Aggressive** + lock; on unplug, restore the snapshot. **When off:** both edges are a no-op (AC fully manual). See "AC = maximum performance" below. |

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
- **Both event monitors self-reconnect (since 2.7.2).** The netlink AC
  monitor and the logind suspend monitor wrap their `stream.next()` loop
  in an **outer reconnect loop**: a single `Err`/`None` from the stream
  (a suspend can perturb the socket) used to fall out of the old
  `while let Some(...)` and silently kill the monitor for the rest of the
  process — stopping live AC detection or resume detection until a daemon
  restart. They now log, back off (2 s), and rebuild; the netlink monitor
  also reconciles the mains node on every (re)connect so a missed edge is
  still emitted. Don't "simplify" either back to a bare `while let`. The
  full lifecycle matrix lives in `docs/dev/LIFECYCLE.md`.
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
- **Boot re-uses `Transition::SystemResumed`.** The composition root
  sends it once at startup to re-assert the full intended state
  (envelope + profile + charge + fan curve) onto the hardware
  unconditionally — a cold boot resets firmware knobs to their defaults,
  so trusting the persisted values without re-applying would make the
  daemon report state the device no longer has. Same path as resume; the
  reducer log says "boot/resume". This is why the boot does **not** send
  separate `SetProfile`/`SetCoolingLevel` transitions any more.
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
- **`set_profile` / `ActiveProfile` / `active_profile` / the `set-profile`
  polkit action all name the ACPI `platform_profile`** (the EPP / power-bias
  lever: `power-saver`/`balanced`/`performance`), mirroring the kernel's
  own `/sys/firmware/acpi/platform_profile` term — that's why the internal
  name is "profile" even though the user-facing surface calls it **"Power
  mode"** (Decky plugin) and **`hpdctl power`** (CLI). The names were kept
  deliberately (kernel-accurate internally, friendly externally) rather than
  renamed, since a rename would break the D-Bus/polkit surface + the
  persisted `state.toml`. Note the `set-profile` polkit action also gates
  the cooling levers (`set_cooling_level` / `set_fan_auto` /
  `reset_fan_curve`) and the AC-lock toggle (`set_ac_max_performance`) —
  it's the shared "low-impact, `auth_admin_keep`"
  bucket, not only the power profile.

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
| Competing power-daemon detection                  | `hpd-dbus/src/conflicts.rs` + `hpd-daemon/src/main.rs` startup check + `get_power_conflicts` / `get_advisory_daemons` in `hpd-dbus/src/service.rs` |
| Power-ownership repair + shared health block      | `hpd-cli/src/doctor.rs` (`doctor::print_health`, reused by `hpdctl status`) |
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
