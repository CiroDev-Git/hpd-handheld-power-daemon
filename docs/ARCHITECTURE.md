# `hpd` — Architecture

> A reader-friendly walk-through of how the Handheld Power Daemon is
> structured, what each layer is responsible for, and how a single
> command travels from `hpdctl` to a sysfs write and back.
>
> The terse, assistant-oriented version of this document lives at
> [`/CLAUDE.md`](../CLAUDE.md). This one is for humans.

---

## 1. What `hpd` does (and doesn't)

`hpd` is a Linux system daemon that manages the **power envelope**
(SPL / SPPT / FPPT), **cooling profile**, **battery charge
threshold**, and **fan reporting** of handheld PCs. It currently
targets the ASUS ROG Ally family (Ally, Ally X, Xbox Ally X). It
ships as a single workspace producing two binaries:

| Binary       | Crate         | Purpose                                                                                |
|--------------|---------------|----------------------------------------------------------------------------------------|
| `hpd-daemon` | `hpd-daemon`  | Long-running root service. Owns the hardware. Exposes `dev.cirodev.hpd.PowerDaemon1`.  |
| `hpdctl`     | `hpd-cli`     | Thin D-Bus client end users invoke from their shell or overlay HUD.                    |

**Out of scope by design:**
- Fan curves — left to vendor firmware. The fan crate is read-only.
- Per-app profiles or autosave-on-game-start — kept stateless above
  the daemon; that complexity belongs in user-space agents.
- Cross-distro packaging — the unit/policy files target a systemd
  + polkit + D-Bus stack and are designed for that environment.
- Display / haptics / RGB — different problem domains.

---

## 2. Workspace layout

The workspace is a strict dependency hierarchy. Lower layers must
**never** depend on higher ones. The numbered `L-1 → L4` labels are
the canonical way the project talks about layers; the `Cargo.toml`
`workspace.members` list is ordered to match.

```
                    ┌────────────────────────────────────────────┐
   L4 (interface)   │  hpd-daemon       hpd-cli       hpd-dbus   │
                    │  (binary)         (binary)      (lib)      │
                    └────────────────────────────────────────────┘
                                       ▲
                    ┌──────────────────┴─────────────────────────┐
   L3 (domain)      │             hpd-core                       │
                    │  reduce()  Executor  state  persistence    │
                    └────────────────────────────────────────────┘
                                       ▲
                    ┌──────────────────┴─────────────────────────┐
   L2 (capability)  │           hpd-capabilities                 │
                    │  HwBackend + Power/Charge/Profile/Fan       │
                    └────────────────────────────────────────────┘
                                       ▲
                    ┌──────────────────┴─────────────────────────┐
   L1 (vendor)      │       hpd-backend-asus  (only one in 1.0)  │
                    └────────────────────────────────────────────┘
                                       ▲
                    ┌──────────────────┴─────────────────────────┐
   L0 (kernel I/O)  │       hpd-sysfs           hpd-netlink      │
                    │       (SysfsIo)           (udev → AC)      │
                    └────────────────────────────────────────────┘
                                       ▲
                    ┌──────────────────┴─────────────────────────┐
   L-1 (errors)     │       hpd-error      (no internal deps)    │
                    └────────────────────────────────────────────┘
```

**Why L2 sits *below* L1 in dependency order:** vendor backends
implement the capability traits, not the other way around. The
hierarchy reads `L-1 → L0 → L2 → L1 → L3 → L4` if you walk the dep
edges, which is exactly the order the workspace manifest lists its
members in.

Per-crate detail lives in each crate's `README.md`. Quick lookup:

