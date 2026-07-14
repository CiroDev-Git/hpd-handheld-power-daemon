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
   `ResetFanCurve`, `ApplyGpuClockRange`, `ResetGpuClocks`,
   `PersistState`. The Executor is the only thing that dispatches them.
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

### Custom fan curves (daemon ≥ 2.9.0)

Alongside the three named presets, a client can hand the daemon an
explicit, hand-drawn curve: `Transition::SetFanCurve` carries exactly 8
`(temp_c, pwm)` points per fan and behaves like `SetCoolingLevel` in every
way that matters — it latches manual cooling (`fan_follows_tdp` off) and
emits the same `ApplyFanCurve` effect, just built from
`FanCurveSelection::Custom` instead of a preset. What's new is
*validation*: the curve is checked twice, once at the D-Bus boundary
against `get_fan_curve_constraints()` and again independently by the L1
backend immediately before the EC write, so a violation is caught even if
a caller bypasses the first check. The constraints
(`FanCurveConstraints` — point count, temp/pwm ranges, and a
`safety_floor` of `(temp_threshold_c, min_pwm)` pairs) are Class C data in
the multi-device sense used elsewhere in this document: hardcoded per
model (e.g. `rc73xa_fan_curve_constraints()` for the RC73XA) because the
EC's safety floor has no generic kernel source to read live — a device
without its own capture inherits the most conservative floor in its
family rather than none. `FanCurve::validate_against` is the stricter
sibling of the `validate()` used for the compile-time presets: it
requires *strictly* increasing temperatures, not merely non-decreasing.
This re-exposes, under the same method name but a genuinely different
signature, functionality the raw preset-only `set_fan_curve` had before
its 2.5.0 retirement — the two are unrelated beyond sharing a name.

### GPU clock range control (daemon ≥ 2.12.0)

