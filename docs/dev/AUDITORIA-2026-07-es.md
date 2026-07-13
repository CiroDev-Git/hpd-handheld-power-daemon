# Auditoría integral — hpd 2.7.2 en ROG Xbox Ally X (RC73XA) + CachyOS

> **📦 ARCHIVADO — snapshot histórico, no un documento vivo.** Los 11
> ítems de §8 ("Corrección — hacer ya" + "Ecosistema CachyOS — hacer
> pronto") están **todos resueltos**, confirmado contra el `CHANGELOG.md`
> de la entrada `[2.7.3]`. La sección de roadmap (§6) está superseded por
> [`GAMING-ROADMAP-es.md`](GAMING-ROADMAP-es.md) — "ante discrepancias,
> manda el roadmap" (ya lo dice este mismo documento). Se conserva sin
> borrar porque `CHANGELOG.md` lo cita como registro histórico de la
> auditoría 2.7.3/2.10.0; léase como eso, no como una lista de pendientes.

> Fecha: 2026-07-07 · Base: `main` @ `4f65db2` (workspace 2.7.2)
> Objetivo: sacar el máximo provecho del daemon + plugin Decky en un
> ROG Xbox Ally X (2025, RC73XA) con CachyOS, apuntando a la mayor
> paridad posible con Armoury Crate SE + Radeon Adrenalin (Windows).
>
> Alcance: revisión de código completa de los 9 crates, packaging
> (systemd/polkit/install.sh) y docs. **No** se pudo ejecutar
> `cargo test` / `clippy` en esta máquina (sin toolchain Rust instalado);
> los hallazgos son de lectura de código, no de ejecución.

---

## 1. Resumen ejecutivo

El proyecto está en muy buen estado: la máquina de estados
(Transition → reduce → Effect) es sólida, el rollback por `Sync*` es un
diseño correcto y raro de ver, los monitores se auto-reconectan (2.7.2),
el sandboxing systemd es ejemplar y la cobertura de tests del reducer es
alta. No se encontró ningún bug "rompe-dispositivo".

Lo que sí hay:

- **2 bugs funcionales reales** (uno confirmado por un comentario en el
  propio test): `EnableFanAuto` no persiste ni re-infiere la curva
  (§2.1), y un auto-deadlock teórico del executor en el rollback (§2.2).
- **1 clase de fallo por configuración no validada** que puede dejar
  *todos* los cambios de TDP fallando en silencio (§2.3).
- **Duplicidades internas** (dos detecciones de AC distintas; lista de
  rivales duplicada daemon/CLI sin test de sincronía) (§4).
- **Huecos en la detección de herramientas del sistema que pelean con
  hpd en CachyOS**: TLP no está en la lista de rivales, `tuned-ppd.service`
  no se enmascara, y el caso `hhd` en el Xbox Ally X merece un matiz
  importante (enmascararlo entero rompe los mandos) (§5).
- **Un roadmap claro de features** para acercarse a Armoury Crate +
  Adrenalin: curvas de ventilador personalizadas por D-Bus (retiradas en
  2.5.0), control de reloj de GPU, toggle de CPU boost, perfiles por
  juego, y telemetría de batería (§6).

---

## 2. Bugs y riesgos de corrección (por severidad)

### 2.1 `EnableFanAuto` no aplica la curva ni persiste el flag — **ALTA**

`hpd-core/src/reducer.rs:306-318`: al activar auto-cooling se flipa
`fan_follows_tdp = true` y se re-reduce con `SetEnvelope(power_target
actual)`. Como el envelope **no cambia**, `apply_power_target`
(`reducer.rs:392`) devuelve **cero efectos**: ni `ApplyFanCurve` (la
curva manual anterior sigue programada en el EC hasta el próximo cambio
de TDP) ni `PersistState` (el flag se pierde si el daemon se reinicia
antes de otro evento que persista).

El propio test lo reconoce (`reducer.rs:989-995`): *"Persistence of the
flag itself is a separate concern tracked in the audit"*.

**Efecto visible en tu Ally**: `hpdctl cool auto` (o el toggle del
plugin) parece funcionar (la propiedad `AutoCooling` cambia a `true`),
pero los ventiladores siguen con la curva manual anterior, y tras un
reinicio el modo vuelve a manual.

**Arreglo propuesto**: en el brazo `EnableFanAuto`, inferir la curva del
SPL actual con `infer_fan_curve_from_spl` y emitir `ApplyFanCurve` +
`PersistState` incondicionalmente (misma forma que `SetCoolingLevel`).

### 2.2 Auto-deadlock potencial del executor en el rollback — **MEDIA (probabilidad baja, coste de arreglo mínimo)**

`hpd-core/src/executor.rs:299`: `Executor::rollback` hace
`self.internal_tx.send(transition).await` sobre el **mismo canal mpsc
acotado (cap. 32 por defecto) que el propio executor drena**. El `send`
ocurre dentro del bucle de efectos, es decir, mientras el único
consumidor está ocupado. Si el canal está lleno (ráfaga de CLI/plugin +
evento AC + resume), el `send().await` no progresa nunca → el executor
queda congelado para siempre y con él todo el daemon (los setters D-Bus
devolverán `executor_down` cuando el canal cierre… que nunca cierra).

**Arreglo**: usar `try_send` y loguear el descarte (el estado divergido
se re-converge en el siguiente boot/resume de todas formas), o un canal
interno separado sin límite solo para los `Sync*`.

### 2.3 `RuntimeConfig` sin validar puede romper todos los cambios de TDP — **MEDIA**

- `derive_boosted_envelope` (`reducer.rs:457-471`) no aplica suelo: si un
  operador pone `sppt_factor = 0.9` en `/etc/hpd/config.toml` (o en un
  hardware raro `sppt_max < spl_max`), el envelope resultante viola
  `SPPT ≥ SPL` y `validate_power_envelope` rechaza **todos** los
  `SetSpl`/`SetPreset` y también el forzado del AC-lock — el daemon
  queda "sano" pero inservible, con solo un error en el journal.
- Ni `DaemonConfig::load` ni el intercept de `ConfigReload` validan
  rangos: `sppt_factor`/`fppt_factor` ≤ 0 o < 1, `low_frac > high_frac`,
  valores fuera de `[0,1]`… todo se acepta.

**Arreglo**: (a) clamp inferior en `derive_boosted_envelope`
(`sppt = sppt.max(spl)`, `fppt = fppt.max(sppt)`); (b) un
`RuntimeConfig::sanitized()` que corrija/rechace valores absurdos al
cargar y al recargar, con warning.

### 2.4 Persistencia sin `fsync` — **MEDIA-BAJA (pero es un handheld)**

`hpd-core/src/persistence.rs:51-71`: `tokio::fs::write` + `rename` sin
`sync_all()` del fichero ni fsync del directorio. El rename es atómico
frente a crashes del proceso, pero **no** frente a un corte de energía
(batería agotada de golpe, hold del botón — escenarios muy handheld):
puedes acabar con un `state.toml` vacío o truncado. El fallback a
defaults evita el desastre, pero pierdes umbral de carga, preferencia de
AC-lock y snapshot DC.

**Arreglo**: `File::create` + `write_all` + `sync_all` antes del rename
(y opcionalmente fsync del directorio). Coste: una llamada.

### 2.5 `SetCoolingLevel` sin comprobación de no-op — **BAJA**

`reducer.rs:124-133`: a diferencia de `SetProfile` /
`ChargeThresholdChanged` / `ResetFanCurve`, siempre emite
`ApplyFanCurve` + `PersistState` aunque la selección no cambie. Cada
pulsación repetida del plugin = 34 escrituras sysfs al EC + un ciclo de
lectura-verificación + una escritura a disco. Inconsistente con el resto
de levers (aunque re-escribir el EC "por si acaso" tiene un valor
defensivo, la persistencia sobra).

### 2.6 Solo `CurrentSpl` es propiedad D-Bus; SPPT/FPPT invisibles — **BAJA**

`hpd-dbus/src/service.rs:154-158` + emisor en
`hpd-daemon/src/main.rs:637`: un `SetEnvelope` que cambie solo SPPT/FPPT
no emite ninguna señal (y no existe propiedad para leerlos). El plugin
no puede reflejar el envelope completo sin polling. Sugerencia: propiedad
`PowerEnvelope (uuu)` o incluir SPPT/FPPT en el diff del emisor.

### 2.7 Menores / cosméticos

- **CLAUDE.md desactualizado**: dice "Current release: 2.7.1"; el
  workspace ya es 2.7.2.
- `resolve_target_string` (`hpd-backend-asus/src/profile.rs:62`) asume
  que `performance` siempre está en `platform_profile_choices` (cierto
  en ASUS; el rollback lo cubriría si no).
- `get_thermal_status` llama `self.backend.thermal()` tres veces por
  invocación (`service.rs:389-415`); trivial pero gratuito de arreglar.
- `preset_curves` devuelve la misma curva para CPU y GPU
  (`fan_curve.rs:107-114`) — documentado como pendiente de calibración
  por modelo; en RC73XA funciona porque el EC evalúa cada pwm contra su
  propio sensor.

---

## 3. Cosas verificadas que están BIEN (no tocar)

Para que la auditoría no invite a "arreglar" lo que ya es correcto:

- **RC73XA detectado** (`detect.rs:36`), rutas `asus-armoury`
  verificadas contra el driver upstream, nodo AC `AC0` cubierto
  (regresión con test), edge de carga USB-C PD (el evento llega por
  `ucsi-*`, no por el nodo Mains) resuelto en `hpd-netlink` re-leyendo
  el nodo canónico.
- Orden de efectos potencia → perfil → **curva al final** (el write de
  `platform_profile` puede tirar la curva del EC) respetado en los tres
  sitios que importan (force-max, SetProfile, boot/resume), con tests de
  ordenación.
- Re-lectura del estado AC real antes de reducir `SystemResumed`
  (executor.rs:113-122) — cubre el (des)enchufe durante suspensión.
- Read-back y fail-closed de la curva de ventilador
  (`fan_curve.rs:218-225`), y `active_selection` que mapea los puntos
  vivos del EC a preset/custom/auto.
- hwmon resuelto **por nombre**, nunca por índice, con el decoy
  `acpi_fan` cubierto por test (bug real del Xbox Ally X).
- Unit systemd: `PrivateNetwork=no` + `IPAddressDeny=any` (la nota sobre
  uevents netlink es correcta y valiosa), `Conflicts=` solo con PPD por
  la razón documentada (la regresión v2.2.2 con tuned D-Bus-activable).
- El debounce de `AcPowerChanged` que evita re-snapshotear el estado
  forzado como si fuera el de batería.

---

## 4. Duplicidades y solapamientos internos

### 4.1 Dos detectores de AC distintos

- `hpd-netlink::read_mains_online_at` escanea `/sys/class/power_supply`
  buscando `type == "Mains"` (robusto, agnóstico al nombre).
- `AsusChargeBackend::is_ac_connected` (`charge.rs:16-53`) prueba una
  **lista fija** de 6 rutas (`AC0`, `AC1`, `AC`, `ACAD`, `ADP0`, `ADP1`)
  y toma la primera legible.

Es el mismo dato con dos algoritmos: el arranque y el resume usan la
lista fija; los eventos en vivo usan el escaneo por tipo. Si un firmware
futuro nombra el nodo distinto (ya pasó con `AC0`), el boot informará DC
mientras el monitor en vivo informa AC. **Unificar**: que el backend use
también el escaneo por `type == Mains` (o que ambos compartan un helper
en `hpd-sysfs`).

### 4.2 Lista de rivales duplicada daemon/CLI sin candado

`hpd-dbus/src/conflicts.rs` (detección) y `hpd-cli/src/doctor.rs:41-46`
(reparación) mantienen listas espejo a mano — el comentario lo admite
("the list is mirrored here"). Hoy coinciden; nada impide el drift (el
próximo rival añadido a la detección no se enmascarará). Opciones: mover
las constantes a un crate mínimo compartido (hpd-capabilities ya es dep
de ambos... la CLI no depende de hpd-dbus a propósito), o al menos un
test de integración a nivel workspace que compare ambas listas.

### 4.3 Polling del plugin vs. señales

El diseño ya empuja `PropertiesChanged` para casi todo (bien), pero la
telemetría (`get_thermal_status`) es pull 1 Hz. Cada llamada re-escanea
hwmon **desde cero**: `find_hwmon_by_name` sonda hasta 24 índices ×
5 consultas (cpu temp, gpu temp, 2 RPM, SoC power) ≈ 100+ opens de
sysfs por segundo con el plugin + `hpdctl monitor` abiertos.
**Optimización**: cachear la ruta hwmon resuelta (p. ej. `OnceLock` por
nombre con invalidación si una lectura falla — los índices solo cambian
entre boots o al recargar drivers).

---

## 5. Herramientas del sistema que compiten con hpd en CachyOS (RC73XA)

Esto responde a "cómo influyen otras herramientas en que el daemon no
consiga su objetivo". Estado actual de la detección y huecos:

| Herramienta | Presencia en CachyOS | ¿Detectada por hpd? | Veredicto |
|---|---|---|---|
| `power-profiles-daemon` | Default en KDE/GNOME | ✅ rival (bus) + `Conflicts=` | Cubierta |
| `tuned` / `tuned-ppd` | Opcional | ✅ rival (bus) / ⚠️ ver 5.2 | Hueco menor |
| `steamos-manager` | Sesión gamescope | ✅ rival + mask user-level | Cubierta |
| `hhd` (Handheld Daemon) | **Default en CachyOS Handheld Edition** | ✅ rival (unidad) / ⚠️ ver 5.1 | Matiz importante |
| `asusd` (asusctl) | Si lo instalas | ✅ advisory (no se enmascara) | Correcto |
| `gamemoded` | Común con Steam | ✅ advisory | Correcto |
| `auto-cpufreq` | Opcional | ✅ advisory (unidad) | Correcto |
| **TLP** | Popular en Arch/CachyOS | ❌ **no detectada** | **Hueco (5.3)** |
| **LACT** (GPU control) | Popular para amdgpu | ❌ no detectada | Hueco advisory (5.4) |
| scx / `scx_loader` (sched-ext CachyOS) | Default | n/a — no toca TDP/EPP/curvas | Compatible, ignorar |
| `ananicy-cpp`, `cachyos-settings` | Default | n/a — nice/ionice, sysctl | Compatible, ignorar |
| MangoHud / gamescope | Sesión | n/a — solo lectura / FPS cap | Compatible (ver §6) |
| SimpleDeckyTDP / PowerControl (Decky) | Si los instalas | ❌ indetectables (documentado) | **Desinstálalos tú** — pelearán con hpd |

### 5.1 `hhd` en el Xbox Ally X: enmascarado **condicional a InputPlumber**

`hpdctl doctor --fix` hace `disable + mask` de `hhd@.service`
incondicionalmente. En el Xbox Ally X, **hhd no es solo TDP**: también
remapea el mando integrado (botones Xbox/ROG, giroscopio, rumble). Ahora
bien, en CachyOS (validado en este equipo) el rol de input lo cubre
**InputPlumber** (`inputplumber.service`, bus del sistema
`org.shadowblip.InputPlumber`), así que enmascarar hhd es seguro *si y
solo si* hay un reemplazo de input activo.

**Diseño propuesto (mask condicional):** en
`hpd-cli/src/doctor.rs::neutralize_rivals_as_root`, tratar
`hhd@.service` como caso especial:

1. Comprobar el reemplazo de input:
   `systemctl is-active --quiet inputplumber.service` (suficiente en la
   CLI; no hace falta D-Bus).
2. **Si InputPlumber está activo** → `disable + stop + mask` de hhd como
   hoy, imprimiendo el motivo ("input cubierto por InputPlumber").
3. **Si no lo está** → NO enmascarar; imprimir advertencia con las dos
   salidas: (a) instalar/activar InputPlumber y relanzar
   `doctor --fix`, o (b) desactivar solo el módulo TDP de hhd
   (`hhd.settings`: sección tdp → enabled: false) conservando hhd para
   el mando.

La **detección** del daemon (`conflicts.rs`) no cambia: hhd sigue
reportándose como rival (sí pelea por el TDP); lo condicional es solo la
**reparación**. Idealmente el mensaje de `doctor` (sin `--fix`) ya
anticipa cuál de las dos ramas tomaría.

### 5.2 `tuned-ppd.service` no se enmascara

`doctor.rs` enmascara `tuned.service`, pero el shim `tuned-ppd` (que
reclama el nombre D-Bus de PPD) corre como **unidad separada**
`tuned-ppd.service` en Arch/CachyOS. Tras el fix, el bus puede seguir
reviviendo el shim. Añadir `tuned-ppd.service` a `RIVAL_UNITS` de la CLI
(y opcionalmente a la detección).

### 5.3 TLP es un rival duro no detectado

TLP escribe `charge_control_end_threshold`, `platform_profile`/EPP y
gobernadores en cada cambio AC/batería — exactamente las superficies de
hpd, con la misma lógica de "reaccionar al enchufe" (pelea directa con
el AC-lock: TLP y hpd se pisarán en cada edge). No tiene nombre D-Bus
propio → añadir `("tlp", "tlp.service")` a `RIVAL_UNITS` en
`conflicts.rs` y a la lista de la CLI.

### 5.4 LACT como advisory

LACT (`lactd.service`) gestiona clocks/power cap de amdgpu. No pisa
SPL/SPPT/FPPT, pero sí el power cap de la GPU, que interactúa con el
envelope. Candidato natural a `ADVISORY_UNITS`, sobre todo si añadís la
capability de GPU (§6.2).

### 5.5 Superficies de UI del sistema que tocan las mismas palancas

Inventario de todo lo que un usuario ve en pantalla (KDE y Steam) que
puede escribir sobre las superficies de hpd, y qué pasa con cada una
cuando hpd es el dueño:

**a) El selector del icono de batería de KDE (Eco/Equilibrado/Rendimiento).**
Ese menú del applet de energía de Plasma **no escribe sysfs por sí
mismo**: es un cliente puro del API D-Bus `net.hadess.PowerProfiles`
(power-profiles-daemon, o el shim `tuned-ppd`). Con PPD enmascarado por
hpd (Conflicts= + doctor), el proveedor desaparece del bus y Plasma
**oculta esa sección del applet** — no hay pelea, pero tampoco selector.
Es decir: hoy la interferencia es cero *después* de `doctor --fix`; el
coste es perder ese menú. La forma de recuperarlo "en favor de hpd" es
el shim de compatibilidad PPD (§6.8).

