# OneRust

Desktop app to mass-update **Lenovo ThinkSystem V3/V4** firmware over **Redfish** (XCC2/XCC3).

It acquires firmware-only Update Bundles with a local **OneCLI** install, stages them with `@Redfish.OperationApplyTime: OnReset`, force-restarts each host, then verifies completion with **OneCLI `update compare`** (success when no further firmware updates are recommended).

Built with **Tauri 2**, **React**, and **shadcn-style** UI components.

## Prerequisites

- Windows (OneCLI path and PowerShell helpers are Windows-oriented)
- [Node.js](https://nodejs.org/) 20+
- [Rust](https://rustup.rs/) (stable) with MSVC toolchain
- Unzipped [Lenovo XClarity Essentials OneCLI](https://datacentersupport.lenovo.com/) in a folder named **`OneCLI`** next to the app (cwd or beside the executable), so `OneCLI/OneCli.exe` is reachable
- Network access from this PC to:
  - Target BMC/XCC management IPs
  - Lenovo download/support sites when acquiring packages (`download.lenovo.com`, `support.lenovo.com`)

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

1. **Models** — Select ThinkSystem V3/V4 models (or add extra 4-character machine types).
2. **Firmware** — Acquire latest firmware-only ZIPs via OneCLI into `firmware/<MT>/` (or use offline local ZIPs).
3. **Targets** — Enter shared XCC credentials, concurrency, and BMC IPs (`user:pass@ip` overrides allowed).
4. **Update** — For each host:
   - Identify serial / machine type over Redfish
   - Multipart-push the matching Update Bundle (`OnReset`)
   - `ForceRestart` the host so staged firmware applies
   - Wait for the BMC/host to return
   - Run OneCLI compare against the local package directory
   - Mark **verified** only when compare reports no remaining firmware updates

Per-host progress is written under `logs/<SERIAL>.log`. Compare artifacts go under `logs/compare/<SERIAL>/`.

## Layout

```text
OneRust/
  OneCLI/          # required: unzipped OneCLI (not committed)
  firmware/        # acquired packages per machine type
  logs/            # per-serial update logs + compare output
  src/             # React UI
  src-tauri/       # Rust / Tauri backend
```

## Notes

- Targets **V3/V4 only** (XCC2/XCC3 multipart update). Older XCC1 / UXSP flows are out of scope.
- BMC TLS verification is off by default (self-signed XCC certs). Enable “Verify BMC TLS” in the UI if needed.
- Host reboot uses Redfish `ComputerSystem.Reset` (`ForceRestart`, with graceful fallback when requested).
- OneCLI, firmware downloads, and logs are gitignored — keep them local.

## License

MIT
