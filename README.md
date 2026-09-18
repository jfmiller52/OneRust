# OneRust

Windows desktop app for **Lenovo ThinkSystem** fleet ops over the BMC/XCC — powered entirely by **Lenovo XClarity Essentials OneCLI**.

OneRust wraps OneCLI in a Tauri + React UI so you can mass-update firmware and apply RAID / UEFI / BMC “blueprints” without hand-running CLI scripts per host.

**Current release:** [v0.2.0](https://github.com/jfmiller52/OneRust/releases/tag/v0.2.0)

## What it does

| Mode | Purpose |
|------|---------|
| **Firmware update** | Acquire Update Bundles, flash them out-of-band (`--bundle` / `OnReset`), reboot, then verify with OneCLI compare |
| **Blueprint** | Apply a local `.ini` / `.xml` / `.txt` blueprint (RAID policy, config settings, batch `set` commands, or compare XML) to many BMCs |

All BMC work goes through OneCLI. There is **no Redfish client** in the app.

## Prerequisites

- **Windows** (OneCLI packaging and helpers are Windows-oriented)
- Network reachability to target XCC/BMC IPs
- For development: [Node.js](https://nodejs.org/) 20+, [Rust](https://rustup.rs/) (stable, MSVC)

On **first launch**, if `OneCLI/OneCli.exe` is not already beside the executable, OneRust downloads Lenovo OneCLI and extracts it into `OneCLI/` next to the app.

## Install (release)

Download from [Releases](https://github.com/jfmiller52/OneRust/releases):

- `OneRust_*_x64-setup.exe` (NSIS), or
- `OneRust_*_x64_en-US.msi`

Launch the app once so OneCLI can bootstrap if needed.

## Develop

```powershell
cd C:\OneRust
npm install
npm run tauri dev
```

Release build:

```powershell
npm run tauri build
```

## Firmware update

Wizard steps: **Models → Firmware → Targets → Update**.

1. **Models** — Pick ThinkSystem V3/V4 families from the catalog, and/or enter extra 4-character machine types.
2. **Firmware** — Acquire latest firmware-only ZIPs with OneCLI `update acquire` into `firmware/<MT>/`. Offline mode uses local ZIPs only.
3. **Targets** — Shared XCC user/password, concurrency, optional TLS verify, and BMC IPs (`user:pass@ip` per-host overrides allowed).
4. **Update** — For each host, OneCLI:

| Step | Command |
|------|---------|
| Identify serial + machine type | `inventory getinfor --device system_overview` |
| Stage bundle | `update flash --bundle --applytime OnReset --noreboot` |
| Reboot | `misc power forcerestart` (or `normalrestart` when graceful) |
| Wait for BMC | poll `misc power state` |
| Verify | `update compare` — **verified** only when no further firmware updates are recommended |

Hosts whose machine type was not selected (no matching bundle) are **skipped**.

## Blueprint

Switch to the **Blueprint** tab. Choose a file; OneRust classifies it and applies it concurrently to the BMC list.

| Detected type | OneCLI action |
|---------------|---------------|
| RAID `.ini` (`[ctrl…]` / `raid_level`) | `misc raid add --force` |
| Settings file (`Setting=Value`, from `config save`) | `config replicate` |
| Batch file (lines of `set …`) | `config batch` |
| Firmware compare `.xml` | `update flash --comparexml` (requires a package directory) |

Serial for each host is resolved with OneCLI inventory before apply. Progress and OneCLI stdout/stderr are kept under `logs/blueprint/<serial>/`.

## Layout

```text
OneRust/
  OneCLI/                 # auto-downloaded on first run (gitignored)
  firmware/<MT>/          # acquired Update Bundles
  logs/
    <SERIAL>.log          # per-host firmware update log
    identify/             # inventory used for identity
    flash/<SERIAL>/       # flash command output
    power/<SERIAL>/       # reboot command output
    compare/<SERIAL>/     # compare XML / console
    blueprint/<SERIAL>/   # blueprint apply output
  src/                    # React UI
  src-tauri/              # Rust / Tauri + OneCLI orchestration
```

## Notes

- Designed for **ThinkSystem V3/V4** out-of-band management through XCC.
- TLS verification is **off** by default (`--never-check-trust`). Turn on “Verify BMC TLS” in the UI when you need certificate checks.
- OneCLI, firmware trees, and logs are gitignored — keep them local to each workstation.
- HTTP is used only to download the OneCLI zip on first run; day-to-day BMC operations are OneCLI subprocesses.

## License

MIT
