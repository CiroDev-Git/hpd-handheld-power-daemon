# hpd-backend-asus

> ASUS armoury firmware-attribute backend.

| Field     | Value                                                  |
|-----------|--------------------------------------------------------|
| Layer     | **L1** — vendor backend                                |
| Stable    | since `1.0.0`                                          |
| Crate     | `hpd-backend-asus`                                     |
| Hardware  | ROG Ally (`RC71L`), ROG Ally X (`RC72L`), Xbox Ally X (`RC73XA`) |

## Purpose

Implements all eight L2 capability traits against the upstream Linux
`asus-armoury` firmware-attributes driver, the standard ACPI
platform-profile interface, and amdgpu's hwmon/DRM sysfs surfaces. The
aggregate [`AsusBackend`] is a thin composition of eight
single-responsibility sub-backends:

| Sub-backend              | Trait                  | Sysfs surface                                                |
|---------------------------|------------------------|--------------------------------------------------------------|
| `AsusPowerBackend`        | `PowerEnvelope`        | `/sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl{1,2,3}_*` |
| `AsusChargeBackend`       | `ChargeControl`        | `/sys/class/power_supply/BAT0/charge_control_end_threshold`  |
| `AsusProfileBackend`      | `PlatformProfile`      | `/sys/firmware/acpi/platform_profile{,_choices}`              |
| `AsusFanBackend`          | `FanControl`           | `/sys/class/hwmon/hwmonN/fan{1,2}_input` (probe order varies) |
| `AsusFanCurveBackend`     | `FanCurveControl`      | `asus_custom_fan_curve` hwmon node: `pwm{1,2}_auto_point{1..8}_{temp,pwm}` + `pwm{1,2}_enable` (`pwm1` = CPU/SoC fan, `pwm2` = GPU fan) |
| `AsusThermalBackend`      | `ThermalSensors`       | `k10temp` hwmon `temp1_input` (CPU/SoC Tctl), `amdgpu` hwmon `temp1_input` (GPU edge) + `power1_input` (SoC power) |
| `AsusTelemetryBackend`    | `SystemTelemetry`      | `power_supply` node with `type == "Battery"`; `/sys/devices/system/cpu/cpufreq/policy*` (CPU freq); `amdgpu` hwmon's sibling DRM `device/` dir (`gpu_busy_percent`, `mem_info_vram_{used,total}`, `freq1_input`/`pp_dpm_sclk`); `/proc/stat` (CPU busy %) |
| `AsusGpuClockBackend`     | `GpuClockRangeControl` | `amdgpu` hwmon `device/power_dpm_force_performance_level` + `device/pp_od_clk_voltage` (OverDrive `OD_RANGE`/`OD_SCLK`) |

Detection lives in `detect.rs`: `matches_asus_handheld(&DmiInfo)`
returns `Some(AsusModel)` only when both the DMI vendor (`ASUSTeK
COMPUTER INC.`) and a known `board_name` are present.

The kernel exposes the PPT rails in whole **watts**; this backend
converts to/from `PowerMilliwatts` at the I/O boundary so the
reducer never sees raw kernel units. Boost-rail maxima (`sppt_max`,
`fppt_max`) fall back to documented Ally / Ally X / Xbox Ally X
values when the kernel's `max_value` attribute is absent.

## Dependencies

| Dep                | Purpose                                          |
|--------------------|--------------------------------------------------|
| `hpd-error`        | `Result<_, HpdError>` on every method.           |
| `hpd-capabilities` | Implements `PowerEnvelope`/`ChargeControl`/…     |
| `hpd-sysfs`        | All reads/writes go through the `SysfsIo` trait. |
| `hpd-sysfs/mock`   | (dev-deps) `MockSysfs` test fixture.             |

## Example

```rust
use hpd_backend_asus::AsusBackend;
use hpd_capabilities::backend::HwBackend;
use hpd_sysfs::RealSysfs;

let backend = AsusBackend::new(RealSysfs);
let limits = backend.power().get_limits()?;
println!("SPL range: {}-{} W", limits.spl_min.as_watts(), limits.spl_max.as_watts());

if let Some(charge) = backend.charge() {
    println!("Charge end threshold: {}%", charge.get_end_threshold()?);
}
```

## Docs

```bash
cargo doc -p hpd-backend-asus --no-deps --open
```

## License

GPL-3.0-or-later. See the workspace `Cargo.toml`.
