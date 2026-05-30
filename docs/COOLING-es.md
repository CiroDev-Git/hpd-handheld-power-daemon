<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Enfriamiento y potencia en hpd — explicado simple 🇪🇸

Guía en español, sin tecnicismos, de cómo `hpd` maneja la potencia y el
enfriamiento de tu consola. Si te perdiste entre "perfil", "modo",
"curva", "clamp", "auto" — esto es para vos.

## Las dos cosas que controlás

Solo hay **dos perillas** que importan:

### 1. ⚡ TDP — cuánta potencia puede pedir el chip

`hpdctl tdp set 20` = "dejá que el APU use hasta 20 watts".
Más watts = más rendimiento y más calor. Menos = más fresco y más batería.

### 2. 🧊 Cooling — cuán fuerte enfría (y cuánta potencia REAL permite)

`hpdctl cool set <nivel>`, con tres niveles:

| Nivel | Ventilador | Potencia real |
|---|---|---|
| `silent` | Silencioso | **Baja** (el chip queda limitado) |
| `balanced` | Moderado | Media |
| `aggressive` | Fuerte | **Full** (el chip puede usar todo) |

Y dos comandos más:
- `hpdctl cool auto` → que `hpd` elija el nivel solo, según el TDP.
- `hpdctl cool reset` → devolverle el ventilador al firmware (como de fábrica).
- `hpdctl cool get` → ver el nivel y el modo actual.

## 💡 Lo importante (y lo que descubrimos midiendo)

Acá está la clave que sorprende: **el nivel de cooling NO es solo la
velocidad del ventilador.** También define **cuánta potencia real puede
sacar el chip.** No es solo ruido — es rendimiento.

Lo medimos en la consola, mismo juego/carga, mismo `tdp set 30`:

- En **silent** (silencioso) → CPU se quedó en **59 °C**
- En **aggressive** (performance) → CPU llegó a **95 °C**

¿Por qué tanta diferencia con el mismo TDP? Porque en `silent` el firmware
**capa** (limita, "clampea") la potencia real muy por debajo de lo que
pediste, para mantenerse fresco y callado. En `aggressive` deja pasar
toda la potencia.

👉 **Por eso `cool` mueve el perfil Y la curva juntos: son la misma
decisión.** "Quiero potencia full y enfriar fuerte" = `aggressive`.
"Quiero silencio y poca potencia" = `silent`.

## 🔁 Auto vs Manual — cuál usar

### Auto (`cool auto`) — el modo por defecto, el recomendado

`hpd` elige el nivel de cooling según el TDP que tengas. Si pedís poco
TDP, baja el nivel (silencioso); si pedís mucho, lo sube (full). **Todo
queda coherente solo.** No tenés que pensar.

### Manual (`cool set <nivel>`) — para cuando querés fijar algo

Fijás un nivel y no se mueve, pase lo que pase con el TDP. Útil para:
- **`cool set aggressive`** jugando algo exigente: querés máximo
  rendimiento + máximo enfriamiento, sin que nada lo baje.
- **`cool set silent`** leyendo, viendo video o emulando algo liviano:
  querés silencio total y no te importa el rendimiento.

## ⚠️ La combinación que NO tiene sentido (y por qué)

**Manual `silent` + `tdp set 30`** (o cualquier "poco enfriamiento + mucho
TDP").

Es una contradicción: estás diciendo *"limitá la potencia para estar
silencioso"* y al mismo tiempo *"dame 30 watts de potencia"*. No se puede
tener las dos. Lo que gana es el nivel de cooling: **el chip queda
clampeado en silent, y esos 30W no se usan de verdad.** El número de TDP
queda "guardado", pero no tiene efecto hasta que subas el nivel de
cooling.

**Recomendación:** si querés que un TDP alto realmente funcione, usá
`cool auto` (que sube el nivel solo) o `cool set aggressive`. En `silent`,
el TDP alto no hace nada.

> Nota: en una versión próxima, si intentás esta combinación contradictoria
> en modo manual, `hpd` te va a **avisar** ("este nivel limita la potencia,
> tu TDP no se va a aplicar del todo") pero igual te deja hacerlo — no te
> bloquea ni te pide confirmación, solo te informa. La idea es no tratarte
> como tonto, solo avisarte de algo que no es obvio.

## 📋 Resumen de comandos

```fish
hpdctl tdp set 18              # potencia: hasta 18W
hpdctl cool set aggressive     # enfriar full (perfil + curva)
hpdctl cool auto               # que hpd elija el cooling según el TDP
hpdctl cool reset              # ventilador como de fábrica
hpdctl cool get                # ver nivel + modo
hpdctl status                  # tablero: TDP, cooling, temps, RPM, batería
```

---

## Para desarrolladores (Decky plugin, otros clientes D-Bus)

Lo que cambió de cara a un cliente externo:

### CLI
- **Se eliminó el namespace `fan`** del CLI (`fan set`, `fan auto`,
  `fan profile`, `fan curve …` ya no existen).
- Todo el enfriamiento vive bajo **`cool`**: `set <silent|balanced|aggressive>`,
  `auto`, `reset`, `get`.

### D-Bus (interfaz `dev.cirodev.hpd.PowerDaemon1`)
La superficie cruda sigue disponible para clientes/GUIs avanzados:

| Método / propiedad | Qué hace |
|---|---|
| `SetCoolingLevel(level: s)` | **El de alto nivel.** `silent`/`balanced`/`aggressive` → setea perfil + curva juntos. Es lo que llama `cool set`. |
| `SetFanAuto()` | Cooling sigue al TDP (lo que llama `cool auto`). |
| `ResetFanCurve()` | Ventilador a firmware (lo que llama `cool reset`). |
| `SetProfile(profile: s)` | Crudo: solo el ACPI platform_profile (`power-saver`/`balanced`/`performance`). |
| `SetFanCurve(preset: s)` | Crudo: solo la curva, sin tocar el perfil. |
| `GetThermalStatus() → (i,i,i,i)` | `(cpu_temp, gpu_temp, cpu_rpm, gpu_rpm)`; `i32::MIN` si un sensor no existe. |
| `fan_curve` (prop) | Nivel/curva activa: `silent`/`balanced`/`aggressive`/`custom`/`auto`. |
| `auto_cooling` (prop) | `true` = modo auto (sigue TDP), `false` = manual. |
| `current_spl`, `active_profile`, `charge_end_threshold`, `is_ac_connected` | Estado, como antes. |

### Recomendación para el plugin
Mostrá **un solo control de cooling** (silent/balanced/aggressive + un
toggle auto), no tres. Para "nivel actual" usá la propiedad `fan_curve`;
para el modo, `auto_cooling`. Para temps/RPM en vivo, `GetThermalStatus`.
Si necesitás el control crudo decoplado (perfil ≠ curva), está en
`SetProfile`/`SetFanCurve` + `fan_curve_follows_profile = false` en la
config, pero para el 99% de los usuarios `SetCoolingLevel` es todo.

Detalle técnico completo: [`docs/fan-curves.md`](fan-curves.md).