**b) El script `game-performance` de CachyOS.** CachyOS recomienda
lanzar juegos con `game-performance %command%`; ese wrapper ejecuta
`powerprofilesctl launch -p performance`, o sea **depende de PPD**. Con
PPD enmascarado, `powerprofilesctl` falla y el juego puede no arrancar
(o arrancar sin el perfil, según versión). **Acción para tu equipo**:
revisa las Launch Options de tus juegos de Steam (y Lutris/Heroic) y
quita `game-performance` mientras hpd sea el dueño — su función (subir
a rendimiento al jugar) ya la cubre el AC-lock/preset de hpd. El shim
PPD (§6.8) también lo dejaría funcionar sin tocar nada.

**c) Límite de carga de batería en Plasma.** Las versiones recientes de
Plasma (≥ 6.2) exponen un control de límite de carga en Ajustes de
energía que escribe el **mismo** `charge_control_end_threshold` que
`hpdctl battery`. No hay daemon que enmascarar (lo escribe PowerDevil
directamente); la regla es operativa: **fija el límite solo en hpd** y
deja el de Plasma sin configurar (verifica si tu build de CachyOS lo
muestra). hpd re-asserta su valor en boot/resume, así que un cambio
manual desde Plasma será sobrescrito en el siguiente ciclo — mejor no
usar los dos a la vez para no confundirse.

