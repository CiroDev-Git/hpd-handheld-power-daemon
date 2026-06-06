<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Potencia y enfriamiento en hpd — explicado simple 🇪🇸

Guía en español, sin tecnicismos, de cómo `hpd` maneja la **potencia** y el
**enfriamiento** de tu consola. Si te perdiste entre "perfil", "modo",
"curva", "EPP", "auto" — esto es para vos.

> 🖼️ **¿Preferís verlo en diagramas?** Toda esta info en imágenes (el
> daemon, el plugin y cómo se comunican, con todas las combinaciones) está
> en [`DIAGRAMS-es.md`](DIAGRAMS-es.md).

> **¿Cambió algo importante?** Sí. Antes el nivel de cooling **también
> decidía cuánta potencia usaba el chip** (estaban pegados). Eso confundía:
> ponías "TDP 25W" pero el chip a veces usaba 13W y no entendías por qué.
> **Ahora están separados.** Más abajo está el por qué y cómo funciona.

---

## Las dos perillas (ahora de verdad independientes)

### 1. ⚡ TDP — cuánta potencia puede pedir el chip

```
hpdctl tdp set 20      # "el APU puede usar hasta 20 watts"
```

Más watts = más rendimiento y más calor. Menos watts = más fresco y más
batería. **El TDP que pongas es el límite real.** Si pedís 20W, el chip
puede llegar a 20W (no se queda corto por culpa del cooling).

### 2. 🧊 Cooling — cuán fuerte trabaja el ventilador

```
hpdctl cool set aggressive   # ventilador a tope (más frío, más ruido)
hpdctl cool set balanced     # equilibrio
hpdctl cool set silent       # ventilador suave (más silencio)
hpdctl cool auto             # que hpd elija la curva según el TDP
hpdctl cool reset            # ventilador como de fábrica (firmware)
hpdctl cool get              # ver nivel + modo actual
```

El cooling **solo controla el ventilador** (ruido ↔ temperatura). **No
toca la potencia.** Podés tener "potencia alta + ventilador silencioso" o
"potencia baja + ventilador fuerte" — cualquier combinación es válida.

---

## 💡 La razón del cambio (lo que descubrimos midiendo)

Medimos en la consola, mismo juego, mismo `tdp set` alto, cambiando solo el
nivel de cooling, y vimos algo que no se ve a simple vista:

| Nivel viejo | Potencia REAL del chip | Temperatura |
|---|---|---|
| `silent` | **~13 W** | ~54 °C |
| `balanced` | **~17–21 W** | ~62 °C |
| `aggressive` | **~40 W** | ~78 °C |

¿Por qué con el **mismo TDP** el consumo era tan distinto? Porque el viejo
nivel de cooling, por debajo, cambiaba el **`platform_profile`** del sistema
(un ajuste del firmware/EPP que decide qué tan agresivo va el chip). En
`silent` ese perfil **capaba** la potencia muy por debajo de tu TDP. O sea:
`silent` no enfriaba por el ventilador — **enfriaba porque frenaba el
chip.**

**El problema:** eso hacía que "TDP 25W" no significara 25W. Tu número de
potencia quedaba pisado por el nivel de cooling, sin que fuera obvio.

**La solución (este cambio):** **separamos las dos cosas.**

- El **TDP** es ahora la **única** perilla de potencia. Lo que pongas, eso
  manda.
- El **cooling** es **solo el ventilador**. Más fuerte = más frío y más
  ruido; más suave = más silencio. Nada más.
- El `platform_profile` quedó fijo en **`performance`** por defecto (la
  perilla de potencia "abierta"), así tu TDP siempre se aplica de verdad.

---

## 🔁 Auto vs Manual del cooling

### Auto (`cool auto`) — el modo por defecto

`hpd` elige la **curva del ventilador** según el TDP que tengas: poco TDP →
curva silenciosa; mucho TDP → curva agresiva. Ya no toca la potencia, solo
ajusta el ventilador a cuánto calor esperás generar. No tenés que pensar.

### Manual (`cool set <nivel>`) — para fijar el ventilador

Fijás un nivel de ventilador y no se mueve, pase lo que pase con el TDP.

---

## ✅ La combinación que antes "no tenía sentido" — ahora SÍ funciona

Antes, `silent` + `tdp set 30` era contradictorio (el silent te frenaba la
potencia). **Ya no.** Ahora significa exactamente lo que parece:

> **"Quiero 30W de potencia, pero con el ventilador suave."**

