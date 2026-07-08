# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each entry references the Audit lote that introduced the change. The
underlying audit and remediation plans are maintained internally and are
not part of the published repository.

---

## [2.7.3] — 2026-07-07

### Added

- Two anti-drift tests (`hpd-cli`, dev-dependency only) that cross-check
  `doctor::RIVAL_UNITS` against what `hpd_dbus::conflicts` detects, so the
  hand-mirrored mask list can't silently fall out of sync with detection
  again.

### Fixed

- **`EnableFanAuto` now applies and persists immediately.** Re-engaging
  auto-cooling (`hpdctl cool auto`) recursed into `SetEnvelope(power_target)`
  to re-trigger the curve inference, but that only produces effects when the
  envelope actually *changes* — so at an unchanged TDP it silently emitted
  zero effects: the EC kept running the stale manual curve, and with no
  `PersistState` the auto-cooling flag itself could be lost on a restart
  before any other transition happened to persist it. The reducer now infers
  and applies the matching curve directly (mirroring `SetCoolingLevel`).
- **Rollback could deadlock the executor under channel saturation.**
  `Executor::rollback` used `send().await` on the same bounded transition
  channel its own `run()` loop drains; if that channel were ever full when a
  rollback fired, the await could never resolve. Switched to `try_send` —
  a dropped rollback under saturation is safe, since the next boot/resume
  re-assert reconciles state against hardware anyway.
- **Out-of-range `config.toml` values could silently reject every TDP
  change.** `derive_boosted_envelope` now floors SPPT at SPL and FPPT at
  SPPT (defends against tight hardware boost rails even with a valid
  factor), and `RuntimeConfig`/`ProfileThresholds` gain a `sanitized()` step
  — wired into `DaemonConfig::load` (covers both initial load and `SIGHUP`
  reload) — that clamps/repairs an operator's out-of-range `sppt_factor` /
  `fppt_factor` / `profile_thresholds` instead of letting a typo make
  `validate_power_envelope` reject every `SetSpl`/`SetPreset`.
- **State persistence now `fsync`s before the atomic rename.** The
  temp-file-then-rename pattern only protected against a crash mid-write;
  a hard power loss (a handheld draining its battery to zero, not a clean
  shutdown) between the write returning and the bytes reaching disk could
  still leave `state.toml` truncated. `StatePersister::save` now calls
  `sync_all` on the temp file first.
- **`AsusChargeBackend::is_ac_connected` unified with the live AC-event
  detection.** It probed a fixed 6-path list of well-known mains node
  names; `hpd-netlink`'s live udev monitor instead scans `power_supply` for
  `type == "Mains"`, which is how the Xbox Ally X's `AC0` node was found
  in the first place. The two paths were quietly using different
  algorithms for the same fact — a future device with a differently-named
  mains node would report DC at boot and AC once a live event fired. Both
  now scan by `type`; `hpd-sysfs::SysfsIo` gains `read_dir_names` to make
  this possible without hard-coding paths.
- **`hpdctl doctor --fix` no longer masks `hhd@.service` unconditionally.**
  On the Xbox Ally X, hhd (Handheld Daemon) also owns gamepad remapping, so
  an unconditional mask could win hpd sole TDP ownership while silently
  taking away controller input. `doctor --fix` now masks it only when
  `inputplumber.service` is active (confirmed as the input replacement on
  CachyOS); otherwise it explains the two alternatives instead of masking.
- **`hpdctl doctor --fix` now also neutralizes `tuned-ppd.service` and
  detects/masks `tlp.service`.** tuned's PPD-compatibility shim runs as its
  own systemd unit, so masking `tuned.service` alone left it running; TLP
  is a standalone power daemon popular on Arch/CachyOS that writes the same
  charge/profile/governor surfaces hpd does.
- Improved the startup error when hardware power limits can't be read
  (usually a missing/too-old `asus-armoury` kernel driver) to name the
  likely cause and point at the fix, and capped `hpd.service`'s restart
  loop (`StartLimitIntervalSec`/`StartLimitBurst`) so that failure mode
  trips systemd's failure state instead of restarting forever.

### Security

- **`quick-xml` bumped 0.36.2 → 0.41.0** (`hpd-dbus` dev-dependency,
  never ships in the release binary). 0.36.2 carried two HIGH (7.5)
  advisories: quadratic runtime on duplicate start-tag attributes
  (RUSTSEC-2026-0194) and unbounded namespace-declaration allocation /
  memory-exhaustion DoS (RUSTSEC-2026-0195). Found running the
  pre-release `cargo audit` gate; `cargo audit --target-os linux` and
  `cargo deny check` are clean after the bump.

Full audit at [`docs/dev/AUDITORIA-2026-07-es.md`](docs/dev/AUDITORIA-2026-07-es.md).

## [2.7.2] — 2026-06-10

### Fixed

- **Event monitors now reconnect instead of dying silently.** Both the
  netlink/udev AC monitor (`hpd-netlink`) and the logind suspend monitor
  (`hpd-daemon/src/suspend.rs`) looped on `while let Some(...) = stream.next()`
  with no recovery: a single `Err`/`None` from the stream — which a suspend
  can produce by perturbing the underlying socket — fell out of the loop and
  **killed that monitor for the rest of the process**, silently stopping live
  AC detection (or resume detection) until the daemon restarted. Each monitor
  is now wrapped in an **outer reconnect loop**: on a dropped stream it logs,
  backs off (2 s), and rebuilds the subscription. The netlink monitor
  additionally **reconciles the canonical mains node on every (re)connect**, so
  an AC edge that happened while it was down (e.g. unplugged mid-suspend) is
  still emitted. Only a dropped executor channel (daemon shutting down) stops a
  monitor for good. Found in the 2026-06 lifecycle audit (GAP #1); see
  [`docs/dev/LIFECYCLE.md`](docs/dev/LIFECYCLE.md).
- `hpd-netlink`'s `tokio` dependency gains the `time` feature (for the
  reconnect backoff).

### Notes

- The post-resume AC re-read (2.7.1) was **confirmed correct on-device** during
  the audit — the daemon re-reads `is_ac_connected` on every `SystemResumed`
  and logs `Boot/resume on AC|battery` accordingly; there is no AC-read race at
  the daemon level. The "AC not taken after suspend" symptom was traced to the
  plugin's connection going stale (fixed in plugin 2.10.1), plus this monitor
  fragility.

## [2.7.1] — 2026-06-08

### Fixed

- **AC-state consistency across a power-off / suspend boundary.** Plugging or
  unplugging the charger while the device is shut down or suspended could
  leave the daemon applying the wrong power policy on the next boot/resume:
  - **Boot/resume on battery after an AC-locked session** re-applied the
    *persisted forced-max* levers — so the device came up at **Max TDP +
    Aggressive fans on battery** (loud, fast drain). `SystemResumed` now
    restores the saved battery snapshot (`last_dc_state`) instead, so you come
    back to your real battery state. (Closes the shutdown-on-AC → boot/resume-
    on-battery and suspend-on-AC → unplug → resume cases.)
  - **Resume re-reads the real AC state from hardware.** The in-memory
    `is_ac_connected` could be stale across a suspend if the charger was
    (un)plugged while asleep and the udev event was missed or arrived after
    `SystemResumed`; the executor now re-queries the backend on every
    boot/resume so the lock/unlock decision matches the actual power source
    regardless of the netlink monitor's timing.
  - Boot is unchanged (it already re-read AC from hardware); the existing
    cold-install-on-AC behaviour (first unplug → quiet Balanced defaults when
    no battery snapshot exists) is preserved.

## [2.7.0] — 2026-06-08

### Added

