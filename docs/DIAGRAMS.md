<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# hpd — Visual guide (for dummies) 🖼️

The manuals, in pictures. A picture is worth a thousand words, so here is
**how the daemon works**, **how the Decky plugin works**, and **how they
talk to each other** — with every possible combination. Spanish version:
[`DIAGRAMS-es.md`](DIAGRAMS-es.md).

> The diagrams are [Mermaid](https://mermaid.js.org): they render on their
> own in GitHub and most Markdown editors.

**Contents**

- [0. The whole idea in one picture](#0-the-whole-idea-in-one-picture)
- [1. Inside the daemon](#1-inside-the-daemon)
- [2. Inside the plugin](#2-inside-the-plugin)
- [3. Plugin ↔ daemon communication](#3-plugin--daemon-communication)
- [4. Every combination](#4-every-combination)
- [5. Master table: CLI ↔ D-Bus ↔ Plugin](#5-master-table-cli--d-bus--plugin)

---

## 0. The whole idea in one picture

**Three independent knobs.** This is the only thing you must understand:

```mermaid
flowchart LR
    subgraph KNOBS["What you control"]
        direction TB
        TDP["⚡ POWER (TDP)<br/>how many watts the chip uses<br/><i>your real limit</i>"]
        COOL["🧊 COOLING<br/>how hard the fans work<br/><i>noise ↔ temperature</i>"]
        BAT["🔋 BATTERY<br/>charge cap (e.g. 80%)<br/><i>long-term health</i>"]
    end

    TDP --> R1["= performance + heat<br/>vs battery"]
    COOL --> R2["= quiet<br/>vs cool"]
    BAT --> R3["= battery<br/>lifespan"]

    PM["🔧 (advanced) Power mode / EPP<br/>default: Performance"] -.->|"normally NOT touched"| TDP

    style TDP fill:#ffe6cc,stroke:#d79b00
    style COOL fill:#dae8fc,stroke:#6c8ebf
    style BAT fill:#d5e8d4,stroke:#82b366
    style PM fill:#f5f5f5,stroke:#999,stroke-dasharray: 4 4
```

**Mental rule:**
- More/less performance or battery? → **TDP**.
- More/less noise? → **Cooling**.
- They are **independent**: "full power + quiet fans" is valid.

---

## 1. Inside the daemon

`hpd` is a background service (root) that writes the firmware "knobs" and
exposes a D-Bus interface. Internally **everything goes through a state
machine** — nothing touches the hardware directly.

### 1.1 The flow of a command (state machine)

```mermaid
flowchart TD
    A["External event<br/>(CLI, plugin, charger, wake-up...)"] --> B["Transition<br/>(SetSpl, SetCoolingLevel, AcPowerChanged...)"]
    B --> C{{"reduce()  — PURE function<br/>validates + decides"}}
    C -->|"list of Effects"| D["Executor"]
    D --> E["Writes to /sys (ASUS backend)"]
    D --> F["Saves state to disk<br/>(/var/lib/hpd/state.toml)"]
    D --> G["watch::ProfileState<br/>(what readers see)"]
    G --> H["D-Bus emits PropertiesChanged"]
    E -->|"if the write fails"| I["Rollback:<br/>re-read hardware → Sync*"]
    I --> C

    style C fill:#fff2cc,stroke:#d6b656
    style D fill:#dae8fc,stroke:#6c8ebf
```

**For dummies:** an event becomes an "intent" (Transition), a pure
function decides what to do (touching nothing), and the Executor is the
only thing that writes hardware and saves state. If a write fails, it
re-reads the hardware so it never lies about the state.

### 1.2 The decouple: which knob writes what

```mermaid
flowchart LR
    subgraph CMD["Commands"]
        T1["hpdctl tdp set N"]
        T2["hpdctl cool set / auto"]
        T3["set_profile (advanced)"]
        T4["hpdctl charge set N"]
    end

    T1 -->|power| SPL["/sys .../ppt_pl1_spl<br/>SPPT + FPPT (boost)"]
    T2 -->|fans| FAN["/sys .../asus_custom_fan_curve<br/>(8 temp→duty points)"]
    T3 -->|EPP / power| PROF["/sys/.../platform_profile<br/>(default: performance)"]
    T4 -->|battery| CHG["/sys .../charge_control_end_threshold"]

    SPL --> POWER(["⚡ REAL POWER"])
    PROF --> POWER
    FAN --> NOISE(["🌀 RPM / temperature"])

    style POWER fill:#ffe6cc,stroke:#d79b00
    style NOISE fill:#dae8fc,stroke:#6c8ebf
    style T3 fill:#f5f5f5,stroke:#999,stroke-dasharray: 4 4
```

> 🔑 **The key change (decouple):** `cool` used to *also* move the
> `platform_profile`, which **clamps real power** (a `silent` level left
> the chip at ~13 W even if you asked for 25 W). Now `cool` only touches
> the fans; the `platform_profile` stays at `performance` so your TDP is
> the real limit.

### 1.3 Auto-cooling: the fans follow the TDP

```mermaid
flowchart LR
    TDPv["TDP you set"] --> FR{"Where does it fall in<br/>the hardware range?"}
    FR -->|"< 33%"| S["SILENT curve"]
    FR -->|"33–67%"| Bd["BALANCED curve"]
    FR -->|"> 67%"| Ag["AGGRESSIVE curve"]
    S --> note["Only changes the fan CURVE.<br/>NEVER the platform_profile."]
    Bd --> note
    Ag --> note
    style note fill:#fff2cc,stroke:#d6b656
```

### 1.4 Lifecycle (system events)

```mermaid
flowchart TD
    BOOT["🟢 Boot"] --> B1["Re-asserts the FULL saved state to hardware<br/>(TDP + profile→default performance + charge + fan curve)"]
    B1 --> B2["= exactly what the daemon reports, even after a<br/>cold boot reset firmware knobs to defaults"]

    AC["🔌 Plug in charger"] --> AC1["udev → AcPowerChanged(true)"]
    AC1 --> AC2["Saves your battery TDP,<br/>ramps to max"]
    ACU["🔋 Unplug"] --> ACU1["AcPowerChanged(false)"]
    ACU1 --> ACU2["Restores your battery TDP"]

    SLEEP["😴 Resume from suspend"] --> SL1["logind → SystemResumed"]
    SL1 --> SL2["Re-applies TDP + profile + curve + battery<br/>(firmware loses them while asleep)"]

    HUP["♻️ systemctl reload"] --> HUP1["SIGHUP → re-reads /etc/hpd/config.toml"]
    STOP["🔴 stop / Ctrl-C"] --> STOP1["Saves state and exits cleanly"]

    style BOOT fill:#d5e8d4,stroke:#82b366
    style STOP fill:#f8cecc,stroke:#b85450
```

### 1.5 Is anyone fighting over the knobs? (rivals)

```mermaid
flowchart LR
    HPD["hpd"] --> CHK{"Does another daemon write<br/>the same knobs?"}
    CHK -->|"hard rival"| R["power-profiles-daemon,<br/>steamos-manager, tuned, hhd<br/>→ doctor --fix masks them"]
    CHK -->|"advisory (reported, not touched)"| A["gamemoded, asusd,<br/>auto-cpufreq"]
    CHK -->|"clean"| OK["✅ hpd is in sole charge"]
    style R fill:#f8cecc,stroke:#b85450
    style A fill:#fff2cc,stroke:#d6b656
    style OK fill:#d5e8d4,stroke:#82b366
```

---

## 2. Inside the plugin

The Decky plugin is **a UI** in Steam's Quick Access menu. It never
touches the hardware: it asks the daemon for everything. It has three
layers.

### 2.1 The plugin's three layers

```mermaid
flowchart TB
    subgraph FE["1 · Frontend (TypeScript / React)"]
        UI["Components: TDP, Cooling, Battery,<br/>Telemetry, Curve, Conflicts, Setup"]
        HOOKS["Hooks: useTdp, useCooling,<br/>useThermalStatus, useAcConnected..."]
        STORE["Store (state snapshot)"]
        UI --> HOOKS --> STORE
    end
    subgraph BE["2 · Backend (Python, main.py)"]
        BRIDGE["EventBridge + D-Bus client<br/>+ AC poll + snapshot push"]
    end
    subgraph DA["3 · Daemon (hpd, root)"]
        DBUS["dev.cirodev.hpd.PowerDaemon1"]
    end

    HOOKS -->|"@decky callable"| BRIDGE
    BRIDGE <-->|"D-Bus system bus<br/>(polkit)"| DBUS
    BRIDGE -->|"decky.emit('hpd:state')"| STORE

    style FE fill:#dae8fc,stroke:#6c8ebf
    style BE fill:#e1d5e7,stroke:#9673a6
    style DA fill:#ffe6cc,stroke:#d79b00
```

**For dummies:** the buttons (TS) talk to the Python backend; the Python
talks to the daemon over D-Bus; and when something changes, the Python
**pushes** the new state to the UI so it updates on its own.

### 2.2 Map of the plugin screen

```mermaid
flowchart TB
    PANEL["📱 hpd panel (Quick Access)"]
    PANEL --> SETUP["⚠️ Setup banner<br/>(if polkit is missing → fix-polkit)"]
    PANEL --> CONF["⚠️ Conflict banner<br/>(if a rival is live → resolve)"]
    PANEL --> PWR["⚡ TDP — slider + presets (Eco/Balanced/Max)"]
    PANEL --> COOL["🧊 Cooling — Auto / Silent / Balanced / Aggressive + Reset"]
    PANEL --> BAT["🔋 Battery cap"]
    PANEL --> TEL["📊 Telemetry — W now / temps / RPM"]
    PANEL --> GRAPH["📈 Fan-curve graph"]
    PANEL --> ADV["🔧 Advanced — Power mode (profile), raw curve"]
    PANEL --> AC["🔌 AC indicator"]
    PANEL --> HELP["❓ Help"]

    style PWR fill:#ffe6cc,stroke:#d79b00
    style COOL fill:#dae8fc,stroke:#6c8ebf
    style BAT fill:#d5e8d4,stroke:#82b366
    style ADV fill:#f5f5f5,stroke:#999,stroke-dasharray: 4 4
```

### 2.3 Two ways to stay current: reactive vs polling

```mermaid
flowchart LR
    subgraph REACT["Reactive (PropertiesChanged)"]
        P1["TDP, profile, battery,<br/>auto_cooling, curve"]
    end
    subgraph POLL["Polling (no signal emitted)"]
        P2["Telemetry (temps/RPM/W) — ~1/sec while panel open"]
        P3["AC connected — every ~10 s"]
    end
    P1 -->|"the daemon notifies instantly"| UIb["UI always fresh"]
    P2 -->|"the plugin asks often"| UIb
    P3 -->|"the plugin asks now and then"| UIb

    style REACT fill:#d5e8d4,stroke:#82b366
    style POLL fill:#fff2cc,stroke:#d6b656
```

> 💡 **AC** is polled because the daemon emits no signal for it. The
> **`AC0`-node fix** makes that poll return the correct value on the Xbox
> Ally X (it used to always say "battery").

---

## 3. Plugin ↔ daemon communication

### 3.1 Communication map (who calls what)

```mermaid
flowchart LR
    subgraph PLUGIN["Plugin (Decky)"]
        FEc["Buttons / sliders"]
        PYc["Python backend"]
    end
    subgraph DAEMON["Daemon hpd"]
        SET["Setters (polkit-gated):<br/>set_spl, set_preset, set_charge_threshold,<br/>set_cooling_level, set_fan_auto,<br/>reset_fan_curve, set_profile, set_ac_max_performance"]
        GET["Reads (no polkit):<br/>get_thermal_status, get_fan_curve,<br/>get_hardware_limits, is_ac_connected,<br/>get_diagnostics, get_power_conflicts"]
        PROP["Properties (PropertiesChanged):<br/>current_spl, active_profile,<br/>charge_end_threshold, auto_cooling, fan_curve"]
    end
    POLKIT(["🔐 polkit<br/>wheel = no password"])

    FEc --> PYc
    PYc -->|"write"| SET
    SET -.->|"checks"| POLKIT
    PYc -->|"read / poll"| GET
    PROP -->|"pushes changes"| PYc

    style SET fill:#ffe6cc,stroke:#d79b00
    style GET fill:#d5e8d4,stroke:#82b366
    style PROP fill:#dae8fc,stroke:#6c8ebf
    style POLKIT fill:#f8cecc,stroke:#b85450
```

### 3.2 Use case — change the TDP from the plugin

```mermaid
sequenceDiagram
    actor U as User
    participant UI as Plugin (UI)
    participant PY as Plugin (Python)
    participant D as Daemon
    participant HW as /sys (hardware)

    U->>UI: drags the slider to 20 W
    UI->>PY: set_spl(20)
    PY->>D: D-Bus SetSpl(20)
    D->>D: polkit OK (wheel) → reduce()
    D->>HW: writes SPL/SPPT/FPPT
    Note over D: if auto-cooling: adjusts the CURVE<br/>(not the profile)
    D-->>PY: PropertiesChanged(current_spl=20)
    PY-->>UI: push 'hpd:state'
    UI-->>U: slider confirms 20 W ✅
```

### 3.3 Use case — change cooling (fans only)

```mermaid
sequenceDiagram
    actor U as User
    participant UI as Plugin
    participant D as Daemon
    participant HW as /sys

    U->>UI: taps "Aggressive"
    UI->>D: SetCoolingLevel("aggressive")
    D->>HW: writes the fan curve
    Note over D,HW: does NOT touch platform_profile<br/>→ power does not change
    D-->>UI: PropertiesChanged(fan_curve="aggressive", auto_cooling=false)
    UI-->>U: "✓ Aggressive" (manual)
```

### 3.4 Use case — opt into GPU clock auto-follow (advanced)

```mermaid
sequenceDiagram
    actor U as User
    participant UI as Plugin
    participant D as Daemon
    participant HW as /sys (amdgpu)

    Note over D: GPU clock untouched until now — no prior gpu auto ever ran
    U->>UI: opens Advanced → GPU clock, taps "Auto (follow TDP)"
    UI->>D: EnableGpuAutoFollow()
    D->>D: infer a tier from the current TDP (same silent/balanced/aggressive<br/>cut already used for the fan curve), resolve it against<br/>the live OD_RANGE (GetGpuClockConstraints())
    D->>HW: writes pp_od_clk_voltage (switch to manual DPM, commit the resolved range)
    D-->>UI: PropertiesChanged(GpuClockRange="balanced", GpuFollowsTdp=true)
    UI-->>U: "Auto (follows TDP)"
    Note over D,UI: this call is what opts the device in — hpd never<br/>touches the GPU clock on its own before this.<br/>There is no method to pin an arbitrary MHz range<br/>(SetGpuClockRange existed through 2.x, removed in 3.0.0)
```

### 3.5 Use case — plug in the charger

```mermaid
sequenceDiagram
    participant K as Kernel (udev)
    participant D as Daemon
    participant PY as Plugin (Python)
    participant UI as Plugin (UI)

    K->>D: power_supply event (AC0 online=1)
    D->>D: AcPowerChanged(true) → snapshot DC state, force Performance / Max / Aggressive, set AcLocked
    D-->>PY: PropertiesChanged: AcConnected=true, AcLocked=true, CurrentSpl, ActiveProfile, FanCurve
    PY-->>UI: indicator "⚡ AC" + disable TDP / preset / power-mode / cooling (charge stays editable)
    Note over D,UI: while AcLocked, the daemon refuses power/cooling writes; unplug restores the DC snapshot
```

### 3.6 Use case — external change (hpdctl in a terminal)

```mermaid
sequenceDiagram
    actor U as User (terminal)
    participant C as hpdctl
    participant D as Daemon
    participant PY as Plugin (Python)
    participant UI as Plugin (UI)

    U->>C: hpdctl cool set silent
    C->>D: SetCoolingLevel("silent")
    D-->>PY: PropertiesChanged(fan_curve="silent")
    PY-->>UI: push 'hpd:state'
    UI-->>U: the plugin updates on its own (no reopen)
    Note over D,UI: single source of truth: the daemon.<br/>CLI and plugin always agree.
```

### 3.7 Use case — polkit missing / a rival is live

```mermaid
flowchart TD
    START["Plugin starts"] --> DIAG["get_diagnostics()"]
    DIAG -->|"polkit_ok = false"| BANNER1["⚠️ Setup banner<br/>button → fix-polkit"]
    DIAG -->|"polkit_ok = true"| CONF["get_power_conflicts()"]
    CONF -->|"rival present"| BANNER2["⚠️ Conflict banner<br/>button → resolve (mask)"]
    CONF -->|"clean"| READY["✅ All set"]

    style BANNER1 fill:#f8cecc,stroke:#b85450
    style BANNER2 fill:#fff2cc,stroke:#d6b656
    style READY fill:#d5e8d4,stroke:#82b366
```

---

## 4. Every combination

Because **power and cooling are independent**, any mix is valid. The
**TDP** decides temperature; the **cooling** decides noise.

```mermaid
quadrantChart
    title TDP (power) vs Cooling (fans)
    x-axis "Low TDP (cool/battery)" --> "High TDP (powerful/hot)"
    y-axis "Cooling silent (quiet)" --> "Cooling aggressive (loud)"
    quadrant-1 "Powerful and loud (full gaming)"
    quadrant-2 "Quiet and cool (reading/video)"
    quadrant-3 "Max battery + silence"
    quadrant-4 "Full TDP, soft fans (runs hot — valid)"
```

### 4.1 Combination matrix (what you get)

| TDP | Cooling | Result |
|---|---|---|
| Low (eco) | Silent | 🟢 Cool, quiet, long battery |
| Low (eco) | Aggressive | Cool and quiet anyway (light load) + fans louder than needed |
| High (max) | Aggressive | 🔥 Max performance, the coolest possible at full tilt, loud |
| High (max) | Silent | Full power but runs **hot** (little airflow) — valid, your call |
| Any | **Auto** | The fans adjust to the TDP on their own (recommended) |

### 4.2 The advanced power knob (platform_profile)

| Power mode | Effect | For whom |
|---|---|---|
| **Performance** *(default)* | Your TDP applies in full | 👍 Almost everyone |
| Balanced | Limits power a little (efficiency) | Advanced |
| Power-saver / Eco | Limits power hard (below the TDP) | Advanced users wanting max efficiency |

> ⚠️ If you set **Power-saver**, the chip can stay below your TDP (it is
> the only knob that "overrides" the TDP). The plugin shows a hint if it
> detects this. **Cooling never limits power.**

### 4.3 Auto vs Manual (cooling)

```mermaid
stateDiagram-v2
    [*] --> Auto
    Auto --> Manual: cool set (level)
    Manual --> Auto: cool auto
    Auto: AUTO — the curve follows the TDP
    Manual: MANUAL — you pin a fan level
    note right of Auto
        Recommended.
        Low TDP → quiet curve
        High TDP → aggressive curve
    end note
```

---

## 5. Master table: CLI ↔ D-Bus ↔ Plugin

What the same thing is called on each side (it all ends at the daemon):

| Action | `hpdctl` | D-Bus | Plugin (UI) | polkit |
|---|---|---|---|---|
| **Power** | `tdp set <W>` | `SetSpl(u)` | TDP slider | `set-tdp` |
| Power preset | `preset eco/balanced/max` | `SetPreset(s)` | Eco/Balanced/Max buttons | `set-tdp` |
| **Cooling (fans)** | `cool set <level>` | `SetCoolingLevel(s)` | Cooling selector | `set-profile` |
| Auto cooling | `cool auto` | `SetFanAuto()` | Auto toggle | `set-profile` |
| Cooling to firmware | `cool reset` | `ResetFanCurve()` | Reset button | `set-profile` |
| **Power mode (advanced)** | `power set <mode>` | `SetProfile(s)` | Advanced → Power mode | `set-profile` |
| **AC lock** | `ac-lock on/off` | `SetAcMaxPerformance(b)` | Settings toggle | `set-profile` |
| **Battery** | `charge set <%>` | `SetChargeThreshold(y)` | battery control | `set-charge` |
| See temps/RPM/W | `status` / `monitor` | `GetThermalStatus()` | telemetry (poll) | — |
| See curve | `cool curve` | `GetFanCurve()` | graph | — |
| See HW range | `limits` | `GetHardwareLimits()` | slider range | — |
| See AC | `status` | `AcConnected` (prop) / `IsAcConnected()` | indicator (reactive) | — |
| See AC lock | `ac-lock` | `AcLocked` / `AcMaxPerformance` (props) | banner + Settings toggle | — |
| Health / polkit | `doctor` | `GetDiagnostics()` | Setup banner | — |
| Rivals | `doctor` | `GetPowerConflicts()` | Conflict banner | — |
| Custom fan curve (advanced) | `cool set-custom <8 pairs>` | `SetFanCurve(a(yy), a(yy))` | Fan-curve editor | `set-profile` |
| Extended telemetry | `status` / `monitor` | `GetTelemetry()` | Extended telemetry section | — |
| **GPU clock (advanced, opt-in)** | `gpu auto` | `EnableGpuAutoFollow()` | Advanced → GPU clock → Auto | `set-profile` |
| GPU clock — reset | `gpu reset` | `ResetGpuClocks()` | Advanced → GPU clock → Reset | `set-profile` |
| GPU clock — read | `gpu get` | `GetGpuClockRange()` / `GpuClockRange` (prop) | GPU clock control (reactive) | — |
| GPU clock — limits | `gpu limits` | `GetGpuClockConstraints()` | GPU clock control (bounds) | — |

---

**Full manuals:** [`MANUAL.md`](MANUAL.md) (English) ·
[`MANUAL-es.md`](MANUAL-es.md) (Spanish) ·
[`fan-curves.md`](fan-curves.md) (fan-curve internals) ·
[`decky-plugin/`](decky-plugin/) (plugin integration).
