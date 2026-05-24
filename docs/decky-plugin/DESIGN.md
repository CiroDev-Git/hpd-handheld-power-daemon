# `hpd-decky-plugin` — Design Document

> Design specification for the Decky Loader plugin that exposes the
> `hpd` (Handheld Power Daemon) D-Bus interface in Steam's Quick Access
> Menu (QAM) on ASUS ROG Ally / Ally X / Xbox Ally X devices.
>
> This is **a design document, not an implementation**. It establishes
> the contracts, layers, error model, UX rules and distribution plan
> before any line of plugin code is written.
>
> Target daemon version: **`hpd 1.0.0`** (the stable surface). The
> plugin is versioned independently and declares the daemon range it
> supports in `plugin.json`.
>
> Companion documents:
> - [`../../CLAUDE.md`](../../CLAUDE.md) — repo-level guidance, daemon
>   architecture, D-Bus / polkit contract.
> - [`../ARCHITECTURE.md`](../ARCHITECTURE.md) — daemon internals.
> - [`../release/PIPELINE.md`](../release/PIPELINE.md) — how the
>   daemon ships; the plugin's own pipeline mirrors it.

---

## 1. Overview

`hpd-decky-plugin` is a **thin, UI-only** Decky Loader plugin. It does
**not** talk to hardware, sysfs, or vendor firmware itself — every
mutation is forwarded to the existing `hpd` daemon via D-Bus on the
system bus. The daemon owns the state machine, authorization,
persistence and rollback semantics already.

The plugin's job is:

1. **Discover** the daemon (installed? running? reachable?
   authorized?).
2. **Read** its hardware limits and current state.
3. **Surface** controls in the Steam QAM that map 1:1 to the daemon's
   D-Bus methods.
4. **Subscribe** to `PropertiesChanged` so the UI reflects external
   state changes (AC plug, suspend/resume, `hpdctl` from a terminal,
   another client).
5. **Disable** any control whose backing capability cannot be
   reliably queried — never display fake or hard-coded values.

Everything else (validation, rollback, persistence, polkit) lives in
the daemon. The plugin is intentionally **stateless** beyond a thin
cache and the user's personal preferences (e.g. "remember my last
preset across reboots" — opt-in).

---

## 2. Goals and non-goals

### Goals

- **Full coverage** of every method and property exposed by
  `dev.cirodev.hpd.PowerDaemon1`.
- **Honest UX**: if the daemon is absent / down / unauthorized, the
  plugin says so explicitly and offers actionable next steps.
- **Conditional capability surfacing**: a control appears only when
  the daemon advertises support and returns valid data for it.
- **Resource frugality**: one persistent D-Bus connection, signal
  subscriptions instead of polling, debounced writes.
- **Defence in depth**: validation in the TS layer (UX), validation
  in the Python layer (boundary check), validation in the daemon
  (authoritative). The daemon's "no" is always final.
- **Reproducible builds** under the same supply-chain discipline as
  the daemon (CI, cargo-deny analogue for npm/pip, signed releases).
- **Documentation parity** with the daemon: ARCHITECTURE / INSTALL /
  TROUBLESHOOTING / CHANGELOG, all maintained alongside the code.

### Non-goals

- **No hardware fallback path.** If the daemon is unreachable, the
  plugin shows a diagnostic panel, not a degraded "talk to sysfs
  directly" mode. Running parallel control planes is how you corrupt
  state.
- **No bundled daemon installer.** The plugin will detect a missing
  daemon and link to the install docs, but it will not download or
  install the daemon binary itself.
- **No telemetry.** No analytics, no crash reporting beyond local
  logs the user can inspect.
- **No SteamOS-only assumptions.** The plugin must work on any
  Linux desktop where Decky runs (CachyOS + KDE, Bazzite, Nobara,
  etc.), not only SteamOS game mode.
- **No vendor-specific UI branches.** When a second vendor backend
  lands in the daemon (Lenovo / Valve / generic), the plugin
  consumes the same trait-driven surface and the UI adapts via
  capability detection, not `if (model === 'asus')`.

---

## 3. System architecture

```
┌────────────────────────────────────────────────────────────────────┐
│                       Steam (game mode or BPM)                     │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │              Decky Loader (CEF renderer host)                │  │
│  │  ┌────────────────────────────────────────────────────────┐  │  │
│  │  │  hpd-decky-plugin — frontend (React/TS)                │  │  │
│  │  │  • PanelSection / SliderField / DropdownItem (Deck UI) │  │  │
│  │  │  • Zustand store (cache + reactivity)                  │  │  │
│  │  │  • Capability guards (disable when unknown)            │  │  │
│  │  └────────────────────────┬───────────────────────────────┘  │  │
│  └───────────────────────────┼──────────────────────────────────┘  │
│                              │ Decky `callable` IPC (JSON-RPC-ish) │
│  ┌───────────────────────────▼──────────────────────────────────┐  │
│  │  hpd-decky-plugin — backend (Python, runs as `deck` user)    │  │
│  │  • DaemonClient (dbus-next, async)                           │  │
│  │  • HealthMonitor (systemd + D-Bus introspection)             │  │
│  │  • StateCache (last-known properties)                        │  │
│  │  • EventBridge (PropertiesChanged → frontend push)           │  │
│  └───────────────────────────┬──────────────────────────────────┘  │
└──────────────────────────────┼─────────────────────────────────────┘
                               │ system D-Bus  (zbus on the other end)
                               │ + polkit auth via PolicyKit1 dialog
              ┌────────────────▼──────────────────┐
              │   hpd-daemon (root systemd unit)  │
              │   dev.cirodev.hpd.PowerDaemon1    │
              │   /dev/cirodev/hpd/PowerDaemon1   │
              └────────────────┬──────────────────┘
                               │
                               ▼
                       /sys/class/firmware-attributes
                       /sys/class/power_supply
```

Trust boundary: the user-space plugin runs as the unprivileged
`deck` (or equivalent) user; every write reaches root only through
the daemon's polkit-gated D-Bus methods. The plugin **never** runs
`sudo`, **never** uses `pkexec`, **never** edits `/etc` or `/sys`.

---

## 4. Daemon surface — the consumed contract

The plugin treats the following as the **stable** contract. It is
mirrored from `crates/hpd-dbus/src/service.rs` and
`crates/hpd-dbus/src/actions.rs`.

### 4.1 Bus coordinates

| Concept     | Value                                |
|-------------|--------------------------------------|
| Bus         | System bus (`/var/run/dbus/system_bus_socket`) |
| Bus name    | `dev.cirodev.hpd.PowerDaemon1`       |
| Object path | `/dev/cirodev/hpd/PowerDaemon1`      |
| Interface   | `dev.cirodev.hpd.PowerDaemon1`       |

### 4.2 Methods

> **Units note.** The daemon's *internal* power representation is
> `PowerMilliwatts`, but the **D-Bus surface is whole watts** (the
> conversion lives on the value type via `as_watts()` /
> `from_watts()`). Every `u32` below carrying power is therefore
> **watts, not milliwatts**.

