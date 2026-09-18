# OneRust

Desktop app to mass-update **Lenovo ThinkSystem V3/V4** firmware using **Lenovo XClarity Essentials OneCLI** (out-of-band against XCC).

It acquires firmware-only Update Bundles with a local **OneCLI** install, flashes them with `--bundle --applytime OnReset`, force-restarts each host via OneCLI power commands, then verifies completion with **OneCLI `update compare`** (success when no further firmware updates are recommended).

Built with **Tauri 2**, **React**, and **shadcn-style** UI components.

## Prerequisites

- Windows (OneCLI path and PowerShell helpers are Windows-oriented)
- [Node.js](https://nodejs.org/) 20+
- [Rust](https://rustup.rs/) (stable) with MSVC toolchain
- Network access from this PC to:
  - Target BMC/XCC management IPs
  - Lenovo download/support sites (`download.lenovo.com`, `support.lenovo.com`)

On **first run**, if `OneCLI/OneCli.exe` is not already next to the executable, OneRust downloads Lenovo OneCLI and extracts it into an `OneCLI/` folder beside the app.

## Quick start

```powershell
cd C:\OneRust
npm install
npm run tauri dev
```

Release build:

```powershell
npm run tauri build
```

## Workflow

### Firmware update

1. **Models** — Select ThinkSystem V3/V4 models (or add extra 4-character machine types).
2. **Firmware** — Acquire latest firmware-only ZIPs via OneCLI into `firmware/<MT>/` (or use offline local ZIPs).
3. **Targets** — Enter shared XCC credentials, concurrency, and BMC IPs (`user:pass@ip` overrides allowed).
4. **Update** — For each host (all via OneCLI):
   - Identify serial / machine type (`inventory getinfor --device system_overview`)
   - Flash the matching Update Bundle (`update flash --bundle --applytime OnReset --noreboot`)
   - Restart the host (`misc power forcerestart`)
   - Wait until the BMC answers again (`misc power state`)
   - Run `update compare` against the local package directory
   - Mark **verified** only when compare reports no remaining firmware updates

Per-host progress is written under `logs/<SERIAL>.log`. Compare artifacts go under `logs/compare/<SERIAL>/`.

### Blueprint

Use the **Blueprint** tab to apply an XML/INI/TXT file to one or more BMCs via OneCLI. The file type is auto-detected:

| File | Applied as |
|------|------------|
| RAID `.ini` (`[ctrl…]` / `raid_level`) | `misc raid add --force` |
| Settings from `config save` (`Setting=Value`) | `config replicate` |
| Batch of `set …` lines | `config batch` |
| Firmware compare `.xml` | `update flash --comparexml` (+ package directory) |

Logs land under `logs/blueprint/<serial>/` (serial resolved with OneCLI `inventory getinfor --device system_overview`).

## Layout

```text
OneRust/
  OneCLI/          # auto-downloaded on first run if missing (not committed)
  firmware/        # acquired packages per machine type
  logs/            # per-serial update logs + compare output
  src/             # React UI
  src-tauri/       # Rust / Tauri backend
```

## Notes

- Targets **V3/V4** ThinkSystem hosts managed out-of-band through XCC. All BMC operations use OneCLI (no direct Redfish client).
- BMC TLS verification is off by default (`--never-check-trust`). Enable “Verify BMC TLS” in the UI if needed.
- Host reboot uses OneCLI `misc power forcerestart` (or `normalrestart` when graceful is requested).
- OneCLI, firmware downloads, and logs are gitignored — keep them local. OneCLI is bootstrapped automatically on first launch when missing.

## License

MIT
