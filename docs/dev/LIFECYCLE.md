<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Lifecycle reference — daemon + Decky plugin

> Every OS-driven lifecycle event hpd can see (systemd / logind / udev /
> Decky / gamescope), the code path that handles it, and its status. Born
> from the 2026-06 on-device lifecycle audit on CachyOS / ROG Xbox Ally X.
> Keep this current when you touch a monitor, the executor's boot/resume
> arm, or the plugin's connection lifecycle.

The golden rule across all of it (see the project's *consistency invariant*):
**neither the daemon nor the plugin may ever report state the device does not
actually have.** Most of the rows below exist to honour that across an event
that can desync in-memory state from hardware.

## A. Daemon (`hpd-daemon`)

| Flow | Trigger | Path | Status |
|------|---------|------|--------|
| Cold boot (AC / battery) | service start | `main.rs` re-reads AC + platform profile from hw, overrides `active_profile` to the configured default, then pushes `SystemResumed` to re-assert envelope→profile→charge→curve | ✅ validated on-device (boot plugged → `Boot/resume on AC: re-asserting`) |
| First install (no `state.toml`) | service start | `DaemonConfig::default()`; `ac_max_performance` seeded from `default_ac_max_performance` | ✅ |
| Shutdown / reboot | `SIGTERM` (systemd) | `Shutdown` → reducer `PersistState` → executor drains → exit (5 s cap) | ✅ |
| Config reload | `SIGHUP` (`systemctl reload`) | re-read `/etc/hpd/config.toml` → `ConfigReload(to_runtime())` | ✅ |
| Resume (AC unchanged) | logind `PrepareForSleep(false)` | `suspend.rs` → `SystemResumed`; executor re-reads AC from hw, reducer re-asserts | ✅ validated on-device |
| Resume (AC changed asleep) | logind | executor re-reads `is_ac_connected` from hw **before** reducing (2.7.1) | ✅ validated (suspend-on-AC → unplug → resume-on-battery restored, not max) |
| Suspend-loop (resume→sleep×N) | flaky s2idle firmware | each `SystemResumed` re-asserts; daemon stays correct | ✅ handled (cosmetic write spam only). **The loop itself is a kernel/firmware issue, out of hpd's scope.** |
| AC plug/unplug (live) | udev `power_supply` | `hpd-netlink` → `AcPowerChanged` | ✅ **fixed 2.7.2** — was GAP #1 (monitor could die) |
| User write racing an unplug | `SetSpl` (etc.) arriving in the seconds around a physical unplug | the write reduces first, then `AcPowerChanged(false)` → `restore_dc_state` replays the DC snapshot **over it** — the call succeeded but its value is then reverted to the snapshot's | ⚠️ **known race, LOW severity, observed on-device 2026-07-19** (a scripted `tdp set` ~2 s after unplug came back as the snapshot value). Window is a few seconds; final state is self-consistent (state == hardware == snapshot); the plugin's echo-wait surfaces the revert as `SilentRejection`, the CLI shows it on the next `status`. A fix would need the reducer to prefer post-unplug user writes over the snapshot replay (ordering/timestamp heuristics) — not worth the complexity unless real users hit it outside scripted scenarios. |
| Monitors after a suspend | stream `Err`/`None` | **outer reconnect loop** rebuilds the udev + logind subscriptions | ✅ **fixed 2.7.2** (GAP #1) |
| Backend write fails | sysfs `Err` | rollback via `Sync*` (reads real hw, re-injects) | ✅ |
| Polkit absent / rivals | startup self-check | `missing_actions` + `power_conflicts`; surfaced over D-Bus | ✅ |

## B. Plugin (`hpd-decky-plugin`)

| Flow | Trigger | Path | Status |
|------|---------|------|--------|
| Decky loads | `_main` | connect → prime → subscribe → AC poll + disconnect watch + health watchdog | ✅ |
| Decky unloads / reloads | `_unload` → `_main` | clean teardown of every task + bus | ✅ |
| Daemon (re)appears | `NameOwnerChanged` | `_handle_daemon_presence_change` → reset binding (keeps bus) → re-prime | ✅ |
| Daemon restart, racy re-bind | watchdog tick | re-prime when `is_connected` but **AC poll task dead** | ✅ (2.9.1) |
| Boot (Decky + daemon fresh) | `_main` | clean bootstrap | ✅ |
| **Suspend — our bus socket breaks** | dbus-next read EOF / failing calls | `wait_for_disconnect()` watcher → `_handle_bus_lost` (tear down + rebuild); **and** watchdog re-builds when `is_connected` but AC poll is **alive-but-failing** (fail streak ≥ `AC_POLL_BROKEN_THRESHOLD`) | ✅ **fixed 2.10.1** — was GAP #2 (the "AC not taken after suspend" stuck-stale) |
| QAM open/close | mount/unmount | thermal poll lifecycle (frontend) | ✅ |

## The two gaps the audit found

### GAP #1 — daemon monitors died on a single stream error (fixed 2.7.2)

`hpd-netlink` and `suspend.rs` both ran `while let Some(...) = stream.next()`
with **no reconnection**. A suspend can make a udev / logind signal stream
yield `Err` or end (`None`); the loop fell out and the monitor was **dead for
the rest of the process** — live AC detection (or resume detection) silently
stopped until the daemon restarted. The post-resume re-read masks the
*moment* of resume, but not live plug/unplug afterwards.

**Fix:** an outer reconnect loop per monitor (log → 2 s backoff → rebuild).
The netlink monitor reconciles the canonical mains node on every (re)connect
so an edge missed while down is still emitted. Only a dropped executor channel
stops a monitor for good. `hpd-netlink`'s `tokio` gains the `time` feature.

### GAP #2 — plugin stayed stale after a suspend broke its bus (fixed 2.10.1)

`DaemonClient.is_connected` only checks that the **dbus-next objects exist**,
not that the socket is alive. When a suspend broke the plugin's system-bus
socket (leaving the objects intact), `is_connected` stayed `True`, the AC poll
looped **alive-but-failing** (`continue` forever, never updating), and — since
the *daemon* never left the bus — **no `NameOwnerChanged` fired** to trigger a
rebind. The 2.9.1 health watchdog only caught a **dead** poll task, so it
no-op'd on this *alive-but-failing* zombie → the panel showed stale AC (e.g.
"On battery" while on AC) until a manual disable/enable.

**Fix (three layers):**
1. **`wait_for_disconnect()` watcher** — dbus-next resolves it when its read
   loop sees the socket close/error; the plugin then rebuilds the bus fast
   (`_handle_bus_lost`: full `disconnect()` + fresh `connect_and_prime`,
   re-entrancy-guarded). Catches the clean-disconnect case near-instantly.
2. **AC-poll failure streak** — the poll counts consecutive failures.
3. **Watchdog escalation** — when `is_connected` but the streak ≥
   `AC_POLL_BROKEN_THRESHOLD` (3 ≈ 30 s), the watchdog treats the bus as dead
   and calls `_handle_bus_lost`. Safety net for a half-broken socket that
   never trips `wait_for_disconnect`.

`reset_daemon_binding` (used for `NameOwnerChanged`) deliberately **keeps** the
bus, so it is the wrong tool when the bus itself is dead — hence the separate
`_handle_bus_lost`.

## What is explicitly NOT hpd's job

- **The s2idle suspend/resume loop** (screen wakes then re-sleeps every ~1 s on
  the Ally) is a **kernel/firmware** issue (`amd_pmc`, wake sources, BIOS). hpd
  only *listens* for resume; it neither triggers nor controls suspend. The
  daemon handles each spurious resume correctly (re-asserts state); the loop is
  out of scope here.

## On-device validation log (2026-06, daemon 2.7.1 / plugin 2.9.x)

- Boot plugged → `AC + locked`, journal `Boot/resume on AC`. ✅
- Suspend (stay plugged) → resume → `AC + locked`, journal `Boot/resume on AC`
  on every wake (even through the firmware suspend-loop). **No daemon AC race.**
- The "AC not taken after suspend" symptom was the plugin (GAP #2), not the
  daemon — `AcConnected`/`AcLocked` read `true` over D-Bus the whole time.