**d) Panel de rendimiento del overlay de Steam (QAM, botón "…").**
Dos grupos distintos dentro del mismo panel:
  - *Limitador de FPS, tasa de refresco, escalado/FSR, half-rate
    shading*: viven **dentro de gamescope** — no tocan TDP ni EPP ni
    curvas. Cero conflicto con hpd; úsalos libremente (son además la vía
    Linux para el rol de Radeon Chill/RSR, §6.6).
  - *Sliders de TDP y reloj de GPU*: solo funcionan a través de
    `steamos-manager`, que hpd ya detecta y `doctor --fix` enmascara
    (sistema y usuario). Con él enmascarado los sliders desaparecen o
    quedan inertes. Si algún día reaparecen tras una actualización de la
    sesión, es señal de que la máscara user-level se perdió —
    `hpdctl doctor` lo mostraría.

**e) Lo que NO interfiere (no perseguir):** UPower (solo lectura de
batería), PowerDevil brillo/suspensión, MangoHud (lectura + FPS cap),
scx/ananicy/zram de CachyOS. Y lo **indetectable por diseño**: plugins
Decky de TDP (SimpleDeckyTDP, PowerControl, PowerTools) — corren dentro
del plugin loader sin unidad ni nombre de bus; la única defensa es
desinstalarlos. Vale la pena un aviso permanente en el README del
plugin de hpd.

