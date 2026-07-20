<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# hpd — User Manual

Everything `hpd` does today, in one place, explained for everyday users.
A Spanish version is at [`MANUAL-es.md`](MANUAL-es.md). Prefer diagrams?
A visual walkthrough is at [`DIAGRAMS.md`](DIAGRAMS.md) (English) /
[`DIAGRAMS-es.md`](DIAGRAMS-es.md) (Spanish).

- [What hpd is](#what-hpd-is)
- [The two knobs: Power and Cooling](#the-two-knobs)
- [GPU clock range (advanced, optional)](#gpu-clock-range-advanced-optional)
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
hpdctl preset efficiency         # sweet spot: best battery while gaming
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

## GPU clock range (advanced, optional)

A **fourth knob**, entirely separate from the three above — and the one
exception in this manual to "hpd manages it from the moment it's
installed." The GPU clock range (`min_mhz`–`max_mhz`, the same kind of
lever as AMD's own Adrenalin "Minimum/Maximum Frequency" on Windows) lets
you shape the GPU's frequency ceiling on top of whatever TDP and cooling
already do.

**Why you'd use it:** TDP already limits total chip power, and cooling
already limits temperature — GPU clock range is a third, more surgical
tool for the case those two don't cover well: an efficiency ceiling that
scales with how much TDP you've actually given the chip, so a low-TDP
session doesn't waste headroom letting the GPU chase clocks it can't
sustain anyway.

```
hpdctl gpu auto             # follow TDP (mirrors `cool auto`)
hpdctl gpu reset            # hand the GPU clock back to firmware, fully un-manages it
hpdctl gpu get              # show the current mode + committed range
hpdctl gpu limits           # show this device's supported MHz range (read-only)
```

**Auto or off — there is no manual pin:**
- **`gpu auto`**: hpd infers a clock ceiling from your current TDP, using
  the same silent/balanced/aggressive tier already computed for the fan
  curve — a low TDP infers a lower ceiling, a high TDP infers a ceiling
  near the device's full range. This re-derives on every TDP change, so
  it's never a stale value left over from an earlier session.
- **`gpu reset`**: hand the clock back to firmware automatic control —
  and, unlike `cool reset` (which returns the *fan curve* to one
  firmware-managed mode among several the daemon still tracks), this
  returns GPU clock all the way to "hpd isn't managing this at all,"
  the same state a fresh install starts in.

There used to be a third command, `gpu set <min> <max>`, pinning an
explicit MHz range that ignored TDP entirely. It was removed in 3.0.0:
on-device use found it was the one control in the whole daemon a user
could set once and forget, silently capping GPU performance below what
their TDP/cooling would otherwise allow with no way for hpd to warn
about it — unlike a low TDP or a Silent fan curve, a low pinned MHz
range has no legitimate everyday use case that `gpu auto` doesn't
already cover. If you're looking for `gpu set` after upgrading, this is
why it's gone; there is no replacement flag.

> **Optional and opt-in, forever — not just at first boot.** hpd never
> touches the GPU clock at all — reads it, writes it, nothing — until you
> run `gpu auto` yourself, at least once. This is different from cooling:
> cooling's real steady state is never "off" (the daemon is always
> driving *some* fan curve), but GPU clock's steady state genuinely is
> "untouched" by default. Nothing else in hpd ever turns this on for you
> — not `restore-defaults`, not plugging in AC, not a fresh install or
> upgrade. Once you *have* opted in, `restore-defaults` and unplugging AC
> hand the GPU clock back to firmware auto along with everything else,
> exactly like they do for cooling — they just never flip the very first
> switch for you.

**What's normal:** `hpdctl gpu get` reports "firmware auto (not managed)"
by default on every fresh install, and again any time after `gpu reset` —
that's the expected, permanent starting state, not a bug. Once you opt
in, `hpdctl gpu limits` shows this device's real supported range, read
live from the kernel every time (on the ROG Xbox Ally X this is about
**600–2900 MHz**) — it is never a hardcoded guess, so it is correct
whatever handheld you're running hpd on.

## Command reference

| Command | What it does |
|---|---|
| `hpdctl status` | One-shot dashboard (power, cooling, temps, fans, battery, AC) |
| `hpdctl monitor` | Same dashboard, refreshed every second |
| `hpdctl limits` | The hardware's min/max watts |
| `hpdctl tdp set <W>` / `tdp get` | Set / read the power limit |
| `hpdctl preset eco\|efficiency\|balanced\|max` | Power shortcut (min / sweet-spot / mid / max watts) |
| `hpdctl cool set <level>` | Set cooling level (`silent`/`balanced`/`aggressive`) |
| `hpdctl cool auto` | Cooling follows the TDP |
| `hpdctl cool reset` | Fans back to firmware control |
| `hpdctl cool get` | Show cooling level + mode |
| `hpdctl cool curve` | Draw the active fan curve |
| `hpdctl cool set-custom <8 temp:pwm pairs>` | Set your own hand-drawn 8-point curve (advanced) |
| `hpdctl power set <mode>` / `power get` | Power mode (advanced): `performance` / `balanced` / `eco` |
| `hpdctl charge set <%>` / `charge get` | Battery charge cap |
| `hpdctl gpu auto` | GPU clock range follows the TDP (advanced, opt-in) |
| `hpdctl gpu reset` | Hand the GPU clock back to firmware, fully un-managed |
| `hpdctl gpu get` | Show the current GPU clock mode + committed range |
| `hpdctl gpu limits` | Show this device's supported GPU clock range |

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
| Plug in AC | By default (`ac_max_performance`) the device **locks to maximum performance**: Power mode → Performance, TDP → Max, cooling → Aggressive, and power/cooling changes are refused until you unplug (the charge limit stays editable). Unplugging restores your battery state. |
| Boot | The daemon re-applies your **full saved state** (TDP, power mode → configured default, charge limit, fan curve) to the hardware — so it matches the device even after a cold boot reset the firmware to its defaults. |

The **platform profile** is the power/EPP lever. It defaults to
`performance` (so your TDP is fully usable) and is never inferred from
cooling or TDP. Power users can change it in `/etc/hpd/config.toml`
(`default_platform_profile = "balanced"` / `"power-saver"`) or live over
D-Bus (`set_profile`) for an efficiency bias — 99 % of users leave it
alone.

## Recommended setups

### `tdp set` vs `preset` — when to use which

- **`preset eco|efficiency|balanced|max`** — quick. Picks a target
  wattage from your hardware range without thinking in watts:
  **eco** = minimum, **efficiency** = the battery-efficient gaming sweet
  spot (lower third of the range — most of the GPU performance at a
  fraction of the power and battery), **balanced** = midpoint,
  **max** = maximum. In auto cooling each lands on the matching fan curve.
- **`tdp set <watts>`** — precise. Set an exact wattage when you have a
  specific budget in mind (e.g. `tdp set 12` for a long-battery target).

### Recommended combinations

| Goal | Setup | Result |
|---|---|---|
| **Just works (recommended)** | `cool auto` + `preset balanced` (or leave the defaults) | The fan curve always matches your power; nothing to babysit. |
| **Best battery while gaming** | `preset efficiency` (leave `cool auto`) | The sweet spot — keeps most of the GPU performance while stretching battery. Ideal for handheld play unplugged. |
| **Max performance** (docked / plugged in) | `preset max` (or `tdp set <high>`) + `cool set aggressive` | Full power, fans maxed — the coolest the chip can be at full tilt. Loud. |
| **Quiet & long battery** (reading, video, light emulation) | `preset eco` + `cool set silent` | Low power (so cool and long battery) with near-silent fans. |
| **Full power but quiet** | `tdp set <high>` + `cool set silent` | The watts land in full; fans stay soft, so the chip runs warmer. Now a valid choice. |
| **Everyday balanced** | `cool auto` (the default) | The daemon picks the fan curve from your TDP. |

### Where the watts actually go (measured)

Numbers from a controlled benchmark campaign on a ROG Xbox Ally X
(sustained loads, battery, defaults — other devices will differ in
absolutes but the shape holds):

- **The GPU stops gaining at ~21 W.** Above that, extra watts feed the
  CPU only — a GPU-bound game gains ~1% from 21 W → 35 W.
- **The gaming sweet spot is 13-16 W**: ~82-91% of the maximum GPU
  performance at a fraction of the power (and heat, and noise).
- **The middle preset (~21 W) is an excellent default**: ~83% of
  everything (CPU, GPU, memory bandwidth all measured), with memory
  bandwidth fully saturated by that point.
- **The maximum is a burst tier, not an everyday setting**: the last
  7 W (28 → 35) buy ~5% CPU and ~1% GPU while adding ~21% power and
  pushing sustained CPU temperature to ~86 °C.
- **Your TDP setting costs nothing at idle** (~7-8 W whole-system draw
  regardless of the cap) — no need to lower it for reading or video;
  lower it for *load* scenarios where you want the battery to last.
- **The power *mode* is not free**: at an identical TDP, `power set
  power-saver` measured ~12% slower than `performance`. It is a battery
  lever, not a free setting.
- **Plugged in vs. battery: identical performance** at the same TDP —
  the battery does not limit this device even below 20% charge.

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

- **Plug in AC — locked to maximum performance.** By default, plugging in
  the charger pins the device to its ceiling: **Power mode → Performance,
  TDP → Max, cooling → Aggressive**, and **locks** those controls so nothing
  can change them while you're on wall power (the CLI and plugin will refuse
  power/cooling changes with a "locked on AC" message). Your **battery
  charge limit stays adjustable** — it's the one setting that still makes
  sense to change while plugged in. When you **unplug**, your exact battery
  (DC) settings — TDP, Power mode and cooling — are restored.
  - **Want to turn the lock off?** Run **`hpdctl ac-lock off`** (or flip the
    "Lock to max on AC" toggle in the plugin's Settings). With it off, **AC
    is fully manual** — plugging in changes nothing and every control stays
    editable. Turn it back on with `hpdctl ac-lock on`. The setting persists
    across reboots (no config file edit needed). `hpdctl ac-lock` with no
    argument prints the current state.
  - **Installed (or first booted) while plugged in?** The daemon starts
    locked at maximum performance, exactly as if you'd just plugged in. The
    **first time you unplug**, since it never recorded a battery preference
    yet, it lands on quiet defaults — **Balanced TDP with auto-cooling** (so
    the fans calm down) — instead of staying on the loud Aggressive curve.
    After that first unplug your settings are remembered normally.
- **Resume from suspend:** hpd re-applies your power, platform profile,
  charge limit and fan curve — fixing the bug where fans could blast at
  full speed after waking. It also **checks the real charger state on
  wake** (in case you plugged or unplugged while it was asleep): resume on
  AC re-asserts the maximum-performance lock; resume on battery brings back
  your battery settings.
- **Plugged/unplugged while off or asleep:** the same applies on a cold
  boot. If you shut down plugged in and boot up on battery, hpd comes back
  to your **battery** settings (not stuck at max); boot up plugged in and it
  starts locked at max. The charger state the device actually has wins.
- **Reboot:** the daemon re-applies your full saved state (TDP, power
  mode, charge limit, fan curve) to the hardware on startup, so what it
  reports always matches the device — even if a cold boot reset the
  firmware's defaults underneath it.

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
| `GetTelemetry() → a{sv}` | Extended telemetry (daemon ≥ 2.8.0): battery power/percent/status/health/cycles, CPU/GPU clocks, GPU busy %, VRAM. A key is present only if the hardware exposes it. |
| `GetFanCurve() → (a(uu), a(uu))` | The 8 `(temp,pwm)` points of CPU & GPU curves, to draw the graph. |
| `SetFanCurve(a(yy), a(yy))` | Custom-curve editor backend (daemon ≥ 2.9.0): exactly 8 `(temp_c, pwm)` points per fan; latches manual cooling like `SetCoolingLevel`. |
| `GetFanCurveConstraints() → a{sv}` | This device's curve limits + safety floor (daemon ≥ 2.9.0): `temp_min_c`/`temp_max_c`, `pwm_min`/`pwm_max`, `safety_floor`. Drive the editor's axes from this, never hardcode. |
| `GetVersion() → (s)` | The daemon's version string (daemon ≥ 2.4.2; older daemons error → "unknown"). |
| `fan_curve` (prop) | Active level: `silent`/`balanced`/`aggressive`/`custom`/`auto`. |
| `auto_cooling` (prop) | `true` = auto, `false` = manual. |
| `current_spl`, `active_profile`, `charge_end_threshold`, `is_ac_connected` | Power / profile / battery / AC state. |
| `SetSpl(u)`, `SetPreset(s)`, `SetChargeThreshold(y)` | Power and battery setters. |
| `SetProfile(s)` | The power-profile lever (`performance`/`balanced`/`power-saver`), decoupled from cooling. |
| `SetAcMaxPerformance(b)` | Toggle the "lock to max on AC" preference (daemon ≥ 2.7.0). |
| `ac_locked` (prop) | Live: power/cooling controls are locked because AC is plugged in and the lock preference is on (daemon ≥ 2.7.0). |
| `ac_max_performance` (prop) | The toggleable "lock to max on AC" preference itself (daemon ≥ 2.7.0), vs. `ac_locked` (the live state). |
| `EnableGpuAutoFollow()` | Re-enable GPU-clock auto-follow of TDP (daemon ≥ 2.12.0) — the opt-in the whole feature is gated behind. There is no method to pin an arbitrary range; `SetGpuClockRange` existed through daemon 2.x and was removed in 3.0.0. |
| `ResetGpuClocks()` | Hand the GPU clock back to firmware auto (daemon ≥ 2.12.0). |
| `GetGpuClockConstraints() → a{sv}` | This device's live supported GPU clock range (`range_min_mhz`/`range_max_mhz`, daemon ≥ 2.12.0). Empty map if not programmable. |
| `GetGpuClockRange() → (u, u)` | The GPU clock range actually committed to hardware (daemon ≥ 2.12.0); `(0, 0)` = not applicable (firmware auto / no programmable range). |
| `gpu_clock_range` (prop) | Active GPU-clock selection: `silent`/`balanced`/`aggressive`/`auto` (daemon ≥ 2.12.0), mirrors `fan_curve`. `unknown` is the rare rollback case, never a settable state. |
| `gpu_follows_tdp` (prop) | `true` = GPU clock follows TDP, `false` = manual or unmanaged (daemon ≥ 2.12.0), mirrors `auto_cooling`. |

**Suggested UI (post-decouple):**
- A **TDP** slider — *this is the power control* (`current_spl` / `SetSpl`,
  range from `GetHardwareLimits`).
- One **Cooling** selector: Silent / Balanced / Aggressive + an **Auto**
  toggle, labelled as a **fans-only** noise↔temperature control (not
  power). (Level from `fan_curve`, mode from `auto_cooling`.)
- A first-class **"Power mode"** control (`active_profile` / `SetProfile`):
  Performance / Balanced / Eco, in the Power section, clearly separate from
  Cooling. Default Performance; show an informative note when Balanced/Eco
  hold power below the TDP (don't disable the slider — the real ceiling is
  workload-dependent).
- **Live readouts** from `GetThermalStatus` (temps + RPM) and an optional
  curve graph from `GetFanCurve`.
- A **Battery cap** control (`charge_end_threshold` / `SetChargeThreshold`).
- **AC indicator:** subscribe to the `AcConnected` property (emits
  `PropertiesChanged`; daemon ≥ 2.4.0) — or poll `is_ac_connected()` on
  older daemons. The `AC0`-node fix makes the value correct on the Xbox
  Ally X.
- An **optional, advanced GPU clock range** control (`gpu_clock_range` /
  `EnableGpuAutoFollow` / `ResetGpuClocks`, daemon ≥ 2.12.0) — hidden
  entirely when `GetGpuClockConstraints()` returns an empty map (no
  programmable range on this device, or an older daemon). Auto/Reset
  only, no manual-range control; never default it on, the daemon itself
  never auto-opts a user in (see the GPU clock section above).

For the thermal rationale and the data behind all this, see
[`fan-curves.md`](fan-curves.md).