| Crate                | Layer | What it owns                                                |
|----------------------|:-----:|-------------------------------------------------------------|
| `hpd-error`          | L-1   | `HpdError`, `SysfsError`, `BackendError`                    |
| `hpd-sysfs`          | L0    | `SysfsIo` trait + `RealSysfs` + (opt) `MockSysfs`           |
| `hpd-netlink`        | L0    | `spawn_power_monitor` (udev `power_supply` events)          |
| `hpd-capabilities`   | L2    | Capability traits, value types, `RuntimeConfig`             |
| `hpd-backend-asus`   | L1    | `AsusBackend` + 4 sub-backends, DMI detector                |
| `hpd-core`           | L3    | `reduce()`, `Effect`, `Executor`, persistence               |
| `hpd-dbus`           | L4    | `PowerDaemonInterface`, polkit gating                       |
| `hpd-cli`            | L4    | `hpdctl` subcommands + D-Bus proxy                          |
| `hpd-daemon`         | L4    | Composition root: detect → wire → run                       |

---

## 3. The state machine

This is the single most important abstraction in the codebase.
Every hardware mutation flows through it. **Don't bypass it** by
calling backend methods directly from D-Bus handlers or monitors.

```
                  +---------------------------+
   external event |                           |
   ┌────────────► │       Transition          │ ── enum, "what happened"
   │ (D-Bus call, |                           |
   │  AC plug,    +-------------┬-------------+
   │  SIGHUP,     ┌─────────────┴─────────────┐
   │  resume…)    │     Executor (async)      │
   │              │   - holds RuntimeConfig   │
   │              │   - holds backend handle  │
   │              │   - holds watch::Sender   │
   │              │     <ProfileState>        │
   │              └─────────────┬─────────────┘
   │                            │ calls (state, t, limits, cfg)
   │                            ▼
   │              +---------------------------+
   │              |      reduce()  PURE       |
   │              |   - validates invariants  |
   │              |   - infers auto fan curve |
   │              |   - returns (state', fx)  |
   │              +-------------┬-------------+
   │                            │ Vec<Effect>
   │                            ▼
   │              +---------------------------+
   │              |   Executor.handle_effect  |
   │              |   - dispatches each fx    |
   │              |   - rolls back on failure |
   │              |     (re-injects Sync*)    │
   │              +-------------┬-------------+
   │                            │ writes /sys
   │                            ▼
   │              +---------------------------+
   │              |   hpd-backend-asus (L1)   |
   │              +---------------------------+
   │                            │
   └─── rollback Transition ◄───┘ on Err
        (Sync* re-reads hw and re-enters reducer)
```

### Three rules the reducer must obey

1. **Pure.** No I/O, no async, no globals, no `println!`.
   Tracing inside `reduce()` is allowed but only via structured
   `tracing::info!` fields.
2. **All side-effects via `Effect`.** Today: `ApplyPowerEnvelope`,
   `ApplyPlatformProfile`, `ApplyChargeThreshold`, `ApplyFanCurve`,
   `ResetFanCurve`, `PersistState`. The Executor is the only thing that
   dispatches them.
3. **`ConfigReload` is intercepted *before* `reduce()` is called.**
   The Executor atomically swaps its `RuntimeConfig`. The next
   transition uses the new values. The reducer treats `ConfigReload`
   as a no-op so calling it in isolation in unit tests is harmless.

### Auto fan-curve inference (power/cooling decoupled)

Power and cooling are independent levers. When
`ProfileState::fan_follows_tdp == true` and a TDP change comes through,
`reduce()` infers the matching **fan-curve preset** (via
`inference::infer_fan_curve_from_spl`) and emits an `ApplyFanCurve` effect
*in the same batch* as the `ApplyPowerEnvelope`. The executor does **not**
re-inject any transition for this — single source of truth, no feedback
loop.

The ACPI `platform_profile` is **never** inferred from TDP: it is a
decoupled power/EPP lever, programmed at boot from
`DaemonConfig::default_platform_profile` (default `Performance`, so the SPL
is the real usable limit) and only changed by the explicit `SetProfile`
transition. `SetCoolingLevel` (`hpdctl cool set`) likewise drives the fan
curve only. This replaced the pre-decouple behaviour where the cooling
level and the TDP auto-follow both drove the platform profile, whose EPP
silently clamped the SoC below the configured SPL.

### Rollback contract

If any `Apply*` effect fails:

1. The Executor logs the failure.
2. It re-reads the live hardware state through the backend.
3. It re-injects a matching `Sync*` transition (`SyncPowerTarget`,
   `SyncPlatformProfile`, `SyncChargeThreshold`) so the in-memory
   `ProfileState` matches what the kernel actually has.