### 5.6 Dependencia dura del driver `asus-armoury`

`get_limits` fallando = el daemon sale (`main.rs:275-279`). El driver
`asus-armoury` con soporte RC73XA es reciente (kernels ≥ 6.17-ish); en
CachyOS (kernel rolling) estás bien, pero un arranque con un kernel LTS
de rescate dejará a hpd saliendo en bucle (`Restart=on-failure` cada
5 s, para siempre). Mejoras baratas: mensaje de error que nombre el
driver/kernel requerido, y `StartLimitBurst`/`StartLimitIntervalSec` en
la unit para no reintentar indefinidamente.

---

## 6. Paridad con Armoury Crate SE + Radeon Adrenalin — roadmap

> **Actualización 2026-07-08**: esta sección tiene ahora un documento
> de diseño sucesor que la detalla, re-prioriza y amplía con features
> nuevas (perfiles por juego, diagnóstico de cuellos de botella,
> asistente de calibración): [`GAMING-ROADMAP-es.md`](GAMING-ROADMAP-es.md).
> Ante discrepancias, manda el roadmap.

Lo que ya tienes equivalente: modos de operación (presets TDP + curvas
Silent/Balanced/Aggressive mejores que el firmware), TDP manual
SPL/SPPT/FPPT (Armoury solo lo da en "Manual"), límite de carga de
batería, telemetría básica, y una política AC que Armoury no tiene
(lock a máximo rendimiento). Lo que falta, por valor/esfuerzo:

