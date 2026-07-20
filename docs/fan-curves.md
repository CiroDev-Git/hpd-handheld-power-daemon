<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Custom fan curves — why the ROG Xbox Ally X runs hot on Linux, and how `hpd` fixes it

This document records the investigation behind `hpd`'s custom fan-curve
support: what made the device run conservatively (hot screen and back
panel even in light games) under CachyOS, and the strategy we chose to
cool it down to roughly Armoury-Crate-on-Windows behaviour while keeping
the hardware safe.

It is written from a concrete on-device capture of a **ROG Xbox Ally X
(board `RC73XA`)** running **CachyOS, kernel `7.0.10-4-cachyos-deckify`**.
The exact PWM↔RPM mapping is hardware-specific, so the preset values are
a calibrated *starting point*, not a final tune — see
[Calibration caveats](#calibration-caveats).

## TL;DR

- Linux exposes the fans through the kernel `asus_custom_fan_curve`
  hwmon, but **nothing was driving it** — `hpd` (like the bare kernel)
  only set the ACPI `platform_profile`, leaving the actual fan curve to
  the firmware.
- The firmware's default curve is **only defined up to ~62 °C and tops
  out around 22 % duty**. Above 62 °C the embedded controller (EC)
  coasts conservatively. We measured the CPU at **87 °C** with the fans
  still only at ~6400 RPM.
- The fix is to write our own EC-mediated curve that **extends a
  monotonic ramp out to ~92 °C** and raises the duty in every
  temperature band, with three presets (`silent` / `balanced` /
  `aggressive`).
- It is done safely: we hand the EC *auto-points*, never raw PWM, so the
  fans keep following the last curve even if `hpd` dies; and every write
  is read back and fails closed if the EC rejected it.
- The presets were later **retuned cooling-first from in-game telemetry**
  and the cooling level was **decoupled from power** (it had been secretly
  clamping the chip) — see [§5](#5-cooling-is-a-fans-only-lever-decoupled-from-power)
  and [§6](#6-decoupling-power-from-cooling).

## What the cooling stack did before

`hpd`'s only cooling lever used to be the ACPI platform profile:

```
/sys/firmware/acpi/platform_profile      # low-power | balanced | performance
```

The daemon wrote one of those strings and the firmware/EC decided the
real fan behaviour. The hwmon `fanN_input` files were read for telemetry
only — there was **no fan write path at all**. So "fan auto" / "fan set"
in the CLI just picked a platform profile; the EC's built-in curve did
the rest.

That is fine for a quiet device, but it means the cooling aggressiveness
is entirely whatever the firmware decided for each profile — and on this
hardware the firmware strongly prioritises silence.

## The root cause (from the RC73XA capture)

The kernel `asus_custom_fan_curve` hwmon exposes an 8-point curve per fan
(`pwm1` = CPU/SoC fan, `pwm2` = GPU fan):

```
pwmN_auto_point{1..8}_temp   # °C threshold
pwmN_auto_point{1..8}_pwm    # duty 0–255
pwmN_enable                  # 1 = custom curve, 2 = firmware automatic
```

Reading back the firmware's **default `performance`-profile curve** told
the whole story:

| Point | CPU `pwm1` temp→duty | GPU `pwm2` temp→duty |
|------:|---------------------|----------------------|
| 1     | 48 °C → 2  (0.8 %)  | 48 °C → 2            |
| 2     | 54 °C → 22 (8.6 %)  | 54 °C → 22           |
| 3     | 59 °C → 45 (17.6 %) | 59 °C → 33 (13 %)    |
| 4     | **62 °C → 56 (22 %)** | **62 °C → 33 (13 %)** |
| 5–8   | 62 °C → 56 *(flat)* | 62 °C → 33 *(flat)*  |

Two things jump out:

1. **The curve is only defined up to 62 °C.** Points 5–8 are dead
   duplicates of point 4 (same `62 °C`, same duty). Above 62 °C the
   curve says nothing, so the EC falls back to its own conservative
   internal behaviour.
2. **It caps at ~22 % duty** (`pwm = 56/255`) on the CPU fan and ~13 %
   on the GPU fan.

At the moment of capture the CPU (`k10temp` Tctl) read **87 °C** — a full
**25 °C above the last defined curve point** — while the fans spun at only
~6400 RPM. That is exactly the "hot screen / hot back, fans barely
audible" complaint. Windows / Armoury Crate avoids it by shipping
aggressive curves that stay defined into the high 80s/90s °C.

So the platform profile was **not** broken (on this device the choices
are `low-power balanced performance`, and `hpd` maps them correctly). The
problem was the *fan curve itself* being undefined and timid above 62 °C.

### A second, latent bug we found along the way

The fan-RPM reader scanned `/sys/class/hwmon` by lowest index and could
latch onto the unrelated **`acpi_fan`** node, which *also* exposes a
`fan1_input`, instead of the real **`asus`** node. hwmon indices are not
stable across boots/driver-load order. The reader now resolves the node
by its `name` attribute. (The fan-curve writer locates
`asus_custom_fan_curve` the same way.)

## Mitigation strategy

### 1. Drive the curve ourselves, extended into the hot range

The core fix: write a custom curve whose points span ~45 → ~92 °C with a
monotonic ramp, instead of letting the firmware flat-line at 62 °C. Every
preset raises duty in each band and, critically, **keeps climbing past
62 °C** where the firmware gave up.

The three presets, **retuned cooling-first from in-game telemetry**
(`pwm1` CPU; `pwm2` GPU shares the same temp→duty shape and naturally
spins less because the GPU runs cooler). Each curve's eight
`temperature → duty` points:

| Preset | Points (°C → duty %) |
|---|---|
| `silent` | 50→6 · 58→11 · 65→22 · 72→37 · 78→59 · 83→78 · 88→92 · 93→100 |
| `balanced` *(default)* | 45→8 · 54→20 · 62→37 · 69→57 · 75→75 · 80→88 · 85→100 · 92→100 |
| `aggressive` | 40→18 · 48→35 · 55→53 · 62→71 · 68→86 · 74→100 · 82→100 · 90→100 |

The design principle is **reach near-max airflow early in temperature**:
the unit's fans saturate at duty ~220 (any higher buys almost no extra
RPM), so the curves climb hard through the mid range and pin to 100 % by
~74–85 °C instead of crawling to 100 % at 92 °C. Even `silent` beats the
firmware default at high temperature, because the firmware has no defined
behaviour there. `balanced` is the shipped default
(`default_fan_curve = "balanced"`), applied on first boot.

### 2. Re-apply the curve on resume from suspend

The EC can drop or reset the custom curve across a suspend/resume cycle —
this is the same mechanism behind the "fans blast at 100 % after waking"
bug. The daemon now re-applies the active curve whenever it sees the
logind `PrepareForSleep` resume signal, so the EC is never left on a
stale or maxed-out curve.

### 3. Keep it fail-safe — EC-mediated auto-points, never raw PWM

We deliberately **do not** drive raw PWM. We hand the EC the 8
auto-points and the EC runs the control loop in firmware. Consequences:

- If `hpd` crashes or is killed mid-session, the fans keep following the
  **last curve we wrote** — they do not freeze at a fixed duty or stop.
- The presets keep a non-trivial duty floor at the low end, so the fans
  never fall to the firmware's near-silent ~1 % under sustained load.
- `hpdctl cool reset` (or `ResetFanCurve` over D-Bus) writes
  `pwmN_enable = 2`, handing control cleanly back to the firmware curve.

### 4. Verify every write (fail closed)

After writing the 16 points the backend reads them back and **errors if
the EC did not store exactly what we asked for**. A silently-rejected
curve must not look like success to the daemon. The write path is also
purely additive at the capability layer: a backend without a
programmable curve simply returns `None` and the daemon treats fan-curve
effects as no-ops.

### 5. Cooling is a fans-only lever (decoupled from power)

Cooling controls **only the fan curve** — noise vs temperature. It is
independent of power (see [the decouple](#6-decoupling-power-from-cooling)):

- **`hpdctl cool set <silent|balanced|aggressive>`** sets the fan curve
  (manual cooling). It does **not** touch the platform profile / power.
- **`hpdctl cool auto`** lets the daemon infer the fan-curve preset from
  the TDP (low TDP → silent curve, high TDP → aggressive curve).
- **`hpdctl cool reset`** hands the fans back to the firmware's own curve
  (`ResetFanCurve`). *(The old `fan_curve_follows_profile` config knob was
  removed in 2.6.0; a stale line in an existing config is silently ignored.
  The unused raw `set_fan_curve` D-Bus method was retired in 2.5.0 — `cool
  set` / `cool reset` cover the curve.)*

### 6. Decoupling power from cooling

The original design coupled the cooling level to the ACPI
`platform_profile` (silent→power-saver, aggressive→performance). We
measured that the profile's EPP **clamps real power** (below), so that
coupling meant the cooling level secretly throttled the chip: a `tdp set 25`
could run at ~13 W just because auto-cooling had picked `silent`/PowerSaver.
"TDP didn't mean TDP," which was confusing.

So power and cooling are now **separate levers**:

- **TDP/SPL** (`tdp set`) is the sole power knob — the value you set is the
  real, usable ceiling.
- The **platform profile** is an independent power lever, defaulting to
  `performance` (config `default_platform_profile`, applied at boot) so it
  never clamps your SPL. `set_profile` over D-Bus changes it for advanced
  efficiency tuning.
- **Cooling** (`cool set` / auto) drives the fan curve only.

Auto-cooling (`fan_follows_tdp`) therefore now infers the *fan curve* from
the TDP (`infer_fan_curve_from_spl`), not the platform profile.

### 7. Custom (hand-drawn) curves — the 2.9.0 editor

The three presets above (§1) are a good fit for most people, but they are
still someone else's curve. Daemon 2.9.0 added a real **custom-curve
editor**: a caller supplies its own 8 `(temp_c, pwm)` points per fan
instead of picking `silent`/`balanced`/`aggressive`. This is a *new*,
safety-floor-aware setter — distinct from the old, raw preset-only
`set_fan_curve` D-Bus method retired in 2.5.0 (§5); the name is reused
because that method is long gone and this is its natural, much safer
replacement.

**CLI syntax:**

```
hpdctl cool set-custom 45:20 54:50 62:95 69:145 75:190 80:225 85:255 92:255
```

Exactly 8 `temp_c:pwm` pairs (`pwm` 0–255), coolest to hottest, applied to
*both* fans in one call (the underlying `SetFanCurve(cpu, gpu)` D-Bus
method actually takes independent CPU/GPU curves — `hpdctl cool
set-custom` just sends the same 8 points to both, which is what the CLI's
single point list is for; a direct D-Bus caller could differentiate the
two fans). Like `cool set`, it latches manual cooling. (The Decky plugin
briefly shipped a full-page editor over this exact method — removed in
plugin 2.21.0 after real-world measurement found the curve *shape*
doesn't move performance; see `docs/dev/PERF-BASELINE-RC73XA.md` and the
plugin repo's `DESIGN.md` §12.9. `hpdctl cool set-custom` above is the
capability's only current caller.)

**Why a safety floor, and how a device communicates its own bounds.**
Handing a user a raw 8-point curve editor is dangerous without limits: a
badly-drawn curve could leave the fans near-silent at a temperature where
the chip actually needs airflow. So every custom curve is checked against
a **`FanCurveConstraints`** value for the running device — fetched live
over D-Bus via `GetFanCurveConstraints() -> a{sv}`:

| Key | Meaning |
|---|---|
| `points` (`u`) | Always `8` — the fixed point count every curve needs. |
| `temp_min_c` / `temp_max_c` (`y`) | The temperature range a point may sit in. |
| `pwm_min` / `pwm_max` (`y`) | The duty range a point may sit in (`pwm_max` is `PWM_MAX` = 255). |
| `safety_floor` (`a(yy)`) | Unordered `(temp_threshold_c, min_pwm)` pairs: **at or above** `temp_threshold_c`, `pwm` must be **at least** `min_pwm`. |

On the ROG Xbox Ally X (RC73XA, the only model with an on-device capture
so far — `rc73xa_fan_curve_constraints()` in
`hpd-backend-asus/src/fan_curve.rs`), the constraints are `temp_min_c:
30`, `temp_max_c: 95`, `pwm_min: 0`, `pwm_max: 255`, and a
`safety_floor` of `[(85, 150), (90, 200)]` — i.e. **at ≥ 85 °C, pwm must
be ≥ 150; at ≥ 90 °C, pwm must be ≥ 200**. A point that satisfies the
*strictest* threshold it has reached passes (e.g. a point at 92 °C is
checked against both thresholds and must clear the higher one, pwm ≥
200). Every other ASUS handheld this crate targets shares the same
`asus_custom_fan_curve` EC interface, so it inherits this same
conservative floor until its own on-device capture lands — a new device
should never ship with *no* floor.

`FanCurve::validate_against` (`hpd-capabilities/src/fan_curve.rs`) is the
function that actually enforces this — stricter than the plain
`validate()` used for the compile-time presets (§1), because a
hand-drawn curve gets no benefit of the doubt:

- **Strictly increasing temperatures** — no two points at the same
  temperature (a preset tail may legitimately repeat a temperature to
  mean "flat past here"; a hand-drawn curve may not).
- **Non-decreasing duty** — a curve may plateau, never dip.
- **Every point inside `[temp_min_c, temp_max_c] × [pwm_min, pwm_max]`.**
- **Every point at or above a `safety_floor` threshold meets that
  threshold's minimum duty** — defense in depth on top of the EC's own
  firmware failsafes; hpd refuses to even *ask* the EC for a reckless
  curve rather than trusting the EC alone to reject it.

Validation happens **twice**, matching the pattern already used
elsewhere in this document (§4's read-back-and-fail-closed write): once
at the D-Bus boundary against the live `GetFanCurveConstraints()`, and
again, independently, by the L1 backend immediately before it writes to
the EC. A violation returns `InvalidArgs` naming the offending point
one-based (matching the hwmon `auto_pointN` numbering), e.g. `"point 8:
92°C requires pwm ≥ 200"` — precise enough to fix without guessing.

`polkit` action: `dev.cirodev.hpd.set-profile` — the same "cooling
levers" bucket as `set_cooling_level`/`reset_fan_curve`; no new polkit
action was added for the custom-curve surface.

## Calibration

Two measurement passes on the ROG Xbox Ally X (RC73XA): one that exposed
the **profile-gates-power** behaviour (motivating the decouple), and one
that **validated the retuned curves in a real game**.

### Pass 1 — the coupling finding (why we decoupled)

Same `tdp set 25`, identical all-core load, only the cooling *level*
changed. Because each old level also set the platform profile, the real
SoC power swung wildly:

| Old level → profile | SoC power (real) | Tctl |
|---|---|---|
| `silent` → PowerSaver | **~13 W** | 54 °C |
| `balanced` → Balanced | **~17–21 W** | 66 °C |
| `aggressive` → Performance | **~29 W** | 75 °C |

`silent` wasn't cool because of the fan — it was cool because the profile
**throttled the chip to 13 W**. With SPL set to 25 W. That hidden gating
is exactly what the decouple removes.

### Pass 2 — in-game validation of the retuned curves

Real GPU-heavy game, platform profile pinned to `performance` (so the
fans, not the power, are what's under test), each candidate curve pushed
to the EC live and held to steady state:

| Curve (profile / power) | Tctl / edge | CPU fan | GPU fan |
|---|---|---|---|
| `aggressive` (Performance, ~40 W) | ~78 °C / ~78 °C | ~8000 rpm | ~8000 rpm |
| `balanced` (Balanced, ~17 W) | ~62 °C / ~60 °C | ~5100 rpm | ~5100 rpm |
| `silent` (PowerSaver, ~13 W) | ~60 °C / ~57 °C | ~3800 rpm | ~3800 rpm |

Key facts learned in-game (these drove the curve shapes):

- **Real power ≈ 40 W** under a game (CPU+GPU), well above the ~29 W a
  CPU-only synthetic load showed.
- **Fan floor ~3700 RPM, ceiling ~8400 RPM** — higher than the ~6600 a
  synthetic load reached; the EC drives the fans harder under real
  thermal demand.
- The `aggressive` curve holds the chip at **~78 °C at a sustained 40 W**
  with the fans pinned near the ceiling — plenty of headroom.

Reproduce with the tuning helpers: `scripts/fan-tune.sh` (push a candidate
curve to the EC + live monitor) and `scripts/fan-characterize.sh`
(PWM→RPM sweep).

### Remaining caveats

1. **`pwm_enable` semantics** — assumed `1 = custom`, `2 = automatic`.
   The read-back guards correctness; the on-device test plan §2 confirms
   the curve actually takes effect.
2. **Other models** — the presets are shared across the ASUS handheld
   family but only measured on the Xbox Ally X (`RC73XA`). Per-model
   tuning lands when captures from the Ally / Ally X exist.

Sustained full duty is safe for the hardware (the fans are rated for it);
it is only louder.

## Integration with the rest of the daemon

The curve is not a bolted-on side feature — it threads through the
existing cooling/power flows:

- **Platform profile ↔ curve.** While a custom curve is active
  (`pwm_enable = 1`), *the curve* drives the fans. Because writing the
  `platform_profile` can make the EC drop the custom curve back to
  automatic, the reducer **re-asserts the active curve after every
  `ApplyPlatformProfile`** (`reassert_curve_after_profile`). This keeps the
  curve alive across the boot-time profile write and any later
  `set_profile`.
- **The platform profile gates real power — so it's now decoupled.**
  Measured on the RC73XA: at a fixed SPL under identical load and fans,
  `power-saver` drew ~13 W while `performance` drew ~29–40 W — amd_pmf
  clamps the actual draw by profile, regardless of the SPL `hpd` writes.
  Because that made a low cooling level silently throttle the chip, the
  profile is **no longer coupled to cooling**: it defaults to `performance`
  (so the SPL is the real limit) and only `set_profile` changes it. See
  [§6 Decoupling power from cooling](#6-decoupling-power-from-cooling).
- **Suspend/resume.** `SystemResumed` re-applies the active curve (and the
  profile) as final effects (the EC can reset them across suspend).
- **AC plug/unplug.** Plugging AC ramps the TDP and, with auto-cooling on,
  the *fan curve* follows it (the profile is left at its default).
- **Telemetry.** The daemon now surfaces live fan RPM (CPU/GPU) and
  CPU/GPU temperatures over D-Bus (`GetThermalStatus`), shown in
  `hpdctl status` / `monitor` alongside the active curve. This revived
  the previously-unused `FanControl` read path and added a
  `ThermalSensors` capability (`k10temp` Tctl + `amdgpu` edge, located by
  hwmon name).

On-device validation of all of the above is scripted in
[`docs/dev/GAMING-ROADMAP-es.md`](dev/GAMING-ROADMAP-es.md) ("Fase 3 —
Curvas de ventilador personalizadas"), including the 2.9.0 custom-curve
editor covered in depth in [§7](#7-custom-hand-drawn-curves--the-290-editor)
above.

## Where this lives in the code

| Concern | Location |
|---|---|
| Capability trait + value types (incl. `FanCurveConstraints`, `validate_against`) | `hpd-capabilities/src/fan_curve.rs` |
| ASUS curve write/read-back + presets + safety floor (`rc73xa_fan_curve_constraints`) | `hpd-backend-asus/src/fan_curve.rs` |
| hwmon lookup by stable `name` | `hpd-backend-asus/src/hwmon.rs` |
| State machine (transition/effect/reducer) | `hpd-core/src/{transition,effect,reducer}.rs` |
| Auto-cooling fan-curve inference | `hpd-core/src/inference.rs` (`infer_fan_curve_from_spl`) |
| Resume re-apply | `hpd-core/src/reducer.rs` (`SystemResumed`) |
| D-Bus methods + property | `hpd-dbus/src/service.rs` |
| polkit action | `hpd-dbus/src/actions.rs`, `package/polkit/dev.cirodev.hpd.policy` |
| CLI | `hpd-cli/src/main.rs` (`cool …`) |
| Config (`default_fan_curve`, `default_platform_profile`) | `hpd-daemon/src/config.rs`, `package/hpd-example.toml` |
| Tuning helpers | `scripts/fan-tune.sh`, `scripts/fan-characterize.sh` |
