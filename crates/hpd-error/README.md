# hpd-error

> Cross-crate error types for the `hpd` workspace.

| Field   | Value                                                  |
|---------|--------------------------------------------------------|
| Layer   | **L-1** — cross-cutting (no internal deps)             |
| Stable  | since `1.0.0`                                          |
| Crate   | `hpd-error`                                            |

## Purpose

Centralises the workspace's error hierarchy so every other crate can
`use hpd_error::HpdError` and bubble failures through `?` without
ad-hoc `map_err` plumbing. Lives below every other layer (L0, L1,
L2, L3, L4) — no internal dependencies.

The hierarchy is intentionally shallow:

```text
HpdError
├── Sysfs(SysfsError)        // raw filesystem read/write failure
├── Backend(BackendError)    // logical / parse failure inside a backend
├── FeatureUnsupported       // hardware does not expose the capability
└── InvariantViolation(msg)  // domain invariant broken by user input
```

`SysfsError::from_io(path, io_err)` is the canonical adapter from
`std::io::Error` — it categorises `ENOENT` / `EACCES` into structured
variants so callers can react to "feature absent" vs. "daemon needs
root" without inspecting error strings.

## Dependencies

| Dep         | Purpose                                  |
|-------------|------------------------------------------|
| `thiserror` | Derive-based error type ergonomics.      |

No tokio, no async, no I/O — this crate is `no_std`-friendly in spirit
(it just uses `std::path::PathBuf`).

## Example

```rust
use hpd_error::{HpdError, SysfsError};
use std::path::PathBuf;

fn read_attr(path: &str) -> Result<String, HpdError> {
    std::fs::read_to_string(path)
        .map_err(|e| SysfsError::from_io(PathBuf::from(path), e))
        .map_err(HpdError::from)
}
```

## Docs

```bash
cargo doc -p hpd-error --no-deps --open
```

## License

GPL-3.0-or-later. See the workspace `Cargo.toml`.