El chip usará hasta 30W *de verdad*, y el ventilador irá tranquilo —
probablemente el chip se ponga más caliente (porque trabaja a 30W con poco
aire), pero es **tu** decisión y se respeta. Sin sorpresas, sin números
"guardados que no hacen nada".

---

## 🎛️ La perilla avanzada: `platform_profile` (potencia/EPP)

Para el 99% de la gente: **no la toques, dejala en `performance`.** Es la
que hace que tu TDP se aplique al máximo.

Si sos usuario avanzado y querés sesgar la eficiencia (gastar un poco menos
a igual carga, a costa de pico de rendimiento), podés bajarla:

```
# por D-Bus (no hay subcomando dedicado en el CLI todavía)
# o en /etc/hpd/config.toml:
default_platform_profile = "balanced"   # o "power-saver"
```

`performance` / `balanced` / `power-saver` (acepta también los alias ACPI
`quiet` / `low-power`). Por defecto: `performance`.

---

## 📋 Resumen de comandos

```fish
hpdctl tdp set 18              # POTENCIA: hasta 18W reales
hpdctl cool set aggressive     # VENTILADOR: a tope (frío + ruidoso)
hpdctl cool set silent         # VENTILADOR: suave (silencioso)
hpdctl cool auto               # ventilador que sigue al TDP
hpdctl cool reset              # ventilador como de fábrica
hpdctl status                  # tablero: TDP, cooling, temps, RPM, batería
```

Regla mental:
- **¿Querés más/menos rendimiento o batería?** → `tdp`.
- **¿Querés más/menos ruido?** → `cool`.

---

## Para desarrolladores (Decky plugin, otros clientes D-Bus)

### Qué cambió a nivel de comportamiento

1. **`SetCoolingLevel` ya NO cambia la potencia.** Solo programa la curva
   de ventilador + fija modo manual. El `platform_profile` no se toca.
2. **El auto-follow (`fan_follows_tdp`) infiere la CURVA**, no el perfil.
3. **El `platform_profile` arranca en `performance`** (config
   `default_platform_profile`) y ya no se deduce del TDP.
4. `SetProfile` queda como la perilla manual de potencia (decoplada del
   cooling). `fan_curve_follows_profile` quedó sin efecto (no-op).

### D-Bus (interfaz `dev.cirodev.hpd.PowerDaemon1`)

| Método / propiedad | Qué hace ahora |
|---|---|
| `SetSpl(w)` / `SetEnvelope(...)` | **La perilla de potencia.** El SPL es el límite real. |
| `SetCoolingLevel(level: s)` | **Solo ventilador**: `silent`/`balanced`/`aggressive`. Lo que llama `cool set`. |
| `SetFanAuto()` | La curva sigue al TDP (`cool auto`). |
| `ResetFanCurve()` | Ventilador a firmware (`cool reset`). |
| `SetProfile(profile: s)` | Perilla de potencia manual: `performance`/`balanced`/`power-saver`. |
| `GetThermalStatus() → (i,i,i,i)` | `(cpu_temp, gpu_temp, cpu_rpm, gpu_rpm)`; `i32::MIN` si falta un sensor. |
| `fan_curve` (prop) | Curva activa: `silent`/`balanced`/`aggressive`/`custom`/`auto`. |
| `auto_cooling` (prop) | `true` = auto (sigue TDP), `false` = manual. |
| `current_spl`, `active_profile`, `charge_end_threshold`, `is_ac_connected` | Estado. |

### Qué debería hacer el plugin (para que el cambio se entienda)

- **Separar visualmente las dos perillas.** El control de "Cooling" debe
  decir que ajusta **solo el ventilador** (ruido ↔ temperatura), **no** la
  potencia. El texto viejo tipo *"Silent caps power / Aggressive unlocks
  the full TDP"* ya no aplica y confunde — hay que cambiarlo.
- **El TDP es la perilla de potencia**, sola y suficiente.
- **(Opcional) Exponer `platform_profile`** como un control avanzado de
  "modo de energía" (Performance/Balanced/Eco) usando `SetProfile`, claramente
  separado de "Cooling". Para la mayoría, dejarlo en Performance y ocultarlo
  está bien.
- **AC en vivo:** el plugin ya pollea `is_ac_connected`; con el fix del nodo
  `AC0` ese valor ahora es correcto en el Xbox Ally X (antes devolvía
  siempre "batería").

Detalle técnico completo: [`docs/fan-curves.md`](fan-curves.md) y la guía de
integración del plugin en [`docs/decky-plugin/`](decky-plugin/).