| Method                          | Args                | Returns                         | Polkit action                     |
|---------------------------------|---------------------|---------------------------------|-----------------------------------|
| `SetSpl`                        | `u32 watts`         | `()`                            | `dev.cirodev.hpd.set-tdp`         |
| `SetPreset`                     | `s name` (`eco` / `balanced` / `max`) | `()`          | `dev.cirodev.hpd.set-tdp`         |
| `SetChargeThreshold`            | `y percent` (20–100)| `()`                            | `dev.cirodev.hpd.set-charge`      |
| `SetProfile`                    | `s profile`         | `()`                            | `dev.cirodev.hpd.set-profile`     |
| `SetFanAuto`                    | —                   | `()`                            | `dev.cirodev.hpd.set-profile`     |
| `GetHardwareLimits`             | —                   | `(u32 spl_min_w, u32 spl_max_w, u32 sppt_max_w, u32 fppt_max_w)` | none (read-only) |
| `IsAcConnected`                 | —                   | `b`                             | none (read-only)                  |

> **Mind the asymmetry.** `SetFanAuto` is a one-shot re-enable
> (takes no args; reducer flips `fan_follows_tdp = true`). To turn
> auto-cooling *off* the operator calls `SetProfile` with an explicit
> profile — that flips `fan_follows_tdp = false` as a side effect.
> There is no `SetFanAuto(false)`; the plugin's toggle UI maps "off"
> to "the user must pick a manual profile".

> **Reducer rejections are silent on the wire.** The D-Bus method
> returns `Ok` as soon as the `Transition` is enqueued. If the
> reducer subsequently rejects the value (e.g. SPL out of hardware
> range, FPPT < SPPT < SPL invariant violation), the caller sees no
> error — only the absence of the corresponding `PropertiesChanged`
> within a short window. The plugin must treat *"no echo within
> 2 s"* as a probable rejection (§12.5).

### 4.3 Properties (read-only, `PropertiesChanged` emitting)

| Property              | Type | Unit     | Notes                                      |
|-----------------------|------|----------|--------------------------------------------|
| `CurrentSpl`          | `u`  | **W**    | Reflects last successful `SetSpl` (whole watts). |
| `ActiveProfile`       | `s`  | opaque   | Daemon's canonical names are `power-saver` / `balanced` / `performance` (plus arbitrary `Custom(s)` vendor strings — `ProfileName::FromStr` accepts ACPI aliases `quiet` / `low-power` but `Display` normalises to `power-saver`). The plugin **must** treat the value as an opaque string — never branch on it. |
| `ChargeEndThreshold`  | `y`  | %        | 20–100 (daemon-enforced). |
| `AutoCooling`         | `b`  | —        | Mirrors `fan_follows_tdp` — `true` while the daemon is inferring the cooling profile from the TDP envelope. |

> **`is_ac_connected` is NOT a `PropertiesChanged`-emitting property.**
> It is a regular `IsAcConnected()` method — `current_spl`,
> `active_profile`, `charge_end_threshold` and `auto_cooling` are
> the only four properties the daemon's
> `spawn_properties_changed_emitter` watches. AC state changes
> reach the daemon via the udev netlink monitor (the reducer
> updates `ProfileState::is_ac_connected`) but no signal is emitted
> on D-Bus. The plugin therefore has three honest options (§14):
> 1. Poll `IsAcConnected()` on a low cadence (e.g. 5 s).
> 2. Hide the AC indicator until a daemon-side signal lands.
> 3. Push for a daemon patch adding `is_ac_connected` to the
>    emitter (cheap; tracked in §27).
>
> The doc recommends **option 3** as the work item, **option 1 at
> 10 s** as the v1 fallback. Option 2 is the fallback to the
> fallback if polling is judged too noisy.

### 4.4 Polkit actions (authorization model)

Column order matches the XML layout in
`package/polkit/dev.cirodev.hpd.policy`.

| Action ID                          | `allow_any`      | `allow_inactive` | `allow_active`   |
|------------------------------------|------------------|------------------|------------------|
| `dev.cirodev.hpd.set-tdp`          | `auth_admin`     | `auth_admin`     | `auth_admin`     |
| `dev.cirodev.hpd.set-charge`       | `auth_admin`     | `auth_admin`     | `auth_admin`     |
| `dev.cirodev.hpd.set-profile`      | `auth_admin_keep`| `auth_admin_keep`| `auth_admin_keep`|

`auth_admin_keep` caches the credential for **5 minutes** — that
window is a polkit default for the `_keep` suffix, not a daemon
configuration value.

The plugin's UX must reflect this: profile and fan-auto changes
prompt once per 5-minute window; TDP and charge prompt every time.

### 4.5 Stability promise

The daemon follows SemVer on this surface. Within the `1.x` line:

- Methods and properties listed above are **frozen** in name, signature
  and semantics.
- New methods/properties may be **added**.
- Polkit action IDs are **frozen**.
- Removals or signature changes require a `2.0`.

The plugin pins a daemon range (e.g. `^1.0`) in `plugin.json` and
fails fast on incompatible majors (see §19).

---

## 5. Project layout