Lote 38 made this uniform across all three apply effects (only
`Apply{PowerEnvelope}` rolled back in pre-1.0 versions).

### Transition catalogue

| Transition                       | Source                                            |
|----------------------------------|---------------------------------------------------|
| `SetSpl(u32)`                    | `hpdctl tdp set`, D-Bus `set_spl`                 |
| `SetEnvelope(PowerEnvelopeTarget)`| Manual full-envelope path (no preset, no derive)  |
| `SetPreset(TdpPreset)`           | `hpdctl preset`, D-Bus `set_preset`               |
| `SetProfile(ProfileName)`        | `hpdctl power set`, D-Bus `set_profile`           |
| `SetCoolingLevel(FanCurvePreset)`| `hpdctl cool set`, D-Bus `set_cooling_level`      |
| `EnableFanAuto`                  | `hpdctl cool auto`, D-Bus `set_fan_auto`          |
| `ChargeThresholdChanged(u8)`     | `hpdctl charge set`, D-Bus `set_charge_threshold` |
| `AcPowerChanged(bool)`           | `hpd-netlink` udev event                          |
| `SystemResumed`                  | logind `PrepareForSleep` (resume edge)            |
| `SyncPowerTarget(target)`        | Rollback after `ApplyPowerEnvelope` failure       |
| `SyncPlatformProfile(name)`      | Rollback after `ApplyPlatformProfile` failure     |
| `SyncChargeThreshold(u8)`        | Rollback after `ApplyChargeThreshold` failure     |
| `ConfigReload(RuntimeConfig)`    | SIGHUP handler in `hpd-daemon`                    |
| `Shutdown`                       | SIGINT / SIGTERM handler in `hpd-daemon`          |

---

## 4. Concurrency model

The daemon is `#[tokio::main(flavor = "multi_thread")]`. The runtime
hosts everything except the netlink monitor.

```
            ┌─────────────────────────────────────────────┐
            │  tokio multi-thread runtime  (main thread)  │
            │                                             │
            │  ┌────────────┐  ┌────────────────────┐    │
            │  │ Executor   │◄─┤ mpsc::Receiver     │    │
            │  │  ::run()   │  │ <Transition>       │    │
            │  └─────┬──────┘  └────────────────────┘    │
            │        │                ▲   ▲     ▲        │
            │        │                │   │     │        │
            │        ▼                │   │     │        │
            │  ┌────────────┐         │   │     │        │
            │  │ watch::    │         │   │     │        │
            │  │  Sender    │         │   │     │        │
            │  │ <Profile-  │         │   │     │        │
            │  │  State>    │         │   │     │        │
            │  └─────┬──────┘         │   │     │        │
            │        │ subscribe      │   │     │        │
            │        ▼                │   │     │        │
            │  ┌────────────┐  ┌──────┴──┐│┌───┴─────┐  │
            │  │ Properties │  │ zbus    │││ Suspend │  │
            │  │ Changed    │  │ server  │││ monitor │  │
            │  │ emitter    │  │ + iface │││ (logind)│  │
            │  └────────────┘  └─────────┘│└─────────┘  │
            │                             │             │
            │                  ┌──────────┴─────┐       │
            │                  │ SIGHUP/SIGINT/ │       │
            │                  │ SIGTERM select │       │
            │                  └────────────────┘       │
            └─────────────────────────────────────────────┘
                                ▲ tx (cross-thread send)
                                │
            ┌───────────────────┴─────────────────────────┐
            │ dedicated std::thread                       │
            │   current-thread tokio runtime              │
            │   + LocalSet                                │
            │   ┌────────────────────────────────────┐    │
            │   │ hpd-netlink::spawn_power_monitor   │    │
            │   │   AsyncMonitorSocket (!Send)       │    │
            │   └────────────────────────────────────┘    │
            └─────────────────────────────────────────────┘
```

