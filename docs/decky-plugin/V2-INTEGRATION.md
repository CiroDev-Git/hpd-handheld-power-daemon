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
| `ResetFanCurve` | `()` | Hand the fans back to the firmware's automatic curve. | `set-fan-curve` |
| `GetThermalStatus` | `() → (iiiii)` | Live `(cpu_temp_c, gpu_temp_c, cpu_rpm, gpu_rpm, soc_power_mw)`. `i32::MIN` = unavailable. **No signal — poll.** | — (read) |
| `GetFanCurve` | `() → (a(uu) cpu, a(uu) gpu)` | The 8 `(temp_c, pwm)` points of each fan's active curve (pwm `0..=255`). Empty if firmware-only. **No signal.** | — (read) |
| `GetHardwareLimits` | `() → (uuuu)` | `(spl_min, spl_max, sppt_max, fppt_max)` in watts — the valid TDP range. | — (read) |
| `IsAcConnected` | `() → (b)` | Whether the charger is plugged in. | — (read) |
| `GetDiagnostics` | `() → (b as)` | `(polkit_ok, missing_action_ids)`. `polkit_ok == false` ⇒ the polkit policy is not installed and **every** gated setter fails with `AuthFailed`. Live check; safe to poll. | — (read) |
| `SetProfile` | `(s profile)` | **The power-profile lever** (ACPI platform profile / EPP): `power-saver`/`balanced`/`performance`. Decoupled from cooling; defaults to `performance` so the SPL is the real limit. Lower it only for an efficiency bias. | `set-profile` |
| `SetFanCurve` | `(s preset)` | **Advanced/raw.** Fan curve preset (`silent`/`balanced`/`aggressive`) directly (`SetCoolingLevel` is the normal path). | `set-fan-curve` |

### Properties (read-only, emit `PropertiesChanged`)

| Property | Type | Meaning |
|---|---|---|
| `CurrentSpl` | `u` | Current TDP cap (watts). |
| `FanCurve` | `s` | Active cooling level: `silent` / `balanced` / `aggressive` / `custom` / `auto` (`auto` = firmware curve, daemon not managing it). |
| `AutoCooling` | `b` | `true` = auto (follows TDP), `false` = manual. |
| `ChargeEndThreshold` | `y` | Battery charge cap (%). |
| `ActiveProfile` | `s` | The power-profile / EPP (`power-saver`/`balanced`/`performance`/custom). Defaults to `performance`. This is the **power** lever now (not cooling); surface it as an optional "Power mode" control, separate from Cooling. |

## Feature → UI mapping, by priority

### 🔴 Obligatorias (sin esto el plugin no tiene sentido)

1. **Cooling control** — one selector `Silent / Balanced / Aggressive` +
   an **Auto** toggle.
   - Read level from the **`FanCurve`** property, mode from **`AutoCooling`**.
   - Set a level → `SetCoolingLevel(level)` (switches to manual).
   - Auto → `SetFanAuto()`.
   - ⚠️ Label it clearly as **fans only** (noise ↔ temperature). Cooling no
     longer changes power, so drop any "Silent caps power / Aggressive
     unlocks the TDP" copy. Power is the TDP slider (#2); the optional
     power-profile lever is separate (#12).
2. **TDP control** — a slider in watts.
   - Read **`CurrentSpl`**; range = `GetHardwareLimits()` (`spl_min..spl_max`).
   - Set → `SetSpl(watts)`.
3. **Live telemetry** — power, temps, fans.
   - `GetThermalStatus()` polled at ~1 Hz **while the panel is open**.
   - Show **actual power vs the TDP cap** (`soc_power_mw / 1000` W next to
     `CurrentSpl`), CPU/GPU °C, CPU/GPU RPM. Render `i32::MIN` as "n/a".
4. **Battery charge cap** — read **`ChargeEndThreshold`**, set
   `SetChargeThreshold(%)`. The single biggest lever for battery longevity.

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
    afterward to clear the banner. **Full implementation brief:**
    [`POLKIT-SETUP-PROMPT.md`](POLKIT-SETUP-PROMPT.md).

### 🟢 Opcionales / avanzadas

12. **Power mode (platform profile)** — `SetProfile` + `ActiveProfile`
    (`Performance` / `Balanced` / `Eco`). This is the EPP power lever,
    decoupled from cooling and defaulting to `Performance`. Surface it as
    an optional "Power mode" control (separate from Cooling), or hide it —
    most users leave it on Performance so their TDP is fully usable.
13. **Raw curve** — `SetFanCurve` (set a fan-curve preset directly;
    `SetCoolingLevel` is the normal path). "Advanced" caveat.
14. **Power-mode hint** — if, under load, `soc_power_mw` stays well below
    the `CurrentSpl` cap **and** `ActiveProfile` is `power-saver`, surface
    a gentle hint: "Eco power mode is limiting the chip below your TDP;
    switch to Performance for the full TDP." (Cooling level no longer
    limits power — only the platform profile does.)
15. **Reassurance copy** — temp/RPM "normal vs. worry" tooltips (e.g. high
    temps under a heavy game with fans maxed are normal; temperature
    tracks your **TDP** now, and cooling just trades fan noise for a few
    degrees). Source the wording from [`../MANUAL.md`](../MANUAL.md) →
    "What's normal vs. what to worry about".

## Update strategy

- **Properties (5):** event-driven via `PropertiesChanged`. No polling.
- **`GetThermalStatus`:** poll **~1 Hz only while the panel is visible**;
  stop when hidden/closed (battery + perf). It has no change signal.
- **`GetFanCurve`:** on-demand (open the curve view / after a cooling
  change). Static between changes.

## Conditional capability surfacing (graceful degradation)

The plugin should not assume every reading exists:

| Condition | Behaviour |
|---|---|
| `GetThermalStatus` field == `i32::MIN` | render "n/a" (sensor/fan absent). |
| `GetFanCurve` returns empty vectors | hide the curve graph (firmware-only / no programmable curve). |
| `GetThermalStatus` `gpu_*` == `i32::MIN` | single-fan / no discrete GPU sensor → show CPU only. |
| `ChargeEndThreshold` unreadable | hide the battery-cap control. |

## ⚠️ Limitations in v2.0.0 (so you don't design around them)

- **No custom (hand-drawn) curve push.** `SetCoolingLevel` and
  `SetFanCurve` take a **preset name** only (`silent`/`balanced`/
  `aggressive`). There is **no** D-Bus method that accepts arbitrary
  16-point curves. So the plugin can **select presets** and **draw the
  active curve** (`GetFanCurve`), but **cannot** offer a curve editor. A
  future daemon method (e.g. `SetCustomFanCurve(a(uu) cpu, a(uu) gpu)`)
  would be needed — file a request if the editor is wanted.
- **Telemetry is poll-only** (`GetThermalStatus`, `GetFanCurve`) — no
  signals; poll while visible.
- **Units / sentinels:** TDP & temps are whole units; **power is
  milliwatts**; `i32::MIN` is the "unavailable" sentinel in
  `GetThermalStatus`.

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
