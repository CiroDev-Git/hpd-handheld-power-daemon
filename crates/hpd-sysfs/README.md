# hpd-sysfs

> The only crate that ever opens a file under `/sys`.

| Field   | Value                                                  |
|---------|--------------------------------------------------------|
| Layer   | **L0** — kernel I/O                                    |
| Stable  | since `1.0.0`                                          |
| Crate   | `hpd-sysfs`                                            |
| Features| `mock` (pulls `tempfile`; enables `MockSysfs`)         |

## Purpose

Defines the [`SysfsIo`] trait — the entire contract between L1
backends and the kernel — and ships two implementations:

- `RealSysfs` (default, zero-sized) — production-grade reader/writer
  for `/sys`. Maps `std::io::Error` into the structured
  [`SysfsError`] hierarchy via `hpd-error::SysfsError::from_io`.
- `MockSysfs` (behind `mock` feature) — `Arc<TempDir>`-backed
  in-memory tree used by every test fixture in the workspace. Cheap
  to clone and shared across sub-backends in tests.

Keeping `/sys` access on one trait means the rest of the codebase has
zero `std::fs` calls — every read/write goes through `SysfsIo` and is
mockable without a real Linux machine.

## Dependencies

| Dep         | Purpose                                                       |
|-------------|---------------------------------------------------------------|
| `hpd-error` | Returns `SysfsError`/`HpdError` from every read/write.        |
| `tracing`   | Debug-level traces on every I/O.                              |
| `tempfile`  | (`mock` feature only) backs `MockSysfs` with a `TempDir`.     |

## Example

```rust
use hpd_sysfs::{RealSysfs, SysfsIo};

let sysfs = RealSysfs;
let pl1 = sysfs.read_string(
    "/sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/current_value",
)?;
```

With the `mock` feature for tests:

```rust
use hpd_sysfs::{MockSysfs, SysfsIo};

let mock = MockSysfs::new();
mock.create_file("sys/class/foo/bar", "42");
assert_eq!(mock.read_string("/sys/class/foo/bar")?.trim(), "42");
```

## Docs

```bash
cargo doc -p hpd-sysfs --features mock --no-deps --open
```

## License

GPL-3.0-or-later. See the workspace `Cargo.toml`.