### 6.1 Curvas de ventilador personalizadas end-to-end (esfuerzo bajo)

Toda la infraestructura existe (`FanCurveSelection::Custom` validada en
el backend, persistida en estado, `get_fan_curve` para leer), pero el
método D-Bus `set_fan_curve` se retiró en 2.5.0 — hoy **ningún cliente
puede programar una curva punto a punto**, solo los 3 presets. Armoury
Crate sí deja editar la curva. Re-exponer el setter (con la validación
de monotonía ya escrita y una acción polkit) es el hueco de paridad más
barato de cerrar, y habilita un editor de curva en el plugin Decky.

### 6.2 Capability de GPU (equivalente a "GPU tuning" de Adrenalin) (esfuerzo medio)

> **Decisión 2026-07-11**: aprobado como el próximo trabajo real, fusionado con
> el suelo/techo de reloj de §6.6/GAMING-ROADMAP §7b (ya no espera al spike de
> FPS — ver ese documento). Dos requisitos añadidos por el usuario: (a) los
> límites min/max de reloj GPU necesitan validación tan rigurosa como el suelo
> de seguridad de curvas de ventilador (`FanCurveConstraints`/
> `validate_against`) — nunca un passthrough directo a `pp_od_clk_voltage` sin
> rango verificado, para no arriesgar daño de hardware; (b) al cambiar de
> preset TDP (eco/balanced/max) debe aplicarse también un preset de reloj GPU
> a juego — el reloj de GPU pasa a formar parte del pipeline de cambio de
> preset, no queda como palanca independiente.