**Why the dedicated thread:** `tokio-udev`'s `AsyncMonitorSocket`
future is `!Send`. It cannot live on a multi-thread runtime that
might migrate it across worker threads. The cleanest fix is a
single-purpose OS thread with its own current-thread runtime + a
`LocalSet`. The `mpsc::Sender<Transition>` is `Send`, so emitting
AC events across the boundary is the easy part.

**Channels at a glance:**

| Channel                                | Producers                  | Consumer       |
|----------------------------------------|----------------------------|----------------|
| `mpsc::Sender<Transition>`             | D-Bus iface, netlink, suspend, SIGHUP, SIGINT/TERM, executor (rollback) | Executor |
| `watch::Sender<ProfileState>`          | Executor (after each `reduce()`) | D-Bus property getters, `PropertiesChanged` emitter |

There is no `Effect::EmitDbusPropertiesChanged`. That variant was
removed in Lote 10 — the `PropertiesChanged` emitter task does a
per-field diff of the watch channel instead, which keeps the Effect
enum small and keeps zbus's signal machinery out of the reducer.

---

## 5. Lifecycle

```
   systemd start
        │
        ▼
   detect DMI ──► no match ──► log + exit
        │
        ▼
   pick backend (cascade today; registry once a 2nd vendor exists)
        │
        ▼
   load /etc/hpd/config.toml  (missing/corrupt → defaults, never fatal)
        │
        ▼
   read live hw state (limits, current target, profile, threshold, AC)
        │
        ▼
   wire channels: mpsc<Transition>, watch<ProfileState>
        │
        ▼
   spawn:
     - Executor.run()
     - zbus server with PowerDaemonInterface
     - PropertiesChanged emitter task
     - suspend monitor (logind PrepareForSleep)
     - SIGHUP handler
     - SIGINT/SIGTERM select
     - std::thread for hpd-netlink::spawn_power_monitor
        │
        ▼
   block on select! until SIGINT/SIGTERM
        │
        ▼
   send Transition::Shutdown
        │
        ▼
   reduce() emits PersistState; executor drains its mpsc queue
        │
        ▼
   await executor join, 5s timeout (systemd allows 90s)
        │
        ▼
   close D-Bus connection; return from main
```

### Signal matrix

| Signal     | Source                      | Daemon response                                                                                                                                                                          |
|------------|-----------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Boot       | daemon startup              | Build initial state (re-read AC + profile from hardware; `active_profile` ← configured default), then push `SystemResumed` to re-assert the full state (envelope + profile + charge + curve) onto the hardware — a cold boot resets firmware knobs to defaults, so reported state would otherwise diverge from the device. |
| `SIGINT`   | Ctrl+C in a terminal        | `Transition::Shutdown` → reducer emits `PersistState` → executor drains → process returns cleanly.                                                                                       |
| `SIGTERM`  | `systemctl stop`/`restart`  | Same as SIGINT.                                                                                                                                                                          |
| `SIGHUP`   | `systemctl reload`          | Re-read `/etc/hpd/config.toml`; push `ConfigReload(new.to_runtime())`. Startup-only fields (`state_path`, `channel_capacity`, `default_charge_threshold`) are logged as "needs restart". |
| Resume     | logind `PrepareForSleep`    | Push `SystemResumed`; reducer re-applies envelope + profile + charge threshold (kernel may have lost them across suspend).                                                               |
| AC plug    | udev `power_supply`         | Push `AcPowerChanged(b)`. With the `ac_max_performance` preference on (default): on plug, snapshot the battery state into `last_dc_state` + force **Performance / Max / Aggressive** + set the `AcLocked` lock; on unplug, restore the snapshot. With it off: both edges are a no-op (AC fully manual). Toggle via `set_ac_max_performance` / `hpdctl ac-lock`.                       |

### Shutdown safety

The daemon awaits the executor join with a 5s `tokio::time::timeout`.
That's safely below systemd's default `TimeoutStopSec=90s`. If
persistence ever hangs, the daemon logs the timeout and returns
cleanly rather than letting systemd `SIGKILL` it mid-write.

---

## 6. Authorization (polkit)