- **"Lock to maximum performance on AC" — a toggleable preference.** When on
  (the default), plugging in the charger pins the device to its ceiling —
  **Power mode → Performance, TDP → Max, cooling → Aggressive** — and
  **rejects every power/cooling change until unplug**. The user's battery
  (DC) preferences (TDP + power mode + cooling) are snapshotted on the plug
  edge and restored verbatim on unplug. The **battery charge threshold stays
  editable** on AC — it is the one knob that legitimately varies on wall
  power. When **off, AC is fully manual** — plugging/unplugging changes
  nothing and everything stays editable.
  - **Toggleable at runtime, persisted** (no config edit / reload needed):
    `hpdctl ac-lock on|off` (or no argument to print the state), and the
    new `set_ac_max_performance(b)` D-Bus method. The live value lives in
    `state.toml`; `default_ac_max_performance` (in `/etc/hpd/config.toml`,
    default `true`) only seeds the very first boot. Toggling is applied
    immediately: enabling while plugged forces max + locks; disabling while
    plugged restores your battery state and unlocks.
  - **New D-Bus properties** `AcMaxPerformance: b` (the preference) and
    `AcLocked: b` (the live lock state, `AcMaxPerformance && on AC`) — both
    emit `PropertiesChanged`. While `AcLocked`, the six power/cooling setters
    (`set_spl`, `set_preset`, `set_profile`, `set_cooling_level`,
    `set_fan_auto`, `reset_fan_curve`) fail fast with a clear "locked on AC"
    error; `set_charge_threshold` and `set_ac_max_performance` are exempt (the
    latter is how you release the lock). The reducer enforces the same rule as
    a backstop for any client.
  - **Boot/resume on AC** re-asserts the forced-max policy (the same
    `SystemResumed` path), so a device booted or resumed straight into AC is
    already pinned + locked.
  - **State:** the persisted `last_dc_target` (envelope only) became
    `last_dc_state` — a full `DcSnapshot` (TDP + power mode + cooling +
    auto-cooling) so the unplug restore brings back every lever, not just the
    watts; plus the new persisted `ac_max_performance` preference. Old
    `state.toml` files load cleanly (`last_dc_state` defaults to "no
    snapshot"; `ac_max_performance` defaults to `true`).
  - **Cold install / first boot on AC.** A device installed or first booted
    while plugged in (with the lock on) starts locked at max — no battery
    snapshot exists yet. The **first unplug** synthesizes quiet battery
    defaults — **Balanced TDP with auto-cooling re-engaged** — so the fan
    curve drops from the forced `Aggressive` instead of leaving the fans loud
    on battery (the power mode stays at the `Performance` default so the SPL
    is usable). From the next plug cycle on, a real snapshot round-trips
    exactly.

### Internal

- Factored the smart-mode SPPT/FPPT envelope maths into one
  `derive_boosted_envelope` helper shared by `Transition::SetSpl` and the
  forced-max path (no behaviour change).

## [2.6.0] — 2026-06-07

### Removed

- **Dead config knob `fan_curve_follows_profile` is gone.** It became a
  no-op when power and cooling were decoupled (the fan curve follows the
  TDP envelope, never the platform profile), but it lingered in
  `RuntimeConfig` and the example config where it could mislead operators
  into thinking it still did something. Removed from the struct, the
  shipped `hpd-example.toml`, and the tests that poked it. **Backward
  compatible:** serde ignores unknown keys, so a `config.toml` that still
  sets `fan_curve_follows_profile` keeps parsing — the value is simply
  dropped, exactly as before.

### Changed

- **Doc-comment sweep: code comments now match the decoupled
  power↔cooling model.** Several `///` comments still described the old
  coupled behaviour the code abandoned — that auto-cooling and
  `set_profile` drive the ACPI `platform_profile`, when in reality
  auto-cooling infers the **fan curve** and `set_profile` is an
  independent power lever that does not touch cooling. Corrected on
  `ProfileState::fan_follows_tdp`, `Transition::SetCoolingLevel`, the
  `auto_cooling` / `set_profile` / `set_fan_auto` D-Bus methods, the
  `TdpPreset` table, and the executor's post-reduce comment. The reducer
  helper `apply_target_and_profile` (which no longer applies a profile)
  was renamed `apply_power_target`. **No behaviour change** — comments and
  one private identifier only.
- **`DIAGRAMS.md` / `DIAGRAMS-es.md`:** dropped the stale `set_fan_curve`
  entry from the setters box (the method was retired in 2.5.0).

### Internal

- **`hpd-dbus`:** the seven polkit-gated setters now funnel their
  "enqueue transition or report the executor is down" step through one
  private `PowerDaemonInterface::send` helper instead of repeating the
  `tx.send(...).is_err()` boilerplate (per AUDIT_V1). No surface change.

## [2.5.2] — 2026-06-07

### Fixed

- **AC plug/unplug is finally detected at runtime — `PrivateNetwork=yes`
  was blocking it.** The `hpd.service` sandbox ran the daemon in a private
  network namespace, but udev delivers `power_supply` uevents over a
  `NETLINK_KOBJECT_UEVENT` multicast that is **per-network-namespace** — so
  the AC/DC monitor received **no** events at all and `IsAcConnected` /
  `AcConnected` only ever reflected the boot/resume sysfs read. Set
  `PrivateNetwork=no` (the daemon needs no real network — D-Bus is a Unix
  socket) and documented why it must stay off. **Verified on-device (ROG
  Xbox Ally X, USB-C charging): plug → `Charger connected = true`, unplug →
  `false`, in real time.** This is the true root cause; the 2.4.3 fix
  (re-read the `Mains` node on any `power_supply` event, needed because the
  USB-C edge fires on the `ucsi-source-psy` node) was necessary but could
  not work while the events were namespaced away.

### Changed

- **Network hardening preserved without breaking udev:** added
  `IPAddressDeny=any` to `hpd.service` in place of `PrivateNetwork`. It
  blocks all IP traffic (the daemon opens no IP sockets) at the eBPF level
  without isolating the network namespace, so `AF_NETLINK` uevents still
  flow. Net sandbox posture is unchanged in practice.

## [2.5.1] — 2026-06-07

### Fixed

- **The reported fan-curve level can no longer claim a value the EC
  refused.** `ApplyFanCurve` / `ResetFanCurve` now roll back like the
  other `Apply*` effects: on a write failure the executor re-reads the
  EC's *actual* selection (new `FanCurveControl::active_selection`, which
  matches the live points back to a preset / `custom` / firmware-`auto`)
  and re-injects `Transition::SyncFanCurve`, so `ProfileState.active_fan_curve`
  — and therefore the `FanCurve` property — always reflects reality. On a
  successful write the existing read-back verification already guaranteed
  this; the gap was only the failure path. All four `Apply*` effects now
  share the same rollback contract.

### Docs

- Clarified that `set_profile` / `ActiveProfile` / the `set-profile` polkit
  action all name the kernel's ACPI `platform_profile` (surfaced as "Power
  mode" / `hpdctl power`), and that `set-profile` is the shared
  `auth_admin_keep` bucket also gating the cooling levers.

## [2.5.0] — 2026-06-07

### Removed

- **The unused raw `set_fan_curve` D-Bus method and its `set-fan-curve`
  polkit action.** `set_fan_curve` set a fan-curve preset directly without
  latching manual mode — so under auto-cooling it silently no-op'd (the
  next TDP change re-inferred and overwrote the curve), and it was reachable
  from no CLI subcommand and (since plugin 2.7.0) no UI. `set_cooling_level`
  (latches manual) and `reset_fan_curve` fully cover the fan curve. The
  `Transition::SetFanCurve` variant and the CLI proxy binding are gone too.
  `reset_fan_curve` now authorises against the **`dev.cirodev.hpd.set-profile`**
  action (grouped with the other cooling levers); the dedicated
  `set-fan-curve` action is retired. Removing a D-Bus method + polkit action
  is normally a breaking change, but there are no external consumers — only
  hpd's own CLI and Decky plugin, neither of which used it — so this ships
  as a minor cleanup. The fan-curve **infrastructure** (presets, EC writes,
  auto-follow, boot/resume re-assert, the graph) is unchanged.

## [2.4.3] — 2026-06-06

### Fixed

- **AC plug/unplug now detected on USB-C-charged handhelds (ROG Ally X).**
  The netlink monitor filtered udev `power_supply` events by an `AC`/`ADP`
  sysname and trusted the event's own `POWER_SUPPLY_ONLINE`. But when
  charging over USB-C the plug/unplug **event** fires on the USB-C PD port
  (`ucsi-source-psy-USBC000:*`, `type == "USB"`) — *not* on the mains node
  (`AC0`) — so every USB-C edge was ignored and `IsAcConnected` / the
  `AcConnected` property stayed frozen at the boot value. The monitor now
  re-reads the canonical `type == "Mains"` node from sysfs on **any**
  `power_supply` event and forwards only genuine, deduplicated edges. Boot
  detection was already correct (re-queried from hardware); this fixes the
  reactive updates after boot.

## [2.4.2] — 2026-06-06

### Added

- **`get_version()` D-Bus method** — read-only, unauthenticated; returns
  the daemon's `CARGO_PKG_VERSION`. Lets a client (the Decky plugin) show
  which daemon version it's talking to. Clients predating it get a D-Bus
  error and fall back to "unknown".

## [2.4.1] — 2026-06-06

### Fixed

