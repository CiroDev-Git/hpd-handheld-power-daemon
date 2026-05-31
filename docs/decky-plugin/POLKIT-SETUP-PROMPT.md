<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Decky plugin — polkit setup detection & auto-repair (copy-paste prompt)

The single most common reason the Decky plugin "connects to the daemon
but every change fails" is that **the daemon's polkit policy was never
installed** — e.g. the `hpd-daemon` binary was deployed by the plugin (or
hand-copied) without `package/polkit/dev.cirodev.hpd.policy` and
`package/polkit/49-hpd.rules`. polkit then doesn't know the
`dev.cirodev.hpd.*` actions, so **every** mutating D-Bus call comes back
`org.freedesktop.DBus.Error.AuthFailed`, while reads (status/telemetry)
keep working — which looks like "connected but broken".

`hpd >= 2.1.0` exposes a self-check and ships a one-command fix. Wire the
plugin to detect the condition and repair it automatically.

---

## Paste this to your plugin's coding agent

> **Task:** Make the hpd Decky plugin detect and auto-repair a missing
> polkit policy so the user can actually use the plugin.
>
> **Context — the daemon:**
> - Bus: **system bus**. Service/interface/path all
>   `dev.cirodev.hpd.PowerDaemon1` / `/dev/cirodev/hpd/PowerDaemon1`.
> - Member names are **PascalCase** on the wire.
> - Health probe method:
>   `GetDiagnostics() → (b polkit_ok, as missing_action_ids)`.
>   `polkit_ok == false` means the polkit policy is **not installed** and
>   every mutating call will fail with `AuthFailed`. It's a **live**
>   check (re-poll it after a repair to confirm). Reads
>   (`GetThermalStatus`, properties, `GetHardwareLimits`) work regardless.
> - The CLI ships the repair tool: **`hpdctl fix-polkit`**. It installs
>   the polkit policy + rules and reloads polkit (no daemon restart). The
>   policy files are embedded in the binary, so it needs no source tree.
>   When run **as root it applies directly** (no `pkexec`); as a normal
>   user it self-elevates via `pkexec`/`sudo`.
> - The Decky plugin's Python backend runs as **root**, so it can simply
>   execute `hpdctl fix-polkit` and it will apply immediately.
>
> **Implement:**
> 1. **On plugin load / panel open**, call `GetDiagnostics()`.
>    - If the call itself fails because the daemon isn't found → show
>      "daemon not running" (existing path, unchanged).
>    - If `polkit_ok == true` → proceed normally.
>    - If `polkit_ok == false` → **gate the controls** (disable the
>      TDP / cooling / charge widgets so the user can't fire doomed
>      writes) and show a **"Finish setup"** banner explaining: "hpd's
>      permissions aren't installed yet — privileged controls are
>      disabled until setup completes."
> 2. **"Finish setup" button** → from the **root** Python backend run:
>    `hpdctl fix-polkit` (capture exit code + stderr). On success
>    (exit 0) → re-poll `GetDiagnostics()`; if now `polkit_ok == true`,
>    remove the banner and enable the controls. On failure → surface the
>    captured stderr in a toast and keep the banner.
>    - Prefer calling `hpdctl` from `$PATH`; it installs to `/usr/bin`
>      (AUR) or `/usr/local/bin` (install.sh). Fall back to those
>      absolute paths if not on `$PATH`.
>    - If `hpdctl` is missing entirely, fall back to writing the two
>      files yourself from the backend (it's root):
>      `/usr/share/polkit-1/actions/dev.cirodev.hpd.policy` and
>      `/usr/share/polkit-1/rules.d/49-hpd.rules` (ship copies in the
>      plugin), `chmod 644`, then `systemctl reload polkit || systemctl
>      try-restart polkit`.
> 3. **Defense in depth at call sites:** wrap every mutating call so an
>    `AuthFailed` doesn't crash the UI. On `AuthFailed`, re-poll
>    `GetDiagnostics()`; if `polkit_ok == false`, show the "Finish setup"
>    banner instead of a raw error toast.
> 4. **Version awareness:** `GetDiagnostics` exists from daemon
>    `>= 2.1.0`. If the method is missing (older daemon), treat it as
>    `polkit_ok == true` (don't gate) and rely only on the call-site
>    `AuthFailed` handling. Read the installed version from
>    `/usr/share/hpd/VERSION` for compat gating (already used for
>    `hpdDaemonCompat`).
>
> **Acceptance:** On a device where the daemon runs but its polkit policy
> is absent, the plugin shows "Finish setup", the button runs
> `hpdctl fix-polkit`, and after it succeeds the TDP/cooling/charge
> controls work without restarting the daemon or the plugin.

---

## Why the plugin (not the daemon) performs the repair

The daemon runs under systemd with `ProtectSystem=strict`, so `/usr` is
read-only **to the daemon** — it cannot self-install its own policy. The
privileged write must come from outside that sandbox: the user-side
`hpdctl fix-polkit`, or the plugin's root backend. See
[`V2-INTEGRATION.md`](V2-INTEGRATION.md) §🟡-11 for the UI mapping and the
`GetDiagnostics` row in the D-Bus surface table.
