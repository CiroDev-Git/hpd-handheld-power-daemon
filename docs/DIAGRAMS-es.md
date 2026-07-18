<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# hpd — Guía visual (para dummies) 🇪🇸🖼️

Los manuales en imágenes. Una imagen dice más que mil palabras, así que
acá está **cómo funciona el daemon**, **cómo funciona el plugin de Decky**,
y **cómo se comunican** — con todas las combinaciones posibles. English
version: [`DIAGRAMS.md`](DIAGRAMS.md).

> Los diagramas son [Mermaid](https://mermaid.js.org): se ven solos en
> GitHub y en la mayoría de editores Markdown.

**Índice**

- [0. La idea en una imagen](#0-la-idea-en-una-imagen)
- [1. El daemon por dentro](#1-el-daemon-por-dentro)
- [2. El plugin por dentro](#2-el-plugin-por-dentro)
- [3. Comunicación plugin ↔ daemon](#3-comunicación-plugin--daemon)
- [4. Todas las combinaciones](#4-todas-las-combinaciones)
- [5. Tabla maestra: CLI ↔ D-Bus ↔ Plugin](#5-tabla-maestra-cli--d-bus--plugin)

---

## 0. La idea en una imagen

**Tres perillas independientes.** Esto es lo único que hay que entender:

```mermaid
flowchart LR
    subgraph KNOBS["Lo que vos controlás"]
        direction TB
        TDP["⚡ POTENCIA (TDP)<br/>cuántos watts usa el chip<br/><i>tu límite real</i>"]
        COOL["🧊 COOLING<br/>cuán fuerte van los ventiladores<br/><i>ruido ↔ temperatura</i>"]
        BAT["🔋 BATERÍA<br/>tope de carga (ej. 80%)<br/><i>salud a largo plazo</i>"]
    end

    TDP --> R1["= rendimiento + calor<br/>vs batería"]
    COOL --> R2["= silencio<br/>vs frescura"]
    BAT --> R3["= vida útil<br/>de la batería"]

    PM["🔧 (avanzado) Power mode / EPP<br/>por defecto: Performance"] -.->|"normalmente NO se toca"| TDP

    style TDP fill:#ffe6cc,stroke:#d79b00
    style COOL fill:#dae8fc,stroke:#6c8ebf
    style BAT fill:#d5e8d4,stroke:#82b366
    style PM fill:#f5f5f5,stroke:#999,stroke-dasharray: 4 4
```

**Regla mental:**
- ¿Más/menos rendimiento o batería? → **TDP**.
- ¿Más/menos ruido? → **Cooling**.
- Son **independientes**: "potencia full + ventilador silencioso" es válido.

---

## 1. El daemon por dentro

`hpd` es un servicio de fondo (root) que escribe los "perillas" del firmware
y expone una interfaz D-Bus. Por dentro **todo pasa por una máquina de
estados**: nada toca el hardware directo.

### 1.1 El flujo de un comando (máquina de estados)

```mermaid
flowchart TD
    A["Evento externo<br/>(CLI, plugin, cargador, despertar...)"] --> B["Transition<br/>(SetSpl, SetCoolingLevel, AcPowerChanged...)"]
    B --> C{{"reduce()  — función PURA<br/>valida + decide"}}
    C -->|"lista de Effects"| D["Executor"]
    D --> E["Escribe a /sys (backend ASUS)"]
    D --> F["Guarda estado en disco<br/>(/var/lib/hpd/state.toml)"]
    D --> G["watch::ProfileState<br/>(lo que leen los lectores)"]
    G --> H["D-Bus emite PropertiesChanged"]
    E -->|"si la escritura falla"| I["Rollback:<br/>re-lee el hardware → Sync*"]
    I --> C

    style C fill:#fff2cc,stroke:#d6b656
    style D fill:#dae8fc,stroke:#6c8ebf
```

**Para dummies:** un evento se convierte en una "intención" (Transition),
una función pura decide qué hacer (sin tocar nada), y el Executor es el
único que escribe al hardware y guarda el estado. Si una escritura falla,
re-lee el hardware para no mentir sobre el estado.

### 1.2 El desacople: qué perilla escribe qué

```mermaid
flowchart LR
    subgraph CMD["Comandos"]
        T1["hpdctl tdp set N"]
        T2["hpdctl cool set / auto"]
        T3["set_profile (avanzado)"]
        T4["hpdctl charge set N"]
    end

    T1 -->|potencia| SPL["/sys .../ppt_pl1_spl<br/>SPPT + FPPT (boost)"]
    T2 -->|ventilador| FAN["/sys .../asus_custom_fan_curve<br/>(8 puntos temp→duty)"]
    T3 -->|EPP / potencia| PROF["/sys/.../platform_profile<br/>(default: performance)"]
    T4 -->|batería| CHG["/sys .../charge_control_end_threshold"]

    SPL --> POWER(["⚡ POTENCIA REAL"])
    PROF --> POWER
    FAN --> NOISE(["🌀 RPM / temperatura"])

    style POWER fill:#ffe6cc,stroke:#d79b00
    style NOISE fill:#dae8fc,stroke:#6c8ebf
    style T3 fill:#f5f5f5,stroke:#999,stroke-dasharray: 4 4
```

> 🔑 **El cambio clave (desacople):** antes `cool` movía *también* el
> `platform_profile`, que **capa la potencia real** (un `silent` dejaba el
> chip a ~13 W aunque pidieras 25 W). Ahora `cool` solo toca el ventilador;
> el `platform_profile` queda fijo en `performance` para que tu TDP sea el
> límite de verdad.

### 1.3 Auto-cooling: el ventilador sigue al TDP

```mermaid
flowchart LR
    TDPv["TDP que pusiste"] --> FR{"¿En qué parte del<br/>rango del hardware cae?"}
    FR -->|"< 33%"| S["curva SILENT"]
    FR -->|"33–67%"| Bd["curva BALANCED"]
    FR -->|"> 67%"| Ag["curva AGGRESSIVE"]
    S --> note["Solo cambia la CURVA del ventilador.<br/>NUNCA el platform_profile."]
    Bd --> note
    Ag --> note
    style note fill:#fff2cc,stroke:#d6b656
```

### 1.4 Ciclo de vida (eventos del sistema)

```mermaid
flowchart TD
    BOOT["🟢 Arranque"] --> B1["Re-asienta TODO el estado guardado al hardware<br/>(TDP + perfil→default performance + charge + curva)"]
    B1 --> B2["= exactamente lo que reporta el daemon, aunque un<br/>boot en frío haya reseteado el firmware a defaults"]

    AC["🔌 Enchufás cargador"] --> AC1["udev → AcPowerChanged(true)"]
    AC1 --> AC2["Guarda tu TDP de batería,<br/>sube a máximo"]
    ACU["🔋 Desenchufás"] --> ACU1["AcPowerChanged(false)"]
    ACU1 --> ACU2["Restaura tu TDP de batería"]

    SLEEP["😴 Despertar de suspensión"] --> SL1["logind → SystemResumed"]
    SL1 --> SL2["Re-aplica TDP + profile + curva + batería<br/>(el firmware los pierde al dormir)"]

    HUP["♻️ systemctl reload"] --> HUP1["SIGHUP → relee /etc/hpd/config.toml"]
    STOP["🔴 stop / Ctrl-C"] --> STOP1["Guarda estado y sale limpio"]

    style BOOT fill:#d5e8d4,stroke:#82b366
    style STOP fill:#f8cecc,stroke:#b85450
```

### 1.5 ¿Hay alguien peleando por las perillas? (rivales)

```mermaid
flowchart LR
    HPD["hpd"] --> CHK{"¿Otro daemon escribe<br/>las mismas perillas?"}
    CHK -->|"rival duro"| R["power-profiles-daemon,<br/>steamos-manager, tuned, hhd<br/>→ doctor --fix los enmascara"]
    CHK -->|"advisory (se reporta, no se toca)"| A["gamemoded, asusd,<br/>auto-cpufreq"]
    CHK -->|"limpio"| OK["✅ hpd manda solo"]
    style R fill:#f8cecc,stroke:#b85450
    style A fill:#fff2cc,stroke:#d6b656
    style OK fill:#d5e8d4,stroke:#82b366
```

---

## 2. El plugin por dentro

El plugin de Decky es **una UI** en el menú rápido de Steam. No toca el
hardware: le pide todo al daemon. Tiene tres capas.

### 2.1 Las tres capas del plugin

```mermaid
flowchart TB
    subgraph FE["1 · Frontend (TypeScript / React)"]
        UI["Componentes: TDP, Cooling, Batería,<br/>Telemetría, Curva, Conflictos, Setup"]
        HOOKS["Hooks: useTdp, useCooling,<br/>useThermalStatus, useAcConnected..."]
        STORE["Store (snapshot del estado)"]
        UI --> HOOKS --> STORE
    end
    subgraph BE["2 · Backend (Python, main.py)"]
        BRIDGE["EventBridge + cliente D-Bus<br/>+ poll de AC + push de snapshots"]
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

**Para dummies:** los botones (TS) le hablan al backend Python; el Python
le habla al daemon por D-Bus; y cuando algo cambia, el Python le **empuja**
el estado nuevo a la UI para que se actualice sola.

### 2.2 Mapa de la pantalla del plugin

```mermaid
flowchart TB
    PANEL["📱 Panel hpd (Quick Access)"]
    PANEL --> SETUP["⚠️ Banner de Setup<br/>(si falta polkit → fix-polkit)"]
    PANEL --> CONF["⚠️ Banner de Conflictos<br/>(si hay un rival → resolver)"]
    PANEL --> PWR["⚡ TDP — slider + presets (Eco/Balanced/Max)"]
    PANEL --> COOL["🧊 Cooling — Auto / Silent / Balanced / Aggressive + Reset"]
    PANEL --> BAT["🔋 Tope de batería"]
    PANEL --> TEL["📊 Telemetría — W ahora / temps / RPM"]
    PANEL --> GRAPH["📈 Gráfico de la curva de ventilador"]
    PANEL --> ADV["🔧 Avanzado — Power mode (perfil), curva cruda"]
    PANEL --> AC["🔌 Indicador AC"]
    PANEL --> HELP["❓ Ayuda"]

    style PWR fill:#ffe6cc,stroke:#d79b00
    style COOL fill:#dae8fc,stroke:#6c8ebf
    style BAT fill:#d5e8d4,stroke:#82b366
    style ADV fill:#f5f5f5,stroke:#999,stroke-dasharray: 4 4
```

### 2.3 Dos formas de mantenerse al día: reactivo vs polling

```mermaid
flowchart LR
    subgraph REACT["Reactivo (PropertiesChanged)"]
        P1["TDP, perfil, batería,<br/>auto_cooling, curva"]
    end
    subgraph POLL["Polling (no emiten señal)"]
        P2["Telemetría (temps/RPM/W) — ~1/seg con panel abierto"]
        P3["AC conectado — cada ~10 s"]
    end
    P1 -->|"el daemon avisa al instante"| UIb["UI siempre fresca"]
    P2 -->|"el plugin pregunta seguido"| UIb
    P3 -->|"el plugin pregunta cada tanto"| UIb

    style REACT fill:#d5e8d4,stroke:#82b366
    style POLL fill:#fff2cc,stroke:#d6b656
```

> 💡 El **AC** se consulta por polling porque el daemon no emite señal para
> él. El **fix del nodo `AC0`** hace que esa consulta devuelva el valor
> correcto en el Xbox Ally X (antes decía "batería" siempre).

---

## 3. Comunicación plugin ↔ daemon

### 3.1 Mapa de comunicación (quién llama a qué)

```mermaid
flowchart LR
    subgraph PLUGIN["Plugin (Decky)"]
        FEc["Botones / sliders"]
        PYc["Backend Python"]
    end
    subgraph DAEMON["Daemon hpd"]
        SET["Setters (gated por polkit):<br/>set_spl, set_preset, set_charge_threshold,<br/>set_cooling_level, set_fan_auto,<br/>reset_fan_curve, set_profile, set_ac_max_performance"]
        GET["Lecturas (sin polkit):<br/>get_thermal_status, get_fan_curve,<br/>get_hardware_limits, is_ac_connected,<br/>get_diagnostics, get_power_conflicts"]
        PROP["Propiedades (PropertiesChanged):<br/>current_spl, active_profile,<br/>charge_end_threshold, auto_cooling, fan_curve"]
    end
    POLKIT(["🔐 polkit<br/>wheel = sin password"])

    FEc --> PYc
    PYc -->|"escribir"| SET
    SET -.->|"verifica"| POLKIT
    PYc -->|"leer / pollear"| GET
    PROP -->|"empuja cambios"| PYc

    style SET fill:#ffe6cc,stroke:#d79b00
    style GET fill:#d5e8d4,stroke:#82b366
    style PROP fill:#dae8fc,stroke:#6c8ebf
    style POLKIT fill:#f8cecc,stroke:#b85450
```

### 3.2 Caso de uso — cambiar el TDP desde el plugin

```mermaid
sequenceDiagram
    actor U as Usuario
    participant UI as Plugin (UI)
    participant PY as Plugin (Python)
    participant D as Daemon
    participant HW as /sys (hardware)

    U->>UI: arrastra el slider a 20 W
    UI->>PY: set_spl(20)
    PY->>D: D-Bus SetSpl(20)
    D->>D: polkit OK (wheel) → reduce()
    D->>HW: escribe SPL/SPPT/FPPT
    Note over D: si auto-cooling: ajusta la CURVA<br/>(no el perfil)
    D-->>PY: PropertiesChanged(current_spl=20)
    PY-->>UI: push 'hpd:state'
    UI-->>U: el slider confirma 20 W ✅
```

### 3.3 Caso de uso — cambiar cooling (solo ventilador)

```mermaid
sequenceDiagram
    actor U as Usuario
    participant UI as Plugin
    participant D as Daemon
    participant HW as /sys

    U->>UI: toca "Aggressive"
    UI->>D: SetCoolingLevel("aggressive")
    D->>HW: escribe la curva del ventilador
    Note over D,HW: NO toca el platform_profile<br/>→ la potencia no cambia
    D-->>UI: PropertiesChanged(fan_curve="aggressive", auto_cooling=false)
    UI-->>U: "✓ Aggressive" (manual)
```

### 3.4 Caso de uso — optar por el auto-follow de clock de GPU (avanzado)

```mermaid
sequenceDiagram
    actor U as Usuario
    participant UI as Plugin
    participant D as Daemon
    participant HW as /sys (amdgpu)

    Note over D: El clock de GPU no fue tocado hasta ahora — nunca corrió gpu auto
    U->>UI: abre Avanzado → Clock de GPU, toca "Auto (seguir TDP)"
    UI->>D: EnableGpuAutoFollow()
    D->>D: infiere un nivel a partir del TDP actual (el mismo corte<br/>silent/balanced/aggressive ya usado para la curva del ventilador),<br/>lo resuelve contra el OD_RANGE en vivo (GetGpuClockConstraints())
    D->>HW: escribe pp_od_clk_voltage (pasa a DPM manual, commitea el rango resuelto)
    D-->>UI: PropertiesChanged(GpuClockRange="balanced", GpuFollowsTdp=true)
    UI-->>U: "Auto (sigue al TDP)"
    Note over D,UI: este llamado es lo que activa la función — hpd nunca<br/>toca el clock de GPU por su cuenta antes de esto.<br/>No existe método para fijar un rango MHz arbitrario<br/>(SetGpuClockRange existió en la línea 2.x, eliminado en la 3.0.0)
```

### 3.5 Caso de uso — enchufar el cargador

```mermaid
sequenceDiagram
    participant K as Kernel (udev)
    participant D as Daemon
    participant PY as Plugin (Python)
    participant UI as Plugin (UI)

    K->>D: evento power_supply (AC0 online=1)
    D->>D: AcPowerChanged(true) → snapshot estado DC, fuerza Performance / Max / Aggressive, set AcLocked
    D-->>PY: PropertiesChanged: AcConnected=true, AcLocked=true, CurrentSpl, ActiveProfile, FanCurve
    PY-->>UI: indicador "⚡ AC" + deshabilita TDP / preset / power-mode / cooling (carga sigue editable)
    Note over D,UI: mientras AcLocked, el daemon rechaza escrituras de potencia/cooling; al desenchufar restaura el snapshot DC
```

### 3.6 Caso de uso — cambio externo (hpdctl en una terminal)

```mermaid
sequenceDiagram
    actor U as Usuario (terminal)
    participant C as hpdctl
    participant D as Daemon
    participant PY as Plugin (Python)
    participant UI as Plugin (UI)

    U->>C: hpdctl cool set silent
    C->>D: SetCoolingLevel("silent")
    D-->>PY: PropertiesChanged(fan_curve="silent")
    PY-->>UI: push 'hpd:state'
    UI-->>U: el plugin se actualiza solo (sin abrirlo de nuevo)
    Note over D,UI: una sola fuente de verdad: el daemon.<br/>CLI y plugin siempre coinciden.
```

### 3.7 Caso de uso — falta polkit / hay un rival

```mermaid
flowchart TD
    START["Plugin arranca"] --> DIAG["get_diagnostics()"]
    DIAG -->|"polkit_ok = false"| BANNER1["⚠️ Banner Setup<br/>botón → fix-polkit"]
    DIAG -->|"polkit_ok = true"| CONF["get_power_conflicts()"]
    CONF -->|"hay rival"| BANNER2["⚠️ Banner Conflicto<br/>botón → resolver (mask)"]
    CONF -->|"limpio"| READY["✅ Todo listo"]

    style BANNER1 fill:#f8cecc,stroke:#b85450
    style BANNER2 fill:#fff2cc,stroke:#d6b656
    style READY fill:#d5e8d4,stroke:#82b366
```

---

## 4. Todas las combinaciones

Como **potencia y cooling son independientes**, cualquier mezcla es válida.
La temperatura la decide el **TDP**; el ruido lo decide el **cooling**.

```mermaid
quadrantChart
    title TDP (potencia) vs Cooling (ventilador)
    x-axis "TDP bajo (fresco/batería)" --> "TDP alto (potente/caliente)"
    y-axis "Cooling silent (silencioso)" --> "Cooling aggressive (ruidoso)"
    quadrant-1 "Potente y ruidoso (gaming a tope)"
    quadrant-2 "Silencioso y fresco (lectura/video)"
    quadrant-3 "Batería max + silencio"
    quadrant-4 "Full TDP, fans suaves (corre caliente — válido)"
```

### 4.1 Matriz de combinaciones (qué obtenés)

| TDP | Cooling | Resultado |
|---|---|---|
| Bajo (eco) | Silent | 🟢 Frío, silencioso, mucha batería |
| Bajo (eco) | Aggressive | Frío y silencioso igual (poca carga) + fans fuertes "de más" |
| Alto (max) | Aggressive | 🔥 Máximo rendimiento, lo más fresco posible a tope, ruidoso |
| Alto (max) | Silent | Potencia full pero corre **caliente** (poco aire) — válido, es tu decisión |
| Cualquiera | **Auto** | El ventilador se ajusta solo al TDP (recomendado) |

### 4.2 La perilla avanzada de potencia (platform_profile)

| Power mode | Efecto | Para quién |
|---|---|---|
| **Performance** *(default)* | Tu TDP se aplica completo | 👍 Casi todos |
| Balanced | Limita un poco la potencia (eficiencia) | Avanzados |
| Power-saver / Eco | Limita fuerte la potencia (por debajo del TDP) | Avanzados que quieren máxima eficiencia |

> ⚠️ Si ponés **Power-saver**, el chip puede quedar por debajo de tu TDP
> (es la única perilla que "pisa" el TDP). El plugin avisa con un hint si
> detecta esto. **Cooling nunca limita la potencia.**

### 4.3 Auto vs Manual (cooling)

```mermaid
stateDiagram-v2
    [*] --> Auto
    Auto --> Manual: cool set (nivel)
    Manual --> Auto: cool auto
    Auto: AUTO — la curva sigue al TDP
    Manual: MANUAL — fijás un nivel de ventilador
    note right of Auto
        Recomendado.
        Poco TDP → curva silenciosa
        Mucho TDP → curva agresiva
    end note
```

---

## 5. Tabla maestra: CLI ↔ D-Bus ↔ Plugin

Cómo se llama lo mismo en cada lado (todo termina en el daemon):

| Acción | `hpdctl` | D-Bus | Plugin (UI) | polkit |
|---|---|---|---|---|
| **Potencia** | `tdp set <W>` | `SetSpl(u)` | slider TDP | `set-tdp` |
| Preset de potencia | `preset eco/balanced/max` | `SetPreset(s)` | botones Eco/Balanced/Max | `set-tdp` |
| **Cooling (ventilador)** | `cool set <nivel>` | `SetCoolingLevel(s)` | selector Cooling | `set-profile` |
| Cooling automático | `cool auto` | `SetFanAuto()` | toggle Auto | `set-profile` |
| Cooling a firmware | `cool reset` | `ResetFanCurve()` | botón Reset | `set-profile` |
| **Power mode (avanzado)** | `power set <modo>` | `SetProfile(s)` | Avanzado → Power mode | `set-profile` |
| **AC lock** | `ac-lock on/off` | `SetAcMaxPerformance(b)` | toggle en Settings | `set-profile` |
| **Batería** | `charge set <%>` | `SetChargeThreshold(y)` | control de batería | `set-charge` |
| Ver temps/RPM/W | `status` / `monitor` | `GetThermalStatus()` | telemetría (poll) | — |
| Ver curva | `cool curve` | `GetFanCurve()` | gráfico | — |
| Ver rango HW | `limits` | `GetHardwareLimits()` | rango del slider | — |
| Ver AC | `status` | `AcConnected` (prop) / `IsAcConnected()` | indicador (reactivo) | — |
| Ver AC lock | `ac-lock` | `AcLocked` / `AcMaxPerformance` (props) | banner + toggle en Settings | — |
| Salud / polkit | `doctor` | `GetDiagnostics()` | banner Setup | — |
| Rivales | `doctor` | `GetPowerConflicts()` | banner Conflicto | — |
| Curva custom (avanzado) | `cool set-custom <8 pares>` | `SetFanCurve(a(yy), a(yy))` | Editor de curva | `set-profile` |
| Telemetría extendida | `status` / `monitor` | `GetTelemetry()` | Sección de telemetría extendida | — |
| **Clock de GPU (avanzado, opt-in)** | `gpu auto` | `EnableGpuAutoFollow()` | Avanzado → Clock de GPU → Auto | `set-profile` |
| Clock de GPU — reset | `gpu reset` | `ResetGpuClocks()` | Avanzado → Clock de GPU → Reset | `set-profile` |
| Clock de GPU — lectura | `gpu get` | `GetGpuClockRange()` / `GpuClockRange` (prop) | Control de clock de GPU (reactivo) | — |
| Clock de GPU — límites | `gpu limits` | `GetGpuClockConstraints()` | Control de clock de GPU (límites) | — |

---

**Manuales completos:** [`MANUAL-es.md`](MANUAL-es.md) ·
[`COOLING-es.md`](COOLING-es.md) (el desacople explicado) ·
[`fan-curves.md`](fan-curves.md) (lo técnico de las curvas) ·
[`decky-plugin/`](decky-plugin/) (la integración del plugin).