Every privileged D-Bus setter calls `hpd_dbus::polkit::check(...)`
*before* enqueuing its `Transition`. The check talks directly to
`org.freedesktop.PolicyKit1.Authority` over D-Bus — no extra crate
dependency.

| Action ID                          | Used by                                         | Default rule (non-admin) |
|------------------------------------|-------------------------------------------------|--------------------------|
| `dev.cirodev.hpd.set-tdp`          | `set_spl`, `set_preset`                         | `auth_admin`             |
| `dev.cirodev.hpd.set-charge`       | `set_charge_threshold`                          | `auth_admin`             |
| `dev.cirodev.hpd.set-profile`      | `set_profile`, `set_fan_auto`                   | `auth_admin_keep`        |

`auth_admin_keep` caches the affirmative answer for 5 minutes, so
flipping between profiles in quick succession does not pile up
prompts. TDP and charge changes use plain `auth_admin` — they need
re-authorization on every call.

### `wheel` passwordless grant

The `<defaults>` above are the baseline for **non-administrator**
callers. The device owner (a member of the `wheel` group) is granted
every `dev.cirodev.hpd.*` action **without a prompt** by the companion
JavaScript rule `package/polkit/49-hpd.rules`:

```js
polkit.addRule(function (action, subject) {
    if (action.id.indexOf("dev.cirodev.hpd.") === 0 &&
        subject.isInGroup("wheel")) {
        return polkit.Result.YES;
    }
});
```

The rule keys on **group membership rather than the `allow_active` /
`allow_inactive` / `allow_any` session tiers**, on purpose: on handheld
desktop sessions a physically-local terminal frequently registers as
`Remote=yes` (when driven over SSH, or by a display manager that does
not attach the session to the seat), which would drop the owner into
`allow_any` and force a password prompt. A group check is stable
regardless of how the session is registered, and it covers SSH access
from the owner's own machine. It needs a polkit build with the JS rules
engine (>= 0.106, standard on modern distributions); where that engine
is absent the rule is simply ignored and the `auth_admin` defaults
apply to everyone.

Action IDs are declared once in `hpd-dbus/src/actions.rs` as the
`PolkitAction` enum. Adding a new privileged setter is a single
edit there + a matching `<action>` block in
`package/polkit/dev.cirodev.hpd.policy` (the `49-hpd.rules` grant
already matches any `dev.cirodev.hpd.*` action by prefix). The Rust
enum's `as_id` match arm gives you a compile error if the wiring
drifts.

### Fail-closed contract

Any error path in `polkit::check` (proxy creation failure, method
call timeout, malformed reply, missing sender header) is logged as a
warning and **returns `false`**. The daemon would rather refuse a
legitimate request than allow an unauthenticated one.

### Simulator bypass

Under `#[cfg(feature = "simulator")]` the polkit check
unconditionally returns `true`. Session-bus runs on macOS or dev
hosts have no polkit authority to talk to and gating every setter
would make the simulator unusable. The `simulator` feature lives on
`hpd-dbus` and is auto-enabled by the daemon's own `simulator`
feature.

---

## 7. Persistence

State is persisted to `/var/lib/hpd/state.toml` via the standard
atomic `tempfile + rename` pattern. Under systemd the path is
resolved from the `STATE_DIRECTORY` env var injected by
`StateDirectory=hpd` in the unit; outside systemd it falls back to
the config file's `state_path`.

The on-disk schema is plain TOML, generated from `ProfileState` by
serde. Two fields are intentionally skipped:

```rust
#[serde(skip)]
pub is_ac_connected: bool,
#[serde(skip)]
pub ac_locked: bool, // derived: is_ac_connected && config.ac_max_performance
```

`ac_locked` is a pure function of the (re-queried) AC state and the
persisted `ac_max_performance` preference, so it is recomputed by the
executor on every publish and surfaced over D-Bus as the **`AcLocked`**
property — it never lives on disk. (The persisted `last_dc_state:
Option<DcSnapshot>` carries the user's battery TDP / power mode / cooling so
the unplug restore survives a reboot, and the persisted toggleable
`ac_max_performance` preference — seeded from `default_ac_max_performance` —
is exposed as the **`AcMaxPerformance`** property.)