Modelled on the upstream
[`SteamDeckHomebrew/decky-plugin-template`](https://github.com/SteamDeckHomebrew/decky-plugin-template).

```
hpd-decky-plugin/
├── plugin.json                  # Decky metadata
├── package.json                 # pnpm + vite + @decky/api + @decky/ui
├── pyproject.toml               # backend deps (dbus-next, etc.)
├── requirements.txt             # pinned backend deps for the loader
├── rollup.config.js             # bundling for Decky's CEF
├── tsconfig.json
├── LICENSE                      # GPL-3.0-or-later (matches daemon)
├── README.md
├── CHANGELOG.md
├── docs/
│   ├── ARCHITECTURE.md          # mirrors §3 of this doc, kept fresh
│   ├── INSTALL.md               # end-user install (decky → plugin)
│   ├── TROUBLESHOOTING.md
│   ├── COMPATIBILITY.md         # daemon-version matrix
│   └── DEVELOPER.md             # local dev loop, debugging
├── main.py                      # backend entry — Decky `Plugin` class
├── backend/
│   ├── __init__.py
│   ├── client.py                # DaemonClient (D-Bus wrapper)
│   ├── health.py                # HealthMonitor (systemd + ping)
│   ├── cache.py                 # StateCache
│   ├── bridge.py                # PropertiesChanged → frontend events
│   ├── models.py                # @dataclass mirrors of D-Bus types
│   ├── errors.py                # typed error taxonomy
│   └── settings.py              # plugin-local prefs (decky Settings)
├── src/                         # frontend
│   ├── index.tsx                # plugin entry, registers QAM panel
│   ├── api/
│   │   ├── callables.ts         # typed wrappers around Decky `callable`
│   │   ├── types.ts             # interfaces matching backend models
│   │   └── errors.ts            # mirror of backend error taxonomy
│   ├── store/
│   │   ├── daemon.ts            # Zustand store: state + actions
│   │   └── selectors.ts         # derived state (capabilities, etc.)
│   ├── hooks/
│   │   ├── useDaemonState.ts
│   │   ├── useCapabilities.ts
│   │   └── useDebouncedSetter.ts
│   ├── components/
│   │   ├── DaemonStatusBadge.tsx
│   │   ├── TdpControl.tsx
│   │   ├── PresetSelector.tsx
│   │   ├── ProfileSelector.tsx
│   │   ├── ChargeThresholdControl.tsx
│   │   ├── FanAutoToggle.tsx
│   │   ├── AcIndicator.tsx
│   │   ├── DiagnosticsPanel.tsx
│   │   └── ErrorBoundary.tsx
│   ├── views/
│   │   ├── MainPanel.tsx        # default QAM view
│   │   ├── UnavailablePanel.tsx # shown when daemon is down
│   │   └── SettingsPanel.tsx    # plugin prefs
│   ├── theme/
│   │   └── tokens.ts            # spacing, colours (Deck-UI compatible)
│   └── utils/
│       ├── format.ts            # mW → W, %, etc.
│       └── logger.ts            # wraps Decky logger
├── defaults/
│   └── settings.json            # opt-in user prefs defaults
├── assets/
│   └── icon.png
└── .github/
    └── workflows/
        ├── ci.yml               # lint + typecheck + unit + bundle
        ├── release.yml          # tag → GitHub Release + Decky store PR
        └── dependency-review.yml
```

---

## 6. Backend architecture (Python)

The backend is a single `Plugin` class instance per Decky session, as
required by the Decky framework. Inside it, four collaborators do the
heavy lifting.

### 6.1 `DaemonClient` (D-Bus wrapper)

- **Library**: [`dbus-next`](https://pypi.org/project/dbus-next/) —
  pure Python, asyncio-native, no GLib dependency. Decky's Python
  backend runs an asyncio loop; `dbus-next` plugs in cleanly.
  Fallback considered: `jeepney` (also pure Python). `dbus-next` wins
  on ergonomics (typed proxies, signal subscription helpers).
- **Single connection**: created on `_main()`, held for the plugin's
  lifetime. Reconnect with exponential back-off (1s → 30s cap) on
  drop, log every state transition.
- **Bus selection**: production always connects to the **system bus**
  (§4.1). For local development and integration testing against the
  daemon's `--features simulator` build (session bus + polkit
  bypass), the client honours the env var `HPD_DECKY_SIMULATOR=1`
  to switch to the session bus. The variable is read once in
  `_main()`; no runtime switching mid-session.
- **Typed proxy**: an introspected proxy object exposing
  `set_spl`, `set_preset`, `set_charge_threshold`, `set_profile`,
  `set_fan_auto`, `get_hardware_limits`, `is_ac_connected`, plus
  property getters.
- **Signal subscription**: registers a single handler for
  `org.freedesktop.DBus.Properties.PropertiesChanged` on the
  interface; updates `StateCache` and notifies `EventBridge`.
- **Timeout policy**: every method call has a 5-second timeout.
  Anything longer is a hung daemon; surface as a typed error.

### 6.2 `HealthMonitor`

Distinguishes four states the daemon can be in. State names are
chosen to align 1:1 with the corresponding error kinds in §10
(`NotRunning` ↔ `DaemonNotRunning`, etc.) so a future maintainer
never has to translate between two naming systems.

| State          | How we detect                                                                  |
|----------------|--------------------------------------------------------------------------------|
| `NotInstalled` | systemd unit `hpd.service` is missing AND `/usr/local/bin/hpd-daemon` is missing |
| `NotRunning`   | unit file exists (or binary is present) but `systemctl is-active hpd.service` returns inactive/failed |
| `Running`      | unit is active AND D-Bus name `dev.cirodev.hpd.PowerDaemon1` is owned          |
| `Unreachable`  | bus name is owned but a recent method call timed out / errored, OR no polkit agent is reachable |

The monitor:
1. On plugin start, runs a one-shot probe.
2. Subscribes to `org.freedesktop.DBus.NameOwnerChanged` on the bus
   to detect daemon start/stop without polling.
3. If the bus connection itself drops, falls back to a 5-second poll
   loop until reconnected, then re-subscribes.

Output is a `HealthSnapshot` dataclass pushed to the frontend any
time it changes:

```python
@dataclass(frozen=True)
class HealthSnapshot:
    state: Literal["NotInstalled", "NotRunning", "Running", "Unreachable"]
    daemon_version: Optional[str]         # see §19.2 — sidecar VERSION file in v1
    last_error: Optional[str]
    checked_at: datetime
```

### 6.3 `StateCache`

In-memory snapshot of:
- Hardware limits (`spl_min_w`, `spl_max_w`, `sppt_max_w`,
  `fppt_max_w`) — fetched once on first successful connect,
  re-fetched on reconnect.
- All four `PropertiesChanged`-emitting properties (`CurrentSpl`,
  `ActiveProfile`, `ChargeEndThreshold`, `AutoCooling`).
- Last `IsAcConnected` value — **polled** on a 10 s cadence
  (separate `asyncio.Task`) until a daemon-side signal lands. See
  the explicit "not emitting" warning in §4.3 and the work item in
  §27.

The cache is the **single source of truth** the frontend reads from
via `get_state()`. Direct D-Bus calls happen for: (a) mutations,
(b) the initial fetch, and (c) the AC poll task — but **never for
routine reads of the four signalling properties**, because the
daemon emits `PropertiesChanged` for every change there.

### 6.4 `EventBridge`

Decky exposes `decky.emit(event, payload)` to push asynchronous
events from the Python backend to the React frontend. The bridge:
- Throttles bursts: coalesces multiple `PropertiesChanged` within a
  100 ms window into one frontend event.
- Tags every event with a monotonic sequence number so the frontend
  can drop out-of-order updates if any.
- Emits two event channels:
  - `hpd:state` — full `StateSnapshot` (state, limits, capabilities)
  - `hpd:health` — `HealthSnapshot` changes only

### 6.5 Settings (plugin-local)

Stored via Decky's `Settings` helper (a JSON file under
`~/homebrew/settings/hpd-decky-plugin/`):
- `rememberPresetAcrossReboots: bool` (default `false`)
- `rememberCharge: bool` (default `false`)
- `confirmTdpAbove: number | null` (default 25 W; warn if user drags
  the slider above this)
- `theme: 'auto' | 'light' | 'dark'` (default `auto`)

These are **plugin preferences**, distinct from daemon state. The
daemon owns hardware state; the plugin owns "how do I want to
present it to you".

---

## 7. Frontend architecture (TypeScript/React)

### 7.1 Type system

Everything that crosses the IPC boundary has a TypeScript interface
matching a Python dataclass. They are kept in sync by a single
authoring document — `docs/COMPATIBILITY.md` — listing every type and
its versioning rule. No code generation; the surface is small enough
that manual mirroring is cheaper than tooling.

Illustrative shape (not implementation). All power fields are in
**whole watts** to match the D-Bus contract (§4.2) — no `Mw`
suffixes because the wire format is watts.

```ts
// src/api/types.ts
export type HealthState =
  | 'NotInstalled'
  | 'NotRunning'
  | 'Running'
  | 'Unreachable';

export interface HardwareLimits {
  splMinW: number;
  splMaxW: number;
  spptMaxW: number;
  fpptMaxW: number;
}

export interface DaemonState {
  currentSplW: number;
  activeProfile: string;     // opaque — never branch on the value
  chargeEndThreshold: number; // 20..=100
  autoCooling: boolean;
  isAcConnected: boolean | null; // null until first poll completes
}

export interface Capabilities {
  canSetTdp: boolean;        // limits known AND daemon reachable
  canSetCharge: boolean;
  canSetProfile: boolean;
  canSetFanAuto: boolean;
  // Presets are pinned to the daemon's stable contract:
  // ['eco', 'balanced', 'max'] for hpd ^1.0. Hard-coded against the
  // pinned daemon major (§13). The daemon does not enumerate them.
  supportedPresets: readonly string[];
  // Profiles are NOT enumerable in hpd 1.0 — see §13. v1 plugin
  // ships an empty array (dropdown disabled) until the daemon adds
  // `GetSupportedProfiles()` (§27).
  supportedProfiles: readonly string[];
}

export interface StateSnapshot {
  health: HealthState;
  limits: HardwareLimits | null;
  state: DaemonState | null;
  capabilities: Capabilities;
  lastError: PluginError | null;
}
```