`GpuClockRangeControl` gives GPU tuning parity with the TDP/cooling
levers: on-device research on the ROG Xbox Ally X found the amdgpu
OverDrive interface exposes exactly one real knob at this generation —
the SCLK frequency range (`pp_od_clk_voltage`'s `OD_SCLK`) — with no
separate VRAM clock, no voltage curve, and no GPU power cap distinct from
the SPL/SPPT/FPPT budget the daemon already manages. The new D-Bus
surface mirrors the fan curve one-for-one: `set_gpu_clock_range(min_mhz,
max_mhz)` is the manual override, `enable_gpu_auto_follow` /
`reset_gpu_clocks` mirror `set_fan_auto` / `reset_fan_curve`, and
`get_gpu_clock_constraints()` / `get_gpu_clock_range()` mirror the fan
curve's constraints/active-curve reads. All of it reuses the existing
`dev.cirodev.hpd.set-profile` polkit action — no new action ID needed.

Two things about this feature are deliberately asymmetric with the fan
curve, and both are load-bearing:

- **The constraints are Class A, not Class C.** Unlike the fan curve's
  hardcoded-per-model safety floor, `GpuClockConstraints` is read
  **live** from the kernel's `OD_RANGE` on every call — a generic amdgpu
  interface, portable to a future device with zero recalibration. Don't
  "optimize" `GpuClockRangeControl::constraints` into a cached or
  hardcoded value; the live read is the point.
- **`active_gpu_clock` defaults to `None` forever, not just at first
  boot.** The fan curve's steady state is never "off" — the daemon
  always manages *some* curve. GPU clock is the opposite: the daemon
  must never touch `power_dpm_force_performance_level` /
  `pp_od_clk_voltage` until the user calls `enable_gpu_auto_follow` or
  `set_gpu_clock_range` at least once. Every site that unconditionally
  re-pins the fan curve (`force_ac_max_performance`, the AC-plug-restore
  branch, `SystemResumed`'s full reapply) guards its matching GPU-clock
  effect on `active_gpu_clock.is_some()` — otherwise plugging in AC on a
  fresh install would silently auto-opt every user into managed GPU
  clocks the moment they first charge the device.

A GPU-clock write is also a genuinely riskier hardware operation than a
fan-curve write: it's two sequential steps (switch DPM to `manual`, then
commit a range) with a real unsafe intermediate state a crash could land
in. That's why `SystemResumed`, when a GPU clock is actively managed,
does `ResetGpuClocks` *then* `ApplyGpuClockRange` rather than a bare
re-apply — resuming into a known-clean firmware-auto baseline before
reapplying, instead of risking a resume that inherits a half-written
state. It's also why `Effect::ApplyGpuClockRange` carries the *abstract*
`GpuClockSelection` (`Custom(range)` or `Preset(tier)`) rather than a
pre-resolved `GpuClockRange`: resolving a `Preset(tier)` needs both
`RuntimeConfig::gpu_clock_fractions` (which the pure reducer holds but
must never use to do I/O) and a live `OD_RANGE` read (which the reducer
must never perform, and which the L1 backend must never resolve itself
since it never sees `RuntimeConfig`). Only the `Executor` holds both
inputs, so it alone calls `gpu_clock_range_for_tier` immediately before
`GpuClockRangeControl::set_range`. When `gpu_follows_tdp` is on, a TDP
change re-infers a ceiling from the same `FanCurvePreset` tier already
computed for the fan curve — `fan_follows_tdp` and `gpu_follows_tdp` are
independent flags; either, both, or neither can be on at once.

### Rollback contract

If any `Apply*` / `Reset*` effect fails:

1. The Executor logs the failure.
2. It re-reads the live hardware state through the backend.
3. It re-injects a matching `Sync*` transition (`SyncPowerTarget`,
   `SyncPlatformProfile`, `SyncChargeThreshold`, `SyncFanCurve`,
   `SyncGpuClockRange`) so the in-memory `ProfileState` matches what the
   kernel actually has. `SyncFanCurve`/`SyncGpuClockRange` read back
   through `FanCurveControl::active_selection` /
   `GpuClockRangeControl::active_range`, which map the live EC/OD_RANGE
   state back to a preset, `custom`, or firmware `auto`/`None`.

Lote 38 made this uniform across the original three apply effects (only
`Apply{PowerEnvelope}` rolled back in pre-1.0 versions); the fan-curve
and GPU-clock effects added later followed the same contract from the
day they shipped.

### Transition catalogue

| Transition                       | Source                                            |
|----------------------------------|---------------------------------------------------|
| `SetSpl(u32)`                    | `hpdctl tdp set`, D-Bus `set_spl`                 |
| `SetEnvelope(PowerEnvelopeTarget)`| Manual full-envelope path (no preset, no derive)  |
| `SetPreset(TdpPreset)`           | `hpdctl preset`, D-Bus `set_preset`               |
| `SetProfile(ProfileName)`        | `hpdctl power set`, D-Bus `set_profile`           |
| `SetCoolingLevel(FanCurvePreset)`| `hpdctl cool set`, D-Bus `set_cooling_level`      |
| `SetFanCurve { cpu, gpu }`       | `hpdctl cool set-custom`, D-Bus `set_fan_curve` (daemon ≥ 2.9.0) |
| `ResetFanCurve`                  | `hpdctl cool reset`, D-Bus `reset_fan_curve`      |
| `EnableFanAuto`                  | `hpdctl cool auto`, D-Bus `set_fan_auto`          |
| `SetGpuClockRange { min_mhz, max_mhz }` | `hpdctl gpu set`, D-Bus `set_gpu_clock_range` (daemon ≥ 2.12.0) |
| `EnableGpuAutoFollow`            | `hpdctl gpu auto`, D-Bus `enable_gpu_auto_follow` (daemon ≥ 2.12.0) |
| `ResetGpuClocks`                 | `hpdctl gpu reset`, D-Bus `reset_gpu_clocks` (daemon ≥ 2.12.0) |
| `RestoreDefaults`                | `hpdctl restore-defaults`, D-Bus `restore_defaults` (daemon ≥ 2.14.0) — composes `SetPreset(Balanced)` → `SetProfile(Performance)` → `ChargeThresholdChanged(80)` → `ResetFanCurve` → conditionally `ResetGpuClocks` (only if already opted in) via recursive `reduce()` calls inside one atomic transaction; a full no-op if already at every default |
| `SetAcMaxPerformance(bool)`      | `hpdctl ac-lock on/off`, D-Bus `set_ac_max_performance` |
| `ChargeThresholdChanged(u8)`     | `hpdctl charge set`, D-Bus `set_charge_threshold` |
| `AcPowerChanged(bool)`           | `hpd-netlink` udev event                          |
| `SystemResumed`                  | logind `PrepareForSleep` (resume edge)            |
| `SyncPowerTarget(target)`        | Rollback after `ApplyPowerEnvelope` failure       |
| `SyncPlatformProfile(name)`      | Rollback after `ApplyPlatformProfile` failure     |
| `SyncChargeThreshold(u8)`        | Rollback after `ApplyChargeThreshold` failure     |
| `SyncFanCurve(Option<selection>)`| Rollback after `ApplyFanCurve`/`ResetFanCurve` failure |
| `SyncGpuClockRange(Option<range>)` | Rollback after `ApplyGpuClockRange`/`ResetGpuClocks` failure |
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
| Resume     | logind `PrepareForSleep`    | Push `SystemResumed`. Executor re-reads the real AC state from hardware first (stale across suspend if (un)plugged while asleep), then the reducer applies the right policy: force max on AC / restore the `last_dc_state` battery snapshot on battery / else re-apply persisted (kernel may have lost levers across suspend). |
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
| `dev.cirodev.hpd.set-profile`      | `set_profile`, `set_cooling_level`, `set_fan_auto`, `reset_fan_curve`, `set_ac_max_performance` | `auth_admin_keep`        |

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

## 7. Competing power daemons

`hpd` expects to be the **sole** manager of the platform's power knobs.
Several daemons commonly found on handheld Linux images write the same
underlying files and so fight it — whoever writes last wins, and the
effective TDP/profile/fan state visibly flaps. They fall into two
buckets:

- **Hard rivals**, which must never co-run and which `hpdctl doctor
  --fix` masks outright: `power-profiles-daemon` (writes
  `platform_profile` + EPP), Valve's `steamos-manager` (TDP / charge /
  fan behind Game Mode), `tuned` (Fedora/Bazzite's default tuner, whose
  `tuned-ppd` shim also claims the PPD bus name), and `hhd` (Handheld
  Daemon, Bazzite's default on Ally hardware).
- **Advisory** daemons, which are wanted and so are only *reported*,
  never masked: Feral's `gamemoded` (governor boost during games),
  ASUS's own `asusd` (also drives platform profile / fan / charge, but
  additionally owns keyboard RGB / Aura, so masking it would be the
  wrong call), and `auto-cpufreq` (governor / EPP only).