Nueva trait `GpuControl` en L2 + implementación amdgpu en L1:
`power_dpm_force_performance_level` (auto/low/high/manual),
`pp_od_clk_voltage` (min/max gfxclk) y `power1_cap` del hwmon amdgpu.
En un handheld, fijar el reloj mínimo/máximo de GPU es de las palancas
más eficaces para estabilizar frametimes a TDP bajo (lo que Adrenalin
llama "Minimum/Maximum Frequency"). Encaja limpio en el pipeline de
efectos existente. (Si se hace, añadir LACT como advisory — §5.4.)

### 6.3 Toggle de CPU boost (esfuerzo bajo, gran ganancia en batería)

> **Decisión 2026-07-11**: descartado por ahora — queda solo documentado,
> sin experimento de admisión ni implementación, hasta que se revise de nuevo.

`/sys/devices/system/cpu/cpufreq/boost` (amd_pstate). Armoury/Adrenalin
lo exponen indirectamente; en handheld apagar boost a TDP bajo mejora
mucho perf/W. Un `Transition::SetCpuBoost(bool)` + effect + propiedad.

### 6.4 Perfiles por juego (esfuerzo medio, en el plugin, no en el daemon)

El "scenario profiles" de Armoury. El daemon no debe saber de juegos; el
plugin Decky puede escuchar el appid en foco de Steam y aplicar un
preset guardado (TDP + curva + power mode) por juego. Todo el API D-Bus
necesario ya existe.