The AC state is **re-queried from hardware on every boot** rather
than trusted from disk. Stale persisted state could otherwise lie to
the reducer about whether the daemon is in AC or DC mode for the
first transition after boot.

---

## 8. Configuration

`DaemonConfig` (`hpd-daemon/src/config.rs`) is loaded from
`/etc/hpd/config.toml` at startup, resolved via
`CONFIGURATION_DIRECTORY` from systemd. The schema is intentionally
minimal: `serde + toml`, no `figment`, no filesystem watcher.

Two invariants:

1. **Missing or corrupt config is never fatal.** The daemon falls
   back to `DaemonConfig::default()` and keeps running.
2. **Every field has `#[serde(default)]`.** Adding a new field
   never breaks an existing operator's TOML — they just get the
   default for the new field until they edit the file.

### Hot reload via SIGHUP

`systemctl reload hpd` sends SIGHUP. The handler re-reads the file
and pushes `Transition::ConfigReload(new.to_runtime())` to the
executor. Two classes of fields:

- **Runtime-tunable** (`sppt_factor`, `fppt_factor`,
  `profile_thresholds`, `fan_follows_tdp`) — applied on the *next*
  transition after reload.
- **Startup-only** (`state_path`, `channel_capacity`,
  `default_charge_threshold`) — changes are logged as "requires
  restart"; the daemon keeps running with the old values until a
  full restart.

The example config installed at `/etc/hpd/config.toml.example`
documents every field. Operators copy it to `config.toml` to
override defaults; `install.sh` never overwrites an existing
`config.toml`.

---

## 9. Where to look for things

| You want to find…                                | Look in                                                  |
|--------------------------------------------------|----------------------------------------------------------|
| The state machine (transitions / reducer / effects)| `hpd-core/src/{transition,reducer,effect,executor}.rs`    |
| Hardware-write contracts                         | `hpd-capabilities/src/{power,charge,fan,platform_profile}.rs` |
| ASUS firmware-attribute paths                    | `hpd-backend-asus/src/{power,charge,fan,profile}.rs`     |
| DMI detection                                    | `hpd-daemon/src/probe.rs`, `hpd-backend-asus/src/detect.rs` |
| D-Bus method / property surface                  | `hpd-dbus/src/service.rs`                                |
| Polkit action IDs                                | `hpd-dbus/src/actions.rs`                                |
| Polkit fail-closed contract                      | `hpd-dbus/src/polkit.rs`                                 |
| Config schema + reload behaviour                 | `hpd-daemon/src/config.rs`                               |
| Composition root / signal wiring                 | `hpd-daemon/src/main.rs`                                 |
| Suspend / resume monitor                         | `hpd-daemon/src/suspend.rs`                              |
| Atomic state persistence                         | `hpd-core/src/persistence.rs`                            |
| Per-property D-Bus signal emission               | `hpd-daemon/src/main.rs::spawn_properties_changed_emitter` |
| systemd unit + sandboxing                        | `package/hpd.service`                                    |
| polkit policy file                               | `package/polkit/dev.cirodev.hpd.policy`                  |
| D-Bus bus-level policy                           | `package/dev.cirodev.hpd.conf`                           |
| Example operator config                          | `package/hpd-example.toml`                               |

---

## 10. Extending the system

### Adding a new vendor backend

1. Create `crates/hpd-backend-<vendor>/` (model on
   `hpd-backend-asus`).
2. Implement `PowerEnvelope`, `ChargeControl`, `PlatformProfile`,
   and `FanControl` from `hpd-capabilities` for the rails the
   hardware actually exposes. Return `Option<None>` from
   `HwBackend`'s capability accessors for anything missing.
3. Implement `HwBackend` on the aggregate struct, returning each
   sub-backend via the accessor.
4. Add `detect.rs` returning `Option<Model>` from a `DmiInfo`.
5. Register the crate in the root `Cargo.toml` `workspace.members`
   list (preserves dependency order).
