<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# hpd — User Manual

Everything `hpd` does today, in one place, explained for everyday users.
A Spanish version is at [`MANUAL-es.md`](MANUAL-es.md). Prefer diagrams?
A visual walkthrough is at [`DIAGRAMS.md`](DIAGRAMS.md) (English) /
[`DIAGRAMS-es.md`](DIAGRAMS-es.md) (Spanish).

- [What hpd is](#what-hpd-is)
- [The two knobs: Power and Cooling](#the-two-knobs)
- [Command reference](#command-reference)
- [What every combination does](#what-every-combination-does)
- [How cooling gets assigned](#how-cooling-gets-assigned-you-never-set-the-profile-directly)
- [Recommended setups](#recommended-setups)
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

How hard the **fans** work — purely a trade between **noise and
temperature**. Cooling is **independent of power**: it does *not* change
how many watts the chip uses (that is the TDP knob, above). One lever,
three levels:

| Level | Fans | Effect |
|---|---|---|
| `silent` | quiet | warmer, near-silent |
| `balanced` *(default)* | moderate | the usual middle ground |
| `aggressive` | strong | coolest, loudest |

```
hpdctl cool set silent|balanced|aggressive   # pick a fan level (manual)
hpdctl cool auto       # let hpd pick the fan curve from your TDP
hpdctl cool reset      # hand the fans back to the firmware
hpdctl cool get        # show the current level and mode
hpdctl cool curve      # draw the active fan curve
```

**Auto vs manual:**
- **Auto** (default): hpd picks the **fan curve** from your TDP. Low TDP →
  quiet curve; high TDP → aggressive curve. The fans match how much heat
  you're likely to make — without ever touching your power.
- **Manual**: you pin a fan level and it stays, whatever the TDP does.

> **This changed in the power↔cooling decouple.** Cooling used to also
> clamp real power (a low level secretly limited the chip). It doesn't any
> more — `tdp set` is the only power lever now. See
> [the decouple explainer](COOLING-es.md) for the full story.

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
| `hpdctl power set <mode>` / `power get` | Power mode (advanced): `performance` / `balanced` / `eco` |
| `hpdctl charge set <%>` / `charge get` | Battery charge cap |

Reading commands need no password. Changing things needs no password if
you are the device owner (in the `wheel` group) — even over SSH; other
users are asked to authenticate.

## What every combination does

The two knobs are now **independent** — power is power, cooling is fans.
Any combination is valid:

| You do | In **auto** cooling | In **manual** cooling |
|---|---|---|
| `tdp set` low | hpd picks a quiet fan curve | the power applies; your pinned fan level stays |
| `tdp set` high | hpd picks an aggressive fan curve | the power applies in full; your pinned fan level stays |
| `cool set <level>` | switches to manual at that fan level | sets that fan level |
| `cool auto` | (already auto) | back to auto |

**No contradictory combinations any more.** *Manual `silent` + a high
TDP* used to be a trap (the low level clamped the power). Now it means
exactly what it says: **"use the full TDP, but keep the fans quiet."** The
watts land in full; the chip just runs warmer because it's working hard
with soft fans. That's your call, and hpd respects it.

## How cooling and power relate (they're decoupled)

Cooling and power are two separate levers:

- **Power** = the TDP/SPL you set, plus the ACPI **platform profile**
  (EPP). The profile defaults to `performance` so your TDP is the real,
  usable ceiling — it is **not** derived from anything you do with cooling.
- **Cooling** = the **fan curve** only (noise ↔ temperature).

The daemon writes the **fan curve** at these moments:

| When | What the daemon sets |
|---|---|
| You run `cool set <level>` | The fan curve → that level (and switches to manual). Power untouched. |
| You run `cool auto` | Switches to auto; the curve is then derived from the current TDP. |
| In **auto**, you change the TDP (`tdp set` / `preset`) | The fan curve is re-derived from where the TDP sits in your hardware range — `< 33 %` → silent, `33–67 %` → balanced, `> 67 %` → aggressive. The platform profile is **not** touched. |
| Resume from suspend | The active fan curve (and the profile) are re-applied (the firmware can drop them across sleep). |
| Plug in AC | The TDP ramps up; in auto, the fan curve follows it. |
| Boot | The platform profile is set to the configured default (`performance`); your last saved fan curve is restored. |

The **platform profile** is the power/EPP lever. It defaults to
`performance` (so your TDP is fully usable) and is never inferred from
cooling or TDP. Power users can change it in `/etc/hpd/config.toml`
(`default_platform_profile = "balanced"` / `"power-saver"`) or live over
D-Bus (`set_profile`) for an efficiency bias — 99 % of users leave it
alone.

## Recommended setups

### `tdp set` vs `preset` — when to use which

- **`preset eco|balanced|max`** — quick. Picks the **min / middle / max**
  watts of your hardware range. Use it when you just want "low / medium /
  high" without thinking in watts. In auto cooling these land exactly on
  silent / balanced / aggressive.
- **`tdp set <watts>`** — precise. Set an exact wattage when you have a
  specific budget in mind (e.g. `tdp set 12` for a long-battery target).

### Recommended combinations

| Goal | Setup | Result |
|---|---|---|
| **Just works (recommended)** | `cool auto` + `preset balanced` (or leave the defaults) | The fan curve always matches your power; nothing to babysit. |
| **Max performance** (docked / plugged in) | `preset max` (or `tdp set <high>`) + `cool set aggressive` | Full power, fans maxed — the coolest the chip can be at full tilt. Loud. |
| **Quiet & long battery** (reading, video, light emulation) | `preset eco` + `cool set silent` | Low power (so cool and long battery) with near-silent fans. |
| **Full power but quiet** | `tdp set <high>` + `cool set silent` | The watts land in full; fans stay soft, so the chip runs warmer. Now a valid choice. |
| **Everyday balanced** | `cool auto` (the default) | The daemon picks the fan curve from your TDP. |

### The "perfect config" checklist

1. **Battery:** run `hpdctl charge set 80` once — the single biggest thing
   for long-term battery health.
2. Use **`tdp set`** (or `preset`) as your one **power** lever — the value
   you set is the real limit now.
3. **Leave `cool auto`** unless you want to pin the fans louder or quieter.
4. Glance at `hpdctl status`: the **Power** line shows the actual draw next
   to your cap, so you can tell whether you are power-limited.
5. Want **full power with quiet fans** (or vice-versa)? Go ahead — the two
   knobs are independent now, so no combination is "wrong".

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
  under a heavy GPU game, that game just isn't using all the budget — the
  cooling level no longer limits power. (If you set a `power-saver`
  platform profile, that *would* hold it below your TDP — but the default
  `performance` does not.)
- **Cooling `(auto)`** = the fan curve follows your TDP. `(manual)` = you
  pinned a fan level.
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
| 90–100 °C | Hot but **within spec** — these APUs are rated to ~100 °C. Normal at a high TDP under a heavy game. If the fans are working hard, the cooling is doing its job. |
| Sustained 100 °C with stutter | The chip is *thermal-throttling* to protect itself — it is not being damaged, but you are losing performance. Lower the **TDP** (less heat) and/or raise the **cooling** level (more airflow), or check for dust / blocked vents. |

Temperature now tracks your **TDP** (how hard the chip works), and the
**cooling** level trades fan noise for a few degrees at that power:

✅ **Normal:** ~78 °C at a 40 W game with `aggressive` fans (measured on
the Xbox Ally X). Want it cooler? **Lower the TDP.** Want it quieter?
**Lower the cooling level** (it'll run a touch warmer).

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

- **Plug in AC:** hpd ramps the power up (and, in auto cooling, the fan
  curve follows it), then restores your battery setting when you unplug.
- **Resume from suspend:** hpd re-applies your power, platform profile,
  charge limit and fan curve — fixing the bug where fans could blast at
  full speed after waking.
- **Reboot:** your last settings are restored from disk.

## For developers: the Decky / D-Bus surface

D-Bus interface `dev.cirodev.hpd.PowerDaemon1`. **The CLI has no `fan`
namespace anymore — cooling is one concept (`cool`).** A UI should mirror
that: one cooling control, not three.

| Method / property | Use |
|---|---|
| `SetCoolingLevel(s)` | The cooling control: `silent`/`balanced`/`aggressive` → **fan curve only** (power untouched). |
| `SetFanAuto()` | Fan curve follows TDP (auto mode). |
| `ResetFanCurve()` | Fans back to firmware. |
| `GetThermalStatus() → (i,i,i,i)` | Live `(cpu_temp, gpu_temp, cpu_rpm, gpu_rpm)`; `i32::MIN` = sensor absent. |
| `GetFanCurve() → (a(uu), a(uu))` | The 8 `(temp,pwm)` points of CPU & GPU curves, to draw the graph. |
| `fan_curve` (prop) | Active level: `silent`/`balanced`/`aggressive`/`custom`/`auto`. |
| `auto_cooling` (prop) | `true` = auto, `false` = manual. |
| `current_spl`, `active_profile`, `charge_end_threshold`, `is_ac_connected` | Power / profile / battery / AC state. |
| `SetSpl(u)`, `SetPreset(s)`, `SetChargeThreshold(y)` | Power and battery setters. |
| `SetProfile(s)` | The power-profile lever (`performance`/`balanced`/`power-saver`), decoupled from cooling. |
| `SetFanCurve(s)` | Raw fan-curve set (advanced). |

**Suggested UI (post-decouple):**
- A **TDP** slider — *this is the power control* (`current_spl` / `SetSpl`,
  range from `GetHardwareLimits`).
- One **Cooling** selector: Silent / Balanced / Aggressive + an **Auto**
  toggle, labelled as a **fans-only** noise↔temperature control (not
  power). (Level from `fan_curve`, mode from `auto_cooling`.)
- **Optional advanced "Power mode"** (`active_profile` / `SetProfile`):
  Performance / Balanced / Eco, clearly separate from Cooling. Default
  Performance; safe to hide for most users.
- **Live readouts** from `GetThermalStatus` (temps + RPM) and an optional
  curve graph from `GetFanCurve`.
- A **Battery cap** control (`charge_end_threshold` / `SetChargeThreshold`).
- **AC indicator:** subscribe to the `AcConnected` property (emits
  `PropertiesChanged`; daemon ≥ 2.4.0) — or poll `is_ac_connected()` on
  older daemons. The `AC0`-node fix makes the value correct on the Xbox
  Ally X.

For the thermal rationale and the data behind all this, see
[`fan-curves.md`](fan-curves.md).