- **Reported state could diverge from the hardware after a cold boot.**
  The daemon only re-applied the platform profile and fan curve at boot
  and trusted the persisted `power_target` / `charge_end_threshold`
  without writing them — but a cold boot resets several firmware knobs to
  their defaults (e.g. `platform_profile` → `balanced`, charge limit →
  100%). The daemon then reported a value the device no longer had (a
  user's 80% charge limit was silently lost yet still shown as 80%), and
  the chip could sit at `balanced` clamping power below the user's TDP.
  Boot now **re-asserts the full intended state** (envelope + profile +
  charge + fan curve) onto the hardware unconditionally — the same path
  resume uses — so what the daemon (and the Decky plugin) report always
  matches the device, and the user's TDP / charge / cooling are restored
  after a firmware reset. Found + verified on-device on the Xbox Ally X.

## [2.4.0] — 2026-06-06

### Added

- **`default_platform_profile` config key** (startup-only, default
  `performance`). Programs the ACPI `platform_profile` / EPP at every
  boot. Accepts `performance` / `balanced` / `power-saver`
  (case-insensitive, plus the ACPI aliases `quiet` / `low-power`).
- **`hpdctl power <performance|balanced|eco>`** — the power-mode lever on
  the CLI (previously `set_profile` was D-Bus only). `eco` maps to
  `power-saver`. `power get` prints the current mode. Independent of `tdp`
  (watts) and `cool` (fans).
- **`AcConnected` D-Bus property** — emits `PropertiesChanged` on every AC
  plug/unplug edge, so clients (the Decky plugin) can drop their AC poll.
  The `is_ac_connected()` method is kept for backwards compatibility.

### Changed

- **Power and cooling are now decoupled.** Previously the "cooling level"
  (`silent` / `balanced` / `aggressive`) and the TDP auto-follow both
  drove the ACPI `platform_profile`, whose EPP silently clamped the APU
  far below the configured SPL — so a 25 W TDP could run at ~13 W just
  because auto-follow had selected `PowerSaver`. Now:
  - **`tdp set` is the single power lever** — the SPL you set is the real
    usable limit. The `platform_profile` defaults to `performance`
    (configurable via `default_platform_profile`) and is **no longer
    inferred from TDP**, so it never throttles your SPL.
  - **`cool set` / auto-cooling controls the fan curve only** (noise vs
    temperature), with zero effect on power. Auto-cooling (`fan_follows_tdp`)
    now infers the *fan curve* preset from TDP instead of the profile.
  - `set_profile` remains the manual power-profile lever over D-Bus,
    decoupled from cooling. The boot-time apply also migrates a device
    left in a throttling profile by an older hpd back to `performance`.
  - `fan_curve_follows_profile` is now a **no-op** (kept only so existing
    configs still parse).

### Fixed

- **AC charger detection on the Xbox Ally X.** `AsusChargeBackend::is_ac_connected`
  probed only `AC`, `ACAD`, `ADP0` and `ADP1`, but the ROG Xbox Ally X
  (RC73XA) exposes its mains node as **`AC0`**. None matched, so the read
  always fell through to the fail-safe `false` — the daemon reported
  "Battery (DC)" even while physically plugged (most visibly when booted on
  the charger, where no udev edge later corrects it). The probe list now
  includes `AC0`/`AC1` ahead of the legacy names. Regression tests cover
  the `AC0` node, the unplugged read, and the absent-node fail-safe.

### Changed

- **Fan-curve presets retuned (cooling-first) from on-device telemetry.**
  `Silent` / `Balanced` / `Aggressive` were recalibrated against the Xbox
  Ally X (RC73XA) using in-game captures (real GPU load, not synthetic).
  The unit's fans have a hard ~3700 RPM floor and a ~8400 RPM ceiling, and
  airflow saturates by duty ~220, so the curves now reach near-max airflow
  earlier in temperature instead of chasing a floor the fan can't undercut.
  `Aggressive` holds ~78 °C under a sustained 40 W Performance game load;
  `Balanced` keeps the chip in the low 60s °C; `Silent` rides the fan floor
  while the PowerSaver profile keeps the APU at ~13 W. New tuning helper:
  `scripts/fan-tune.sh` (apply a candidate curve to the EC + live monitor)
  and `scripts/fan-characterize.sh` (PWM→RPM sweep).

## [2.3.0] — 2026-06-03

### Added

- **`hpdctl status` now ends with a "System health" section** — the same
  checks `hpdctl doctor` runs, inlined into the everyday status command so
  the user gets a one-shot answer to "is anything overriding hpd, or is it
  all good?". The section reports, with an explicit all-clear when nothing
  is wrong:
  - **polkit** — installed (privileged commands work) or not (and which
    action IDs are unregistered).
  - **competing daemons** — whether a hard rival (`power-profiles-daemon`,
    `steamos-manager`) is live on the bus and fighting hpd over
    TDP / platform_profile / charge. A masked rival (e.g. PPD after the
    v2.2.3 `post_install` mask) has no bus owner and correctly shows as
    "none".
  - **advisory tools** — whether a power-adjacent daemon that is *wanted*
    (so reported, never masked) is live: Feral `gamemoded`, ASUS `asusd`
    (it also drives the platform profile / fan / charge, but owns RGB / Aura
    too, so masking it is the wrong call), or `auto-cpufreq`.
  - **session** — a `gamescope` (Steam Game Mode) hint, detected
    client-side from the session environment, noting `steamos-manager` is
    the TDP backend in that context.
- **Expanded competing-daemon coverage.** `get_power_conflicts` (hard
  rivals, masked by `doctor --fix`) now also detects `tuned` (Fedora /
  Bazzite's increasingly-default power tuner) and `hhd` (Handheld Daemon,
  Bazzite's Ally default). Because `hhd` and `auto-cpufreq` own no
  well-known D-Bus name, detection gained a second mechanism: a read-only
  `org.freedesktop.systemd1` `ListUnitsByPatterns` query
  (`hpd_dbus::conflicts::{RIVAL_UNITS, ADVISORY_UNITS}`) alongside the
  existing `NameHasOwner` path. `hpdctl doctor --fix` masks `tuned.service`
  and the templated `hhd@.service` (stopping live `hhd@<user>` instances
  via the instance glob first).
- **New D-Bus method `get_advisory_daemons() -> as`** on
  `dev.cirodev.hpd.PowerDaemon1`, the advisory counterpart to
  `get_power_conflicts` (`gamemoded`, `asusd`, `auto-cpufreq`). The hard-rival
  and advisory lists are kept disjoint across both detection axes by a
  regression test, so `doctor --fix` never masks a daemon it only meant to
  report.

### Known limitation

- Tools that write TDP from **inside another process** — Decky plugins
  (SimpleDeckyTDP, PowerControl) running in the plugin loader, or a manual
  `ryzenadj` invocation — own no service or bus name and so cannot be
  detected; the health section cannot warn about them.

### Changed

- `hpdctl doctor` and `hpdctl status` share one health renderer
  (`doctor::print_health`), so the two never drift. `doctor` keeps its
  banner; `status` wraps the block in the dashboard's frame.

---

## [2.2.3] — 2026-06-03

### Fixed

- **`hpd.service` reliably survives boot on images that ship
  `power-profiles-daemon` (CachyOS, SteamOS-based).** The v2.2.2 fix
  (`After=power-profiles-daemon.service`) solved the systemd startup race
  but not the D-Bus activation path: KDE Plasma / Gamescope request
  `net.hadess.PowerProfiles` one second into the user session, which
  D-Bus-activates PPD; the symmetric `Conflicts=` then kills hpd. The
  fix is to *mask* PPD — masking blocks both systemd and D-Bus activation.
  The AUR `post_install` and `post_upgrade` hooks now call
  `_neutralize_ppd()` which runs `systemctl disable --now` + `mask`
  when the unit exists, automatically and without user intervention.
  Both PKGBUILDs also declare `conflicts=('power-profiles-daemon')` so
  pacman prevents co-installation at the package-manager level.
  The detect-and-warn block added in v2.2.2 is removed — action
  supersedes warning. The `hpdctl doctor --fix` reference in the
  install message is narrowed to steamos-manager, which still requires
  it. `After=` + `Conflicts=` in `hpd.service` are kept as a safety
  net for `install.sh` deployments that do not run pacman hooks.

---

## [2.2.2] — 2026-06-03

### Fixed

- **`hpd.service` now reliably wins the boot-time conflict with
  `power-profiles-daemon`.** Both units are `WantedBy=multi-user.target`
  and systemd starts them in parallel; without an explicit ordering,
  whichever finishes starting *last* stops the other (systemd `Conflicts=`
  is symmetric). On CachyOS and similar distributions that ship
  `power-profiles-daemon` enabled by default, PPD frequently won the race
  and killed hpd — the daemon would disappear after every reboot, and
  D-Bus callers would receive `org.freedesktop.DBus.Error.ServiceUnknown`
  ("name is not activatable"). The fix adds
  `After=power-profiles-daemon.service` to `[Unit]`, pairing it with the
  existing `Conflicts=` as the systemd documentation recommends. hpd now
  always starts after PPD and deterministically stops it via the conflict,
  regardless of whether the user has run `hpdctl doctor --fix`. The
  `post_install` hook also now emits a prominent warning when
  `power-profiles-daemon` is detected active at install time. Regression
  introduced in v2.2.0 when `Conflicts=` was added without `After=`.

---

## [2.2.1] — 2026-06-01

### Fixed

- **Introspection XML is now well-formed under strict parsers.** zbus copies
  each `///` doc-comment line verbatim into the introspection
  `<!-- ... -->` block, and the `GetPowerConflicts` doc-comment contained
  `hpdctl doctor --fix` — `--` (two ASCII hyphens) is forbidden inside an XML
  comment. Lenient parsers (libxml2, gdbus) tolerated it, but Python's expat
  (used by the Decky plugin's dbus-next) rejected the *entire* document with
  `not well-formed (invalid token)`, leaving the plugin stuck on
  "Daemon: unreachable". The doc-comment was reworded to drop the `--`
  while keeping its meaning, and a regression test
  (`introspection_xml_is_well_formed`) now validates the exported object
  path's introspection XML under a strict parser (`quick-xml` with
  `check_comments`) so this cannot regress. No D-Bus contract change — same
  methods and signatures.

---

## [2.2.0] — 2026-06-01

### Added

- **`hpdctl doctor` / `hpdctl doctor --fix`** — one command to make hpd the
  sole power manager. `doctor` reports whether the polkit policy is
  installed and whether a competing power daemon
  (`power-profiles-daemon`, `steamos-manager`) is live and fighting hpd
  over TDP / platform profile / charge. `doctor --fix` neutralizes those
  daemons (`disable --now` + `mask`) and installs the polkit policy in one
  elevated step (`pkexec`, falling back to `sudo`) — a superset of
  `fix-polkit`. The per-user `steamos-manager` proxy is masked as the
  invoking user before elevation.
- New D-Bus method
  `GetPowerConflicts() → (as conflicting_daemons)` on
  `dev.cirodev.hpd.PowerDaemon1`, listing competing power daemons that
  currently own their well-known bus name. Detection lives in the daemon
  (`hpd-dbus/src/conflicts.rs`) and uses `NameHasOwner` (which does not
  D-Bus-activate, so checking never revives a masked rival). Surfaced in
  `hpdctl doctor` / `hpdctl status` and available to the Decky plugin.
- The daemon's startup self-check now also warns when a competing power
  daemon is live, pointing at `hpdctl doctor --fix` (mirrors the polkit
  self-check).
- `package/hpd.service` declares `Conflicts=power-profiles-daemon.service`,
  so starting hpd stops PPD automatically (and vice versa). `steamos-manager`
  is D-Bus-activated and handled by `hpdctl doctor --fix` instead.

### Fixed

- **`GetDiagnostics` is now actually implemented** on the D-Bus
  interface. It was declared in the CLI proxy and documented in 2.1.0 but
  never wired into `PowerDaemonInterface`, so every caller (`hpdctl
  status`, the Decky plugin's diagnostics panel) silently received a
  method-not-found error and no diagnostics. The polkit health surface now
  works as documented.

### Changed

- AUR `post_install` message now points users at `hpdctl doctor --fix` to
  make hpd the sole power manager.

---

## [2.1.0] — 2026-05-31

### Added

- **polkit registration self-check.** The daemon now verifies at startup
  that every `dev.cirodev.hpd.*` action is registered with polkit
  (`EnumerateActions`) and logs a loud, actionable warning if any is
  missing — the tell-tale of a partial install (binary copied without
  `package/polkit/*`), which otherwise surfaces only as an opaque
  `AuthFailed` on every privileged command. The daemon keeps running so
  telemetry stays available and the grant returns the moment the policy
  is installed.
- New D-Bus method `GetDiagnostics() → (b polkit_ok, as missing_action_ids)`
  on `dev.cirodev.hpd.PowerDaemon1`, exposing the same check live so
  `hpdctl status` and the Decky plugin can show the user *why* commands
  are denied and how to fix it.
- **`hpdctl fix-polkit`** — one-command recovery that installs the polkit
  policy + rules and reloads polkit. The canonical files are embedded in
  the binary (`include_str!`), so it works without the source tree; an
  unprivileged run re-execs itself through `pkexec` (falling back to
  `sudo`), both of which rely on polkit's always-registered core
  `org.freedesktop.policykit.exec` action.
- `hpdctl status` now warns when the polkit policy is missing and, when
  run interactively, **offers to install it on the spot** (`Install it
  now? [Y/n]`) instead of printing a script to copy elsewhere. `hpdctl`'s
  generic error path special-cases `AuthFailed` and points at
  `hpdctl fix-polkit`.
- `install.sh` gained a post-install verification step (step 5) that
  reloads polkit and confirms every hpd action registered via `pkaction`.

### Changed

- `hpd-dbus`'s polkit helper now special-cases polkit's "action is not
  registered" error with a precise remediation message instead of the
  generic fail-closed warning.

### Fixed

- **AUR `pkgrel`** — both `package/aur/PKGBUILD*.template` files reset
  `pkgrel` to `1` (they had carried `3` from the 1.0.0 packaging respins).
  Takes effect from the next published version; the already-shipped
  `2.0.0-3` packages are left as-is (re-syncing would be a `pkgrel`
  downgrade).

---

## [2.0.0] — 2026-05-30

Adds EC-mediated custom fan curves, live power/temperature telemetry, and
unifies cooling into a single `cool` lever. A **major** bump for one
reason — the `hpdctl fan` subcommands were removed (see Breaking) — with
everything else additive.

### ⚠ Breaking — `hpdctl` users

- **The `hpdctl fan` namespace was removed**; cooling is now one concept
  under `cool`. Migration:
  - `hpdctl fan set <profile>` → `hpdctl cool set <silent|balanced|aggressive>`
    (or the raw ACPI profile via the D-Bus `set_profile` method).
  - `hpdctl fan auto` → `hpdctl cool auto`.
  - `hpdctl fan curve set|get|reset` → `hpdctl cool set|get|reset`
    (and `hpdctl cool curve` to draw the active curve).

  Per the no-deprecation-alias policy
  ([`VERSIONING.md` §6](docs/release/VERSIONING.md)), the old forms are
  removed, not aliased. The raw, decoupled platform profile and fan curve
  remain available over D-Bus (`set_profile` / `set_fan_curve`).

### Added

- **Custom fan curves (ASUS / ROG Xbox Ally X)** — the daemon can now
  program the EC-mediated custom fan curve exposed by the
  `asus_custom_fan_curve` hwmon, instead of only selecting an ACPI
  platform profile. The firmware's default curve is defined only up to
  ~62 °C and tops out near 22 % duty, so the chip runs hot under sustained
  load; the new curves extend a monotonic ramp out to ~92 °C.
  - Three named presets — `silent`, `balanced`, `aggressive` — written as
    EC-mediated auto-points (never raw PWM), so the embedded controller
    keeps running the last curve even if the daemon stops.
  - New CLI under `cool`: `hpdctl cool set <silent|balanced|aggressive>`,
    `hpdctl cool auto`, `hpdctl cool reset`, `hpdctl cool get`, and
    `hpdctl cool curve` (draws the active curve as bars).
  - New D-Bus methods `SetCoolingLevel`, `SetFanCurve`, `ResetFanCurve`,
    `GetFanCurve`, and a read-only `fan_curve` property on
    `dev.cirodev.hpd.PowerDaemon1`.
  - New polkit action `dev.cirodev.hpd.set-fan-curve` (`auth_admin_keep`;
    `wheel` members are granted it passwordless by `49-hpd.rules`).
  - New config keys: `default_fan_curve` (preset applied on first boot,
    defaults to `balanced`) and `fan_curve_follows_profile`.
  - The active curve is re-applied on resume from suspend (fixing the bug
    where fans could blast at full speed after wake) and re-asserted after
    any platform-profile change (writing the ACPI profile can make the EC
    drop the custom curve, so TDP auto-follow no longer silently loses it).
  - New L2 capabilities `FanCurveControl` and `ThermalSensors` +
    `fan_curve()` / `thermal()` accessors on `HwBackend` (additive;
    existing backends default to `None`).
- **Live power, fan & temperature telemetry** — `hpdctl status` /
  `monitor` now show the **actual SoC power draw** (vs the configured TDP
  cap), CPU/GPU temperatures and CPU/GPU fan RPM, alongside the active
  fan curve, via the D-Bus `GetThermalStatus` method (a 5-tuple including
  `soc_power_mw`). This revives the previously-unsurfaced `FanControl`
  read path and reads the CPU `k10temp` Tctl, GPU `amdgpu` edge, and SoC
  power from `amdgpu` `power1_input` (located by hwmon name). Seeing
  actual power makes the manual-clamp case visible: a low cooling level
  holds the draw well below a high TDP cap.
- **Documentation** — a full bilingual user manual
  ([`docs/MANUAL.md`](docs/MANUAL.md) / [`docs/MANUAL-es.md`](docs/MANUAL-es.md)),
  a Spanish cooling explainer, the thermal rationale
  ([`docs/fan-curves.md`](docs/fan-curves.md)), and an on-device test plan.

### Changed

- **Unified cooling into a single lever.** `hpdctl cool set` programs the
  platform profile *and* the matching fan curve together (the profile
  also gates the chip's real power, so they are one decision); the status
  dashboard collapses the former three cooling lines into one
  `Cooling: <level> (auto|manual)`.
- `fan_curve_follows_profile` now defaults to **`true`** so the profile
  and curve always move together; set it to `false` to drive them
  independently over D-Bus (advanced).
- Fan-curve presets **validated on the ROG Xbox Ally X (RC73XA)**:
  `silent` ≈58 °C, `balanced` ≈68 °C, `aggressive` ≈95 °C (fans maxed)
  under a sustained all-core load — `balanced` solves the original
  ~87 °C firmware behaviour.

### Fixed

- **Fan-RPM read targeted the wrong hwmon** — the reader scanned
  `/sys/class/hwmon` by lowest index and could latch onto the unrelated
  `acpi_fan` node (which also exposes a `fan1_input`) instead of the
  `asus` node. It now resolves the sensor node by its `name` attribute,
  which is stable across boots and driver-load order.

---

## [1.0.0] — 2026-05-28

This release jumps directly from `0.1.0` to `1.0.0`. The intermediate
`0.2.0` trajectory the project briefly advertised was abandoned: the
cumulative breaking changes accumulated through the V1 remediation
lotes (7, 8, 9, 10, 11, 16, 18, 20) constitute a SemVer-major bump by
themselves, and the post-Lote-22 base is the right place to commit to
backwards compatibility. From `1.0.0` onward the public surface
(D-Bus interface `dev.cirodev.hpd.PowerDaemon1`, `hpdctl` subcommands,
on-disk state at `/var/lib/hpd/state.toml`, polkit action IDs in
`dev.cirodev.hpd.{set-tdp,set-charge,set-profile}`) follows SemVer
strictly.

### ⚠ Breaking — operators / packagers

- **State file location** — Persistent state moved from
  `/var/tmp/hpd_state.toml` (world-writable, symlink-race surface) to
  `/var/lib/hpd/state.toml`. Under systemd the path is resolved via the
  `STATE_DIRECTORY` env var injected by `StateDirectory=hpd`. There is
  no automatic migration: a fresh state file is created on first boot
  after upgrade.
  *(Lote 7 — Audit §7.4)*
- **systemd unit consolidation** — `dist/systemd/hpd.service` removed.
  `package/hpd.service` rewritten with full sandboxing
  (`ProtectSystem=strict`, `StateDirectory=hpd`, `PrivateNetwork`,
  `NoNewPrivileges`, `CapabilityBoundingSet=`,
  `SystemCallFilter=@system-service`, complete `ReadWritePaths` for
  all sysfs roots actually written). Packagers should rebuild against
  the new unit.
  *(Lote 7 — Audit §7.1)*

### ⚠ Breaking — D-Bus clients

- **`active_profile` property format** — Values are now stable
  lowercase kebab-case (`power-saver`, `balanced`, `performance`) or
  the raw custom string. Previously the daemon emitted Rust `Debug`
  output (`PowerSaver`, `Balanced`, `Performance`, `Custom("foo")`),
  which was an unstable internal representation. The new format
  roundtrips through `set_profile` and `ProfileName::FromStr`.
  *(Lote 9 — Audit §3.6)*
- **`PropertiesChanged` signals are now emitted.** Previously every
  property change was a silent in-memory update; D-Bus clients had
  to poll. They now receive
  `org.freedesktop.DBus.Properties.PropertiesChanged` whenever
  `current_spl`, `active_profile` or `charge_end_threshold` change.
  This may surface latent bugs in clients that previously assumed
  signals would never fire.
  *(Lote 10 — Audit §3.1)*
- **`set_preset` value set changed.** `silent`, `performance`, `turbo`
  are rejected with a clear error. Use `eco`, `balanced`, `max`
  instead. Reason: the old `performance` overloaded with
  `ProfileName::Performance` while meaning a different thing
  (midpoint TDP vs. max cooling profile). The rename is intentional
  and aliases were not kept so the confusion cannot resurface.
  *(Lote 11 — Audit §3.7)*

### ⚠ Breaking — CLI clients

- **`hpdctl preset turbo|silent|performance` is gone.** Use
  `hpdctl preset max|eco|balanced`. The CLI subcommand error message
  guides users to the new names.
  *(Lote 11 — Audit §3.7)*

### ⚠ Breaking — internal API (Rust)

- **`HpdError::Backend`** — Changed from struct variant
  `{ reason: String }` to tuple variant wrapping the new
  `BackendError` (`HpdError::Backend(BackendError)`). External Rust
  consumers (none today) would need to migrate `match` arms.
  *(Lote 8 — Audit §4.1)*
- **`HwBackend` trait surface — optional capability accessors.**
  Pre-1.0 `HwBackend` was a supertrait of `PowerEnvelope +
  ChargeControl + PlatformProfile + FanControl`, forcing every
  vendor to implement all four. It is now a standalone trait with
  one mandatory accessor (`fn power(&self) -> &dyn PowerEnvelope`)
  and three optional ones (`fn charge / profile / fan(&self) ->
  Option<&dyn …>`) defaulting to `None`. Vendors with partial
  hardware support (e.g. a future Steam Deck backend with no ACPI
  `platform_profile`) now implement only the accessors they can
  honour. ASUS continues to expose all four (`Some(...)` for each).
  External Rust consumers (none today) implementing `HwBackend`
  must migrate from "blanket impl over the four sub-traits" to
  "explicit accessors". The D-Bus / CLI / on-disk surfaces are
  unchanged. Side effect: the 11-method blanket-delegation block on
  `AsusBackend` (V1 §12.1 smell) disappears entirely.
  *(Lote 39 — Audit V1 §16.2 / V2 §4.18.2)*

### ⚠ Breaking — packagers / developers

- **Simulator mode is now a build-time feature.** Previously the
  `mock` Cargo feature on `hpd-sysfs` was always enabled by
  `hpd-daemon`, so production binaries shipped with the simulator
  code path linked in. Now the path is compiled in only with
  `cargo build -p hpd-daemon --features simulator`; the default
  feature set is `vendor-asus` only. `HPD_SIMULATOR=1` is a no-op
  on builds without the `simulator` feature. macOS / dev hosts
  must add `--features simulator`.
  *(Lote 16)*
- **System-bus policy widened; polkit now gates access.**
  `dev.cirodev.hpd.conf` previously only allowed `root` and members
  of the `wheel` group to send to the daemon. It now lets any user
  send method calls; per-action authorization is enforced by polkit
  via the new `dev.cirodev.hpd.{set-tdp,set-charge,set-profile}`
  action IDs. `wheel`-group members (the device owner) are granted
  these actions without a prompt by `package/polkit/49-hpd.rules` (see
  *Fixed* below); non-`wheel` callers hit the `auth_admin` defaults and
  need a polkit auth agent (polkit-gnome, kde-polkit, or `pkttyagent`
  for terminal use), otherwise their privileged calls fail with
  `AuthFailed`. `install.sh` deploys the policy file to
  `/usr/share/polkit-1/actions/` and the rule to
  `/usr/share/polkit-1/rules.d/`.
  *(Lote 20)*

### Added

- **`auto_cooling` D-Bus property** on
  `dev.cirodev.hpd.PowerDaemon1`. Read-only `bool` backed by
  `ProfileState::fan_follows_tdp` — `true` when the daemon is
  inferring the cooling profile from the TDP envelope, `false`
  after an operator has called `set_profile` (until they call
  `set_fan_auto` to re-enable inference). Closes the audit
  finding that the mode was silently flipped by `set_profile`
  with no way for clients to observe it. `hpdctl status` now
  surfaces it as "Cooling Mode: auto (follows TDP) | manual".
  PropertiesChanged is emitted by the daemon's
  `spawn_properties_changed_emitter` task on every flip.
  *(Lote 42 — Audit V1 §3.8 / V2 §4.7.1)*
- **`spawn_properties_changed_emitter` task** in `hpd-daemon/main.rs`
  watches the executor's state channel and calls zbus's generated
  `<prop>_changed` notifiers for each property whose underlying field
  changed (per-field diff to avoid spam).
  *(Lote 10 — Audit §3.1)*
- **`hpd-error` crate** (workspace layer L-1) holding the canonical
  `HpdError`, `SysfsError` and the new structured `BackendError`. Only
  dependency is `thiserror`. License `GPL-3.0-or-later`. Public surface
  is doc-commented end-to-end.
  *(Lote 8 — Audit §4.1)*
- **`BackendError::ParseFailed { field, raw, reason }`** structured
  parse-error variant. Backends now produce typed parse errors instead
  of `format!`-stringified messages.
  *(Lote 8)*
- **`ProfileName: Display`** — symmetric with `FromStr`. Documented as
  the stable D-Bus contract.
  *(Lote 9 — Audit §3.6)*
- **`TdpPreset` enum** with `Eco | Balanced | Max` variants and
  `Display`/`FromStr` symmetric in kebab-case. Replaces the previous
  `SystemPreset` whose `Performance` variant collided semantically
  with `ProfileName::Performance` (the former meant midpoint TDP, the
  latter meant max cooling profile).
  *(Lote 11 — Audit §3.7)*
- **`ProfileName::FromStr`** now accepts ACPI-native aliases (`quiet`,
  `low-power`) and preserves unknown values as
  `ProfileName::Custom(...)` instead of erroring.
  *(Lote 9)*
- **`uninstall.sh`** mirror of `install.sh` with optional `--purge`
  flag to remove `/var/lib/hpd/`.
  *(Lote 7)*
- **5 new tests** in `hpd-capabilities/profile.rs` covering Display
  format, Display→FromStr roundtrip, alias acceptance, custom
  preservation, and empty rejection.
  *(Lote 9)*
- **`test_set_spl_overflow_rejected`** in `hpd-core/reducer.rs`
  guarding against `watts * 1000` wrap-around.
  *(Lote 5 — Audit §3.3)*
- **`test_ac_plugged_persists_last_dc_target_even_when_target_unchanged`**
  in `hpd-core/reducer.rs` guarding the `AcPowerChanged` persistence
  hole.
  *(Lote 6 — Audit §3.5)*
- **Test fixture and assertion for canonical ASUS `ppt_pl3_fppt`
  attribute** in `hpd-backend-asus/power.rs`. Regression coverage for
  the silent-fallback bug.
  *(Lote 4 — Audit §3.2)*
- **`.gitignore`** entries for coverage (`*.profraw`, `*.profdata`,
  `coverage/`, `tarpaulin-report.*`), packaging output (`/dist/release/`,
  `*.deb`, `*.rpm`), logs, tmp artifacts, plus editor backups
  (`*~`, `*.bak`, `.#*`) and `.env*` files.
  *(Lotes 1, 17)*
- **`hpd-capabilities::testing::MockBackend`** — Arc-shared in-memory
  backend implementing all four L2 capability traits plus the blanket
  `HwBackend`. Records every write in `write_log` and can simulate
  hardware failure via `fail_writes`. Gated behind the new `testing`
  Cargo feature so production builds never link it.
  *(Lote 14)*
- **`crates/hpd-core/tests/executor_e2e.rs`** — three integration
  tests exercising the full Transition → reducer → Effect → backend
  pipeline: happy path with disk persistence, hardware-write rollback
  via `SyncPowerTarget` re-injection, and `watch::Receiver`
  propagation.
  *(Lote 14)*
- **15 reducer branch-coverage tests** in `hpd-core/src/reducer.rs`
  covering `AcPowerChanged` (debounce, plug, unplug with/without
  `last_dc_target`), `SystemResumed`, `EnableFanAuto`,
  `ChargeThresholdChanged`, `SetSpl` boundaries (min/max ±1),
  `SetEnvelope` FPPT-below-SPPT invariant, and `SyncPowerTarget`
  rollback.
  *(Lote 13)*
- **`PowerMilliwatts::from_watts` / `as_watts`** and the
  `MILLIWATTS_PER_WATT` constant — single source of truth for the
  W↔mW conversion previously inlined as `* 1000` / `/ 1000` across
  reducer, executor, daemon, D-Bus and ASUS backend.
  *(Lote 15)*
- **Domain constants** centralising the previously magic literals:
  `ProfileThresholds::DEFAULT` (0.33/0.67), `DEFAULT_CHARGE_THRESHOLD`
  (80), `SPPT_FACTOR` / `FPPT_FACTOR` (1.15 / 1.25),
  `TRANSITION_CHANNEL_CAPACITY` (32), ASUS-specific
  `ASUS_DEFAULT_SPPT_MAX_MW` / `ASUS_DEFAULT_FPPT_MAX_MW`, and a
  private `FanIndex { Cpu = 1, Gpu = 2 }` enum replacing bare
  `fan{1,2}_input` integers.
  *(Lote 15)*
- **Per-vendor Cargo features on `hpd-daemon`** — `vendor-asus`
  (default), `vendor-lenovo`, `vendor-valve`, and `simulator`. Each
  vendor flag gates one L1 backend crate via `dep:`. Production
  release builds no longer link Lenovo/Valve stubs or the MockSysfs
  path. `simulator` implies `vendor-asus` because the simulator
  currently only models ASUS firmware. With no vendor feature
  enabled the daemon still compiles and exits cleanly at startup.
  *(Lote 16)*
- **`README.md`, `LICENSE` (GPL-3.0), `.gitignore` expansion** —
  the repository is now presentable: hardware support matrix,
  install / usage / development sections, license recognised by
  GitHub.
  *(Lote 17)*
- **`DaemonConfig` + on-disk configuration** at
  `/etc/hpd/config.toml`. Every field is optional and defaults are
  applied per-field via `#[serde(default)]`, so partial / empty TOML
  files never break the daemon and adding fields never breaks
  existing configs. Missing or corrupt file → log + fall back to
  defaults (daemon survives). The new `package/hpd-example.toml` ships
  as `/etc/hpd/config.toml.example` to document the schema.
  *(Lote 18)*
- **`Transition::ConfigReload(RuntimeConfig)`** — reintroduced as a
  functional hot-reload pathway. The `RuntimeConfig` type
  (`hpd-capabilities::profile`) bundles the runtime-tunable subset
  (`profile_thresholds`, `sppt_factor`, `fppt_factor`) that the
  reducer consumes on every transition. The Executor intercepts
  `ConfigReload` before `reduce()` and swaps its own copy
  atomically; the next transition uses the new values.
  *(Lote 18)*
- **SIGHUP handler in `hpd-daemon`** — re-reads
  `/etc/hpd/config.toml` and pushes a `ConfigReload` transition.
  Mapped from `systemctl reload hpd` via `ExecReload=/bin/kill -HUP
  $MAINPID` in the unit file. `ConfigurationDirectory=hpd` also
  added.
  *(Lote 18)*
- **Graceful shutdown** — `hpd-daemon` now listens for both SIGINT
  (Ctrl+C) and SIGTERM (systemd). On either signal it sends the new
  `Transition::Shutdown` to the executor, which flushes the
  in-memory state to disk via `PersistState` and then breaks its
  `run()` loop. The daemon then awaits the executor (5s timeout
  guard, well below systemd's 90s `TimeoutStopSec`) and closes the
  D-Bus connection before returning. Previously the runtime simply
  dropped on Ctrl+C and any pending state mutation was lost.
  *(Lote 19)*
- **Polkit per-action authorization.** The D-Bus interface now
  delegates access control to polkit instead of the coarse
  `group="wheel"` policy that used to live in
  `dev.cirodev.hpd.conf`. Three action IDs are declared in
  `package/polkit/dev.cirodev.hpd.policy`:
  `dev.cirodev.hpd.{set-tdp,set-charge,set-profile}`. TDP and
  charge changes require `auth_admin` (prompt on every call);
  cooling-profile changes use `auth_admin_keep` (5-minute cache).
  Property reads remain unauthenticated. The check is **fail-closed**
  — any error talking to polkit results in `AuthFailed` — and is
  short-circuited to `true` under `#[cfg(feature = "simulator")]`
  so session-bus dev hosts remain usable. Implementation is a small
  manual call to `org.freedesktop.PolicyKit1.Authority` over zbus;
  no extra dependency added.
  *(Lote 20)*
- **`#![warn(missing_docs)]` on `hpd-capabilities`.** Every public
  trait, struct, enum, variant, field and method in the L2 capability
  crate now carries a `///` doc comment, and the lint is enforced at
  the crate root so future additions can't slip in undocumented.
  `cargo doc -p hpd-capabilities --no-deps` renders without warnings.
  `hpd-core` and `hpd-sysfs` got the same documentation pass at item
  level (module-level `//!` headers + `///` on every pub item) but
  without enabling the lint yet — a follow-up will turn it on once
  the existing items are stable.
  *(Lote 21)*
- **GitHub Actions CI** (`.github/workflows/ci.yml`). Two jobs:
  `build-test` on Ubuntu runs `cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, and `cargo build --workspace --release`;
  `build-macos-simulator` on macOS verifies the
  `cargo build -p hpd-daemon -p hpd-cli --features hpd-daemon/simulator`
  path keeps working. Both jobs cache cargo artefacts via
  `Swatinem/rust-cache` and cancel stale runs on the same ref.
  README gains the CI + license badges.
  *(Lote 22)*
- **`rust-toolchain.toml`** pinning the channel to `stable` with
  `rustfmt` and `clippy` components, so contributor toolchains stay
  in lockstep with CI.
  *(Lote 22)*

- **`/usr/share/hpd/VERSION` sidecar shipped by `install.sh`** —
  single-line text file (`X.Y.Z`) written at install time by
  extracting the workspace `Cargo.toml` `version`. Consumed by
  external clients (e.g. `hpd-decky-plugin`) that need the installed
  daemon version without parsing `journalctl` or requiring
  `systemd-journal` group membership. `uninstall.sh` removes it and
  the empty `/usr/share/hpd` directory. No code path inside the daemon
  reads this file; it is purely a consumer-facing affordance.
- **`missing_docs` lint enabled workspace-wide** — every public item
  carries a `///` doc comment and every module file opens with a
  `//!` block. CI runs with `-D warnings` so this is effectively an
  error in CI. Rustdoc inline documentation now exists across the 6
  crates that were missing it post-Lote-21: `hpd-error`,
  `hpd-netlink`, `hpd-backend-asus`, `hpd-dbus`, `hpd-cli`,
  `hpd-daemon`. Documentation coverage now matches the L-1→L4
  workspace layout.
  *(Lote 43 — Audit V2 Phase 3)*
- **Per-crate `README.md` for all 9 crates.** Each crate now ships a
  one-page README covering purpose, workspace layer, dependencies,
  a runnable example, and the `cargo doc` invocation that opens the
  generated rustdoc. The daemon README additionally documents the
  composition root's architecture diagram, signal handling, and the
  on-disk filesystem layout. Useful entry-point for contributors who
  want to navigate the workspace without opening every `lib.rs` first.
  *(Lote 44 — Audit V2 Phase 3)*
- **`docs/ARCHITECTURE.md` — global architecture document** (~550
  lines, 12 sections). Human-oriented walk-through covering the
  L-1→L4 workspace layout, the `Transition → reducer → Effect →
  Executor` pipeline (with ASCII diagrams of the data flow and
  rollback contract), the multi-runtime concurrency layout
  including the dedicated `std::thread` for `tokio-udev`'s `!Send`
  socket, the full lifecycle / signal matrix, the polkit fail-closed
  contract, the persistence and configuration models, a "where to
  look for things" lookup table, the recipes for adding a new
  vendor backend or D-Bus method, and a curated reading order for
  newcomers. The root README and crate READMEs now link here as the
  canonical design reference; `CLAUDE.md` remains the
  assistant-oriented variant.
  *(Lote 45 — Audit V2 Phase 3)*
- **`docs/dev/LINUX.md` — Linux development guide** (~300 lines, 11
  sections). End-to-end loop on a Linux host: toolchain pinning
  via `rust-toolchain.toml`, per-distro build-dep one-liners,
  workspace command reference, the full feature matrix CI runs,
  both running paths (production-shape `install.sh` walkthrough
  *and* iterative `cargo run` against the system bus with policy
  files installed), logging via `RUST_LOG`+`journalctl`, D-Bus
  introspection with `busctl`/`dbus-monitor`, polkit debugging
  with `pkaction`/`pkcheck`, manual suspend/resume + AC plug
  testing, filesystem layout reference, and 6 common pitfalls
  with diagnoses. Entry point for any contributor working on a
  Linux dev host.
  *(Lote 46 — Audit V2 Phase 4)*
- **`docs/dev/MACOS.md` — macOS development guide** (~250 lines, 9
  sections). Simulator-first workflow for Mac dev hosts. Includes
  an explicit "what works / what doesn't" matrix (no real sysfs,
  no udev, no logind, no polkit), Homebrew + Xcode CLT
  prerequisites, two recipes for starting the session D-Bus
  (`brew services start dbus` and `dbus-launch`), an end-to-end
  two-terminal walkthrough of the simulator including the exact
  `MockSysfs` seed values the daemon pre-populates, manual D-Bus
  calls with `dbus-send --session`, the limits of what the
  simulator can model (rollback, polkit denial, AC events), and
  6 common pitfalls. Catches the macOS contributor before they
  trip on `HPD_SIMULATOR=1` + `--features simulator` having to
  be passed together.
  *(Lote 47 — Audit V2 Phase 4)*
- **`CONTRIBUTING.md` — contribution guide** (~370 lines, 12
  sections). The contract between contributors and maintainers:
  scope (welcome vs. out-of-scope contributions), prerequisites,
  the four local gates CI enforces (`fmt`/`clippy`/`test`/`doc`)
  with the workspace.lints rules they translate into, hard rules
  (no `unsafe_code`, no `.unwrap()`/`.expect()`/`panic!` in
  production code, pure reducer, polkit-before-enqueue, SPDX
  headers, `missing_docs`), commit conventions (imperative
  subject ≤70 chars, body wrapped at 72, audit-lote tag,
  co-author trailer, atomic commits), CHANGELOG hygiene
  (Keep-a-Changelog format, breaking-by-audience subsection,
  release rename ritual), the SemVer policy on the public
  surface (D-Bus interface, CLI, on-disk state, polkit actions,
  config), short-form recipes for adding a D-Bus method or
  vendor backend cross-linking the full version in
  `docs/ARCHITECTURE.md`, a copy-pasteable PR checklist, review
  process, security disclosure channel, and code of conduct. The
  root README now points contributors here as the entry-point.
  *(Lote 48 — Audit V2 Phase 4)*
- **`docs/release/` — release pipeline design + runbook** (3 files,
  ~870 lines total). Three companion documents establishing the
  GitHub-native release model:
  - `PIPELINE.md` (~310 lines) — the *why*: three environments
    (QA = main CI, STG = `vX.Y.Z-rc.N` draft Release, PROD =
    `vX.Y.Z` public Release), tag conventions, artifact contents
    (tarball + checksums + optional GPG sig), per-environment
    workflow behaviour, GPG signing as opt-in via repo secrets,
    AUR distribution model, immutable-release rollback policy,
    permissions model, explicit non-goals (no nightlies, no
    .deb/.rpm in v1.0, no containers, no release bot), and an
    end-to-end ASCII diagram.
  - `VERSIONING.md` (~175 lines) — the *bump rules*: strict
    SemVer-2.0 from v1.0.0 onward, exact definition of "the
    public surface", a top-to-bottom decision matrix mapping
    every change category to MAJOR/MINOR/PATCH, the project's
    deliberate "no deprecation aliases" policy with rationale,
    pre-release suffix grammar, and four worked examples
    (including hypotheticals from the project's own surface).
  - `RELEASE_CHECKLIST.md` (~385 lines) — the maintainer's
    literal command-by-command runbook: prerequisites + repo
    secrets, day-of pre-release sanity (all four CI gates +
    feature matrix + supply-chain), version pick walking
    VERSIONING.md, bump ritual across `Cargo.toml` +
    `Cargo.lock` + `CHANGELOG.md`, annotated-tag creation with
    HEREDOC message template, `release.yml` watch step, AUR
    manual fallback for both source and binary packages,
    post-release housekeeping (re-open `[Unreleased]`, announce,
    48-hour bug watch), recovery recipes for four common
    failure modes, and a time budget table.
  Cross-linked from `CONTRIBUTING.md` and the root `README.md`.
  *(Lote 49 — Audit V2 Phase 5)*
- **`.github/workflows/release.yml`** + **`scripts/extract-changelog-section.sh`**.
  Implements the GitHub-native release model designed in
  [`docs/release/PIPELINE.md`](docs/release/PIPELINE.md). Triggers on
  annotated tags matching `v<X>.<Y>.<Z>` (stable → Public Release) and
  `v<X>.<Y>.<Z>-*` (RC/alpha/beta → Draft Release).
  Two jobs: `verify` re-runs the four CI gates (fmt/clippy/test/doc) on
  the exact tagged commit; `release` (a) guards that
  `workspace.package.version` in `Cargo.toml` matches the tag, (b)
  builds the stripped `x86_64-linux` binaries, (c) assembles
  `hpd-X.Y.Z-x86_64-linux.tar.gz` with the layout locked in
  PIPELINE.md §3 (binaries + install/uninstall scripts + LICENSE +
  README + CHANGELOG + full `package/` tree), (d) computes
  `SHA256SUMS`, (e) optionally GPG-signs it when
  `GPG_PRIVATE_KEY` + `GPG_PASSPHRASE` repo secrets are configured
  (skipped with a `::notice::` otherwise), (f) extracts the matching
  CHANGELOG section as release notes (falls back to the
  annotated-tag message if absent), and (g) calls
  `gh release create` with `--draft --prerelease` for RCs or a plain
  publish for stable. All artifacts are also uploaded as a 90-day
  workflow artifact for safekeeping.
  The helper script is standalone-runnable (`./scripts/extract-changelog-section.sh 1.0.0`),
  exits 1 with a clear error and a list of available headers when
  the version isn't found.
  *(Lote 50 — Audit V2 Phase 5)*
- **AUR packaging — templates + opt-in sync workflow** (Phase 5
  closeout). Five new files plus targeted updates to PIPELINE.md
  and RELEASE_CHECKLIST.md to reference the real paths instead of
  placeholders:
  - `package/aur/PKGBUILD.template` — source-build package
    `hpd-handheld-power-daemon` rendered against
    `$url/archive/v$pkgver.tar.gz` and built with
    `cargo build --release --frozen --workspace`. Installs
    binaries to `/usr/bin` (AUR convention) and rewrites the
    shipped `package/hpd.service` `ExecStart` path to match via
    `sed` so the unit works against the AUR install layout.
  - `package/aur/PKGBUILD-bin.template` — prebuilt-repack package
    `hpd-handheld-power-daemon-bin` (`provides=` + `conflicts=`
    the source one) that downloads
    `releases/download/v$pkgver/hpd-$pkgver-x86_64-linux.tar.gz`
    and skips compilation entirely. Same install layout.
  - `package/aur/hpd.install` — shared pacman hook for both
    packages: `daemon-reload` on install/upgrade/remove, sends
    SIGHUP to a running `hpd.service` on upgrade (matching the
    project's documented hot-reload contract), prints "next
    steps" message on first install with the
    `systemctl enable --now` and `config.toml` copy hints.
  - `scripts/aur-sync.sh` — standalone-runnable renderer
    (`<pkgname> <version>`) used by both the workflow and the
    manual fallback path in RELEASE_CHECKLIST §5. Downloads the
    matching upstream tarball, computes its sha256, renders the
    chosen template via sed, regenerates `.SRCINFO` via
    `makepkg --printsrcinfo`, clones the AUR repo over SSH,
    commits + pushes. Detects "no changes to push" as a no-op
    so re-running for the same version is safe.
  - `.github/workflows/aur-sync.yml` — opt-in CI workflow
    triggered on `release.published`. Runs inside an
    `archlinux:base-devel` container so `makepkg` is available
    without third-party actions. Skips pre-releases (RCs go to
    Draft, AUR is for stable only); skips silently with a
    `::notice::` when `AUR_SSH_KEY` repo secret is unset. Sets
    up a non-root `builder` user (`makepkg` refuses root),
    pins AUR's host key via `ssh-keyscan`, and pushes both
    source + bin packages in sequence.
  *(Lote 51 — Audit V2 Phase 5)*
- **`scripts/doctor.sh` standalone preflight.** Diagnoses every
  prerequisite `install.sh` assumes — Linux + x86_64, sudo, the Rust
  toolchain at MSRV (1.85), systemd as pid 1, D-Bus, polkit, `wheel`
  group membership (for passwordless `hpdctl` writes), a C linker — and
  DMI-probes the board against the supported ASUS list
  (RC71L / RC72L / RC72LA / RC73XA). Reports pass/warn/fail with
  copy-paste remediation hints; supports `--quiet` and `--strict`.
- **Richer `--help` for `hpdctl` and `hpd-daemon`.** `hpdctl --help`
  now explains what the daemon manages, notes that `wheel`-group users
  change settings without `sudo`, lists worked examples, and gives
  every subcommand and
  argument an explanatory paragraph (`hpdctl <cmd> --help`).
  `hpd-daemon` gains a dependency-free `--help` / `-V` handler: run by
  hand it prints a one-screen orientation (systemctl/journalctl usage,
  config/state paths, env vars, and a pointer to `hpdctl`) instead of
  silently launching a foreground service.

### Changed

- **`install.sh` now runs `scripts/doctor.sh` as a preflight** and
  aborts with exit 1 if any prerequisite is missing. Bypass with
  `./install.sh --skip-doctor`. This stops the "`cargo: command not
  found` halfway through the install" failure mode that hit fresh
  CachyOS / minimal distro installs.

### Fixed

- **`wheel`-group members no longer hit `AuthFailed` on every
  privileged call.** The polkit `<defaults>` required admin
  authentication for all three actions, so the device owner was
  challenged for a password on every TDP / charge / profile change —
  and where no polkit auth agent was running (a minimal handheld
  session, or a terminal driven over SSH) the call failed outright with
  `org.freedesktop.DBus.Error.AuthFailed`. A new companion rule,
  `package/polkit/49-hpd.rules`, grants every `dev.cirodev.hpd.*`
  action to members of the `wheel` group without a prompt. It keys on
  **group membership rather than the `allow_active`/`allow_inactive`/
  `allow_any` session tiers**, because on handheld desktop sessions a
  physically-local terminal can register as `Remote=yes` (driven over
  SSH, or a display manager that doesn't attach the session to the
  seat), which would otherwise drop the owner into `allow_any` and
  force a prompt. Non-`wheel` callers still fall through to the
  `auth_admin` defaults, and the polkit helper remains fail-closed.
  Requires a polkit build with the JS rules engine (>= 0.106, standard
  on modern distros).
- `install.sh` and `uninstall.sh` are now tracked as `100755` in git
  (previously `100644`), so users no longer need to `chmod +x` them
  after cloning the repo.

### CI

- **`aur-sync.yml` gains `workflow_dispatch`.** Maintainers can
  trigger the AUR sync manually from the Actions tab — with an empty
  `version` input it only smoke-tests `AUR_SSH_KEY` against
  `aur@aur.archlinux.org` (no PKGBUILD push, no AUR side-effects);
  with a `version` + `packages` choice (`both` / `source` / `bin` /
  `none`) it re-runs the full push for that version. Useful for
  validating SSH credentials before cutting a release and for
  recovering from a partial release-driven failure.
- **`aur-sync.yml` smoke-test surfaces actionable diagnostics and no
  longer chases a phantom auth failure.** It prints the offered key's
  fingerprint (safe to log) and runs `ssh -v` with the verbose log
  dumped only on failure, alongside copy-paste recovery commands. It
  also fixes the underlying false negative: the container runs as root
  with `$HOME=/github/home`, but root's passwd entry points at
  `/root`, so ssh's internal `~` expansion missed `known_hosts` and
  reported a misleading "Host key verification failed" that looked
  like a publickey rejection. The smoke-test now forces absolute
  `-F` / `-i` / `UserKnownHostsFile` paths so bash and ssh agree on
  where the SSH state lives.
- **All workflows opt into Node.js 24** ahead of the 2026-06-02
  deprecation of the Node 20 action runtime.

### Packaging

- **AUR packages auto-migrate from a manual `install.sh` deployment and
  self-enable** (shipped in `1.0.0-2`). The shared `package/aur/hpd.install`
  hook: (a) in `pre_install`/`pre_upgrade` removes the files a prior
  `install.sh` left at non-package paths (`/usr/local/bin/{hpd-daemon,hpdctl}`,
  `/etc/systemd/system/hpd.service`, `/etc/dbus-1/system.d/...`,
  `/usr/share/hpd/VERSION`, and the polkit policy + rule) — fixing the
  `error: failed to commit transaction (conflicting files)` and the old
  `/usr/local/bin` binaries shadowing the packaged ones; (b) enables and
  starts `hpd.service` in `post_install` so there is no manual `systemctl`
  step; (c) restarts the daemon on upgrade so a new binary actually takes
  effect.
- **Fix `hpd.service` left stopped after an AUR upgrade** (`1.0.0-3`).
  The `1.0.0-2` hook stopped the service unconditionally in
  `pre_upgrade` and then used `try-restart` in `post_upgrade`, which is a
  no-op on an inactive unit — so every upgrade left the daemon dead. The
  migration now only stops the service when it actually finds an
  `install.sh` deployment to clean up, and `post_upgrade` `restart`s the
  unit when it is enabled.