### 7.2 State store

[Zustand](https://github.com/pmndrs/zustand) — already used by many
Decky plugins, ~1 KB, no provider tree, plays well with React 18's
concurrent rendering. Single store, slices:

- `health`, `state`, `limits`, `capabilities` — populated from the
  `hpd:state` / `hpd:health` events.
- `actions` — async thunks that wrap typed callable wrappers and
  handle optimistic updates + rollback on error.
- `errors` — bounded queue of the last 10 user-facing errors for
  the diagnostics panel.

Selectors live in `store/selectors.ts` and **derive** capabilities
from the state — never store derived data directly.

### 7.3 Hooks layer

Components never import the store directly. They go through hooks
that expose narrow slices:

- `useDaemonHealth()` → `HealthState` + retry callback.
- `useTdp()` → `{ value, min, max, enabled, set(mw) }`.
- `useProfile()` → `{ value, options, enabled, set(name) }`.
- `useChargeThreshold()` → `{ value, enabled, set(pct) }`.
- `useFanAuto()` → `{ value, enabled, toggle() }`.
- `useAcConnected()` → `boolean | null` (null = unknown).
- `useDebouncedSetter(fn, delayMs)` — generic helper.

The `enabled` field is the **capability guard** — components must
honour it (§13).

### 7.4 Components

All components are pure presentational React. Layout uses
`@decky/ui` primitives (`PanelSection`, `SliderField`, `ToggleField`,
`DropdownItem`, `ButtonItem`, `NotchLabel`, `Field`) so the plugin
inherits Steam Deck UI styling automatically (large hit targets,
controller focus, light/dark themes).

Component tree at a glance:

```
<MainPanel>
  ├── <DaemonStatusBadge />
  ├── <AcIndicator />
  ├── <Field label="Power">
  │     ├── <PresetSelector />
  │     └── <TdpControl />
  ├── <Field label="Cooling">
  │     ├── <ProfileSelector />
  │     └── <FanAutoToggle />
  ├── <Field label="Battery">
  │     └── <ChargeThresholdControl />
  └── <DiagnosticsPanel collapsed />

<UnavailablePanel>          // swapped in when health != Running
  ├── <DaemonStatusBadge />
  ├── <reason text>
  └── <action buttons: Retry / Open Docs>
```

---

## 8. Communication patterns

### 8.1 Frontend → backend (RPC)

Through Decky's `callable` mechanism, the frontend invokes:

| Callable                  | Purpose                                  | Validation                                       |
|---------------------------|------------------------------------------|--------------------------------------------------|
| `getSnapshot()`           | Pull cached state on mount / focus       | none                                             |
| `refresh()`               | Force a re-probe + re-fetch              | none                                             |
| `setSpl(watts)`           | Forward to daemon                        | `splMinW ≤ watts ≤ splMaxW` against cached limits |
| `setPreset(name)`         |                                          | `name ∈ supportedPresets` (pinned `eco`/`balanced`/`max` for `^1.0`) |
| `setProfile(name)`        |                                          | `name ∈ supportedProfiles` IF non-empty; otherwise the dropdown is disabled and this callable is unreachable from the UI |
| `setChargeThreshold(pct)` |                                          | `20 ≤ pct ≤ 100`                                 |
| `setFanAuto()`            | No args — re-enables auto-cooling        | none (to turn auto OFF, call `setProfile`)       |
| `getDiagnostics()`        | Return logs + health for the panel       | none                                             |

Every callable returns a discriminated-union result:
`{ ok: true, value: T } | { ok: false, error: PluginError }`.
No exceptions cross the IPC boundary — they become typed errors.

> **Silent rejection caveat.** Because the daemon's D-Bus methods
> return `Ok` on enqueue (§4.2), `{ ok: true }` from a mutating
> callable only guarantees "the daemon accepted the transition for
> processing", not "the value was applied". The plugin must wait
> for the matching `PropertiesChanged` echo (timeout 2 s) before
> committing the optimistic UI value — see §12.5.

### 8.2 Backend → frontend (events)

Two channels:
- `hpd:state` — full `StateSnapshot`. Coalesced (100 ms window).
- `hpd:health` — `HealthSnapshot`. Sent only on change.

The frontend store listens to both and merges. Components re-render
through Zustand's normal change detection.

### 8.3 Backend → daemon (D-Bus)

- **Reads**: only on bootstrap and on reconnect. Routine state is
  always served from the cache populated by `PropertiesChanged`.
- **Writes**: each one is a single D-Bus method call with a 5 s
  timeout. The daemon's polkit prompt is what gates the call —
  the plugin does not retry past a polkit denial (a denial is a
  user decision, not an error to recover from).

### 8.4 Debouncing and coalescing

- TDP slider: 250 ms trailing debounce. The user dragging from
  10 → 25 W generates one `SetSpl(25 W)` call, not fifteen.
- Charge threshold: 250 ms trailing debounce.
- Toggles and dropdowns: fire immediately (no race risk; one-shot
  intent).

---

## 9. State detection and lifecycle

The plugin handles four daemon situations distinctly:

### 9.1 `NotInstalled`

Detected by absence of both the systemd unit and the binary. The UI
renders `<UnavailablePanel>` with:
- A short explanation: "The hpd daemon is not installed on this
  device."
- A button that copies the install command for the operator's
  distro (Arch / non-Arch detected from `/etc/os-release`).
- A link to `docs/INSTALL.md`.

No controls are mounted. No D-Bus calls are attempted.

### 9.2 `NotRunning`

Unit exists but is inactive. UI renders `<UnavailablePanel>` with:
- "The hpd daemon is installed but not running."
- A "Start daemon" button — but this **only** copies a system-bus
  command for the user to paste into a terminal
  (`sudo systemctl start hpd` + optional
  `sudo systemctl enable hpd` to enable at boot). The plugin does
  **not** invoke `pkexec systemctl` itself. Auto-starting a root
  service from a user plugin is a footgun, and `hpd.service` is a
  system unit, not a `--user` unit.

### 9.3 `Running` (happy path)

Full `<MainPanel>` UI. Limits and state fetched, properties
subscribed, capabilities derived.

### 9.4 `Unreachable`

The daemon owns the bus name but a method call timed out, raised an
unexpected error, or the polkit infrastructure is missing. The UI
shows the last known cached state (greyed out, marked stale) and a
banner explaining the situation, with a retry button. Mutations are
disabled until reachability is restored.

### 9.5 Plugin lifecycle hooks

| Decky lifecycle  | Plugin action                                                              |
|------------------|----------------------------------------------------------------------------|
| `_main`          | Build collaborators, do first probe, subscribe to bus, start emitter.      |
| `_unload`        | Cancel subscriptions, close the D-Bus connection, flush pending writes.    |
| `_migration`     | Run settings-schema migrations (see §16).                                  |
| frontend `onActivate` | Push current snapshot to the QAM panel; resume animations if any.     |
| frontend `onDeactivate` | Pause non-essential timers; keep store hot.                          |

---

## 10. Error handling

A single typed taxonomy crosses every layer:

```ts
type PluginError =
  | { kind: 'DaemonNotInstalled' }
  | { kind: 'DaemonNotRunning' }
  | { kind: 'DaemonUnreachable'; detail: string }
  | { kind: 'PolkitDenied'; action: string }
  | { kind: 'PolkitUnavailable' }            // no agent running
  | { kind: 'InvalidValue'; field: string; reason: string }
  | { kind: 'OutOfRange'; field: string; min: number; max: number; got: number }
  | { kind: 'SilentRejection'; field: string; sentValue: unknown }
  | { kind: 'Timeout'; operation: string; timeoutMs: number }
  | { kind: 'BackendCrash'; detail: string }
  | { kind: 'Unknown'; detail: string };
```

> `UnsupportedProfile` was intentionally **not** included: the
> daemon's `ProfileName::FromStr` parses any string into
> `ProfileName::Custom(s)` and never fails, so an "unsupported
> profile" error can only be raised by the *plugin's own*
> client-side validation, which collapses into `InvalidValue`. The
> taxonomy stays smaller for it.
>
> `SilentRejection` is the daemon-side counterpart to "the D-Bus
> method returned `Ok` but the value never echoed back as
> `PropertiesChanged`" (§4.2). The plugin synthesises this error
> after the 2 s post-write echo window expires with no matching
> change.

Rules of the taxonomy:

1. **One shape per cause.** No string-matching downstream.
2. **All user-presented messages** live in a single
   `src/utils/errorMessages.ts` keyed by `kind`. i18n-ready even if
   the v1 ships English only.
3. **Logged structurally.** Backend logs the dataclass mirror; the
   frontend logger serialises the discriminated union with its
   discriminator.
4. **Recovery hint per kind.** Each error has an associated
   "try this" — surfaced in the diagnostics panel:
   - `DaemonNotRunning` → "Run `sudo systemctl start hpd`."
   - `PolkitUnavailable` → "Start a polkit agent (KDE: shipped;
     Hyprland: `/usr/lib/polkit-kde-authentication-agent-1 &`)."
   - `OutOfRange` → "This device supports {min}–{max} W."
