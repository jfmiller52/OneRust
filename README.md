# OneRust

Windows desktop app for **Lenovo ThinkSystem** fleet ops over the BMC/XCC — powered entirely by **Lenovo XClarity Essentials OneCLI**.

OneRust wraps OneCLI in a Tauri + React UI so you can mass-update firmware and apply / verify RAID + UEFI/BMC blueprints without hand-running CLI scripts per host.

**Current release:** [v1.0.0](https://github.com/jfmiller52/OneRust/releases/tag/v1.0.0)

## What it does

| Mode | Purpose |
|------|---------|
| **Firmware update** | Acquire Update Bundles, flash out-of-band (`--bundle` / `OnReset`), reboot, verify with OneCLI compare |
| **Blueprint** | Apply **or verify** settings and/or RAID blueprints against many BMCs — separate files or combined with `#RAID` |

All BMC work goes through OneCLI. There is **no Redfish client** in the app.

### What’s new in 1.0.0

- Separate pickers for **settings/firmware** and **RAID** blueprints
- When both are selected, apply settings then RAID and **restart once** after both succeed
- Combined `#RAID` files still work if the RAID picker is left empty
- Fleet hardening from 0.3.x (verify, cancel, reboot wait, OneCLI SHA-256 pin, CI)

## Prerequisites

- **Windows** (OneCLI packaging and helpers are Windows-oriented)
- Network reachability to target XCC/BMC IPs
- For development: [Node.js](https://nodejs.org/) 20+, [Rust](https://rustup.rs/) (stable, MSVC)

On **first launch**, if `OneCLI/OneCli.exe` is not already beside the executable, OneRust downloads a **pinned** Lenovo OneCLI Windows zip (`5.7.0`), verifies its **SHA-256**, then extracts it into `OneCLI/` next to the app. To bump OneCLI, update `ONECLI_DOWNLOAD_URL` and `ONECLI_ZIP_SHA256` together in `src-tauri/src/lenovo.rs`.

## Install (release)

Download from [Releases](https://github.com/jfmiller52/OneRust/releases):

- `OneRust_1.0.0_x64-setup.exe` (NSIS), or
- `OneRust_1.0.0_x64_en-US.msi`

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

Switch to the **Blueprint** tab. Choose a settings / firmware blueprint and/or a separate RAID `.ini`, then **Apply** or **Verify against hosts**.

When both a settings blueprint and a RAID file are selected, OneRust applies settings first, then RAID, and **restarts the host only once after both succeed**.

### Separate settings + RAID files

| Picker | Typical contents |
|--------|------------------|
| Settings / firmware blueprint | BMC/UEFI `Setting=Value` lines, `set …` batch lines, or firmware compare `.xml` |
| RAID settings (`.ini`) | OneCLI RAID sample format (`[ctrl…]` sections) |

Either picker alone is enough. If both are set, the separate RAID file is used (embedded `#RAID` in the settings file is ignored for apply/verify).

### Combined settings + RAID file

You can still put BMC/UEFI settings (and optional `set …` batch lines) **above** a `#RAID` marker in one file and leave the RAID picker empty.
**Everything below `#RAID`** is written to a temporary `.ini` and sent with `misc raid add --force` — use OneCLI’s RAID sample format as-is.

```ini
# BMC / UEFI (from config save, or hand-edited Setting=Value lines)
UEFI.BootMode=UEFI Mode
IMM.HostName1=node01

#RAID
[ctrl1-vol0]
disks=0,1
raid_level=1
vol_name=os
```

`# RAID` (with a space) or `#RAID: …` also works as the marker. RAID-only files (no marker) still work when they use `[ctrl…]` sections — or pick them in the RAID settings box.

**Apply** runs, in order:

1. `config replicate` (settings lines)
2. `config batch` (`set …` lines, if present)
3. `misc raid add --force` (separate RAID file, or body below `#RAID`)
4. **Force-restart** each host **once** (`misc power forcerestart`) — only after all selected steps succeed
5. **Wait for BMC** (`misc power state` until the host answers again)

Verify does **not** reboot.

| Detected part | Apply action |
|---------------|--------------|
| Settings (`Setting=Value`) | `config replicate` |
| Batch (`set …`) | `config batch` |
| Separate RAID `.ini` or body below `#RAID` | `misc raid add --force` |
| Firmware compare `.xml` | `update flash --comparexml` (needs a package directory) |

### Verify against hosts

Read-only check of the live BMC against the same blueprint(s):

| Part | Verify method |
|------|----------------|
| Settings / batch | `config compare --file` (batch `set` lines converted to `Setting=Value`) |
| RAID | `misc raid show`, then look for expected `vol_name` / RAID level |
| Firmware XML | `update compare` against the package directory (pass when no updates remain) |

Outcomes: **verified**, **mismatch**, or **failed**. Details under `logs/blueprint/<serial>/verify/`.

Suggested layout for your own library:

```text
blueprints/
  sr650v3-settings.ini      # BMC/UEFI settings
  sr650v3-raid.ini          # RAID policy
  sr650v3-baseline.ini      # optional: settings + #RAID together
  batch/
    ldap.txt                # set … lines only
  firmware/
    7D76-compare.xml        # from update compare
```

## Layout

```text
OneRust/
  OneCLI/                      # auto-downloaded on first run (gitignored)
  firmware/<MT>/               # acquired Update Bundles
  logs/
    <SERIAL>.log               # per-host firmware update log
    identify/                  # inventory used for identity
    flash/<SERIAL>/            # flash command output
    power/<SERIAL>/            # reboot command output
    compare/<SERIAL>/          # compare XML / console
    blueprint/<SERIAL>/        # apply output + _parts/
    blueprint/<SERIAL>/verify/ # verify output
  src/                         # React UI
  src-tauri/                   # Rust / Tauri + OneCLI orchestration
```

## Notes

- Designed for **ThinkSystem V3/V4** out-of-band management through XCC.
- TLS verification is **off** by default (`--never-check-trust`). Turn on “Verify BMC TLS” in the UI when you need certificate checks.
- OneCLI, firmware trees, and logs are gitignored — keep them local to each workstation.
- HTTP is used only to download the OneCLI zip on first run; day-to-day BMC operations are OneCLI subprocesses.

## License

MIT
