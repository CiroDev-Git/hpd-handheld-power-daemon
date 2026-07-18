<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Decky plugin — v2.0.0 integration spec

The authoritative, **complete** map of what `hpd` v2.0.0 exposes and what
the Decky plugin should build on top of it. This **supersedes the
"§4 Daemon surface" of [`DESIGN.md`](DESIGN.md)**, which predates v2.0.0
(it has no cooling level, fan curves, or thermal/power telemetry). Use
this as the contract; update `DESIGN.md` §4 / §12 / §13 to match.

The plugin lives in a separate repo. Nothing here changes the daemon —
it documents the **public D-Bus contract** the plugin consumes.

## Connection

| | |
|---|---|
| **Bus** | system bus (production). Session bus when `HPD_SIMULATOR` is set + daemon built `--features simulator` (dev). |
| **Bus name** | `dev.cirodev.hpd.PowerDaemon1` |
| **Object path** | `/dev/cirodev/hpd/PowerDaemon1` |
| **Interface** | `dev.cirodev.hpd.PowerDaemon1` |

D-Bus member names are **PascalCase** on the wire (e.g. `SetCoolingLevel`,
`GetThermalStatus`). Properties emit
`org.freedesktop.DBus.Properties.PropertiesChanged`.

## Complete D-Bus surface

### Methods