5. **The daemon's `no` is final.** A 4xx-equivalent (validation,
   polkit denied) is **not** retried automatically. A 5xx-equivalent
   (timeout, transport) is retried once with back-off, then surfaced.

---

## 11. Authorization flow (polkit)

The plugin **never** talks to polkit directly. The daemon does that
on every privileged setter (see `crates/hpd-dbus/src/polkit.rs`).
The user-visible flow is:

1. User changes a control in the QAM.
2. Plugin backend issues the D-Bus method call.
3. Daemon's `polkit::check(...)` calls `PolicyKit1.Authority`.
4. The user's polkit agent shows a password prompt (or, for cached
   `auth_admin_keep`, silently approves within the 5-minute window).
5. Daemon proceeds (success) or returns the polkit-denied error.

Plugin responsibilities:

- **Verify a polkit agent is running** during bootstrap. The
  `HealthMonitor` checks for an owner of
  `org.freedesktop.PolicyKit1.AuthenticationAgent` on the bus. If
  none is present, surface `PolkitUnavailable` with the agent-start
  hint for the user's WM (KDE, GNOME, Hyprland, Sway, Cinnamon
  documented in `TROUBLESHOOTING.md`).
- **Cache duration awareness**: `set-profile` is `auth_admin_keep`
  (5 min). The plugin can show a subtle "auth cached for 5:00" UI
  hint after a successful profile change, with a countdown.
- **Never assume a setter succeeded.** Always wait for the daemon's
  reply before updating the optimistic UI; on error, roll the UI
  back to the previous value.

---

## 12. UI / UX specification

### 12.1 Layout (QAM panel)

```
┌─ HPD ───────────────────────────────── ⚠/✓ ─┐
│ ┌─────────────────────────────────────────┐ │
│ │ Daemon: running · AC: plugged in        │ │   ← DaemonStatusBadge + AcIndicator (AC may lag up to 10 s, §6.3)
│ └─────────────────────────────────────────┘ │
│                                              │
│ Power                                        │
│ Preset       [ Balanced ▾ ]                  │   ← Dropdown bound to supportedPresets (eco/balanced/max)
│ TDP          ──────●──────────  20 W         │   ← Slider, min/max from cached limits (whole watts)
│                                              │
│ Cooling                                      │
│ Profile      [ Performance ▾ ]               │   ← v1: single-option (current) until GetSupportedProfiles() lands
│ Auto fan     [●─── ]                         │   ← Toggle ON = SetFanAuto(); OFF = pick a profile manually
│                                              │
│ Battery                                      │
│ Charge to    ────●──────  80 %               │   ← 20–100 daemon-enforced; plugin warns below 60%
│                                              │
│ [▾ Diagnostics]                              │   ← Collapsed by default
└──────────────────────────────────────────────┘
```

### 12.2 Capability-driven rendering

For every control:

| Control                | Disabled when…                                                                |
|------------------------|-------------------------------------------------------------------------------|
| TDP slider             | `limits == null` OR `splMin == splMax` OR health ≠ Running                    |
| Preset dropdown        | `supportedPresets.length === 0`                                               |
| Profile dropdown       | `supportedProfiles.length === 0`                                              |
| Charge threshold       | health ≠ Running                                                              |
| Auto-fan toggle        | health ≠ Running                                                              |
| All setters            | `lastError.kind === 'PolkitUnavailable'`                                      |

"Disabled" means visually de-emphasised, not interactive, with a
tooltip explaining why ("Hardware limits unknown — retry needed").
**Never** show a control with a fabricated default value.

### 12.3 Empty / loading states

- **Loading** (first 500 ms): controls render with a shimmer/skeleton,
  not zeros.
- **No data** (after timeout): controls render disabled with an
  explanatory note; never with `0 W` or `quiet` as a stand-in.

### 12.4 Confirmation prompts

A confirmation modal appears when:
- TDP slider crosses `settings.confirmTdpAbove` (default 25 W).
- Charge threshold is set below 60 %. **The daemon allows down to
  20 %**; the plugin only *warns* (does not clamp) below 60 % and
  shows a one-liner explaining that very low thresholds shorten
  realised battery capacity. The user can always proceed.

### 12.5 Optimistic updates with rollback

The "round-trip" here is **two-stage** because the D-Bus method
returns `Ok` on enqueue, not on apply (§4.2):

1. **Stage 1 — enqueue ack** (≤ 5 s, the `DaemonClient` timeout).
   `{ ok: true }` from the callable means the daemon accepted the
   transition. No UI commit yet; the slider stays locked.
2. **Stage 2 — apply echo** (≤ 2 s after Stage 1).
   The plugin waits for the matching `PropertiesChanged`
   notification with the new value. Echo received → commit the
   optimistic value, unlock the control. Echo NOT received in
   2 s → surface `{ kind: 'SilentRejection', field, sentValue }`,
   revert the UI to the previous value, toast a message pointing
   the user at `journalctl -u hpd` for the rejection reason.

For toggles/dropdowns: same two-stage pattern, with the previous
value preserved in a ref until the echo lands.

### 12.6 Accessibility

- Controller focus order matches visual order.
- Every interactive element has an accessible name.
- Error banners use `role="alert"` so screen readers pick them up.
- Colour is never the only signal (icons + text accompany every
  status colour).

---

## 13. Conditional capability surfacing

This is the rule the user called out explicitly: **if the daemon
cannot reliably tell us what's supported, the control is not
enabled — full stop.**

How it's implemented:

1. On every connect, `getHardwareLimits()` is called. If it fails or
   returns a degenerate range (`splMinW >= splMaxW`),
   `canSetTdp = false` and the slider is disabled with a tooltip.