Detection has to work even for rivals with no persistent process
identity, so `hpd_dbus::conflicts` uses two independent mechanisms and
unions them: a **D-Bus name check** (`NameHasOwner`, which deliberately
never D-Bus-activates a name — checking for a rival can never revive
it) for daemons that own a bus name, and a **systemd unit check**
(`ListUnitsByPatterns` over `org.freedesktop.systemd1`, read-only and
allowed under `ProtectSystem=strict`) for the ones that don't (`hhd`,
`auto-cpufreq`). The daemon runs `power_conflicts()` once at startup
(logging a loud warning if anything hard turns up) and exposes both the
hard-rival and advisory lists live over D-Bus
(`get_power_conflicts()`/`get_advisory_daemons()`); a regression test
keeps the two lists disjoint on both detection axes so `doctor --fix`
can never mask something that was only ever meant to be reported.
Nothing can see a tool that writes TDP from *inside* the same process
tree without its own service or bus name — a Decky plugin doing raw
`ryzenadj` calls, for instance — that class of conflict is undetectable
by design and stays out of scope.

The split mirrors how polkit setup issues are handled: **the daemon
detects, the CLI repairs, the packaging only informs.**
`hpdctl doctor` reports polkit health, conflicts, advisory daemons, and
gamescope-session status through a shared renderer
(`doctor::print_health`, also tacked onto the end of `hpdctl status`);
`hpdctl doctor --fix` elevates once and both `disable --now`s +
`mask`s the rival system units *and* installs the polkit policy — a
superset of the narrower `fix-polkit` repair. The daemon itself cannot
perform any of this (`ProtectSystem=strict` makes `/usr`, and therefore
`systemctl`'s unit files, read-only to it), so the privileged write has
to originate from the user-invoked CLI. At the packaging level, only
`power-profiles-daemon` gets automatic, unconditional neutralization —
`package/hpd.service` declares `Conflicts=power-profiles-daemon.service`
and the AUR `post_install` hook runs `doctor --fix`. That's a deliberate
asymmetry, not an oversight: PPD is headless, ubiquitous, and
boot-race-proven safe to mask silently, whereas `tuned` and `hhd` are
user-chosen stacks that some installations may genuinely want — their
neutralization stays opt-in through `doctor --fix` rather than being
forced at install time. (`Conflicts=` is also symmetric, so unlike
`tuned` — which is D-Bus-activatable and would otherwise be revived and
kill hpd, the exact v2.2.2 regression — it only helps once a rival is
already masked.)

## 8. PPD compatibility shim (`net.hadess.PowerProfiles`)

Masking the real `power-profiles-daemon` fixes the fight over
`platform_profile`, but it has a side effect: every client that only
ever talks to *whoever currently owns* the `net.hadess.PowerProfiles`
bus name goes dark. That includes the KDE Plasma battery applet's
Eco/Balanced/Performance selector, the `powerprofilesctl` CLI, and
CachyOS's `game-performance` launch wrapper. None of those clients ever
touch hardware directly — they are pure D-Bus callers, so from their
point of view "no PPD" and "some other program answering as PPD" are
indistinguishable.

Since daemon **2.10.0**, `hpd-daemon` exploits exactly that
indistinguishability: at startup it best-effort claims the
`net.hadess.PowerProfiles` name itself
(`request_name_with_flags(DoNotQueue)`, deliberately **without**
`ReplaceExisting` — a real, unmasked PPD or `tuned-ppd` must never be
stolen from), and `hpd_dbus::ppd_shim::PowerProfilesShim` implements the
real-world subset of PPD's D-Bus API at `/net/hadess/PowerProfiles`:
the read-write `ActiveProfile` property, `Profiles`, `Actions`,
`ActiveProfileHolds`, `PerformanceInhibited`, `PerformanceDegraded`, the
`HoldProfile`/`ReleaseProfile` methods, and the `ProfileReleased`
signal. Every request the shim receives becomes an ordinary
`Transition::SetProfile` fed through the same reducer, AC-lock, and
rollback path as any other caller — the shim is a second *door*, not a
second source of truth or a parallel state machine.

`HoldProfile` — the method `game-performance` actually uses — snapshots
the current profile, forces `power-saver` or `performance` for the
duration of the hold, and restores the snapshot once every outstanding
hold has drained; concurrent holds resolve with upstream PPD's own
precedence (`power-saver` outranks `performance`). A holder that
disconnects without calling `ReleaseProfile` — a crashed game, say — is
caught by watching `NameOwnerChanged` and its holds are released the
same way a clean `ReleaseProfile` would. Deliberately, this whole
surface has **no polkit gating**: upstream PPD requires none for
`ActiveProfile` or the hold methods, and adding it to the shim would
silently regress the exact clients it exists to revive.

One subtlety worth flagging for anyone touching conflict detection: now
that hpd can legitimately own the PPD bus name itself, a naive
"does anything own `net.hadess.PowerProfiles`" check would misreport
hpd's own shim as the rival it's standing in for. `power_conflicts()`
avoids this by comparing the name's *owner* (`GetNameOwner`) against
the connection's own unique name, rather than just asking
`NameHasOwner` — only a name owned by someone else counts as a live
PPD rival. `hpdctl status`/`doctor` surface the shim's state as a
"compat PPD: active/inactive" line, backed by the `get_ppd_shim_active`
D-Bus method.

## 9. Extended telemetry (`GetTelemetry`, daemon ≥ 2.8.0)

`GetThermalStatus`'s fixed `(cpu_temp_c, gpu_temp_c, cpu_rpm, gpu_rpm,
soc_power_mw)` tuple can never grow without breaking every existing
client — a `(iiiii)` D-Bus signature is load-bearing wire format, not
just a return type. Since 2.8.0, everything new lands instead on
`get_telemetry() -> a{sv}`, an open-ended map where a key is present
**only** when the running hardware genuinely exposes that reading: a
battery-less board, or one without amdgpu's `gpu_busy_percent`, simply
omits the corresponding key rather than reporting a placeholder zero.
`get_thermal_status` itself is unchanged and stays fully supported —
`get_telemetry` is a superset, not a replacement, and also carries the
original five readings under the same key names (`cpu_temp_c`,
`gpu_temp_c`, `cpu_fan_rpm`, `gpu_fan_rpm`, `soc_power_mw`) so a client
that has already migrated needs only the one call.

The new keys cover the ground `GetThermalStatus` never could:
`battery_power_mw` (discharge only), `battery_percent`,
`battery_status` (the raw kernel string), `battery_health_pct`
(current vs. design capacity), `battery_cycles`, `cpu_freq_mhz`
(averaged across every `cpufreq/policy*`), `gpu_freq_mhz` (amdgpu
hwmon's `freq1_input`, falling back to the active `pp_dpm_sclk` line),
`gpu_busy_pct`, and `vram_used_mb`/`vram_total_mb`. `gpu_throttle_status`
is defined in the capability trait but not yet populated by the ASUS
backend — there is no stable, non-debugfs sysfs source for it at time
of writing. Since **2.11.0**, the map also carries `cpu_busy_pct`
(0–100), the one telemetry field the plugin's bottleneck-diagnosis
heuristic needed and didn't have: because `/proc/stat`'s aggregate
`cpu` line reports cumulative jiffies since boot rather than a rate, a
percentage requires a delta between two time-separated samples. That
makes `cpu_busy_pct` the daemon's first genuinely *stateful*
telemetry accessor — every other `get_telemetry` field is a stateless
instantaneous read — backed by a mutex-guarded previous sample that
returns `None` on the first call after daemon start or on any call
within 200 ms of the last one.

Implementation-wise, the new `hpd-capabilities::telemetry::
SystemTelemetry` trait is an additive, optional `HwBackend::telemetry()`
accessor — the same "capability that may simply be absent" pattern used
throughout the trait hierarchy — with `hpd-backend-asus::telemetry`
providing the ASUS implementation. Like `GetThermalStatus` and
`GetFanCurve`, `GetTelemetry` is **poll-only**: it has no
`PropertiesChanged` signal, so a client (the plugin, `hpdctl monitor`)
samples it at roughly 1 Hz while it actually needs the data, and
`Variant`-wrapped values arrive needing an unwrap step at the D-Bus
client boundary.

---

## 10. Persistence

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

## 11. Configuration

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

## 12. Where to look for things

| You want to find…                                | Look in                                                  |
|--------------------------------------------------|----------------------------------------------------------|
| The state machine (transitions / reducer / effects)| `hpd-core/src/{transition,reducer,effect,executor}.rs`    |
| Hardware-write contracts                         | `hpd-capabilities/src/{power,charge,fan,fan_curve,gpu_clock,thermal,platform_profile,telemetry}.rs` |
| ASUS firmware-attribute paths                    | `hpd-backend-asus/src/{power,charge,fan,fan_curve,gpu_clock,thermal,profile}.rs` |
| GPU clock ceiling inference (tier → MHz)         | `hpd-core/src/inference.rs::gpu_clock_range_for_tier` + `RuntimeConfig::gpu_clock_fractions` in `hpd-capabilities/src/profile.rs` |
| DMI detection                                    | `hpd-daemon/src/probe.rs`, `hpd-backend-asus/src/detect.rs` |
| D-Bus method / property surface                  | `hpd-dbus/src/service.rs`                                |
| Polkit action IDs                                | `hpd-dbus/src/actions.rs`                                |
| Polkit fail-closed contract                      | `hpd-dbus/src/polkit.rs`                                 |
| Competing power-daemon detection                 | `hpd-dbus/src/conflicts.rs` + `hpd-daemon/src/main.rs` startup check + `get_power_conflicts`/`get_advisory_daemons` in `hpd-dbus/src/service.rs` |
| PPD compat shim (`net.hadess.PowerProfiles`)     | `hpd-dbus/src/ppd_shim.rs` + name-claim/task-spawn in `hpd-daemon/src/main.rs` + `get_ppd_shim_active` in `hpd-dbus/src/service.rs` |
| Power-ownership repair + shared health block     | `hpd-cli/src/doctor.rs` (`doctor::print_health`, reused by `hpdctl status`) |
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

## 13. Extending the system

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

## 14. Things that look weird but are intentional

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
- **GPU clock constraints are read live; fan-curve constraints are
  hardcoded.** `GpuClockConstraints` comes straight from the kernel's
  `OD_RANGE` on every call — a generic amdgpu interface that needs no
  per-device recalibration. `FanCurveConstraints` is the opposite: a
  hardcoded-per-model constant, because the EC's safety floor has no
  generic kernel source to read. Don't try to make these two symmetric.
- **`active_gpu_clock` defaults to `None` forever, not just at first
  boot** — unlike `active_fan_curve`, whose real steady state is never
  `None`. The daemon must never touch `power_dpm_force_performance_level`
  / `pp_od_clk_voltage` until the user opts in at least once via
  `enable_gpu_auto_follow` or `set_gpu_clock_range`.
- **`SystemResumed` resets GPU clocks before reapplying them**, unlike
  the fan curve's plain re-apply. A GPU-clock write is a two-step
  hardware operation with a real unsafe intermediate state a crash
  could land in, so resume goes through a known-clean firmware-auto
  baseline first.

---

## 15. Reading order

If you've just opened the repo and have an hour:

1. This document.
2. `crates/hpd-daemon/README.md` (composition root + lifecycle).
3. `crates/hpd-core/README.md` and then
   `hpd-core/src/{transition,reducer,effect,executor}.rs` in that
   order.
4. `crates/hpd-capabilities/README.md`.
5. `crates/hpd-backend-asus/README.md` for a concrete backend.
6. `crates/hpd-dbus/README.md` for the public surface.

If you want to *add* something: jump straight to §13 above, then
read the file referenced for the layer you're touching.

---

*Last updated: 2026-07-14 (synced through v2.14.0: competing power
daemons, the PPD compat shim, extended telemetry, custom fan curves,
GPU clock range control, and the `RestoreDefaults` composed
transaction).*
