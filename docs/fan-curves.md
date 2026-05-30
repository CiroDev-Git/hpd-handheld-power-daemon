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

The three presets (CPU `pwm1`; the GPU `pwm2` shares the same temp→duty
shape and naturally spins less because the GPU runs cooler):

| ≈°C | `silent` | `balanced` (default) | `aggressive` |
|----:|---------:|---------------------:|-------------:|
| 45–48 | 8 (3 %)  | 15 (6 %)   | 26 (10 %) |
| 54    | 20 (8 %) | 33 (13 %)  | 64 (25 %) |
| 59–62 | 38 (15 %)| 64 (25 %)  | 102 (40 %) |
| 65–70 | 64 (25 %)| 102 (40 %) | 140 (55 %) |
| 76–77 | 102 (40 %)| 140 (55 %)| 178 (70 %) |
| 82–83 | 140 (55 %)| 178 (70 %)| 210 (82 %) |
| 87–88 | 190 (75 %)| 216 (85 %)| 240 (94 %) |
| 91–93 | 230 (90 %)| 255 (100 %)| 255 (100 %) |

Even `silent` beats the firmware default at high temperature, because the
firmware simply has no defined behaviour there. `balanced` is the
shipped default (`default_fan_curve = "balanced"`), applied on first
boot.

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
- `hpdctl fan curve reset` (or `ResetFanCurve` over D-Bus) writes
  `pwmN_enable = 2`, handing control cleanly back to the firmware curve.

### 4. Verify every write (fail closed)

After writing the 16 points the backend reads them back and **errors if
the EC did not store exactly what we asked for**. A silently-rejected
curve must not look like success to the daemon. The write path is also
purely additive at the capability layer: a backend without a
programmable curve simply returns `None` and the daemon treats fan-curve
effects as no-ops.

### 5. One user-facing lever

Cooling is presented as a single concept so users don't have to juggle
"profile" vs "curve" vs "mode":

- **`hpdctl cool set <silent|balanced|aggressive>`** sets the platform
  profile *and* the matching fan curve together (manual cooling).
- **`hpdctl cool auto`** lets the daemon infer the level from the TDP.
- `fan_curve_follows_profile` defaults **on**, which is what keeps the
  profile and curve in lock-step (for both the `cool` command and
  auto-cooling). Advanced users set it `false` and drive the raw
  `hpdctl fan profile …` / `hpdctl fan curve …` controls independently.

## Calibration caveats

These presets are a **sensible starting point, not a final calibration**:

1. **`pwm_enable` semantics** — we assume `1 = custom curve`,
   `2 = firmware automatic`, based on the kernel interface and the
   captured default state. The read-back guards correctness, but confirm
   on-device that applying a curve actually changes fan behaviour.
2. **PWM→RPM mapping is unknown per unit** — the duty values were chosen
   for a monotonic, safe ramp, not measured against this unit's real RPM
   response. Recommended tune loop: apply `balanced`, run a sustained
   load, watch `k10temp`/`amdgpu` temps and `asus` RPMs, and adjust the
   bands before treating the values as final.
3. **One model captured** — the presets are currently shared across the
   ASUS handheld family but only calibrated against the Xbox Ally X
   (`RC73XA`). Per-model tuning lands when captures from the Ally / Ally
   X exist.

Sustained full duty is safe for the hardware (the fans are rated for it);
it is only louder.

## Integration with the rest of the daemon

The curve is not a bolted-on side feature — it threads through the
existing cooling/power flows:

- **Platform profile ↔ curve.** While a custom curve is active
  (`pwm_enable = 1`), *the curve* drives the fans, overriding whatever
  fan behaviour the ACPI `platform_profile` would have applied. Because
  writing the profile can make the EC drop the custom curve back to
  automatic, the reducer **re-asserts the active curve after every
  `ApplyPlatformProfile`** (`reassert_curve_after_profile`), decoupled
  from `fan_curve_follows_profile`. This is what keeps the curve alive
  when the default `fan_follows_tdp` auto-cooling nudges the profile as
  the TDP changes.
- **The platform profile gates real power.** Measured on the RC73XA: at a
  fixed `tdp set 30` under identical load and fans, `power-saver` settled
  at 59 °C while `performance` hit 95 °C — amd_pmf clamps the actual power
  draw by profile, regardless of the SPL `hpd` writes. The profile is
  therefore the *dominant* performance/thermal lever, not a cosmetic
  hint, which is exactly why `cool` couples it to the curve as one level
  (`silent`→power-saver = low power + quiet, `aggressive`→performance =
  full power + hard cooling). See `docs/dev/FAN_CURVE_TESTING.md` §11.
- **Suspend/resume.** `SystemResumed` re-applies the active curve as a
  final effect (the EC can reset it across suspend).
- **AC plug/unplug.** Plugging AC ramps the TDP and, with auto-cooling
  on, the profile — which (via the re-assert above, or
  `fan_curve_follows_profile`) carries the curve along.
- **Telemetry.** The daemon now surfaces live fan RPM (CPU/GPU) and
  CPU/GPU temperatures over D-Bus (`GetThermalStatus`), shown in
  `hpdctl status` / `monitor` alongside the active curve. This revived
  the previously-unused `FanControl` read path and added a
  `ThermalSensors` capability (`k10temp` Tctl + `amdgpu` edge, located by
  hwmon name).

On-device validation of all of the above is scripted in
[`docs/dev/FAN_CURVE_TESTING.md`](dev/FAN_CURVE_TESTING.md).

## Where this lives in the code

| Concern | Location |
|---|---|
| Capability trait + value types | `hpd-capabilities/src/fan_curve.rs` |
| ASUS curve write/read-back + presets | `hpd-backend-asus/src/fan_curve.rs` |
| hwmon lookup by stable `name` | `hpd-backend-asus/src/hwmon.rs` |
| State machine (transition/effect/reducer) | `hpd-core/src/{transition,effect,reducer}.rs` |
| Resume re-apply | `hpd-core/src/reducer.rs` (`SystemResumed`) |
| D-Bus methods + property | `hpd-dbus/src/service.rs` |
| polkit action | `hpd-dbus/src/actions.rs`, `package/polkit/dev.cirodev.hpd.policy` |
| CLI | `hpd-cli/src/main.rs` (`fan curve …`) |
| Config (`default_fan_curve`, `fan_curve_follows_profile`) | `hpd-daemon/src/config.rs`, `package/hpd-example.toml` |