2. **Profiles** (`SetProfile` / `ActiveProfile`) are *not*
   enumerable in `hpd 1.0`. The daemon parses any string into
   `ProfileName::Custom(s)` via `FromStr`, so there is no
   client-discoverable list. v1 reads `ActiveProfile` to seed the
   dropdown with the current value; future daemon versions are
   expected to expose `GetSupportedProfiles()` (see §27 — Future
   Work). Until that method exists, the dropdown shows
   `[ ActiveProfile ]` as the only option with a notice
   "Profile switching disabled — daemon does not yet enumerate
   supported profiles", and the operator can change them via
   `hpdctl` directly. Honest > fake.
3. **Presets** (`SetPreset`) are different: they are a *closed
   enum* in the daemon's stable contract — `eco` / `balanced` /
   `max` — frozen for the whole `1.x` line per §4.5. The plugin
   therefore hardcodes `supportedPresets = ['eco', 'balanced',
   'max']` for `^1.0` (not "wait for a discovery method"). If a
   future minor adds a new preset, the plugin will bump its
   `hpdDaemonCompat` minimum and update the constant in the same
   release. This is the **one exception** to the "never hard-code"
   rule, justified by the contract-stability promise.
4. **AC indicator**: there is no `PropertiesChanged` signal for
   `IsAcConnected`; the plugin polls at 10 s. If three consecutive
   polls fail, the indicator switches to "AC: unknown" rather than
   freezing the last value (§4.3 warning).

This is the **single most important design rule** and is encoded as
a lint at code-review time: any new control PR must answer the
question "what disables this?" in the description.

---

## 14. Resource management and performance

| Concern                  | Strategy                                                                  |
|--------------------------|---------------------------------------------------------------------------|
| D-Bus connections        | Exactly one per plugin lifetime. Reuse the same `MessageBus` instance.    |
| Signal subscriptions     | Subscribe once on connect (`PropertiesChanged` + `NameOwnerChanged`), unsubscribe on `_unload`. |
| Polling                  | One narrow exception: `IsAcConnected()` at 10 s (§4.3 — daemon does not emit a signal for AC). Plus reconnect back-off (1 s → 30 s) when the bus drops. |
| Memory                   | Cache is bounded (4 limits + 4 signalling properties + 1 AC value + 1 health). No history kept. |
| CPU (background)         | Idle: ~0%. Asyncio event loop + the 10 s AC poll tick; everything else is signal-driven. |
| CPU (slider drag)        | Debounce + coalesce — at most 4 D-Bus calls/sec during continuous drag.   |
| Frontend re-renders      | Zustand selectors + `React.memo` on field components.                     |
| Bundle size              | <250 KB gzipped (Deck has constrained CEF heap). Tree-shake `@decky/ui`.  |
| Logs                     | Rolling 1 MB per channel under `~/homebrew/logs/hpd-decky-plugin/`.       |

---

## 15. Security considerations

- **No `pkexec` / `sudo` calls** in plugin code. Every privileged
  operation goes through the daemon's polkit-gated D-Bus method,
  which is the audited path.
- **No filesystem writes outside `~/homebrew/settings/` and
  `~/homebrew/logs/`**. The plugin never touches `/etc`, `/var`,
  `/sys`.
- **No subprocess execution** other than a small, audited allow-list:
  - `systemctl is-active hpd.service`
  - `systemctl list-unit-files hpd.service`
  - reading `/etc/os-release` (file read, no subprocess strictly speaking)
  - reading `/usr/share/hpd/VERSION` (daemon-shipped sidecar — see §19.2)

  All are read-only. **`journalctl` is intentionally NOT in the
  allow-list**: the plugin must never depend on journal read
  permissions (which require `systemd-journal` group membership and
  vary across distros). Any new subprocess invocation requires a
  CODEOWNERS-blessed review.
- **Input validation at three layers**: TS UI (UX guard), Python
  backend (boundary check), daemon (authoritative). Don't trust the
  layer below; don't depend on the layer above.
- **No remote URLs hit at runtime**. The plugin works fully
  offline; documentation links open the user's browser, the plugin
  itself fetches nothing.
- **Supply chain**: `npm audit --omit=dev` clean in CI; Python deps
  pinned with hashes; both run through GitHub's
  `dependency-review-action` on every PR.
