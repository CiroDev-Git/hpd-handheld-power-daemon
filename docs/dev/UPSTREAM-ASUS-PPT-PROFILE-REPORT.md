<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Upstream report — `platform_profile` writes drop `ppt_*` limits

> **Status: drafted, not yet filed.** This is the report
> [`POWER-ENFORCEMENT-GAPS.md`](POWER-ENFORCEMENT-GAPS.md) calls for
> ("an upstream report with the deterministic repro above is worth
> filing"). It is kept here, ready to paste, so the finding does not
> live only in a scratch file.
>
> **Where it goes:** <https://gitlab.com/asus-linux/asusctl/-/issues/new>
>
> Once filed, replace this block with the issue link and update the
> matching bullet in `POWER-ENFORCEMENT-GAPS.md`.

**Why file it at all, given hpd already works around it?** The
workaround (re-assert the envelope after every profile write, shipped
in `3.1.1`) cures it for hpd's own writes only. The kernel
`asus-armoury` driver, `asusctl`, and every other tool that touches
both surfaces have the same exposure, and the failure is invisible from
sysfs — so the behaviour deserves to be documented where those authors
will find it.

---

## Title

Writing the ACPI platform profile silently drops previously-set `ppt_*`
limits (EC keeps stale sysfs values) — deterministic repro on ROG Xbox
Ally X (RC73XA)

## Body

### Summary

On the ROG Xbox Ally X (RC73XA), an actual change of the ACPI platform
profile makes the EC silently drop the PPT limits previously written via
asus-armoury's `ppt_pl1_spl` / `ppt_pl2_sppt` / `ppt_pl3_fppt`
attributes. The sysfs attributes keep reading back the old (now
unenforced) values, and the chip runs at the new profile's own default
limits instead — with no error or event anywhere. A fresh write to the
ppt attributes re-establishes enforcement.

We initially shaped this as an intermittent "limits not enforced at the
lowest tier" mystery; a controlled benchmark campaign then made it fully
deterministic and identified the profile write as the trigger. Sharing
here because (a) any user or tool that mixes `platform_profile` writes
with `ppt_*` writes hits this, (b) the failure is invisible (stale sysfs
read-back), and (c) it plausibly explains existing "custom PPT limits
don't work" reports on other ASUS models.

### Environment

- **Device:** ASUS ROG Xbox Ally X (RC73XA), BIOS RC73XA.317 (latest)
- **Kernel:** 7.1.4-1-cachyos-deckify (CachyOS), in-tree `asus_armoury`
  (`wmi:0B3CBB35-E3C2-45ED-91C2-4C5A6D195D1C`) + ACPI `platform_profile`
- **Userspace:** hpd power daemon (writes both surfaces; the repro below
  is reproducible with plain sysfs writes too)

### Deterministic reproduction

1. Set the platform profile to `performance`, then write low PPT limits
   (e.g. SPL 15 / SPPT 17 / FPPT 19 via the armoury attributes). Run a
   sustained CPU+GPU load → limits enforced correctly (measured package
   power settles at SPL). ✅
2. While the load keeps running, change the platform profile
   (`performance` → `balanced`). → Within seconds, measured package
   power rises past the configured FPPT and stays there (21–25 W
   sustained vs a 19 W ceiling in our runs). The `ppt_*` attributes
   still read back 15/17/19. 🚨
3. More violent variant: limits set with SPL 13, profile changed
   `power-saver` → `performance` under load → 21–34 W sustained for four
   full minutes against a 13 W target. The benchmark scores themselves
   corroborate it (the "13 W" run scored identically to an unconstrained
   28–35 W run). 🚨
4. Control: same sequence with no actual profile change → enforced
   correctly for the entire run. ✅
5. Recovery: any fresh write to the ppt attributes after the profile
   change re-establishes enforcement immediately. ✅

Notes:

- Only an *actual value change* of the profile triggers it (we dedupe
  same-value writes in userspace, so we cannot say whether a same-value
  rewrite also triggers it).
- A power-saver-type profile masks the symptom (its own EPP bias keeps
  draw below the stale ceiling), which is part of why this looked
  intermittent for so long.
- Measured power read from the amdgpu hwmon `power1_input` (same source
  ryzenadj/MangoHud trust).

### Why this is nasty

- **The sysfs read-back lies:** after the profile write, `current_value`
  of the ppt attributes shows limits the EC is no longer holding. There
  is no way to detect the drop from the attributes themselves.
- **The failure mode is *more* power than requested** — on a handheld
  this silently destroys battery-life expectations (our original field
  report was "TDP set to 7 W, chip drawing 25–30 W in-game").

### Userspace mitigation (what hpd now does)

hpd ≥ 3.1.1 re-asserts the power envelope immediately after every
platform-profile write, and orders profile-before-limits in every
composed write sequence (boot/resume re-assert, AC/DC transitions). That
fully cures it in practice on this device — the same strategy hpd
already used for the (long-known, same-class) EC behaviour of dropping
the custom fan curve on profile writes.

### Possibly related existing reports

- `seerge/g-helper#4996` (ProArt P16): custom PPT wattage `DeviceSet()`
  accepted but ignored — if g-helper writes limits before/without
  re-asserting after its profile write, this could be the same root
  cause rather than a device quirk.
- The general recurring pattern of "PPT sliders don't stick on model X"
  reports may partly reduce to this ordering issue.

### Ask

1. Consider documenting this EC behaviour wherever `ppt_*` attributes
   are documented (kernel ABI doc / asusctl docs): *"changing the
   platform profile resets PPT limits to the profile defaults; re-write
   them afterwards"*.
2. Opinions welcome on whether the kernel driver itself should re-assert
   armoury-set ppt values after a `platform_profile` change (it has the
   last-written values cached — the stale read-back proves it), or
   whether this stays a userspace contract.
3. If anyone can test on other models (Ally RC71L/RC72L, ROG laptops):
   does an actual profile change drop armoury-set ppt limits there too?

Happy to provide raw campaign data (2,100+ telemetry samples), the exact
scripts, or run further experiments on this device.
