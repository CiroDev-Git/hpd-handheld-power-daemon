<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Known power-limit enforcement gaps

> Devices where hpd writes a power envelope correctly (the value round-trips
> back from sysfs unchanged) but the firmware/EC does not actually hold the
> hardware to it. Born from an on-device investigation (2026-07-18, ROG Xbox
> Ally X / RC73XA, CachyOS) triggered by a user report: TDP set to 7W,
> `EnableGpuAutoFollow` on, measured SoC power sustained at 25-30W during
> real gameplay (Kingdom Come: Deliverance).

## Why this list exists

Architectural rule 1 (`CLAUDE.md`) is that hpd is the sole authority over
TDP/fans/charge and never fakes a reading. That rule covers hpd's own
correctness — it says nothing about whether the *firmware* actually honours
a value hpd correctly wrote. This doc is where that second, narrower kind of
gap gets tracked once confirmed, so it isn't rediscovered from scratch on
every report of "my TDP doesn't feel like it's applying."

A device belongs on this list only after ruling out every daemon-side and
OS-side cause first (see "Investigation checklist" below) — most reports of
"TDP isn't working" turn out to be one of those, not a firmware gap.

## Confirmed gaps

### ROG Xbox Ally X (RC73XA) — STAPM/PPT not enforced at the SPL floor

- **Symptom**: with `SPL = 7W` (this device's `spl_min`, the lowest tier),
  `SPPT`/`FPPT` correctly floored at their own hardware minimums (13W/19W —
  see `hpd-core::reducer::derive_boosted_envelope`'s existing doc comment on
  that floor), measured `soc_power_mw` (read from the `amdgpu` hwmon's
  `power1_input`, the same source `ryzenadj`/CoreCtrl/MangoHud all trust)
  sustained at 25-30W during real gameplay — 32-58% over even the highest
  configured rail (FPPT), for minutes at a time, not a momentary boost
  spike.
- **Confirmed NOT the cause** (checked on-device before concluding this is a
  firmware gap):
  - hpd wrote the right values: `ppt_pl1_spl` / `ppt_pl2_sppt` /
    `ppt_pl3_fppt` under `/sys/class/firmware-attributes/asus-armoury/attributes/`
    read back exactly `7` / `13` / `19`, matching `hpdctl status`'s own
    `CurrentSpl`.
  - No competing power daemon (`hpdctl status`'s health block: "hpd owns the
    power knobs").
  - GPU clock ceiling *was* being honoured correctly — observed 1821 MHz /
    94% busy, within the ~1865 MHz ceiling `gpu_clock_range_for_tier`
    computes for the `Silent` preset at this device's live `OD_RANGE`
    (600-2900 MHz). This is a **separate** WMI control path from
    STAPM/PPT, and it worked — only the power-limit path didn't.
  - No relevant package updated on the affected device before the symptom
    first appeared: no kernel, no `linux-firmware`, no `fwupd`-driven BIOS
    flash. hpd itself was on 2.14.0 → 2.14.1 → 3.0.0 the same day, and
    neither jump touches the SPL/SPPT/FPPT write path or the low-end floor
    logic (`2.14.1` was CLI/doc-only; `3.0.0`'s one power-adjacent fix only
    changes behaviour at `spl_max`, the opposite end of the range). Most
    likely explanation: this is the first time a user pushed this device to
    its literal minimum SPL under real load — an always-present gap, not a
    regression.
  - BIOS was already the latest available (`RC73XA.317`, 2026-04-30) at the
    time of the report. An earlier BIOS (316) did fix a distinct,
    ASUS-acknowledged "SPL settings issue of Manual mode" — that fix
    predates and is included in 317, so it is not this gap.
- **Likely cause (not proven, no further lever to pull)**: either the
  RC73XA's EC/SMU firmware doesn't actually enforce the WMI-set PPT
  registers at this device's literal floor tier, or the very new
  `asus-armoury` Linux kernel driver has a gap Windows + Armoury Crate SE
  don't exercise the same way (ASUS validates BIOS releases against
  Windows; this device ships Windows-first). hpd has no way to distinguish
  between those two from userspace, and no more-forceful write path exists
  to try — the WMI attribute is the only interface `AsusPowerBackend`
  (or ASUS's own tools) can use.
- **What hpd does about it**: nothing more than tell the truth. There is no
  daemon-side retry, no alternate write path, and no plan to add one — this
  is outside architectural rule 1's scope (hpd is not lying about what it
  set; the hardware is not doing what it was told). The mitigation is
  purely observational: `GetTelemetry`'s `boost_ceiling_mw` key (added
  alongside this doc) plus `hpdctl status`'s matching warning line surface
  the gap the moment it's measurable, instead of requiring an SSH
  debugging session to notice it, as this report did.

## Investigation checklist (before adding a device here)

Rule out every one of these first — most "TDP doesn't apply" reports are
one of them, not a firmware gap:

1. **hpd's own write actually landed.** Read the backend's raw sysfs/WMI
   attribute directly (for the ASUS backend:
   `/sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl{1,2,3}_*/current_value`)
   and compare against `hpdctl status`'s `CurrentSpl`. A mismatch here is an
   hpd bug, not a firmware gap — file it as one.
2. **No competing power daemon.** `hpdctl status`'s health block, or
   `GetPowerConflicts()` directly.
3. **Not AC-locked.** `AcLocked` forces max performance by design (see
   `CLAUDE.md`'s "AC = maximum performance" section) — that is not a bug of
   any kind.
4. **No recent relevant update.** Check `pacman`/distro package log (or
   equivalent) for a kernel, `linux-firmware`, `fwupd`/BIOS flash, or hpd
   update around when the symptom first appeared, and cross-reference each
   against its own changelog for anything power-related.
5. **BIOS is current**, or at least confirmed to already include any
   ASUS-published power-management fix for that model.
6. **The measurement source is credible.** Prefer the `amdgpu` hwmon's
   `power1_input` (what `hpdctl status`/`GetTelemetry`'s `soc_power_mw`
   reads) over a third-party overlay, which may read whole-system battery
   draw instead of SoC package power — see `TelemetryPanel`'s own doc
   comment in the plugin repo on that exact confusion.

Only once all six are ruled out does a sustained, reproducible gap between
`soc_power_mw` and `boost_ceiling_mw` belong on this list.
