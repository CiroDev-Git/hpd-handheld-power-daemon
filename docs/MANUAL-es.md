<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# hpd — Manual de usuario 🇪🇸

Todo lo que hace `hpd` hasta hoy, en un solo lugar y explicado simple.
Versión en inglés: [`MANUAL.md`](MANUAL.md). ¿Lo querés en **diagramas**?
[`DIAGRAMS-es.md`](DIAGRAMS-es.md) (visual, para dummies).

- [Qué es hpd](#qué-es-hpd)
- [Las dos perillas: Potencia y Enfriamiento](#las-dos-perillas)
- [Lista de comandos](#lista-de-comandos)
- [Qué hace cada combinación](#qué-hace-cada-combinación)
- [Cómo se asigna el cooling](#cómo-se-asigna-el-cooling-vos-nunca-seteás-el-profile-directo)
- [Configuraciones recomendadas](#configuraciones-recomendadas)
- [Leer el tablero (status)](#leer-el-tablero-status)
- [Dibujar la curva del ventilador](#dibujar-la-curva-del-ventilador)
- [Vida útil de la batería](#vida-útil-de-la-batería)
- [Qué es normal vs. cuándo preocuparse](#qué-es-normal-vs-cuándo-preocuparse)
- [Comportamiento con AC, batería y al despertar](#comportamiento-con-ac-batería-y-al-despertar)
- [Para desarrolladores: Decky / D-Bus](#para-desarrolladores-decky--d-bus)

## Qué es hpd

`hpd` es un servicio que corre en segundo plano y maneja la potencia y el
enfriamiento de una consola portátil (por ahora la familia ASUS ROG
Ally). Te deja balancear rendimiento, calor, ruido y batería como
quieras — desde silencioso, fresco y con mucha batería, hasta ruidoso y a
toda potencia — y recuerda tu elección entre reinicios y suspensiones.

Lo controlás con el comando `hpdctl`, o con cualquier app que hable con
su interfaz D-Bus (como un plugin de Decky).

## Las dos perillas

Aunque suene complicado, solo controlás **dos cosas**:

### ⚡ Potencia (TDP)

Cuántos watts puede usar el chip. Más watts = más rendimiento y más
calor; menos = más fresco y más batería.

```
hpdctl tdp set 18      # permití hasta 18 W
hpdctl preset eco|balanced|max   # atajos: min / medio / máximo
```

### 🧊 Enfriamiento (nivel + modo)

Cuán fuerte trabaja el **ventilador** — puro trade entre **ruido y
temperatura**. El enfriamiento es **independiente de la potencia**: no
cambia cuántos watts usa el chip (eso es el TDP, arriba). Una palanca,
tres niveles:

| Nivel | Ventilador | Efecto |
|---|---|---|
| `silent` | silencioso | más tibio, casi sin ruido |
| `balanced` *(default)* | moderado | el punto medio de siempre |
| `aggressive` | fuerte | lo más fresco, lo más ruidoso |

```
hpdctl cool set silent|balanced|aggressive   # elegí nivel de fan (manual)
hpdctl cool auto       # que hpd elija la curva según el TDP
hpdctl cool reset      # devolver el ventilador al firmware
hpdctl cool get        # ver nivel y modo
hpdctl cool curve      # dibujar la curva activa
```

**Auto vs manual:**
- **Auto** (default): hpd elige la **curva del ventilador** según tu TDP.
  Poco TDP → curva silenciosa; mucho TDP → curva agresiva. El ventilador
  acompaña cuánto calor vas a generar, sin tocar nunca tu potencia.
- **Manual**: fijás un nivel de fan y no se mueve, pase lo que pase con el TDP.

> **Esto cambió en el desacople potencia↔cooling.** Antes el cooling
> también capaba la potencia real (un nivel bajo frenaba el chip). Ya no —
> `tdp set` es ahora la única palanca de potencia. La historia completa en
> [el explicador del desacople](COOLING-es.md).

### 🔋 Límite de carga de batería (una tercera perilla, aparte)

Limita cuánto se carga la batería. Mantenerla en 80 % en vez de 100 %
alarga mucho su vida.

```
hpdctl charge set 80   # dejar de cargar en 80 %
hpdctl charge get
```

## Lista de comandos

| Comando | Qué hace |
|---|---|
| `hpdctl status` | Tablero de una sola vez (potencia, cooling, temps, fans, batería, AC) |
| `hpdctl monitor` | El mismo tablero, refrescado cada segundo |
| `hpdctl limits` | Los watts min/max del hardware |
| `hpdctl tdp set <W>` / `tdp get` | Setear / ver el límite de potencia |
| `hpdctl preset eco\|balanced\|max` | Atajo de potencia (min / medio / máx) |
| `hpdctl cool set <nivel>` | Setear nivel (`silent`/`balanced`/`aggressive`) |
| `hpdctl cool auto` | El cooling sigue al TDP |
| `hpdctl cool reset` | Ventilador al firmware |
| `hpdctl cool get` | Ver nivel + modo |
| `hpdctl cool curve` | Dibujar la curva activa |
| `hpdctl power set <modo>` / `power get` | Modo de potencia (avanzado): `performance` / `balanced` / `eco` |
| `hpdctl charge set <%>` / `charge get` | Tope de carga de batería |

Los comandos de lectura no piden contraseña. Cambiar cosas no pide
contraseña si sos el dueño de la consola (grupo `wheel`) — incluso por
SSH; a otros usuarios se les pide autenticarse.

## Qué hace cada combinación

Las dos perillas ahora son **independientes** — la potencia es potencia, el
cooling es ventilador. Cualquier combinación es válida:

| Hacés | En cooling **auto** | En cooling **manual** |
|---|---|---|
| `tdp set` bajo | hpd elige una curva silenciosa | la potencia aplica; tu nivel de fan fijo queda |
| `tdp set` alto | hpd elige una curva agresiva | la potencia aplica completa; tu nivel de fan fijo queda |
| `cool set <nivel>` | pasa a manual en ese nivel de fan | setea ese nivel de fan |
| `cool auto` | (ya está en auto) | vuelve a auto |

**Ya no hay combinaciones contradictorias.** *Manual `silent` + un TDP
alto* antes era una trampa (el nivel bajo capaba la potencia). Ahora
significa exactamente lo que dice: **"usá todo el TDP, pero con el
ventilador suave".** Los watts aplican completos; el chip simplemente
corre más caliente porque trabaja fuerte con poco aire. Es tu decisión y
hpd la respeta.

## Cómo se relacionan cooling y potencia (están desacoplados)

Cooling y potencia son dos palancas separadas:

- **Potencia** = el TDP/SPL que ponés, más el **platform profile** ACPI
  (EPP). El profile arranca en `performance` para que tu TDP sea el límite
  real y usable — **no** se deriva de nada que hagas con el cooling.
- **Cooling** = solo la **curva del ventilador** (ruido ↔ temperatura).

El daemon escribe la **curva del ventilador** en estos momentos:

| Cuándo | Qué setea el daemon |
|---|---|
| Corrés `cool set <nivel>` | La curva → ese nivel (y pasa a manual). La potencia no se toca. |
| Corrés `cool auto` | Pasa a auto; la curva se deriva del TDP actual. |
| En **auto**, cambiás el TDP (`tdp set` / `preset`) | La curva se re-deriva según dónde cae el TDP en el rango — `< 33 %` → silent, `33–67 %` → balanced, `> 67 %` → aggressive. El platform profile **no** se toca. |
| Volver de suspensión | Se re-aplican la curva (y el profile) activos (el firmware los puede perder al dormir). |
| Enchufar AC | Por defecto (`ac_max_performance`) se **bloquea en máximo rendimiento**: Power mode → Performance, TDP → Max, cooling → Aggressive, y se rechazan los cambios de potencia/cooling hasta desenchufar (el tope de carga sigue editable). Al desenchufar se restaura tu estado de batería. |
| Arranque | El daemon re-aplica tu **estado completo guardado** (TDP, power mode → default configurado, tope de carga, curva) al hardware — así coincide con el device aunque un boot en frío haya reseteado el firmware a sus defaults. |

El **platform profile** es la palanca de potencia/EPP. Arranca en
`performance` (para que tu TDP sea totalmente usable) y nunca se deriva del
cooling ni del TDP. Los usuarios avanzados pueden cambiarlo en
`/etc/hpd/config.toml` (`default_platform_profile = "balanced"` /
`"power-saver"`) o en vivo por D-Bus (`set_profile`) para sesgar la
eficiencia — el 99 % lo deja como está.

## Configuraciones recomendadas

### `tdp set` vs `preset` — cuándo usar cada uno

- **`preset eco|balanced|max`** — rápido. Elige los watts **min / medio /
  máx** del rango de tu hardware. Usalo cuando solo querés "bajo / medio /
  alto" sin pensar en watts. En cooling auto, caen justo en silent /
  balanced / aggressive.
- **`tdp set <watts>`** — preciso. Poné un wattaje exacto cuando tenés un
  presupuesto en mente (ej. `tdp set 12` para apuntar a más batería).

### Combinaciones recomendadas

| Objetivo | Setup | Resultado |
|---|---|---|
| **Que funcione solo (recomendado)** | `cool auto` + `preset balanced` (o dejar los defaults) | La curva del ventilador siempre acompaña tu potencia; no hay que vigilar nada. |
| **Máximo rendimiento** (dock / enchufado) | `preset max` (o `tdp set <alto>`) + `cool set aggressive` | Potencia full, fans al máximo — lo más fresco a tope. Ruidoso. |
| **Silencioso y mucha batería** (lectura, video, emulación liviana) | `preset eco` + `cool set silent` | Poca potencia (fresco y mucha batería) con fans casi mudos. |
| **Potencia full pero silencioso** | `tdp set <alto>` + `cool set silent` | Los watts aplican completos; los fans quedan suaves, así que el chip corre más caliente. Ahora es una opción válida. |
| **Equilibrado de todos los días** | `cool auto` (el default) | El daemon elige la curva según tu TDP. |

### Checklist de "config perfecta"

1. **Batería:** corré `hpdctl charge set 80` una vez — lo más importante
   para la salud de la batería a largo plazo.
2. Usá **`tdp set`** (o `preset`) como tu única palanca de **potencia** — el
   valor que pongas es el límite real ahora.
3. **Dejá `cool auto`** salvo que quieras fijar los fans más fuertes o más suaves.
4. Mirá `hpdctl status`: la línea **Power** muestra el consumo real al lado
   de tu tope, así ves si estás limitado por potencia.
5. ¿Querés **potencia full con fans suaves** (o al revés)? Dale — las dos
   perillas son independientes ahora, así que ninguna combinación está "mal".

## Leer el tablero (status)

```
   ⚡ Power:            16W now · 18W TDP cap   ← consumo real · tu límite
   🧊 Cooling:          balanced (auto)         ← nivel + modo
   🌡️ Temps:            CPU 68°C · GPU 58°C
   💨 Fans:             CPU 5300 RPM · GPU 5300 RPM
   🔌 Power adapter:    🔋 Battery (DC)
   🔋 Battery Limit:    80%
```

- **Power** = los watts que el chip está usando *ahora mismo*, al lado del
  tope (TDP) que pusiste. En reposo está bajo; bajo carga sube hacia el
  tope (y puede pasarlo un instante, por el boost). Si "now" se queda muy
  por debajo del tope en un juego pesado de GPU, ese juego simplemente no
  usa todo el presupuesto — el cooling ya no limita la potencia. (Si
  ponés un platform profile `power-saver`, *eso* sí lo mantendría por
  debajo de tu TDP — pero el default `performance` no.)
- **Cooling `(auto)`** = la curva del ventilador sigue al TDP. `(manual)` =
  fijaste un nivel de fan.
- **Temps / Fans** son lecturas en vivo, directo del hardware.

## Dibujar la curva del ventilador

`hpdctl cool curve` te muestra la curva temperatura→velocidad real que
está corriendo el chip, en barras:

```
🌀 Fan curve: aggressive
  CPU fan  (temp → speed):
     40°C │██                      │  10%
     54°C │██████                  │  25%
     62°C │█████████               │  40%
     ...
     91°C │████████████████████████│ 100%
```

Se lee de izquierda a derecha: a medida que el chip se calienta, el
ventilador sube. Un nivel más alto sube toda la curva (más velocidad a
cada temperatura).

## Vida útil de la batería

Lo más efectivo para la salud de la batería a largo plazo es **limitar la
carga** (`hpdctl charge set 80`). Una batería de litio mantenida en 80 %
envejece mucho más lento que una en 100 %. Esto importa **mucho más** que
la temperatura o los ventiladores. Lo seteás una vez y queda.

## Qué es normal vs. cuándo preocuparse

Esta es la parte que pone nervioso a la gente. En corto: **las consolas
AMD modernas están hechas para correr calientes, y los ventiladores
fuertes significan que el enfriamiento funciona, no que falla.**

### Temperatura

| Lectura | Qué significa |
|---|---|
| 40–70 °C | Fresco. Reposo o uso liviano. |
| 70–90 °C | Tibio. Totalmente normal bajo carga. |
| 90–100 °C | Caliente pero **dentro de especificación** — estos APU aguantan ~100 °C. Normal con un TDP alto en un juego pesado. Si los fans trabajan fuerte, el enfriamiento está haciendo su trabajo. |
| 100 °C sostenido con tirones | El chip está *throttleando* (bajando solo) para protegerse — no se daña, pero perdés rendimiento. Bajá el **TDP** (menos calor) y/o subí el nivel de **cooling** (más aire), o revisá si hay polvo / rejillas tapadas. |

La temperatura ahora sigue tu **TDP** (cuán fuerte trabaja el chip), y el
nivel de **cooling** cambia ruido del fan por unos grados a esa potencia:

✅ **Normal:** ~78 °C en un juego de 40 W con fans `aggressive` (medido en
el Xbox Ally X). ¿Lo querés más fresco? **Bajá el TDP.** ¿Más silencioso?
**Bajá el nivel de cooling** (correrá un poco más caliente).

⚠️ **Para prestar atención:** temperatura alta (85 °C+) con los fans
**lentos** — eso significa que los fans *no* están subiendo (un fan
trabado, o el comportamiento conservador viejo del firmware). Con las
curvas de hpd esto no debería pasar; si pasa, corré `hpdctl cool curve` y
`hpdctl status` y verificá que las RPM suban con la temperatura.

### RPM del ventilador

Los fans del Ally X llegan a ~**8000–8100 RPM**. Estar ahí en un juego
pesado es normal y los fans están hechos para eso — **ruidoso no es
roto.** Preocupate solo por: un fan en **0 RPM bajo carga** (falla), o un
ruido de traqueteo/rozamiento (problema físico).

### ¿Algo de esto acorta la vida de la consola?

- **Calor:** estar dentro de especificación (bajo ~100 °C) en uso normal
  no acorta de forma significativa la vida del chip. El APU está diseñado
  para eso.
- **Ventiladores:** correr al máximo cuando hace falta es para lo que
  están; no los desgasta antes de tiempo.
- **Batería:** la palanca real de vida útil es el **tope de carga**, no el
  enfriamiento. Topealo en 80 % y ya estás haciendo lo importante.
- **Seguridad:** las curvas las corre el controlador embebido (EC), así
  que aunque `hpd` se caiga los fans siguen la última curva — nunca se
  frenan ni se quedan fijos.

## Comportamiento con AC, batería y al despertar

- **Enchufar AC — bloqueado en máximo rendimiento.** Por defecto, enchufar
  el cargador fija el device en su tope: **Power mode → Performance, TDP →
  Max, cooling → Aggressive**, y **bloquea** esos controles para que nada
  los cambie mientras estás en corriente (la CLI y el plugin rechazan los
  cambios de potencia/cooling con un mensaje "bloqueado en AC"). Tu **tope
  de carga de batería sigue ajustable** — es lo único que tiene sentido
  cambiar enchufado. Al **desenchufar**, se restauran tus ajustes de batería
  (DC) exactos: TDP, Power mode y cooling.
  - ¿No querés el bloqueo? Poné `ac_max_performance = false` en
    `/etc/hpd/config.toml` (y `sudo systemctl reload hpd`). Con eso, enchufar
    solo sube el TDP a Max y no bloquea nada — el comportamiento histórico.
  - **¿Instalado (o arrancado por primera vez) estando enchufado?** El daemon
    arranca bloqueado en máximo rendimiento, igual que si acabaras de
    enchufar. La **primera vez que desenchufás**, como todavía no registró una
    preferencia de batería, cae en valores tranquilos — **TDP Balanced con
    auto-cooling** (para que los fans se calmen) — en vez de quedarse con la
    curva Aggressive ruidosa. Después de ese primer desenchufe, tus ajustes se
    recuerdan normalmente.
- **Volver de suspensión:** hpd re-aplica tu potencia, platform profile,
  tope de carga y curva de ventilador — arreglando el bug donde los fans
  arrancaban a tope al despertar. Si volvés enchufado, el bloqueo de máximo
  rendimiento se re-afirma.
- **Reinicio:** el daemon re-aplica tu estado completo guardado (TDP,
  power mode, tope de carga, curva) al hardware al arrancar, así lo que
  reporta siempre coincide con el device — aunque un boot en frío haya
  reseteado los defaults del firmware por debajo.

## Para desarrolladores: Decky / D-Bus

Interfaz D-Bus `dev.cirodev.hpd.PowerDaemon1`. **El CLI ya no tiene el
namespace `fan` — el enfriamiento es un solo concepto (`cool`).** Una UI
debería reflejar eso: un control de cooling, no tres.

| Método / propiedad | Uso |
|---|---|
| `SetCoolingLevel(s)` | El control de cooling: `silent`/`balanced`/`aggressive` → **solo la curva** (la potencia no se toca). |
| `SetFanAuto()` | La curva sigue al TDP (modo auto). |
| `ResetFanCurve()` | Fans al firmware. |
| `GetThermalStatus() → (i,i,i,i)` | En vivo `(cpu_temp, gpu_temp, cpu_rpm, gpu_rpm)`; `i32::MIN` = sensor ausente. |
| `GetFanCurve() → (a(uu), a(uu))` | Los 8 puntos `(temp,pwm)` de las curvas CPU y GPU, para dibujar el gráfico. |
| `GetVersion() → (s)` | La versión del daemon (daemon ≥ 2.4.2; daemons viejos dan error → "unknown"). |
| `fan_curve` (prop) | Nivel activo: `silent`/`balanced`/`aggressive`/`custom`/`auto`. |
| `auto_cooling` (prop) | `true` = auto, `false` = manual. |
| `current_spl`, `active_profile`, `charge_end_threshold`, `is_ac_connected` | Estado de potencia / perfil / batería / AC. |
| `SetSpl(u)`, `SetPreset(s)`, `SetChargeThreshold(y)` | Setters de potencia y batería. |
| `SetProfile(s)` | La palanca de potencia (`performance`/`balanced`/`power-saver`), decoplada del cooling. |

**UI sugerida (post-desacople):**
- Un slider de **TDP** — *este es el control de potencia* (`current_spl` /
  `SetSpl`, rango desde `GetHardwareLimits`).
- Un selector de **Cooling**: Silent / Balanced / Aggressive + un toggle
  **Auto**, etiquetado como control **solo de ventilador** (ruido ↔
  temperatura), no de potencia. (Nivel desde `fan_curve`, modo desde
  `auto_cooling`.)
- Un control **"Power mode"** de primera clase (`active_profile` /
  `SetProfile`): Performance / Balanced / Eco, en la sección Power,
  claramente separado de Cooling. Default Performance; mostrá una nota
  informativa cuando Balanced/Eco recortan la potencia bajo el TDP (sin
  deshabilitar el slider — el techo real depende de la carga).
- **Lecturas en vivo** desde `GetThermalStatus` (temps + RPM) y un gráfico
  opcional de la curva desde `GetFanCurve`.
- Un control de **tope de batería** (`charge_end_threshold` /
  `SetChargeThreshold`).
- **Indicador de AC:** suscribite a la propiedad `AcConnected` (emite
  `PropertiesChanged`; daemon ≥ 2.4.0) — o polleá `is_ac_connected()` en
  daemons viejos. El fix del nodo `AC0` hace que el valor sea correcto en
  el Xbox Ally X.

Para el razonamiento térmico y los datos detrás de todo esto, ver
[`fan-curves.md`](fan-curves.md).