| Member | Signature | Does | Polkit |
|---|---|---|---|
| `SetSpl` | `(u watts)` | Set the TDP / sustained power limit (whole watts). SPPT/FPPT derived. | `set-tdp` |
| `SetPreset` | `(s name)` | TDP preset: `eco` / `balanced` / `max` (min/mid/max of the hw range). | `set-tdp` |
| `SetChargeThreshold` | `(y percent)` | Battery charge cap, `20..=100`. | `set-charge` |
| `SetCoolingLevel` | `(s level)` | **The cooling lever — fans only.** `silent` / `balanced` / `aggressive` → sets the **fan curve** and switches to manual cooling. Does **not** change power (decoupled). | `set-profile` |
| `SetFanAuto` | `()` | Auto cooling: the **fan curve** follows the TDP. | `set-profile` |
| `ResetFanCurve` | `()` | Hand the fans back to the firmware's automatic curve. | `set-profile` |
| `GetThermalStatus` | `() → (iiiii)` | Live `(cpu_temp_c, gpu_temp_c, cpu_rpm, gpu_rpm, soc_power_mw)`. `i32::MIN` = unavailable. **No signal — poll.** Prefer `GetTelemetry` in new code. | — (read) |
| `GetTelemetry` | `() → a{sv}` | Extensible telemetry (daemon ≥ 2.8.0): everything `GetThermalStatus` has (`cpu_temp_c`, `gpu_temp_c`, `cpu_fan_rpm`, `gpu_fan_rpm`, `soc_power_mw`) plus `battery_power_mw` (discharge only), `battery_percent`, `battery_status`, `battery_health_pct`, `battery_cycles`, `cpu_freq_mhz`, `gpu_freq_mhz`, `gpu_busy_pct`, `vram_used_mb`/`vram_total_mb`. A key is **present only if the hardware exposes it** — absence, not a placeholder, means unsupported. **No signal — poll.** | — (read) |
| `GetFanCurve` | `() → (a(uu) cpu, a(uu) gpu)` | The 8 `(temp_c, pwm)` points of each fan's active curve (pwm `0..=255`). Empty if firmware-only. **No signal.** | — (read) |
| `SetFanCurve` | `(a(yy) cpu, a(yy) gpu)` | **Custom curve editor backend** (daemon ≥ 2.9.0). Exactly 8 `(temp_c, pwm)` points per fan; latches manual cooling like `SetCoolingLevel`. Validated against `GetFanCurveConstraints` — a violation returns `InvalidArgs` naming the offending point (e.g. "point 8: 92°C requires pwm ≥ 200"). | `set-profile` |
| `GetFanCurveConstraints` | `() → a{sv}` | This device's curve limits (daemon ≥ 2.9.0): `points` (`u`, always 8), `temp_min_c`/`temp_max_c` (`y`), `pwm_min`/`pwm_max` (`y`), `safety_floor` (`a(yy)` of `(temp_threshold_c, min_pwm)` — at/above the threshold, pwm must be at least that minimum). Drive the editor's axes and forbidden zone from this, never hardcoded constants. Empty map if the device has no programmable curve. | — (read) |
| `GetHardwareLimits` | `() → (uuuu)` | `(spl_min, spl_max, sppt_max, fppt_max)` in watts — the valid TDP range. | — (read) |
| `IsAcConnected` | `() → (b)` | Whether the charger is plugged in. | — (read) |
| `GetVersion` | `() → (s)` | The daemon's version string (daemon ≥ 2.4.2). Errors on older daemons → show "unknown". | — (read) |
| `GetDiagnostics` | `() → (b as)` | `(polkit_ok, missing_action_ids)`. `polkit_ok == false` ⇒ the polkit policy is not installed and **every** gated setter fails with `AuthFailed`. Live check; safe to poll. | — (read) |
| `SetProfile` | `(s profile)` | **The power-profile lever** (ACPI platform profile / EPP): `power-saver`/`balanced`/`performance`. Decoupled from cooling; defaults to `performance` so the SPL is the real limit. Lower it only for an efficiency bias. | `set-profile` |
| `SetAcMaxPerformance` | `(b enabled)` | Toggle the **"lock to max on AC"** preference (daemon ≥ 2.7.0). On = plugging in pins Performance/Max/Aggressive + locks power/cooling; off = AC fully manual. Persisted; applied immediately. **Not** rejected while locked (this releases the lock). | `set-profile` |
| `EnableGpuAutoFollow` | `()` | Re-enable GPU-clock auto-follow: the daemon resumes inferring a clock ceiling from the active TDP envelope, applying it immediately rather than waiting for the next TDP change (daemon ≥ 2.12.0, mirrors `SetFanAuto`). **This is the opt-in the whole feature is gated behind** — the daemon never touches the GPU clock until this is called at least once (opt-in *forever*, not just at first boot — unlike auto-cooling, whose steady state is never "off", GPU-clock management genuinely defaults to permanently untouched). There is **no** method to pin an arbitrary `(min_mhz, max_mhz)` range — `SetGpuClockRange` existed through the daemon's 2.x line and was removed in 3.0.0: real-world use found it was the one control in the whole stack a caller could set to a value that silently capped performance with no way for the daemon to distinguish intent from an oversight. | `set-profile` |
| `ResetGpuClocks` | `()` | Hand the GPU clock back to firmware automatic control (daemon ≥ 2.12.0, mirrors `ResetFanCurve`). | `set-profile` |
| `GetGpuClockConstraints` | `() → a{sv}` | This device's GPU clock range bounds (daemon ≥ 2.12.0): `range_min_mhz`/`range_max_mhz` (`u`), read **live from the kernel's `OD_RANGE`** on every call — unlike the fan-curve safety floor (a per-model calibration), this is Class-A generic-kernel-interface data needing no recalibration on a new device. Empty map if the device has no programmable GPU clock range, or the live read failed. | — (read) |
| `GetGpuClockRange` | `() → (u, u)` | The GPU clock range **actually committed to hardware** (daemon ≥ 2.12.0), read back from the backend exactly like `GetFanCurve` — not the constraints. `(0, 0)` is the daemon's own sentinel for "not applicable" (no programmable range, firmware auto, or an unreachable read) — 0 MHz is never a real value on any exposed hardware, mirroring `GetThermalStatus`'s `i32::MIN` convention at this narrower unsigned-only boundary. | — (read) |
| `GetPowerConflicts` | `() → as` | Friendly names of competing power daemons currently live on the bus (daemon ≥ 2.2.0), e.g. `["steamos-manager"]` — daemons that write the same TDP/platform-profile/charge surfaces hpd owns, so its settings may not stick. Empty list = hpd is the sole power owner. **Non-blocking**: never folds into `last_error` or gates a control; drives a dismissible warning banner only. | — (read) |
| `GetAdvisoryDaemons` | `() → as` | Friendly names of power-adjacent *advisory* daemons currently live on the bus (daemon ≥ 2.3.0) — today Feral `gamemoded`, activated by Steam/Lutris around a running game. Unlike `GetPowerConflicts`, these are **not** rivals to neutralize — reported only, never masked. Empty list = none live. Errors against a daemon predating this method; callers should degrade to "unknown" rather than treating an error as "none". | — (read) |
| `GetPpdShimActive` | `() → b` | Whether hpd's `net.hadess.PowerProfiles` compatibility shim actually claimed its bus name at startup (daemon ≥ 2.10.0). `false` means a real `power-profiles-daemon`/`tuned-ppd` was live and unmasked, so PPD-only clients (KDE's power applet, `powerprofilesctl`) still see no owner. Purely informational — the plugin has no action to take on this beyond display. | — (read) |
| `RestoreDefaults` | `()` | **Restore recommended defaults in one atomic transaction** (daemon ≥ 2.14.0): TDP → Balanced preset, Power mode → Performance, Charge cap → 80% (`DEFAULT_CHARGE_THRESHOLD`, not 100% — that disables the cap entirely), Cooling → hpd-managed auto (`AutoCooling = true`, curve inferred from the just-set Balanced TDP — **not** firmware auto; changed in a daemon ≥ 2.14.2 patch fix, see CHANGELOG — matches the documented "just works" recommendation of `cool auto` + `preset balanced`, and a fresh install's own boot default), and — only if the device is already opted into GPU-clock auto-follow — GPU clock → firmware auto too (never opts a fresh user in). Rejected as a whole while AC-locked; a full no-op (zero effects) if already at every default. Reuses all three existing polkit actions (`set-tdp` **and** `set-charge` **and** `set-profile`, gated on all three — no new action). The plugin has no client-side equivalent to build: `useRestoreDefaults`/`RestoreDefaultsButton` call this directly via `commitRestoreDefaults`, which echo-waits on `ActiveProfile`/`ChargeEndThreshold` reaching their targets plus **either** `FanCurve == "auto"` **or** `AutoCooling == true` (version-tolerant: accepts the terminal state of both the pre-2.14.2 firmware-auto behaviour and the current hpd-auto one, so the same plugin build works against either daemon patch level without a new capability probe) — not `CurrentSpl`, whose target is hardware-limit-derived, and not `GpuClockRange`, whose target is conditional. Version-gated by a plugin-internal `hasattr(iface, "call_restore_defaults")` introspection probe (`get_restore_defaults_supported` IPC), not a `hpdDaemonCompat` bump — mirrors every other daemon-version-gated feature in this table. | `set-tdp` **and** `set-charge` **and** `set-profile` |

> **The AC-lock contract (daemon ≥ 2.7.0).** While `AcLocked` is `true`, the
> daemon **rejects** `SetSpl` / `SetPreset` / `SetProfile` / `SetCoolingLevel`
> / `SetFanAuto` / `ResetFanCurve` with a "locked on AC" `Failed` error;
> `SetChargeThreshold` and `SetAcMaxPerformance` still work. The plugin should
> read `AcLocked` and disable those controls (it folds `!locked` into their
> `enabled`), and use `AcMaxPerformance` to drive a "Lock to max on AC" toggle.
> On a daemon < 2.7.0 both properties are absent → never locks, toggle hidden.

### Properties (read-only, emit `PropertiesChanged`)

| Property | Type | Meaning |
|---|---|---|
| `CurrentSpl` | `u` | Current TDP cap (watts). |
| `FanCurve` | `s` | Active cooling level: `silent` / `balanced` / `aggressive` / `custom` / `auto` (`auto` = firmware curve, daemon not managing it). |
| `AutoCooling` | `b` | `true` = auto (follows TDP), `false` = manual. |
| `ChargeEndThreshold` | `y` | Battery charge cap (%). |
| `ActiveProfile` | `s` | The power-profile / EPP (`power-saver`/`balanced`/`performance`/custom). Defaults to `performance`. This is the **power** lever now (not cooling); surfaced as the first-class **Power mode** control (Performance/Balanced/Eco), separate from Cooling. |
| `AcConnected` | `b` | Charger plugged in (daemon ≥ 2.4.0). **Emits `PropertiesChanged`** — subscribe instead of polling `IsAcConnected()`. Falls back to the method on older daemons. |
| `AcLocked` | `b` | Power/cooling controls are **locked** because the device is on AC with the lock preference on (daemon ≥ 2.7.0). Disable the TDP/preset/power-mode/cooling controls while `true`; battery charge stays editable. |
| `AcMaxPerformance` | `b` | The toggleable **"lock to max on AC" preference** itself (daemon ≥ 2.7.0), vs `AcLocked` (the live state). Drives the Settings toggle. |
| `GpuClockRange` | `s` | Active GPU-clock selection (daemon ≥ 2.12.0): `silent`/`balanced`/`aggressive`/`auto`. `auto` = firmware in charge — the **permanent default** until the user opts in via `EnableGpuAutoFollow`. `unknown` is the rare rollback case (a failed write whose own cleanup also failed) — never a state a caller can request. Mirrors `FanCurve`. |
| `GpuFollowsTdp` | `b` | Whether the daemon is currently inferring the GPU clock ceiling from the TDP envelope (daemon ≥ 2.12.0). `false` is the permanent default until opt-in (same trigger as `GpuClockRange` above). Mirrors `AutoCooling`. |

## Feature → UI mapping, by priority

### 🔴 Obligatorias (sin esto el plugin no tiene sentido)

1. **Cooling control** — one selector `Silent / Balanced / Aggressive` +
   an **Auto** toggle.
   - Read level from the **`FanCurve`** property, mode from **`AutoCooling`**.
   - Set a level → `SetCoolingLevel(level)` (switches to manual).
   - Auto → `SetFanAuto()`.
   - ⚠️ Label it clearly as **fans only** (noise ↔ temperature). Cooling no
     longer changes power, so drop any "Silent caps power / Aggressive
     unlocks the TDP" copy. Power is the TDP slider (#2); the **Power mode**
     lever is its own first-class control (#5).
2. **TDP control** — a slider in watts.
   - Read **`CurrentSpl`**; range = `GetHardwareLimits()` (`spl_min..spl_max`).
   - Set → `SetSpl(watts)`.
3. **Live telemetry** — power, temps, fans.
   - `GetThermalStatus()` polled at ~1 Hz **while the panel is open**.
   - Show **actual power vs the TDP cap** (`soc_power_mw / 1000` W next to
     `CurrentSpl`), CPU/GPU °C, CPU/GPU RPM. Render `i32::MIN` as "n/a".
4. **Battery charge cap** — read **`ChargeEndThreshold`**, set
   `SetChargeThreshold(%)`. The single biggest lever for battery longevity.
5. **Power mode** — a first-class control: `Performance / Balanced / Eco`
   (`SetProfile` + `ActiveProfile`; `Eco` = `power-saver`). The power/EPP
   lever, separate from TDP and Cooling, default `Performance`. When the
   user picks `Balanced`/`Eco`, show an **informative** note that real
   power is held below the TDP (don't disable the slider — the real
   ceiling is workload-dependent). Implemented in the plugin's Power
   section.

### 🟡 Importantes (muy recomendadas)

5. **TDP presets** — `Eco / Balanced / Max` quick buttons → `SetPreset(name)`.
6. **AC/battery indicator** — `IsAcConnected()` (and re-check on the
   `CurrentSpl` change signal, since AC plug ramps TDP).
7. **Reactive updates** — subscribe to `PropertiesChanged` for the 5
   properties so the UI reflects **external** changes instantly (AC plug,
   `hpdctl` from a terminal, auto-cooling re-inference) without polling.
8. **Reset to firmware** — `ResetFanCurve()` ("hand fans back to firmware").
9. **Fan curve graph** — `GetFanCurve()` → draw CPU/GPU curves
   (temp → duty). Refresh on open and after a cooling change.
10. **Hardware-limits–aware slider** — clamp the TDP slider to
    `GetHardwareLimits()`; show `sppt_max`/`fppt_max` as info.
11. **Polkit-aware actions** — `wheel` members (Deck owner) are **not**
    prompted (passwordless via `49-hpd.rules`); non-`wheel` callers get a
    polkit prompt. Handle `AuthFailed` gracefully (toast, not a crash).
    On load, call **`GetDiagnostics()`**: if `polkit_ok` is false the
    daemon's polkit policy was never installed (common with a hand-copied
    binary), so **every** privileged write will be denied — show a
    "finish setup" banner instead of surfacing raw `AuthFailed` per
    action. The banner's action button should run **`hpdctl fix-polkit`**
    (it self-elevates via `pkexec`, installs the policy, reloads polkit —
    no daemon restart). It's a live check, so re-poll `GetDiagnostics()`
    afterward to clear the banner.

> **Note:** Power mode (#5) was promoted from this "optional/advanced"
> tier to a first-class control. The Power-mode hint below is now folded
> into that control (an informative note shown when Balanced/Eco is
> active). The only thing left genuinely advanced is the *raw* fan curve.

### 🟢 Opcionales / avanzadas

12. **Custom fan-curve editor** (daemon ≥ 2.9.0) — drag the 8 points of a
    hand-drawn curve, per fan or shared. Seed the graph from
    `GetFanCurve()` (the currently active curve) or a preset as a
    starting template. Build the editor's axes and forbidden (unsafe)
    zone from **`GetFanCurveConstraints()`** — never hardcode
    `temp_min_c`/`temp_max_c`/`safety_floor`, so a future device with a
    different zone just works. Clamp drags to the safe zone client-side
    for a good UX, but the daemon is the source of truth: `SetFanCurve`
    still validates and returns `InvalidArgs` naming the offending point
    on anything that slips through.
13. **Power-mode hint (folded into #5)** — when `ActiveProfile` is
    `balanced`/`power-saver`, the Power mode control shows an informative
    note that real power is held below the TDP, with the current TDP
    value. (Cooling never limits power — only the platform profile does.)
14. **Extended telemetry section** (daemon ≥ 2.8.0) — battery discharge
    wattage/percent/health/cycles, CPU/GPU clocks, GPU load, VRAM via
    **`GetTelemetry()`**. Render each field independently; a device that
    doesn't expose a given key (e.g. no discrete GPU, no battery) simply
    omits it — hide that row rather than showing a placeholder.
15. **Reassurance copy** — temp/RPM "normal vs. worry" tooltips (e.g. high
    temps under a heavy game with fans maxed are normal; temperature
    tracks your **TDP** now, and cooling just trades fan noise for a few
    degrees). Source the wording from [`../MANUAL.md`](../MANUAL.md) →
    "What's normal vs. what to worry about".
16. **GPU clock range control** (daemon ≥ 2.12.0) — an **Auto** button
    (`EnableGpuAutoFollow` / `GpuFollowsTdp`) and a **Reset**
    (`ResetGpuClocks`). No manual MHz editor: the daemon has no method to
    pin an arbitrary range (`SetGpuClockRange` existed through the 2.x
    line and was removed in 3.0.0 — real-world use found a manually-set
    range was the one control in the whole plugin a user could set once
    and forget, silently capping performance with no explanation). Show
    the device's bounds from **`GetGpuClockConstraints()`** as
    informational context only (what range `Auto`'s curated tiers resolve
    within) — never as editable slider bounds. **Hidden entirely** when
    `GetGpuClockConstraints()` returns an empty map (no programmable
    range on this device, or an older daemon) — this is a stricter gate
    than the fan-curve editor's, because unlike cooling, GPU-clock
    management is genuinely **opt-in forever**: do not add a default-on
    toggle or auto-enable this from any other control (not even "Restore
    defaults" turns it on — it only resets an auto-follow the user
    already opted into).
17. **Competing-power-daemon banner** (`GetPowerConflicts`, daemon ≥
    2.2.0) — a dismissible, non-blocking warning when another daemon
    (`power-profiles-daemon`, `steamos-manager`, …) is live on the bus
    and writing the same TDP/profile/charge surfaces hpd does. Poll on
    panel open (not on a timer); re-poll after the user runs the fix.
    Never gates a control and never folds into the generic error state.
18. **Advisory-daemon / PPD-shim status** (`GetAdvisoryDaemons`,
    `GetPpdShimActive`, daemons ≥ 2.3.0 / ≥ 2.10.0) — informational-only
    reads (e.g. a "compat PPD: active" line): a live `gamemoded` around a
    running game is expected and never reported as a problem, and the PPD
    shim's active/inactive state has no plugin action attached to it.

## Update strategy

- **Properties (5):** event-driven via `PropertiesChanged`. No polling.
- **`GetThermalStatus` / `GetTelemetry`:** poll **~1 Hz only while the
  panel is visible**; stop when hidden/closed (battery + perf). Neither
  has a change signal.
- **`GetFanCurve`:** on-demand (open the curve view / after a cooling
  change). Static between changes.
- **`GetFanCurveConstraints`:** on-demand, once per session (opening the
  curve editor) — it's a static per-device fact, not live state.
- **`GetGpuClockConstraints`:** on-demand, once per connection (opening
  the GPU clock control) — like `GetFanCurveConstraints`, treat it as a
  static per-device fact even though the daemon itself re-reads the
  kernel live on every call.
- **`GetGpuClockRange`:** on-demand (opening the GPU clock control /
  after a GPU-clock change), same cadence as `GetFanCurve`.
- **`GetPowerConflicts` / `GetAdvisoryDaemons` / `GetPpdShimActive`:**
  poll-only (no signal); primed on connect / panel open, and re-polled
  after the user runs the conflict-resolution fix. Not on a timer.

## Conditional capability surfacing (graceful degradation)

The plugin should not assume every reading exists:

| Condition | Behaviour |
|---|---|
| `GetThermalStatus` field == `i32::MIN` | render "n/a" (sensor/fan absent). |
| `GetTelemetry` key absent | hide that row entirely — never show a placeholder like `0`. |
| `GetFanCurve` returns empty vectors | hide the curve graph (firmware-only / no programmable curve). |
| `GetThermalStatus` `gpu_*` == `i32::MIN` | single-fan / no discrete GPU sensor → show CPU only. |
| `GetFanCurveConstraints` returns an empty map | hide the curve editor (no programmable curve on this device). |
| `GetGpuClockConstraints` returns an empty map | hide the GPU clock range control entirely (no programmable range on this device, or an older daemon). |
| `GetGpuClockRange` returns `(0, 0)` | render "not applicable" — firmware auto, no programmable range, or the read failed; never show it as `0 MHz`. |
| `ChargeEndThreshold` unreadable | hide the battery-cap control. |

## ⚠️ Limitations (so you don't design around them)

- **Telemetry is poll-only** (`GetThermalStatus`, `GetTelemetry`,
  `GetFanCurve`, `GetFanCurveConstraints`) — no signals; poll while
  visible.
- **Units / sentinels:** TDP & temps are whole units; **power is
  milliwatts**; `i32::MIN` is the "unavailable" sentinel in
  `GetThermalStatus` (`GetTelemetry` instead omits the key entirely).
- **`GetTelemetry` keys can vary by device and daemon version** — always
  check for key presence, never assume a fixed set (this is *why* it's
  an `a{sv}` map and not a tuple).
- **A `PropertiesChanged` echo can arrive before the matching `Get*` call
  reflects the new value.** The executor publishes the new `ProfileState`
  (and so the property echo) before it finishes dispatching the effect
  that actually writes the hardware — this is a property of the daemon's
  executor loop generally, not specific to one feature. It's already
  documented for the fan curve (`FanCurve == "custom"` can echo before
  `GetFanCurve` reflects the new points) and applies identically to
  `GpuClockRange == "<tier>"` (after `EnableGpuAutoFollow`) vs.
  `GetGpuClockRange`. Harmless either way: a beat of staleness that
  self-corrects.
- **GPU clock management is opt-in *forever*, not just at first boot**
  (daemon ≥ 2.12.0). Unlike auto-cooling — whose real steady state is
  never "off" — `GpuClockRange`/`GpuFollowsTdp` stay `auto`/`false`,
  meaning the daemon never touches the GPU clock at all, until the user
  calls `EnableGpuAutoFollow` at least once. No other daemon action (an
  AC plug, `RestoreDefaults`, a fresh install) ever opts a user into
  this.
- **`GetGpuClockConstraints`/`GetGpuClockRange`/`ResetGpuClocks` work
  against the `HPD_SIMULATOR` dev build (daemon ≥ 2.13.0), but
  `EnableGpuAutoFollow` does not.** `pp_od_clk_voltage` is a *command*
  file on real hardware — the driver updates its own `OD_SCLK`/
  `OD_RANGE` report only after a `s`/`c` write, a commit-and-read-back
  the simulator's flat `MockSysfs` store can't model yet. If a change
  against the simulator build silently fails only on that call, this is
  why — it is not a regression to chase.

## Suggested minimal panel

```
┌ hpd ───────────────────────────────┐
│ Cooling   [Silent][Balanced][Aggr]  │  ← SetCoolingLevel / FanCurve
│           Auto  ( ●)                 │  ← SetFanAuto / AutoCooling
│ TDP       ▓▓▓▓▓▓░░░  18 W            │  ← SetSpl / CurrentSpl / GetHardwareLimits
│           [Eco][Balanced][Max]       │  ← SetPreset
│ ── live ──                           │
│ Power     16 W / 18 W   🔋 Battery   │  ← GetThermalStatus + IsAcConnected
│ Temp      CPU 68° · GPU 58°          │
│ Fans      CPU 5300 · GPU 5300 rpm    │
│ Battery   cap 80 %                   │  ← SetChargeThreshold / ChargeEndThreshold
│ [▸ Fan curve graph]  [Reset to fw]   │  ← GetFanCurve / ResetFanCurve
└──────────────────────────────────────┘
```

For the thermal rationale and user-facing wording, see
[`../MANUAL.md`](../MANUAL.md) / [`../MANUAL-es.md`](../MANUAL-es.md) and
[`../fan-curves.md`](../fan-curves.md).