### 6.5 Telemetría ampliada (esfuerzo bajo)

Para el overlay del plugin al nivel del de Armoury: consumo de batería
(`BAT0/power_now`), porcentaje y estado de carga, frecuencias CPU/GPU
(`scaling_cur_freq`, `pp_dpm_sclk`). Sugerencia de diseño: en vez de
seguir engordando la tupla rígida de `get_thermal_status` (ya es
`(iiiii)`), añadir `get_telemetry() -> a{sv}` extensible sin romper
firma.

### 6.6 Fuera del alcance razonable del daemon (documentarlo y no perseguirlo)

- **RSR / escalado**: eso es `gamescope --fsr-upscaling` / opciones de
  la sesión — el plugin puede documentarlo, el daemon no pinta nada.
- **AFMF (frame generation)**: no existe equivalente de driver en Linux.
- **Radeon Chill / FPS cap**: MangoHud (`fps_limit`) o gamescope; como
  mucho, integración desde el plugin.
- **RGB / Aura, remapeo del mando**: territorio de `asusd` / hhd /
  InputPlumber — la decisión de mantenerlos advisory es correcta.
- **UMA/VRAM asignada**: solo BIOS en esta plataforma.

### 6.7 Shim de compatibilidad `net.hadess.PowerProfiles` (esfuerzo medio, valor alto)

La materialización literal de "que el daemon funcione **en lugar de**
los otros": que hpd reclame el nombre de bus `net.hadess.PowerProfiles`
e implemente el API de PPD (propiedad `ActiveProfile`, `Profiles`,
`HoldProfile`/`ReleaseProfile`) mapeándolo a `set_profile` — igual que
hace `tuned-ppd` para tuned. Efectos inmediatos:

- El **selector del icono de batería de KDE vuelve a funcionar**, pero
  ahora manda sobre hpd (Eco→power-saver, Equilibrado→balanced,
  Rendimiento→performance).
- El script **`game-performance` de CachyOS vuelve a funcionar** sin
  tocar las Launch Options: su `HoldProfile(performance)` se traduce en
  un `SetProfile(Performance)` temporal que se revierte al salir el
  juego (la semántica hold/release de PPD encaja con un snapshot-and-
  restore como el del AC-lock).

Requisitos/cuidados: solo puede reclamarse el nombre si PPD/tuned-ppd
están enmascarados (ya es el estado post-doctor); necesita su propio
`<policy>` en la conf de bus; el API de PPD no exige polkit para
`ActiveProfile` (mismo comportamiento upstream — decidir si se acepta o
se restringe); y el lock de AC debe ganar a un `HoldProfile` entrante
(orden de precedencia documentado).

### 6.8 Ajustes de política pensados para tu uso

