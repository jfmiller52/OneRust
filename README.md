<p align="center">
  <img src="src-tauri/icons/128x128.png" alt="OneRust" width="96" height="96">
</p>

# OneRust

Windows desktop app for **Lenovo ThinkSystem** fleet ops over the BMC/XCC — powered entirely by [Lenovo XClarity Essentials OneCLI](https://support.lenovo.com/us/en/solutions/ht116433).

[![CI](https://img.shields.io/github/actions/workflow/status/jfmiller52/OneRust/ci.yml?branch=main&style=flat-square&label=CI)](https://github.com/jfmiller52/OneRust/actions)
[![Release](https://img.shields.io/github/v/release/jfmiller52/OneRust?style=flat-square)](https://github.com/jfmiller52/OneRust/releases/latest)
[![Windows](https://img.shields.io/badge/Windows-x64-0078D4?style=flat-square&logo=windows&logoColor=white)](https://github.com/jfmiller52/OneRust/releases)
[![Tauri](https://img.shields.io/badge/Tauri-2-FFC131?style=flat-square&logo=tauri&logoColor=black)](https://tauri.app/)
[![Rust](https://img.shields.io/badge/Rust-stable-dea584?style=flat-square&logo=rust&logoColor=black)](https://www.rust-lang.org/)
[![React](https://img.shields.io/badge/React-19-61DAFB?style=flat-square&logo=react&logoColor=black)](https://react.dev/)

[Overview](#overview) • [Install](#install) • [Develop](#develop) • [Firmware update](#firmware-update) • [Blueprint](#blueprint) • [Layout](#layout) • [Notes](#notes) • [Troubleshooting](#troubleshooting)

Mass-update firmware and apply or verify RAID + UEFI/BMC blueprints across many hosts — without hand-running OneCLI scripts per BMC.

> [!NOTE]
> All BMC work goes through OneCLI subprocesses. There is **no Redfish client** in the app.

## Overview

OneRust is a Tauri + React desktop UI around Lenovo OneCLI. Pick ThinkSystem V3/V4 models, acquire Update Bundles, flash out-of-band, reboot, and verify — or push settings / RAID policies from local blueprint files.

| Mode | What it does |
|------|----------------|
| **Firmware update** | Acquire bundles → flash (`--bundle` / `OnReset`) → reboot via OneCLI power → compare |
| **Blueprint** | Apply or verify settings and/or RAID against many BMCs |

On first launch, if `OneCLI/OneCli.exe` is missing beside the executable, OneRust downloads a **pinned** OneCLI Windows zip (`5.7.0`), verifies its **SHA-256**, and extracts it next to the app.

### What’s new in 1.2.0

- Treat OneCLI flash exit **86** as staged success (OnReset), not a failure
- Hide OneCLI console windows during acquire/flash/reboot/verify
- **Open logs** button in the header (and on update / blueprint actions)

### From 1.1.0

- Firmware flash no longer passes `--noreboot` with `--bundle` (OneCLI 5.7 rejects that combo)
- Reboot remains a separate `misc power` step after a successful flash

## Features

- **Fleet firmware updates** — ThinkSystem V3/V4 catalog, concurrent flash, reboot, and compare verify
- **Blueprint apply & verify** — separate pickers for settings/firmware and RAID, or a combined `#RAID` file
- **One reboot when both apply** — settings then RAID; restart only after both succeed
- **Cancel in-flight jobs** — stop a running fleet job cleanly
- **Per-host credentials** — shared XCC user/password, or `user:pass@ip` overrides
- **Pinned OneCLI bootstrap** — download URL + SHA-256 checked on first run
- **Local logs** — OneCLI stdout/stderr under `logs/` by serial

## Install

Download the latest build from [Releases](https://github.com/jfmiller52/OneRust/releases/latest):

- `OneRust_1.2.0_x64-setup.exe` (NSIS), or
- `OneRust_1.2.0_x64_en-US.msi`

Launch once so OneCLI can bootstrap if needed.

> [!IMPORTANT]
> OneRust targets **Windows x64**. You need network reachability to target XCC/BMC IPs.

## Develop

**Prerequisites:** [Node.js](https://nodejs.org/) 20+, [Rust](https://rustup.rs/) (stable, MSVC toolchain).

```powershell
git clone https://github.com/jfmiller52/OneRust.git
cd OneRust
npm install
npm run tauri dev
```

Release build (NSIS + MSI under the Cargo target `bundle/` tree):

```powershell
npm run tauri build
```

Checks:

```powershell
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
```

To bump the bundled OneCLI version, update `ONECLI_DOWNLOAD_URL` and `ONECLI_ZIP_SHA256` together in `src-tauri/src/lenovo.rs`.

## Firmware update

Wizard: **Models → Firmware → Targets → Update**.

1. **Models** — Select ThinkSystem V3/V4 families from the catalog, and/or enter extra 4-character machine types.
2. **Firmware** — Acquire latest firmware-only ZIPs with OneCLI `update acquire` into `firmware/<MT>/`. Offline mode uses local ZIPs only.
3. **Targets** — Shared XCC credentials, concurrency, optional TLS verify, and BMC IPs (`user:pass@ip` per-host overrides allowed).
4. **Update** — For each host:

| Step | OneCLI |
|------|--------|
| Identify serial + machine type | `inventory getinfor --device system_overview` |
| Stage bundle | `update flash --bundle --applytime OnReset` |
| Reboot | `misc power forcerestart` (or `normalrestart` when graceful) |
| Wait for BMC | poll `misc power state` |
| Verify | `update compare` — **verified** only when no further updates are recommended |

> [!NOTE]
> OneCLI 5.7 rejects `--bundle` and `--noreboot` together. OneRust stages with `--bundle` / `OnReset`, then reboots separately with `misc power`. Exit code **86** means *Staged* (success for OnReset); it is not treated as a failure.

Hosts whose machine type was not selected (no matching bundle) are **skipped**.

## Blueprint

Switch to the **Blueprint** tab. Choose a settings / firmware blueprint and/or a separate RAID `.ini`, then **Apply** or **Verify against hosts**.

When both a settings blueprint and a RAID file are selected, OneRust applies settings first, then RAID, and **restarts the host only once after both succeed**.

### Separate settings + RAID

| Picker | Typical contents |
|--------|------------------|
| Settings / firmware blueprint | BMC/UEFI `Setting=Value` lines, `set …` batch lines, or firmware compare `.xml` |
| RAID settings (`.ini`) | OneCLI RAID sample format (`[ctrl…]` sections) |

Either picker alone is enough. If both are set, the separate RAID file is used (embedded `#RAID` in the settings file is ignored for that run).

### Combined settings + RAID file

Put BMC/UEFI settings (and optional `set …` batch lines) **above** a `#RAID` marker and leave the RAID picker empty. Everything below the marker is written verbatim to a temp INI for `misc raid add --force`.

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

`# RAID` (with a space) or `#RAID: …` also works. RAID-only files without a marker still work when they use `[ctrl…]` sections — or pick them in the RAID settings box.

**Apply** order:

1. `config replicate` (settings lines)
2. `config batch` (`set …` lines, if present)
3. `misc raid add --force` (separate RAID file, or body below `#RAID`)
4. Force-restart **once** (`misc power forcerestart`) — only after all selected steps succeed
5. Wait for BMC (`misc power state`)

Verify does **not** reboot.

| Part | Apply | Verify |
|------|-------|--------|
| Settings (`Setting=Value`) | `config replicate` | `config compare --file` |
| Batch (`set …`) | `config batch` | converted to `Setting=Value`, then compare |
| RAID (file or `#RAID` body) | `misc raid add --force` | `misc raid show` (vol name / level) |
| Firmware compare `.xml` | `update flash --comparexml` | `update compare` (needs package dir) |

Outcomes: **applied** / **verified** / **mismatch** / **failed**. Details under `logs/blueprint/<serial>/`.

Suggested library layout:

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

> [!TIP]
> TLS verification is **off** by default (`--never-check-trust`). Turn on **Verify BMC TLS** in the UI when you need certificate checks.

- Designed for **ThinkSystem V3/V4** out-of-band management through XCC.
- OneCLI, firmware trees, and logs are gitignored — keep them local to each workstation.
- HTTP is used only to download the OneCLI zip on first run; day-to-day BMC operations are OneCLI subprocesses.
- CI runs frontend build + `cargo test` on Windows (`main` pushes and PRs).

## Troubleshooting

| Symptom | What to check |
|---------|----------------|
| `--bundle` / `--noreboot` cannot be specified at same time | Fixed — flash uses `--bundle` only; reboot is a separate `misc power` step. Rebuild/restart the app. |
| Flash exit 86 reported as FAILED | Exit 86 means **Staged** (success with OnReset). Current builds treat it as success and continue to reboot. |
| OneCLI setup failed | Network to Lenovo download URL; SHA-256 mismatch means bump URL + hash together |
| Host skipped (firmware) | Machine type not in selection / no ZIP under `firmware/<MT>/` |
| Blueprint verify mismatch | Diff under `logs/blueprint/<serial>/verify/`; RAID expectations need `vol_name` / `raid_level` |
| BMC never returns after reboot | Reachability and credentials; wait timeout defaults to 90 minutes |
| Empty INI refused | Settings / RAID files must contain real content, not comments only |

If something else breaks, [open an issue](https://github.com/jfmiller52/OneRust/issues) with the relevant `logs/` snippets (redact passwords).
