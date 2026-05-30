<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# hpd — User Manual

Everything `hpd` does today, in one place, explained for everyday users.
A Spanish version is at [`MANUAL-es.md`](MANUAL-es.md).

- [What hpd is](#what-hpd-is)
- [The two knobs: Power and Cooling](#the-two-knobs)
- [Command reference](#command-reference)
- [What every combination does](#what-every-combination-does)
- [Reading the status dashboard](#reading-the-status-dashboard)
- [Drawing the fan curve](#drawing-the-fan-curve)
- [Battery longevity](#battery-longevity)
- [What's normal vs. what to worry about](#whats-normal-vs-what-to-worry-about)
- [Behaviour on AC, battery and resume](#behaviour-on-ac-battery-and-resume)
- [For developers: the Decky / D-Bus surface](#for-developers-the-decky--d-bus-surface)

## What hpd is

`hpd` is a background service that manages the power and cooling of a
handheld PC (currently the ASUS ROG Ally family). It lets you trade
performance, heat, noise and battery the way you want — from a quiet,
cool, long-battery setup to a loud, full-power one — and it remembers
your choice across reboots and suspend.

You drive it with the `hpdctl` command, or any app that talks to its
D-Bus interface (like a Decky plugin).

## The two knobs

Despite all the words, there are only **two things you control**:

### ⚡ Power (TDP)

How many watts the chip may draw. More watts = more performance and more
heat; fewer = cooler and longer battery.

```
hpdctl tdp set 18      # allow up to 18 W
hpdctl preset eco|balanced|max   # shortcuts: min / middle / max watts
```

### 🧊 Cooling (level + mode)

How hard the device cools. **Important:** the cooling level is not just
fan speed — it also sets how much power the chip is actually *allowed* to
use (see [the gating finding](fan-curves.md)). One lever, three levels:

| Level | Fans | Real power |
|---|---|---|
| `silent` | quiet | low (chip is held back) |
| `balanced` *(default)* | moderate | medium |
| `aggressive` | strong | full |

```
hpdctl cool set silent|balanced|aggressive   # pick a level (manual)
hpdctl cool auto       # let hpd pick the level from your TDP
hpdctl cool reset      # hand the fans back to the firmware
hpdctl cool get        # show the current level and mode
hpdctl cool curve      # draw the active fan curve
```

**Auto vs manual:**
- **Auto** (default): hpd picks the level from your TDP. Low TDP → quiet
  and cool; high TDP → full power and strong fans. Everything stays
  coherent automatically.
- **Manual**: you pin a level and it stays, whatever the TDP does.

### 🔋 Battery charge limit (a third, independent knob)

Caps how full the battery charges. Holding at 80 % instead of 100 %
greatly slows battery wear.

```
hpdctl charge set 80   # stop charging at 80 %
hpdctl charge get
```

## Command reference

| Command | What it does |
|---|---|
| `hpdctl status` | One-shot dashboard (power, cooling, temps, fans, battery, AC) |
| `hpdctl monitor` | Same dashboard, refreshed every second |
| `hpdctl limits` | The hardware's min/max watts |
| `hpdctl tdp set <W>` / `tdp get` | Set / read the power limit |
| `hpdctl preset eco\|balanced\|max` | Power shortcut (min / mid / max watts) |
| `hpdctl cool set <level>` | Set cooling level (`silent`/`balanced`/`aggressive`) |
| `hpdctl cool auto` | Cooling follows the TDP |
| `hpdctl cool reset` | Fans back to firmware control |
| `hpdctl cool get` | Show cooling level + mode |
| `hpdctl cool curve` | Draw the active fan curve |
| `hpdctl charge set <%>` / `charge get` | Battery charge cap |

Reading commands need no password. Changing things needs no password if
you are the device owner (in the `wheel` group) — even over SSH; other
users are asked to authenticate.

## What every combination does

The two knobs interact. Here is the full picture:

| You do | In **auto** cooling | In **manual** cooling |
|---|---|---|
| `tdp set` low | hp lowers the cooling level (quiet, cool) | TDP applies within your fixed level |
| `tdp set` high | hpd raises the cooling level (full power, strong fans) | **only fully applies if your level is `aggressive`** |
| `cool set <level>` | switches to manual at that level | sets that level |
| `cool auto` | (already auto) | back to auto |

**The one combination to know about:** *manual `silent` + a high TDP.*
This is contradictory — "limit my power to stay quiet" and "give me lots
of power" at the same time. The cooling level wins: the chip stays
clamped low and the high TDP simply does not take effect (it is stored
but inert). If you want a high TDP to actually work, use `cool auto` or
`cool set aggressive`. **In auto mode this never happens**, because the
level always matches the TDP.

## Reading the status dashboard

```
   ⚡ Power:            16W now · 18W TDP cap   ← actual draw · your limit
   🧊 Cooling:          balanced (auto)         ← level + mode
   🌡️ Temps:            CPU 68°C · GPU 58°C
   💨 Fans:             CPU 5300 RPM · GPU 5300 RPM
   🔌 Power adapter:    🔋 Battery (DC)
   🔋 Battery Limit:    80%
```

- **Power** = the watts the chip is drawing *right now*, next to the TDP
  cap you set. At idle it sits low; under load it climbs toward (and can
  briefly exceed, via boost) the cap. If "now" stays well below the cap
  under heavy load, something else is limiting you (e.g. a low cooling
  level — see [combinations](#what-every-combination-does)).
- **Cooling `(auto)`** = the level follows your TDP. `(manual)` = you
  pinned it.
- **Temps / Fans** are live readings, straight from the hardware.

## Drawing the fan curve

`hpdctl cool curve` shows the actual temperature→speed curve the chip is
running, as bars:

```
🌀 Fan curve: aggressive
  CPU fan  (temp → speed):
     40°C │██                      │  10%
     54°C │██████                  │  25%
     62°C │█████████               │  40%
     ...
     91°C │████████████████████████│ 100%
```

Read it left to right: as the chip gets hotter, the fan ramps up. A
higher level shifts the whole curve up (more speed at every temperature).

## Battery longevity

The single most effective thing you can do for long-term battery health
is **cap the charge** (`hpdctl charge set 80`). A lithium battery held at
80 % ages far slower than one kept at 100 %. This matters much more than
temperature or fan settings. Set it once; it persists.

## What's normal vs. what to worry about

This is the part people get nervous about. Short version: **modern AMD
handhelds are built to run hot, and loud fans mean the cooling is
working, not failing.**

### Temperature

| Reading | Meaning |
|---|---|
| 40–70 °C | Cool. Idle or light use. |
| 70–90 °C | Warm. Totally normal under load. |
| 90–100 °C | Hot but **within spec** — these APUs are rated to ~100 °C. Normal at full power (`aggressive`). If the fans are working hard, the cooling is doing its job. |
| Sustained 100 °C with stutter | The chip is *thermal-throttling* to protect itself — it is not being damaged, but you are losing performance. Lower the cooling level or TDP, or check for dust / blocked vents. |

✅ **Normal:** 95 °C in `aggressive` with the fans maxed — that is just
the chip at full power. Pick `balanced` (≈68 °C) or `silent` (≈58 °C) if
you want it cooler.

⚠️ **Worth attention:** a high temperature (85 °C+) while the fans stay
**slow** — that means the fans are *not* ramping (a stuck fan, or the old
firmware-conservative behaviour). With hpd's curves this should not
happen; if it does, run `hpdctl cool curve` and `hpdctl status` and check
the fan RPM is rising with temperature.

### Fan RPM

The Ally X fans top out around **8000–8100 RPM**. Running there under a
heavy game is normal and the fans are rated for it — **loud is not
broken.** Worry only about: a fan reading **0 RPM under load** (failure),
or a rattling/grinding noise (physical issue).

### Does any of this shorten the console's life?

- **Heat:** staying within spec (under ~100 °C) during normal use does
  not meaningfully shorten the chip's life. The APU is designed for it.
- **Fans:** running at max when needed is what they are for; it does not
  wear them out prematurely.
- **Battery:** the real longevity lever is the **charge cap**, not
  cooling. Cap at 80 % and you are doing the important thing.
- **Safety:** the fan curves are run by the embedded controller, so even
  if `hpd` crashes the fans keep following the last curve — they never
  stop or freeze.

## Behaviour on AC, battery and resume

- **Plug in AC:** hpd ramps the power up (and, in auto cooling, the
  cooling level with it), then restores your battery setting when you
  unplug.
- **Resume from suspend:** hpd re-applies your power, cooling profile,
  charge limit and fan curve — fixing the bug where fans could blast at
  full speed after waking.
- **Reboot:** your last settings are restored from disk.

## For developers: the Decky / D-Bus surface

D-Bus interface `dev.cirodev.hpd.PowerDaemon1`. **The CLI has no `fan`
namespace anymore — cooling is one concept (`cool`).** A UI should mirror
that: one cooling control, not three.

| Method / property | Use |
|---|---|
| `SetCoolingLevel(s)` | The main cooling control: `silent`/`balanced`/`aggressive` → profile + curve together. |
| `SetFanAuto()` | Cooling follows TDP (auto mode). |
| `ResetFanCurve()` | Fans back to firmware. |
| `GetThermalStatus() → (i,i,i,i)` | Live `(cpu_temp, gpu_temp, cpu_rpm, gpu_rpm)`; `i32::MIN` = sensor absent. |
| `GetFanCurve() → (a(uu), a(uu))` | The 8 `(temp,pwm)` points of CPU & GPU curves, to draw the graph. |
| `fan_curve` (prop) | Active level: `silent`/`balanced`/`aggressive`/`custom`/`auto`. |
| `auto_cooling` (prop) | `true` = auto, `false` = manual. |
| `current_spl`, `active_profile`, `charge_end_threshold`, `is_ac_connected` | Power / profile / battery / AC state. |
| `SetSpl(u)`, `SetPreset(s)`, `SetChargeThreshold(y)` | Power and battery setters. |
| `SetProfile(s)`, `SetFanCurve(s)` | Raw, decoupled controls (advanced; only meaningful with `fan_curve_follows_profile = false`). |

**Suggested UI:**
- One **Cooling** selector: Silent / Balanced / Aggressive + an **Auto**
  toggle. (Level from `fan_curve`, mode from `auto_cooling`.)
- A **TDP** slider (`current_spl` / `SetSpl`, range from `GetHardwareLimits`).
- **Live readouts** from `GetThermalStatus` (temps + RPM) and an optional
  curve graph from `GetFanCurve`.
- A **Battery cap** control (`charge_end_threshold` / `SetChargeThreshold`).
- Show a gentle note if the user pins a low cooling level while asking
  for a high TDP (see "the one combination to know about").

For the thermal rationale and the data behind all this, see
[`fan-curves.md`](fan-curves.md).
