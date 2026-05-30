<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# hpd — Manual de usuario 🇪🇸

Todo lo que hace `hpd` hasta hoy, en un solo lugar y explicado simple.
Versión en inglés: [`MANUAL.md`](MANUAL.md).

- [Qué es hpd](#qué-es-hpd)
- [Las dos perillas: Potencia y Enfriamiento](#las-dos-perillas)
- [Lista de comandos](#lista-de-comandos)
- [Qué hace cada combinación](#qué-hace-cada-combinación)
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

Cuán fuerte enfría. **Importante:** el nivel de enfriamiento no es solo la
velocidad del ventilador — también define **cuánta potencia real puede
usar el chip** (ver [el gateo](fan-curves.md)). Una sola palanca, tres
niveles:

| Nivel | Ventilador | Potencia real |
|---|---|---|
| `silent` | silencioso | baja (el chip queda limitado) |
| `balanced` *(default)* | moderado | media |
| `aggressive` | fuerte | full |

```
hpdctl cool set silent|balanced|aggressive   # elegí nivel (manual)
hpdctl cool auto       # que hpd elija el nivel según el TDP
hpdctl cool reset      # devolver el ventilador al firmware
hpdctl cool get        # ver nivel y modo
hpdctl cool curve      # dibujar la curva activa
```

**Auto vs manual:**
- **Auto** (default): hpd elige el nivel según tu TDP. Poco TDP →
  silencioso y fresco; mucho TDP → potencia full y ventiladores fuertes.
  Todo queda coherente solo.
- **Manual**: fijás un nivel y no se mueve, pase lo que pase con el TDP.

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
| `hpdctl charge set <%>` / `charge get` | Tope de carga de batería |

Los comandos de lectura no piden contraseña. Cambiar cosas no pide
contraseña si sos el dueño de la consola (grupo `wheel`) — incluso por
SSH; a otros usuarios se les pide autenticarse.

## Qué hace cada combinación

Las dos perillas interactúan. El panorama completo:

| Hacés | En cooling **auto** | En cooling **manual** |
|---|---|---|
| `tdp set` bajo | hpd baja el nivel (silencioso, fresco) | el TDP aplica dentro de tu nivel fijo |
| `tdp set` alto | hpd sube el nivel (potencia full, fans fuertes) | **solo aplica del todo si tu nivel es `aggressive`** |
| `cool set <nivel>` | pasa a manual en ese nivel | setea ese nivel |
| `cool auto` | (ya está en auto) | vuelve a auto |

**La única combinación a tener en cuenta:** *manual `silent` + un TDP
alto.* Es contradictoria — "limitá mi potencia para estar silencioso" y
"dame mucha potencia" al mismo tiempo. Gana el nivel de enfriamiento: el
chip queda limitado y el TDP alto **no tiene efecto** (queda guardado pero
inerte). Si querés que un TDP alto funcione de verdad, usá `cool auto` o
`cool set aggressive`. **En modo auto esto nunca pasa**, porque el nivel
siempre coincide con el TDP.

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
  por debajo del tope bajo carga pesada, algo más te está limitando (por
  ejemplo un nivel de cooling bajo — ver [combinaciones](#qué-hace-cada-combinación)).
- **Cooling `(auto)`** = el nivel sigue al TDP. `(manual)` = lo fijaste vos.
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
| 90–100 °C | Caliente pero **dentro de especificación** — estos APU aguantan ~100 °C. Normal a potencia full (`aggressive`). Si los fans trabajan fuerte, el enfriamiento está haciendo su trabajo. |
| 100 °C sostenido con tirones | El chip está *throttleando* (bajando solo) para protegerse — no se daña, pero perdés rendimiento. Bajá el nivel de cooling o el TDP, o revisá si hay polvo / rejillas tapadas. |

✅ **Normal:** 95 °C en `aggressive` con los fans al máximo — es el chip a
potencia full. Elegí `balanced` (≈68 °C) o `silent` (≈58 °C) si lo querés
más fresco.

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

- **Enchufar AC:** hpd sube la potencia (y, en cooling auto, el nivel con
  ella), y restaura tu config de batería al desenchufar.
- **Volver de suspensión:** hpd re-aplica tu potencia, perfil de cooling,
  tope de carga y curva de ventilador — arreglando el bug donde los fans
  arrancaban a tope al despertar.
- **Reinicio:** tus últimos ajustes se restauran del disco.

## Para desarrolladores: Decky / D-Bus

Interfaz D-Bus `dev.cirodev.hpd.PowerDaemon1`. **El CLI ya no tiene el
namespace `fan` — el enfriamiento es un solo concepto (`cool`).** Una UI
debería reflejar eso: un control de cooling, no tres.

| Método / propiedad | Uso |
|---|---|
| `SetCoolingLevel(s)` | El control principal: `silent`/`balanced`/`aggressive` → perfil + curva juntos. |
| `SetFanAuto()` | El cooling sigue al TDP (modo auto). |
| `ResetFanCurve()` | Fans al firmware. |
| `GetThermalStatus() → (i,i,i,i)` | En vivo `(cpu_temp, gpu_temp, cpu_rpm, gpu_rpm)`; `i32::MIN` = sensor ausente. |
| `GetFanCurve() → (a(uu), a(uu))` | Los 8 puntos `(temp,pwm)` de las curvas CPU y GPU, para dibujar el gráfico. |
| `fan_curve` (prop) | Nivel activo: `silent`/`balanced`/`aggressive`/`custom`/`auto`. |
| `auto_cooling` (prop) | `true` = auto, `false` = manual. |
| `current_spl`, `active_profile`, `charge_end_threshold`, `is_ac_connected` | Estado de potencia / perfil / batería / AC. |
| `SetSpl(u)`, `SetPreset(s)`, `SetChargeThreshold(y)` | Setters de potencia y batería. |
| `SetProfile(s)`, `SetFanCurve(s)` | Controles crudos y decoplados (avanzado; solo tienen sentido con `fan_curve_follows_profile = false`). |

**UI sugerida:**
- Un selector de **Cooling**: Silent / Balanced / Aggressive + un toggle
  **Auto**. (Nivel desde `fan_curve`, modo desde `auto_cooling`.)
- Un slider de **TDP** (`current_spl` / `SetSpl`, rango desde
  `GetHardwareLimits`).
- **Lecturas en vivo** desde `GetThermalStatus` (temps + RPM) y un gráfico
  opcional de la curva desde `GetFanCurve`.
- Un control de **tope de batería** (`charge_end_threshold` /
  `SetChargeThreshold`).
- Mostrá un aviso suave si el usuario fija un nivel de cooling bajo
  mientras pide un TDP alto (ver "la única combinación a tener en cuenta").

Para el razonamiento térmico y los datos detrás de todo esto, ver
[`fan-curves.md`](fan-curves.md).