6. Add a `vendor-<name>` feature in `hpd-daemon/Cargo.toml` gating
   the optional dep, and extend the detection cascade in
   `hpd-daemon/src/main.rs::main`. (The cascade is intentional
   while there is one vendor; once a second backend lands it will
   be cleanly replaced by a detector registry.)
7. SPDX header (`// SPDX-License-Identifier: GPL-3.0-or-later`) on
   every new `.rs` file. `#![deny(unsafe_code)]` is already inherited
   from `[workspace.lints.rust]`.
8. Per-crate `README.md` matching the format used in the existing
   nine.
9. Update the hardware matrix in `package/hpd-example.toml` and
   the main `README.md`.

### Adding a new D-Bus / CLI command

1. Add a `Transition` variant in `hpd-core/src/transition.rs`.
2. Handle it in `reduce()` in `hpd-core/src/reducer.rs`. **No I/O
   here.** Return the new state and any effects.
3. If it produces a new kind of side-effect, add an `Effect`
   variant in `hpd-core/src/effect.rs` and handle it in
   `Executor::handle_effect`.
4. If it changes any field that backs a D-Bus property, extend
   `spawn_properties_changed_emitter` in `hpd-daemon/src/main.rs`
   with a matching diff arm.
5. If it is privileged, add a `PolkitAction` variant in
   `hpd-dbus/src/actions.rs` and a matching `<action>` block in
   `package/polkit/dev.cirodev.hpd.policy`.
6. Expose the method via `#[interface]` in
   `hpd-dbus/src/service.rs`; call `polkit::check(...)` *before*
   enqueuing the transition.
7. Add the proxy method in `hpd-cli/src/dbus.rs` and the matching
   subcommand in `hpd-cli/src/main.rs`.
8. Add a `### Added` / `### Changed` entry in `CHANGELOG.md`.

---

## 11. Things that look weird but are intentional

- **L2 sits before L1 in the workspace manifest.** Backends depend
  on capability traits, not the other way around.
- **`hpd-netlink` spawns a raw `std::thread`** instead of using
  `tokio::task::spawn_local`. Required because
  `tokio-udev::AsyncMonitorSocket` is `!Send`.
- **`is_ac_connected` is `#[serde(skip)]`** in `ProfileState`.
  Re-read from hardware on every boot rather than trusting stale
  state.
- **`SetSpl` derives SPPT and FPPT.** `SetSpl(u32)` multiplies SPL
  by `sppt_factor` (default 1.15×) and `fppt_factor` (default
  1.25×), capped at the hardware envelope. The manual escape
  hatch is `SetEnvelope` (takes all three explicitly).
- **`hpd-capabilities::error` is a one-line `pub use` shim** —
  re-exports `hpd_error::*` for backwards compat across the
  workspace's own callers. New code imports from `hpd_error`
  directly.
- **`Transition::ConfigReload` is intercepted before `reduce()`** —
  the reducer stays pure; the executor owns the runtime-config swap.
- **`MockSysfs` lives inside `hpd_sysfs::mock::testing`** (extra
  module layer) — the inner module scopes
  `#![allow(clippy::unwrap_used, ...)]` so the test fixture can use
  `.unwrap()` freely without polluting the rest of `hpd-sysfs`.
- **`hpd-daemon`'s `simulator` feature transitively pulls
  `vendor-asus` + `hpd-dbus/simulator`.** The simulator needs the
  ASUS firmware model and the polkit bypass in the same build.

---

## 12. Reading order

If you've just opened the repo and have an hour:

1. This document.
2. `crates/hpd-daemon/README.md` (composition root + lifecycle).
3. `crates/hpd-core/README.md` and then
   `hpd-core/src/{transition,reducer,effect,executor}.rs` in that
   order.
4. `crates/hpd-capabilities/README.md`.
5. `crates/hpd-backend-asus/README.md` for a concrete backend.
6. `crates/hpd-dbus/README.md` for the public surface.

If you want to *add* something: jump straight to §10 above, then
read the file referenced for the layer you're touching.

---

*Last updated: 2026-05-24 (v1.0.0 + Phase 3 documentation).*