- **Preset `Balanced` = punto medio del rango** (21 W en 7–35 W): en
  batería, 21 W sostenidos en el RC73XA drenan mucho. Considerar hacer
  los vatios de los presets configurables (`preset_balanced_w = 17`,
  etc. en `config.toml`) en lugar de derivados del rango de hardware.
- **AC-lock por defecto = Aggressive + fans fuertes al enchufar**: es
  el comportamiento documentado, pero en escritorio enchufado
  permanentemente sorprende. El plugin debería mostrar el toggle
  `ac-lock` de forma prominente la primera vez que se detecte el lock.
- **Guard de TDP en batería** (opcional): Armoury restringe Turbo 35 W
  a AC. Un `max_battery_spl_w` opcional en config sería el espejo.

---

## 7. Actualizaciones de dependencias / toolchain

| Dep | Actual | Estado |
|---|---|---|
| zbus | 4.4 | Existe la serie 5.x (API churn moderado). No urgente; planear como tarea propia. |
| thiserror | 1.0 | 2.x disponible; migración mecánica. No urgente. |
| tokio | 1.52 (pin mínimo 1.36) | OK. |
| clap | 4.6 | OK. |
| MSRV / toolchain | 1.85 pinned | OK y bien razonado. |

Ninguna es bloqueante. Si se toca zbus, aprovechar para revisar el
`#[interface]` (zbus 5 cambia macros/propiedades).

---

## 8. Checklist priorizado

**Corrección (hacer ya):**
1. Arreglar `EnableFanAuto` (re-inferir curva + `PersistState`) — §2.1.
2. `try_send` en `Executor::rollback` — §2.2.
3. Clamp + validación de `RuntimeConfig` (factores/thresholds) — §2.3.
4. `fsync` antes del rename en `StatePersister::save` — §2.4.
5. Actualizar CLAUDE.md a 2.7.2 — §2.7.

**Ecosistema CachyOS (hacer pronto):**
6. Añadir `tlp.service` a rivales (detección + doctor) — §5.3.
7. Añadir `tuned-ppd.service` al mask de doctor — §5.2.
8. Mask de hhd **condicional a InputPlumber activo**; si no, sugerir
   desactivar solo su módulo TDP — §5.1.
9. `StartLimitBurst` en la unit + mensaje claro si falta `asus-armoury` — §5.6.
10. Unificar la detección de AC (type==Mains en el backend) — §4.1.
11. Test anti-drift para las listas de rivales daemon/CLI — §4.2.

**Operativa en este equipo (sin código, hazlo tú):**
12. Quitar `game-performance %command%` de las Launch Options de Steam/
    Lutris mientras hpd sea el dueño (depende de PPD, que está
    enmascarado) — §5.5b.
13. No configurar el límite de carga en Plasma; usar solo
    `hpdctl battery` — §5.5c.
14. Desinstalar cualquier plugin Decky de TDP (SimpleDeckyTDP,
    PowerControl, PowerTools): son indetectables para hpd — §5.5e.

**Paridad Armoury/Adrenalin (roadmap):**
15. Re-exponer `set_fan_curve` (curvas custom) — §6.1.
16. Cache de rutas hwmon + `get_telemetry() -> a{sv}` ampliada — §4.3, §6.5.
17. ~~Toggle CPU boost~~ — §6.3. **Descartado por ahora (2026-07-11), solo documentado.**
18. Capability GPU (dpm level / clocks / power cap) + LACT advisory — §6.2.
    **Próximo item real (2026-07-11)**, fusionado con el suelo/techo de
    reloj (GAMING-ROADMAP §7b): requiere validación de rango tan
    rigurosa como el suelo de seguridad de curvas de ventilador, y
    aplicar un preset de reloj GPU en cada cambio de preset TDP.
19. Perfiles por juego en el plugin Decky — §6.4.
20. Shim `net.hadess.PowerProfiles` (recupera el applet de KDE y
    `game-performance` mandando sobre hpd) — §6.7.
21. Presets de TDP configurables en vatios + guard opcional de batería — §6.8.

**Deuda de verificación:**
22. Ejecutar `cargo clippy --workspace --all-targets -- -D warnings` y
    `cargo test` (no fue posible en esta máquina — sin toolchain);
    añadir tests para §2.1–§2.3 al arreglarlos.
