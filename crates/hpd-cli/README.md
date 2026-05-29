# hpd-cli (`hpdctl`)

> User-facing CLI client for the `hpd` daemon.

| Field   | Value                                                  |
|---------|--------------------------------------------------------|
| Layer   | **L4** — interface                                     |
| Stable  | since `1.0.0`                                          |
| Crate   | `hpd-cli`                                              |
| Binary  | `hpdctl`                                               |

## Purpose

Thin D-Bus client that talks to the running `hpd-daemon` over the
`dev.cirodev.hpd.PowerDaemon1` interface. Binds to the **system bus**
in production and to the **session bus** when `HPD_SIMULATOR` is set,
mirroring the daemon's `simulator` feature.

The CLI surface is stable under SemVer from `1.0.0` forward.

## Subcommands

| Command                       | Description                                 |
|-------------------------------|---------------------------------------------|
| `hpdctl tdp set <W>`          | Set SPL in watts (SPPT/FPPT derived).       |
| `hpdctl tdp get`              | Show current SPL.                           |
| `hpdctl charge set <%>`       | Set battery end threshold (20-100).         |
| `hpdctl charge get`           | Show current charge end threshold.          |
| `hpdctl preset <name>`        | Apply preset: `eco` / `balanced` / `max`.   |
| `hpdctl limits`               | Show hardware SPL/SPPT/FPPT ranges.         |
| `hpdctl status`               | One-shot dashboard.                         |
| `hpdctl monitor`              | Live dashboard, refreshed every second.     |
| `hpdctl fan set <profile>`    | Set platform/cooling profile manually.      |
| `hpdctl fan auto`             | Re-enable auto-cooling (follows TDP).       |

Privileged subcommands (`tdp set`, `charge set`, `preset`, `fan set`,
`fan auto`) are authorized by polkit. Members of the `wheel` group (the
device owner) run them without any prompt — including over SSH — via
`package/polkit/49-hpd.rules`. Any other user gets a polkit prompt
(answered once per 5-minute window for `set-profile`, per call for
`set-tdp` / `set-charge`).

## Dependencies

| Dep      | Purpose                                          |
|----------|--------------------------------------------------|
| `clap`   | Argument parsing (`derive` feature).             |
| `zbus`   | D-Bus proxy generated via `#[proxy]`.            |
| `tokio`  | Async runtime (`#[tokio::main]`).                |

The CLI does **not** depend on any internal `hpd-*` crate — its
D-Bus contract is the same one a third-party client would consume.

## Example

```bash
hpdctl status
hpdctl tdp set 15
hpdctl preset eco
hpdctl monitor          # Ctrl+C to exit
```

Simulator mode (macOS / dev hosts):

```bash
HPD_SIMULATOR=1 cargo run -p hpd-cli -- status
```

## Docs

```bash
cargo doc -p hpd-cli --no-deps --open
```

## License

GPL-3.0-or-later. See the workspace `Cargo.toml`.