- **Licence**: GPL-3.0-or-later (matches daemon). All bundled deps
  must be GPL-compatible (a `licenses-check` CI job mirrors the
  daemon's `deny.toml` allow-list).
- **Secrets**: none. The plugin has no API keys, no tokens, no
  remote state.

---

## 16. Configuration and settings

Two distinct configuration surfaces:

### 16.1 User-facing plugin preferences (`SettingsPanel`)

Persisted via Decky's `Settings` module. Schema is **versioned** in
the file:

```json
{
  "version": 1,
  "rememberPresetAcrossReboots": false,
  "rememberCharge": false,
  "confirmTdpAbove": 25,
  "theme": "auto"
}
```

A `migrate(from, to)` function in `backend/settings.py` handles
schema bumps. Migrations are idempotent and never destructive.

### 16.2 Daemon configuration

The daemon's `/etc/hpd/config.toml` is **out of scope** for the
plugin. The plugin does not read, write, or display it — operators
edit it directly. The plugin shows a hint in the diagnostics panel:
"To change advanced daemon settings, edit `/etc/hpd/config.toml` and
run `sudo systemctl reload hpd`."

This separation keeps the plugin from owning a configuration model
that the daemon already owns.

---

## 17. Logging

- **Backend**: uses Decky's logger (`decky.logger.info/warn/error`).
  Structured fields where the logger supports them, otherwise
  JSON-encoded strings. Levels: `DEBUG` only when
  `HPD_DECKY_DEBUG=1`; `INFO` for state transitions; `WARN` for
  recoverable errors; `ERROR` for surfaced-to-user errors.
- **Frontend**: thin wrapper around `console` + a ring buffer of
  the last 200 log lines, displayable in the Diagnostics panel
  (no remote shipping).
- **Daemon logs**: the diagnostics panel links to
  `journalctl -fu hpd` with a one-shot "copy command" button.

---

## 18. Testing strategy

### 18.1 Unit tests

- **Python**: `pytest` + `pytest-asyncio`. Mock `dbus-next` proxies.
  Cover every error branch in `DaemonClient`, every transition in
  `HealthMonitor`, every coalesce path in `EventBridge`.
- **TypeScript**: `vitest` + `@testing-library/react`. Cover
  selectors, hooks, components rendered against a mocked store.
  Snapshot the disabled-state matrix from §13.

### 18.2 Integration tests

A `tests/integration/` harness that spins up the daemon's
**simulator** (`HPD_SIMULATOR=1`, session bus, no polkit) and runs
the plugin backend against it. Verifies the round trip for every
callable.

### 18.3 End-to-end smoke

A documented manual checklist (`docs/QA_CHECKLIST.md`) maintainers
walk through before each release on a real Xbox Ally X:
- Boot, plugin loads, daemon detected.
- Each control round-trips and persists.
- Polkit prompt appears once for `set-profile`, then cached 5 min.
- Suspend/resume re-applies state and the UI reflects new
  `PropertiesChanged`.
- Unload plugin while daemon is running — no orphan subscriptions
  (`busctl --system tree dev.cirodev.hpd.PowerDaemon1` shows no
  stale clients).

### 18.4 Compatibility matrix

CI runs the integration suite against:
- Daemon `1.0.0` (pinned).
- Daemon `main` HEAD (informational only — green is nice, red
  doesn't block plugin merge).

---

## 19. Versioning and compatibility

### 19.1 Plugin SemVer

The plugin follows SemVer independently of the daemon:
- `MAJOR` bump on breaking UI/IPC contract changes.
- `MINOR` bump on new features.
- `PATCH` for bug fixes.

### 19.2 Daemon compatibility declaration

`plugin.json` carries:

```json
{
  "hpdDaemonCompat": ">=1.0.0 <2.0.0"
}
```

On startup, `DaemonClient` discovers the daemon version via the
sidecar text file **`/usr/share/hpd/VERSION`** (single line,
`X.Y.Z`), written by `install.sh` and packaged with the daemon.
This is the resolution of Open Question 1 in §28 — chosen over
parsing `journalctl` because that path requires journal read
permissions the plugin must not depend on (§15). A future daemon
release may add a proper `GetVersion()` D-Bus method (§27); when it
lands the plugin prefers the method and falls back to the sidecar.

On mismatch:

| Situation                       | Behaviour                                                                  |
|---------------------------------|----------------------------------------------------------------------------|
| Daemon < min compat            | `<UnavailablePanel>` with "Plugin requires hpd ≥ {min}. Installed: {got}." |
| Daemon ≥ next major            | Warning banner, UI degrades to read-only mode.                              |
| Version unknown (file missing) | Warning banner ("Daemon version unknown — sidecar `/usr/share/hpd/VERSION` not present; full UI enabled with caveat"). |

### 19.3 Documented compatibility matrix

`docs/COMPATIBILITY.md` lists every released plugin version × daemon
version pair and the expected outcome. Updated on every release.

---

## 20. Plugin lifecycle (install → uninstall)

| Phase     | What happens                                                                                  |
|-----------|------------------------------------------------------------------------------------------------|
| Install   | User installs Decky Loader → finds plugin in store OR side-loads via "Install from URL".      |
| First run | `_main` probes daemon. If `NotInstalled`, the `<UnavailablePanel>` shows install instructions.|
| Update    | Decky pulls new release; plugin's `_migration` runs settings schema bump if needed.            |
| Disable   | Decky calls `_unload`; plugin tears down subscriptions, closes D-Bus connection.               |
| Enable    | `_main` again; cache is cold and re-warms on first frontend `getSnapshot()`.                   |
| Uninstall | Decky removes plugin dir. Settings JSON is **kept** by default; a "Reset settings" button     |
|           | in the SettingsPanel deletes it on demand.                                                     |

---

## 21. Build and distribution

### 21.1 CI

Mirrors the daemon's discipline:

| Job                | Tools                                                              |
|--------------------|--------------------------------------------------------------------|
| `lint-frontend`    | `eslint`, `prettier --check`, `tsc --noEmit`                       |
| `lint-backend`     | `ruff`, `black --check`, `mypy --strict backend/`                  |
| `test-frontend`    | `vitest run`                                                       |
| `test-backend`     | `pytest`                                                           |
| `bundle`           | `pnpm run build`; verifies the output bundle exists and ≤ 250 KB   |
| `integration`      | spins up `hpd-daemon --features simulator`; runs the test harness  |
| `licenses-check`   | parses `package.json` and `pyproject.toml`; rejects non-allowlisted licences |
| `dependency-review`| GitHub's `dependency-review-action` blocks high-severity advisories|

### 21.2 Release pipeline

Modelled on the daemon's tag-driven `release.yml`:

1. Tag `vX.Y.Z` (or `vX.Y.Z-rc.N`).
2. CI runs the full suite.
3. On green, `release.yml` packages the Decky bundle (zip + checksum
   + optional GPG signature) and creates a GitHub Release.
4. For stable tags, an opt-in workflow opens a PR against the
   official [`SteamDeckHomebrew/decky-plugin-database`](https://github.com/SteamDeckHomebrew/decky-plugin-database)
   so the plugin is published to the Decky store (subject to their
   review).
5. The release notes are auto-extracted from `CHANGELOG.md`.

### 21.3 Side-load path (no Decky store)

Operators can install from a tarball URL via Decky's "Install from
URL" feature, pointing at the GitHub Release asset directly. This is
the path documented for early adopters before store approval.

---

## 22. Installation and onboarding (user docs)

`docs/INSTALL.md` walks the operator through the **full happy path**:

1. **Confirm hardware**: ASUS ROG Ally / Ally X / Xbox Ally X
   (board names `RC71L` / `RC72LA` / `RC73XA`).
2. **Install the daemon** first (see daemon's `README.md`). Verify
   `systemctl is-active hpd` returns `active`. Verify `hpdctl status`
   prints state.
3. **Install Decky Loader**:
   - **SteamOS**: `curl -L https://github.com/SteamDeckHomebrew/decky-loader/releases/latest/download/install_release.sh | sh`
   - **CachyOS / Arch**: `paru -S decky-loader` (AUR) or the
     non-SteamOS script: `curl -L .../install_nonsteamdeck.sh | sh`
   - **Other distros**: the non-SteamOS script + manual
     `decky-loader.service` setup.
4. **Verify Decky is running**: open Steam → press `…` (QAM) →
   the plug icon should appear. If not, see Decky's troubleshooting.
5. **Install the plugin**:
   - **Via store** (when published): Decky → Store → search "hpd"
     → Install.
   - **Side-load**: Decky → Settings → "Install from URL" → paste
     the GitHub Release `.zip` URL.
6. **First-run authorization**: changing the platform profile
   triggers the first polkit prompt. Enter your sudo password. The
   prompt won't reappear for 5 minutes per the cache policy.

A second guide, `docs/QUICK_START.md`, is a one-screen "you have hpd
and Decky, now what" — opening the panel, setting a TDP, picking a
profile.

---

## 23. Troubleshooting guide (`docs/TROUBLESHOOTING.md`)

Structured by symptom, not by component. Each entry follows
**symptom → why → fix → verify**.

| Symptom                                                | Most likely cause                                        |
|--------------------------------------------------------|----------------------------------------------------------|
| Plugin shows "Daemon not installed"                    | `hpd.service` unit file missing.                         |
| Plugin shows "Daemon installed but not running"        | Unit exists but stopped — service crashed or disabled.   |
| Plugin shows "Daemon unreachable"                      | D-Bus name not owned: daemon crashed mid-life; or D-Bus broker stuck. |
| Polkit dialog never appears                            | No polkit agent running (esp. Hyprland / Sway).          |
| Polkit dialog appears, accepts password, then "Authentication failed" | User not in `wheel` (or whatever group the policy file allow-lists). |
| Profile dropdown shows only one option                 | Daemon version doesn't yet expose `GetSupportedProfiles()`. |
| TDP slider disabled                                    | `GetHardwareLimits()` failed; daemon log will show why.  |
| Slider moves but value doesn't stick                   | Daemon is rolling back on hardware-write failure — check `journalctl -u hpd`. |
| Plugin doesn't appear in Decky's plugin list           | Bundle malformed; check Decky's logs (`~/homebrew/services/PluginLoader.log`). |
| Changes from `hpdctl` don't reflect in the UI          | `PropertiesChanged` subscription dropped — restart plugin via Decky's "Reload". |
| QAM panel renders blank                                | Frontend bundle failed to load — open Steam → CEF Devtools (URL `http://localhost:8081`). |

Each entry expands to:
- The exact log lines to look for (`journalctl -u hpd`,
  `~/homebrew/logs/...`).
- A `Try this:` block with copy-paste-ready commands.
- A `Still broken?` link to opening a GitHub issue with a template
  pre-filled with the diagnostics.

---

## 24. "Decky Loader not installed" — handling

The plugin itself **cannot** detect this state because it only runs
**within** Decky Loader. The handling lives in the **user docs**:

- `docs/INSTALL.md` §3 is dedicated to Decky Loader installation
  for every supported scenario (SteamOS, CachyOS/Arch, generic
  Linux).
- `README.md` opens with a "Prerequisites" section listing Decky
  Loader as the first requirement.
- The GitHub Release asset's description includes a one-line "If
  you haven't installed Decky Loader yet, start here: [link]".

If a user side-loads the plugin's `.zip` without Decky, the file
simply isn't actionable — there's no entrypoint Steam would run. The
docs are the prevention.

---

## 25. "Plugin not installed" — handling

This is, by definition, the state where there is no plugin code to
run. The handling is the **daemon's** existing `hpdctl` CLI: it
remains a first-class control surface, fully functional without the
plugin. The plugin is a UX layer, not a dependency.

The daemon's `README.md` already documents `hpdctl`; the plugin's
`README.md` cross-links to it as the "without-GUI alternative".

---

## 26. Updates

### 26.1 Plugin updates (via Decky)

- Decky's update mechanism polls the configured plugin source
  (store or side-load URL) and offers an update prompt.
- The plugin's `_migration(from_version, to_version)` hook runs on
  load if the persisted version differs. Settings schema migrations
  live here; D-Bus contract changes are pre-flighted by the
  compatibility check (§19).

### 26.2 Daemon updates (independent)

The daemon updates through the OS package manager (`pacman -Syu`,
AUR) or `./install.sh`. The plugin watches `NameOwnerChanged`:
when the daemon restarts (drop + re-acquire), the plugin clears the
cache, re-fetches limits, and resubscribes.

A subtle case: if the daemon's major version bumps during an
update, the plugin's compatibility check will surface the
incompatibility on the very next `NameOwnerChanged`. UI degrades
gracefully (read-only banner + upgrade hint).

---

## 27. Future work (out of scope for v1)

- **Daemon work items the plugin would benefit from (file as
  daemon issues, not plugin issues):**
  - **`is_ac_connected` should emit `PropertiesChanged`.** Today
    only four properties emit (§4.3); adding AC to
    `spawn_properties_changed_emitter` would let the plugin drop
    its 10 s poll (§14). Highest priority — pure win, no contract
    break.
  - **`GetVersion()` D-Bus method** so the plugin can stop reading
    the `/usr/share/hpd/VERSION` sidecar (§19.2).
  - **`GetSupportedProfiles()`** so the profile dropdown can offer
    real options instead of degrading to "current only" (§13).
  - **`GetSupportedPresets()`** is intentionally NOT listed —
    presets are a closed enum (§13) and hardcoding `eco`/`balanced`/`max`
    is the correct call.
- **Multi-vendor support in the plugin.** Already designed for —
  capabilities drive UI, not vendor flags — but exercised only when
  a second backend lands in the daemon.
- **Per-game profiles** (auto-switch TDP/profile when a specific
  Steam game launches). Requires Decky's Steam game lifecycle hooks
  and a small per-profile store.
- **Battery health dashboard**: historical charge cycles, derived
  from `journalctl -u hpd` or a future `GetBatteryHistory()`
  method.
- **i18n**: ship en + es + de + zh at a minimum; the error-message
  registry is already keyed for it.
- **OSD overlay**: while running a game, show TDP/temp ticker via
  Decky's CSS injection. Opt-in.

---

## 28. Open questions to resolve before implementation

1. **~~Daemon version discovery before `GetVersion()` exists.~~**
   **RESOLVED** (§19.2): sidecar `/usr/share/hpd/VERSION`
   shipped by the daemon's `install.sh` in a `1.0.1` patch.
   `GetVersion()` D-Bus method tracked separately in §27.
2. **Profile enumeration.** v1 plan is "show current only, disable
   switching". Acceptable for an MVP but ugly. Push for
   `GetSupportedProfiles()` in daemon `1.1.0`. (Presets are
   *not* in this category — they are a closed enum per §13.)
3. **Where to host the side-load `.zip`.** GitHub Releases is the
   default. AUR package (`hpd-decky-plugin-bin`) could mirror the
   daemon's AUR strategy if Arch users prefer pacman.
4. **Polkit agent on game-mode SteamOS.** Steam Deck Game Mode
   ships its own polkit handling for installed services. Verify the
   prompt UX with the daemon's `auth_admin_keep` action — may
   require a small UX tweak.
5. **Bundle size budget.** 250 KB gzipped is a target, not a hard
   limit. Revisit once `@decky/ui` tree-shaking is measured.
6. **Echo-window timeout for Stage 2 rollback (§12.5).** 2 s is a
   gut-feel default; the real number should come from measuring
   the longest legitimate apply path (worst case is probably an
   `ApplyPowerEnvelope` that triggers a rollback within the daemon
   — empirically <500 ms on ASUS, but TBD on future backends).

---

## 29. Appendices

### Appendix A — File-by-file ownership

(See §5 for the layout; this table calls out which file is the
single source of truth for which concern.)

| Concern                           | File                                |
|-----------------------------------|-------------------------------------|
| Daemon D-Bus contract (mirror)    | `backend/client.py`, `src/api/types.ts` |
| Error taxonomy                    | `backend/errors.py`, `src/api/errors.ts` |
| Capability rules                  | `src/store/selectors.ts`            |
| Health detection                  | `backend/health.py`                 |
| Settings schema + migrations      | `backend/settings.py`               |
| Bus name / object path constants  | `backend/client.py` (only)          |
| Polkit action IDs (consumed)      | `docs/COMPATIBILITY.md` (informational) |
| User-facing messages              | `src/utils/errorMessages.ts`        |

### Appendix B — External dependencies (intent)

**Frontend** (`package.json`):
- `react`, `react-dom` — peer-provided by Decky.
- `@decky/api`, `@decky/ui` — Decky-provided.
- `zustand` — state store.
- `clsx` — class helper.
- (Dev) `vite`, `vitest`, `@testing-library/react`, `eslint`,
  `prettier`, `typescript`.

**Backend** (`pyproject.toml`):
- `dbus-next` — async D-Bus client.
- (Dev) `pytest`, `pytest-asyncio`, `ruff`, `black`, `mypy`.

Everything else is forbidden by policy without a `licenses-check`
exemption review.

### Appendix C — Reference links

- Decky Loader: https://github.com/SteamDeckHomebrew/decky-loader
- Decky plugin template: https://github.com/SteamDeckHomebrew/decky-plugin-template
- `@decky/api` reference: https://docs.deckbrew.xyz/
- Decky plugin store: https://github.com/SteamDeckHomebrew/decky-plugin-database
- `dbus-next`: https://github.com/altdesktop/python-dbus-next
- `zbus` (daemon side): https://github.com/dbus2/zbus
- polkit docs: https://www.freedesktop.org/software/polkit/docs/latest/

---

*Last updated: 2026-05-24 — pre-implementation design, revision 2
(post-code-verification pass against `hpd-dbus/src/service.rs`,
`hpd-dbus/src/actions.rs`, `hpd-capabilities/src/{power,profile,charge}.rs`
and `hpd-daemon/src/main.rs::spawn_properties_changed_emitter`).
Revise before any code is committed.*
