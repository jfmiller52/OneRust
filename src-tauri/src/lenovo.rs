//! Firmware acquisition via Lenovo XClarity Essentials OneCLI.

use anyhow::{anyhow, bail, Context, Result};
use futures::StreamExt;
use reqwest::Client;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Official OneCLI Windows package used when bootstrapping beside the app.
pub const ONECLI_DOWNLOAD_URL: &str =
    "https://download.lenovo.com/servers/mig/2026/09/02/65219/lnvgy_utl_lxce_onecli01m-5.7.0_windows_indiv.zip";

/// SHA-256 of `ONECLI_DOWNLOAD_URL` (lowercase hex). Bump URL + hash together.
pub const ONECLI_ZIP_SHA256: &str =
    "9e9753b541b16ba753beef09f11bd6b3bd84c5b5634dc7c32bfd0958e9e9408d";

const ONECLI_ZIP_NAME: &str = "lnvgy_utl_lxce_onecli01m-5.7.0_windows_indiv.zip";
const USER_AGENT: &str = "OneRust/0.3.2";

/// HTTP client (kept for shared use; firmware acquire uses OneCLI).
pub fn download_client() -> Result<Client> {
    Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(1800))
        .connect_timeout(Duration::from_secs(30))
        .build()
        .context("build HTTP client")
}

/// Preferred install dir: `OneCLI` beside the running executable.
pub fn preferred_onecli_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            return parent.join("OneCLI");
        }
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("OneCLI")
}

/// Candidate OneCLI locations (existing installs), preferred first.
pub fn onecli_dir_candidates() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let preferred = preferred_onecli_dir();
    dirs.push(preferred.clone());

    if let Ok(cwd) = std::env::current_dir() {
        let cwd_onecli = cwd.join("OneCLI");
        if cwd_onecli != preferred && !dirs.iter().any(|d| d == &cwd_onecli) {
            dirs.push(cwd_onecli);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            // During `tauri dev`, also try workspace root (two levels up from target/*/).
            if let Some(grand) = parent.parent().and_then(|p| p.parent()) {
                let repo = grand.join("OneCLI");
                if !dirs.iter().any(|d| d == &repo) {
                    dirs.push(repo);
                }
            }
        }
    }
    dirs
}

/// Locate `OneCli.exe` under a known `OneCLI` folder.
pub async fn find_onecli() -> Option<PathBuf> {
    for dir in onecli_dir_candidates() {
        if let Ok(found) = find_onecli_under(&dir).await {
            return Some(found);
        }
    }
    None
}

async fn find_onecli_under(dir: &Path) -> Result<PathBuf> {
    if !dir.is_dir() {
        bail!("OneCLI dir missing: {}", dir.display());
    }
    let direct = dir.join("OneCli.exe");
    if direct.is_file() {
        return Ok(direct);
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let mut entries = fs::read_dir(&current).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case("OneCli.exe"))
            {
                return Ok(path);
            }
        }
    }
    bail!("OneCli.exe not found under {}", dir.display());
}

/// Resolve OneCLI beside the app, downloading and extracting on first run if missing.
pub async fn ensure_onecli() -> Result<PathBuf> {
    if let Some(existing) = find_onecli().await {
        return Ok(existing);
    }

    let client = download_client()?;
    let dest_dir = preferred_onecli_dir();
    let parent = dest_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    fs::create_dir_all(&parent)
        .await
        .with_context(|| format!("create {}", parent.display()))?;
    fs::create_dir_all(&dest_dir)
        .await
        .with_context(|| format!("create {}", dest_dir.display()))?;

    let zip_path = parent.join(ONECLI_ZIP_NAME);
    if !zip_path.is_file() {
        download_file(&client, ONECLI_DOWNLOAD_URL, &zip_path).await?;
    }
    verify_onecli_zip_sha256(&zip_path).await?;

    extract_zip(&zip_path, &dest_dir)
        .await
        .with_context(|| format!("extract OneCLI to {}", dest_dir.display()))?;

    find_onecli_under(&dest_dir).await.with_context(|| {
        format!(
            "OneCLI downloaded to {} but OneCli.exe was not found after extract",
            dest_dir.display()
        )
    })
}

async fn download_file(client: &Client, url: &str, dest: &Path) -> Result<()> {
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        bail!("download {url} failed: HTTP {}", resp.status());
    }

    let tmp = dest.with_extension("partial");
    let mut file = fs::File::create(&tmp)
        .await
        .with_context(|| format!("create {}", tmp.display()))?;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("download stream")?;
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    drop(file);
    fs::rename(&tmp, dest)
        .await
        .with_context(|| format!("rename {} -> {}", tmp.display(), dest.display()))?;
    Ok(())
}

async fn verify_onecli_zip_sha256(path: &Path) -> Result<()> {
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncReadExt;

    let mut file = fs::File::open(path)
        .await
        .with_context(|| format!("open {} for checksum", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 256];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = format!("{:x}", hasher.finalize());
    if digest != ONECLI_ZIP_SHA256 {
        let _ = fs::remove_file(path).await;
        bail!(
            "OneCLI zip checksum mismatch for {} (got {digest}, expected {ONECLI_ZIP_SHA256}). \
             Re-download or bump ONECLI_DOWNLOAD_URL + ONECLI_ZIP_SHA256 together.",
            path.display()
        );
    }
    Ok(())
}

/// Extract a zip using PowerShell Expand-Archive, with `tar` fallback.
async fn extract_zip(zip_path: &Path, dest_dir: &Path) -> Result<()> {
    let zip_str = zip_path.to_string_lossy().replace('\'', "''");
    let dest_str = dest_dir.to_string_lossy().replace('\'', "''");
    let ps = format!(
        "Expand-Archive -LiteralPath '{zip_str}' -DestinationPath '{dest_str}' -Force"
    );
    let status = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .status()
        .await
        .context("run Expand-Archive")?;
    if status.success() {
        return Ok(());
    }

    let status = Command::new("tar")
        .args([
            "-xf",
            &zip_path.to_string_lossy(),
            "-C",
            &dest_dir.to_string_lossy(),
        ])
        .status()
        .await
        .context("run tar extract")?;
    if !status.success() {
        bail!("failed to extract {}", zip_path.display());
    }
    Ok(())
}

/// Run OneCLI update acquire for a machine type into `firmware/<mt>/`.
pub async fn onecli_acquire(
    onecli: &Path,
    mt: &str,
    firmware_root: &Path,
) -> Result<PathBuf> {
    let dir = firmware_root.join(mt.to_uppercase());
    fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("create {}", dir.display()))?;

    let output_dir = PathBuf::from("logs").join("onecli").join(mt.to_uppercase());
    fs::create_dir_all(&output_dir)
        .await
        .with_context(|| format!("create {}", output_dir.display()))?;

    // ThinkSystem V3/V4: firmware-only ZIP update bundles
    let status = Command::new(onecli)
        .args([
            "update",
            "acquire",
            "--mt",
            mt,
            "--scope",
            "latest",
            "--type",
            "fw",
            "--zip",
            "--ostype",
            "none",
            "--dir",
            &dir.to_string_lossy(),
            "--output",
            &output_dir.to_string_lossy(),
            "--quiet",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| format!("spawn {}", onecli.display()))?;

    if !status.status.success() {
        let stdout = String::from_utf8_lossy(&status.stdout);
        let stderr = String::from_utf8_lossy(&status.stderr);
        bail!(
            "OneCLI acquire failed for MT {mt} (exit {:?})\nstdout:\n{}\nstderr:\n{}",
            status.status.code(),
            truncate(&stdout, 800),
            truncate(&stderr, 800)
        );
    }

    find_local_bundle(firmware_root, mt)
        .await?
        .ok_or_else(|| {
            anyhow!(
                "OneCLI finished for MT {mt} but no ZIP was found in {}",
                dir.display()
            )
        })
}

/// Locate an existing ZIP under firmware/<mt>/ (prefer MT in name, then bundle-like names).
pub async fn find_local_bundle(firmware_root: &Path, mt: &str) -> Result<Option<PathBuf>> {
    let mt_upper = mt.to_uppercase();
    let mt_lower = mt.to_ascii_lowercase();
    let dir = firmware_root.join(&mt_upper);
    if !dir.is_dir() {
        return Ok(None);
    }

    let mut zips: Vec<(u8, u64, PathBuf)> = Vec::new();
    let mut stack = vec![dir];
    while let Some(current) = stack.pop() {
        let mut entries = fs::read_dir(&current).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_lowercase();
            if !name.ends_with(".zip") {
                continue;
            }
            if name.contains("onecli") || name.contains("lxce") {
                continue;
            }
            let meta = fs::metadata(&path).await?;
            let mut rank: u8 = 0;
            if name.contains(&mt_lower) {
                rank += 2;
            }
            if is_bundle_name(&path) {
                rank += 1;
            }
            zips.push((rank, meta.len(), path));
        }
    }

    if zips.is_empty() {
        return Ok(None);
    }

    zips.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    Ok(zips.into_iter().next().map(|(_, _, p)| p))
}

fn is_bundle_name(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    name.contains("bundle") || name.contains("uxsp") || name.contains("update")
}

/// Acquire latest bundle for `mt` via local OneCLI, or return a local ZIP in offline mode.
pub async fn acquire_bundle_for_mt(
    mt: &str,
    firmware_root: &Path,
    offline_only: bool,
    force_reacquire: bool,
) -> Result<PathBuf> {
    if offline_only {
        return find_local_bundle(firmware_root, mt)
            .await?
            .ok_or_else(|| {
                anyhow!(
                    "no local ZIP for MT {mt} under {}",
                    firmware_root.display()
                )
            });
    }

    if !force_reacquire {
        if let Some(local) = find_local_bundle(firmware_root, mt).await? {
            return Ok(local);
        }
    }

    let onecli = ensure_onecli().await?;
    onecli_acquire(&onecli, mt, firmware_root).await
}

/// Result of OneCLI `update compare` against a live BMC.
#[derive(Debug, Clone)]
pub struct CompareOutcome {
    pub packages_needed: usize,
    pub summary: String,
    pub output_dir: PathBuf,
}

/// Compare installed firmware on a BMC to local packages; `packages_needed == 0` means up to date.
pub async fn onecli_compare(
    bmc_user: &str,
    bmc_pass: &str,
    bmc_ip: &str,
    package_dir: &Path,
    output_dir: &Path,
    never_check_trust: bool,
) -> Result<CompareOutcome> {
    let onecli = ensure_onecli().await?;
    fs::create_dir_all(output_dir)
        .await
        .with_context(|| format!("create {}", output_dir.display()))?;

    let bmc = format!("{bmc_user}:{bmc_pass}@{bmc_ip}");
    let mut cmd = Command::new(&onecli);
    cmd.args([
        "update",
        "compare",
        "--bmc",
        &bmc,
        "--dir",
        &package_dir.to_string_lossy(),
        "--type",
        "fw",
        "--scope",
        "latest",
        "--ostype",
        "none",
        "--output",
        &output_dir.to_string_lossy(),
        "--quiet",
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    if never_check_trust {
        cmd.arg("--never-check-trust");
    }
    let output = cmd
        .output()
        .await
        .with_context(|| format!("spawn {}", onecli.display()))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{stdout}\n{stderr}");

    // Persist raw output for the serial log folder consumers
    let _ = fs::write(output_dir.join("compare-stdout.txt"), &stdout).await;
    let _ = fs::write(output_dir.join("compare-stderr.txt"), &stderr).await;

    let packages_needed = count_packages_needed(output_dir, &combined).await?;

    if !output.status.success() && packages_needed == 0 {
        // OneCLI sometimes exits non-zero even when compare produced usable XML.
        // Only hard-fail when we also couldn't interpret a clean result.
        if !combined.to_lowercase().contains("compare")
            && !dir_has_xml(output_dir).await.unwrap_or(false)
        {
            bail!(
                "OneCLI compare failed (exit {:?}): {}",
                output.status.code(),
                truncate(&combined, 800)
            );
        }
    }

    let summary = if packages_needed == 0 {
        "OneCLI compare: no firmware updates required".to_string()
    } else {
        format!("OneCLI compare: {packages_needed} firmware package(s) still recommended")
    };

    Ok(CompareOutcome {
        packages_needed,
        summary,
        output_dir: output_dir.to_path_buf(),
    })
}

/// Host identity from OneCLI inventory.
#[derive(Debug, Clone)]
pub struct OnecliIdentity {
    pub serial: String,
    pub machine_type: Option<String>,
}

/// Resolve serial + machine type via `inventory getinfor --device system_overview`.
pub async fn onecli_identify(
    bmc_user: &str,
    bmc_pass: &str,
    bmc_ip: &str,
    output_dir: &Path,
    never_check_trust: bool,
) -> Result<OnecliIdentity> {
    let onecli = ensure_onecli().await?;
    fs::create_dir_all(output_dir)
        .await
        .with_context(|| format!("create {}", output_dir.display()))?;

    let bmc = format!("{bmc_user}:{bmc_pass}@{bmc_ip}");
    let mut cmd = Command::new(&onecli);
    cmd.args([
        "inventory",
        "getinfor",
        "--bmc",
        &bmc,
        "--device",
        "system_overview",
        "--quiet",
        "--output",
        &output_dir.to_string_lossy(),
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    if never_check_trust {
        cmd.arg("--never-check-trust");
    }
    let output = cmd
        .output()
        .await
        .with_context(|| format!("spawn {}", onecli.display()))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let _ = fs::write(output_dir.join("getinfor-stdout.txt"), &stdout).await;
    let _ = fs::write(output_dir.join("getinfor-stderr.txt"), &stderr).await;

    let mut blob = format!("{stdout}\n{stderr}\n");
    blob.push_str(&collect_text_files(output_dir).await.unwrap_or_default());

    if !output.status.success()
        && extract_serial_from_inventory(&blob).is_none()
        && extract_machine_type_from_inventory(&blob).is_none()
    {
        bail!(
            "OneCLI inventory getinfor failed (exit {:?}): {}",
            output.status.code(),
            truncate(&blob, 800)
        );
    }

    let serial = extract_serial_from_inventory(&blob)
        .map(|s| crate::logutil::sanitize_serial(&s))
        .unwrap_or_else(|| {
            format!(
                "unknown-{}",
                crate::logutil::sanitize_serial(bmc_ip)
            )
        });
    let machine_type = extract_machine_type_from_inventory(&blob);

    Ok(OnecliIdentity {
        serial,
        machine_type,
    })
}

/// Stage/flash a firmware bundle with the given apply time.
///
/// OneCLI rejects `--bundle` together with `--noreboot`; reboot is handled
/// separately via [`onecli_power_restart`] after a successful flash.
pub async fn onecli_flash_bundle(
    bmc_user: &str,
    bmc_pass: &str,
    bmc_ip: &str,
    package_dir: &Path,
    output_dir: &Path,
    applytime: &str,
    never_check_trust: bool,
) -> Result<String> {
    let onecli = ensure_onecli().await?;
    fs::create_dir_all(output_dir)
        .await
        .with_context(|| format!("create {}", output_dir.display()))?;

    let bmc = format!("{bmc_user}:{bmc_pass}@{bmc_ip}");
    let mut cmd = Command::new(&onecli);
    cmd.args([
        "update",
        "flash",
        "--bmc",
        &bmc,
        "--dir",
        &package_dir.to_string_lossy(),
        "--bundle",
        "--applytime",
        applytime,
        "--quiet",
        "--output",
        &output_dir.to_string_lossy(),
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    if never_check_trust {
        cmd.arg("--never-check-trust");
    }
    let output = cmd
        .output()
        .await
        .with_context(|| format!("spawn {}", onecli.display()))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{stdout}\n{stderr}");
    let _ = fs::write(output_dir.join("flash-stdout.txt"), &stdout).await;
    let _ = fs::write(output_dir.join("flash-stderr.txt"), &stderr).await;

    if !output.status.success() {
        bail!(
            "OneCLI update flash failed (exit {:?}): {}",
            output.status.code(),
            truncate(&combined, 900)
        );
    }

    Ok(format!(
        "OneCLI flash --bundle --applytime {applytime} OK"
    ))
}

/// Force- or graceful-restart a host via OneCLI power commands.
pub async fn onecli_power_restart(
    bmc_user: &str,
    bmc_pass: &str,
    bmc_ip: &str,
    reset_type: &str,
    output_dir: &Path,
    never_check_trust: bool,
) -> Result<()> {
    let onecli = ensure_onecli().await?;
    fs::create_dir_all(output_dir)
        .await
        .with_context(|| format!("create {}", output_dir.display()))?;

    let action = if reset_type.eq_ignore_ascii_case("GracefulRestart")
        || reset_type.eq_ignore_ascii_case("normalrestart")
    {
        "normalrestart"
    } else {
        "forcerestart"
    };

    let bmc = format!("{bmc_user}:{bmc_pass}@{bmc_ip}");
    let mut cmd = Command::new(&onecli);
    cmd.args([
        "misc",
        "power",
        action,
        "--bmc",
        &bmc,
        "--quiet",
        "--output",
        &output_dir.to_string_lossy(),
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    if never_check_trust {
        cmd.arg("--never-check-trust");
    }
    let output = cmd
        .output()
        .await
        .with_context(|| format!("spawn {}", onecli.display()))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let _ = fs::write(output_dir.join(format!("{action}-stdout.txt")), &stdout).await;
    let _ = fs::write(output_dir.join(format!("{action}-stderr.txt")), &stderr).await;

    if !output.status.success() {
        bail!(
            "OneCLI misc power {action} failed (exit {:?}): {}",
            output.status.code(),
            truncate(&format!("{stdout}\n{stderr}"), 800)
        );
    }
    Ok(())
}

/// Poll `misc power state` until the BMC answers successfully (host/BMC back after reboot).
pub async fn onecli_wait_bmc_ready(
    bmc_user: &str,
    bmc_pass: &str,
    bmc_ip: &str,
    timeout: Duration,
    never_check_trust: bool,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<()> {
    use crate::cancel::JobCancel;

    if cancel.is_some_and(JobCancel::is_cancelled) {
        bail!("cancelled while waiting for BMC {bmc_ip}");
    }

    let onecli = ensure_onecli().await?;
    let bmc = format!("{bmc_user}:{bmc_pass}@{bmc_ip}");
    let start = std::time::Instant::now();
    let mut saw_down = false;

    // Give the host a moment to drop after restart (interruptible).
    for _ in 0..20 {
        if cancel.is_some_and(JobCancel::is_cancelled) {
            bail!("cancelled while waiting for BMC {bmc_ip}");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    loop {
        if cancel.is_some_and(JobCancel::is_cancelled) {
            bail!("cancelled while waiting for BMC {bmc_ip}");
        }
        if start.elapsed() > timeout {
            bail!(
                "timed out waiting for BMC {bmc_ip} after {}s",
                timeout.as_secs()
            );
        }

        let mut cmd = Command::new(&onecli);
        cmd.args(["misc", "power", "state", "--bmc", &bmc, "--quiet"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if never_check_trust {
            cmd.arg("--never-check-trust");
        }

        match cmd.output().await {
            Ok(output) if output.status.success() => {
                let text =
                    format!(
                        "{}\n{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    )
                    .to_ascii_lowercase();
                // Prefer returning once power state reports On after we observed a gap,
                // but accept first success if the BMC never fully dropped.
                if text.contains("power off") || (text.contains("off") && !text.contains("on")) {
                    saw_down = true;
                } else if text.contains("on") || text.contains("power") || !saw_down {
                    return Ok(());
                }
            }
            _ => {}
        }

        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}

async fn collect_text_files(dir: &Path) -> Result<String> {
    let mut out = String::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let mut entries = fs::read_dir(&current).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if name.ends_with(".xml")
                || name.ends_with(".txt")
                || name.ends_with(".html")
                || name.ends_with(".log")
            {
                if let Ok(text) = fs::read_to_string(&path).await {
                    out.push_str(&text);
                    out.push('\n');
                }
            }
        }
    }
    Ok(out)
}

/// Pull a serial number out of OneCLI inventory text/XML.
pub fn extract_serial_from_inventory(text: &str) -> Option<String> {
    const TAGS: &[&str] = &[
        "SerialNumber",
        "serial_number",
        "MachineSerialNumber",
        "ProductSerialNumber",
        "Serial",
    ];
    for tag in TAGS {
        if let Some(v) = xml_tag_value(text, tag) {
            if looks_like_serial(&v) {
                return Some(v);
            }
        }
    }

    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if !(lower.contains("serialnumber")
            || lower.contains("serial_number")
            || lower.contains("serial number"))
        {
            continue;
        }
        if let Some(v) = value_after_sep(line) {
            if looks_like_serial(&v) {
                return Some(v);
            }
        }
    }
    None
}

/// Pull a 4-char machine type from OneCLI inventory text/XML.
pub fn extract_machine_type_from_inventory(text: &str) -> Option<String> {
    const TAGS: &[&str] = &[
        "MachineType",
        "machine_type",
        "MT",
        "ProductId",
        "ProductID",
        "MachType",
    ];
    for tag in TAGS {
        if let Some(v) = xml_tag_value(text, tag) {
            let mt = crate::catalog::normalize_machine_type(&v);
            if mt.len() == 4 {
                return Some(mt);
            }
        }
    }

    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if !(lower.contains("machinetype")
            || lower.contains("machine_type")
            || lower.contains("machine type")
            || lower.contains("productid")
            || lower.contains("machtype"))
        {
            continue;
        }
        if let Some(v) = value_after_sep(line) {
            let mt = crate::catalog::normalize_machine_type(&v);
            if mt.len() == 4 {
                return Some(mt);
            }
        }
    }

    // Fall back to catalog heuristics against model/hostname-like strings.
    crate::catalog::extract_machine_type(None, Some(text), Some(text))
}

fn xml_tag_value(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let lower = text.to_ascii_lowercase();
    let open_l = open.to_ascii_lowercase();
    let close_l = close.to_ascii_lowercase();
    let start = lower.find(&open_l)?;
    let after_open = start + open_l.len();
    let gt = text[after_open..].find('>')? + after_open + 1;
    let end_rel = lower[gt..].find(&close_l)?;
    let value = text[gt..gt + end_rel].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn value_after_sep(line: &str) -> Option<String> {
    for sep in [">", "=", ":", "\t"] {
        if let Some((_, rest)) = line.split_once(sep) {
            let cleaned = rest
                .trim()
                .trim_matches(|c: char| c == '"' || c == '\'' || c == '<' || c == '/')
                .trim();
            let cleaned = cleaned.split('<').next().unwrap_or(cleaned).trim();
            if !cleaned.is_empty() {
                return Some(cleaned.to_string());
            }
        }
    }
    None
}

fn looks_like_serial(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() || s.len() > 32 {
        return false;
    }
    let lower = s.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "n/a" | "na" | "none" | "null" | "unknown" | "not available"
    ) {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

async fn dir_has_xml(dir: &Path) -> Result<bool> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let mut entries = fs::read_dir(&current).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("xml"))
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Heuristic: count packages OneCLI still wants to apply from compare XML / console text.
pub async fn count_packages_needed(output_dir: &Path, console: &str) -> Result<usize> {
    let lower = console.to_lowercase();
    if lower.contains("no package")
        || lower.contains("no updates")
        || lower.contains("0 package")
        || lower.contains("nothing to update")
        || lower.contains("system is up to date")
    {
        return Ok(0);
    }

    let mut xml_blob = String::new();
    let mut stack = vec![output_dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(mut entries) = fs::read_dir(&current).await else {
            continue;
        };
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_lowercase();
            if name.ends_with(".xml")
                && (name.contains("compare")
                    || name.contains("result")
                    || name.contains("common")
                    || name.contains("package"))
            {
                if let Ok(text) = fs::read_to_string(&path).await {
                    xml_blob.push_str(&text);
                    xml_blob.push('\n');
                }
            }
        }
    }

    if xml_blob.is_empty() {
        // Fall back: scan console for "update required" style lines
        let hits = lower
            .lines()
            .filter(|l| {
                (l.contains("update") || l.contains("upgrade") || l.contains("flash"))
                    && (l.contains("required")
                        || l.contains("recommend")
                        || l.contains("available")
                        || l.contains("outdated"))
            })
            .count();
        return Ok(hits);
    }

    count_update_packages_in_xml(&xml_blob)
}

/// Count Package nodes that still need flashing. Unknown CompareResult → error.
pub fn count_update_packages_in_xml(xml: &str) -> Result<usize> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let trimmed = xml.trim();
    if trimmed.is_empty() {
        return Ok(0);
    }

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut needed = 0usize;
    let mut packages = 0usize;
    let mut in_package = 0usize;
    let mut in_compare_result = false;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_ascii_lowercase();
                if name == "package" {
                    packages += 1;
                    in_package += 1;
                } else if name == "compareresult" && in_package > 0 {
                    in_compare_result = true;
                }
            }
            Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_ascii_lowercase();
                if name == "package" {
                    bail!("compare XML Package has no CompareResult");
                }
            }
            Ok(Event::Text(t)) => {
                if in_compare_result {
                    let val = t.unescape().unwrap_or_default().to_ascii_lowercase();
                    let val = val.trim();
                    match val {
                        "noupdate" | "no update" | "current" | "uptodate" | "up-to-date"
                        | "same" | "match" => {}
                        "update" | "upgrade" | "notinstalled" | "not installed" | "downgrade"
                        | "critical" => needed += 1,
                        other if other.is_empty() => {
                            bail!("compare XML Package has empty CompareResult");
                        }
                        other => {
                            bail!("unknown CompareResult '{other}' in compare XML");
                        }
                    }
                    in_compare_result = false;
                }
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_ascii_lowercase();
                if name == "compareresult" {
                    in_compare_result = false;
                } else if name == "package" {
                    in_package = in_package.saturating_sub(1);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => bail!("failed to parse compare XML: {e}"),
            _ => {}
        }
        buf.clear();
    }

    if packages == 0 {
        // No Package nodes — not a usable compare document
        if xml.to_ascii_lowercase().contains("<package") {
            bail!("compare XML looks malformed (unclosed Package nodes)");
        }
        return Ok(0);
    }

    Ok(needed)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let trimmed: String = s.chars().take(max).collect();
        format!("{trimmed}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_name_heuristic() {
        assert!(is_bundle_name(Path::new("24a.0-SR650V3-bundle.zip")));
        assert!(!is_bundle_name(Path::new("readme.txt")));
    }

    #[test]
    fn onecli_dir_is_named_onecli() {
        let dirs = onecli_dir_candidates();
        assert!(dirs.iter().any(|d| d.ends_with("OneCLI")));
        assert!(preferred_onecli_dir().ends_with("OneCLI"));
    }

    #[test]
    fn onecli_url_is_windows_zip() {
        assert!(ONECLI_DOWNLOAD_URL.contains("onecli"));
        assert!(ONECLI_DOWNLOAD_URL.ends_with(".zip"));
        assert_eq!(ONECLI_ZIP_SHA256.len(), 64);
    }

    #[tokio::test]
    async fn find_local_bundle_prefers_mt_in_name() {
        let dir = tempfile::tempdir().unwrap();
        let mt_dir = dir.path().join("7D75");
        tokio::fs::create_dir_all(&mt_dir).await.unwrap();
        tokio::fs::write(mt_dir.join("generic-bundle.zip"), b"aaaa").await.unwrap();
        tokio::fs::write(mt_dir.join("lnvgy_fw_uefi_7d75_bundle.zip"), b"bbbbbbbb").await.unwrap();
        let found = find_local_bundle(dir.path(), "7D75").await.unwrap().unwrap();
        assert!(
            found
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_lowercase()
                .contains("7d75")
        );
    }

    #[tokio::test]
    async fn wait_bmc_exits_early_on_cancel() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;
        let flag = Arc::new(AtomicBool::new(true));
        let err = onecli_wait_bmc_ready(
            "u",
            "p",
            "127.0.0.1",
            Duration::from_secs(5),
            true,
            Some(&flag),
        )
        .await
        .unwrap_err();
        assert!(format!("{err:#}").to_ascii_lowercase().contains("cancelled"));
    }

    #[test]
    fn compare_xml_no_updates() {
        let xml = r#"
            <Packages>
              <Package><Name>UEFI</Name><CompareResult>NoUpdate</CompareResult></Package>
              <Package><Name>XCC</Name><CompareResult>Current</CompareResult></Package>
            </Packages>
        "#;
        assert_eq!(count_update_packages_in_xml(xml).unwrap(), 0);
    }

    #[test]
    fn compare_xml_needs_updates() {
        let xml = r#"
            <Packages>
              <Package><Name>UEFI</Name><CompareResult>Update</CompareResult></Package>
              <Package><Name>XCC</Name><CompareResult>NoUpdate</CompareResult></Package>
            </Packages>
        "#;
        assert_eq!(count_update_packages_in_xml(xml).unwrap(), 1);
    }

    #[test]
    fn compare_xml_unknown_result_errors() {
        let xml = r#"
            <Packages>
              <Package><Name>UEFI</Name><CompareResult>WeirdState</CompareResult></Package>
            </Packages>
        "#;
        assert!(count_update_packages_in_xml(xml).is_err());
    }

    #[test]
    fn compare_xml_packages_without_result_not_all_counted() {
        // Previously every bare <Package> was counted; now unknown/malformed fails.
        let xml = r#"
            <Packages>
              <Package><Name>UEFI</Name></Package>
            </Packages>
        "#;
        // No CompareResult text → needed stays 0 (package present but no update signal)
        assert_eq!(count_update_packages_in_xml(xml).unwrap(), 0);
    }

    #[test]
    fn extracts_serial_from_xml_tag() {
        let xml = r#"
            <System>
              <MachineType>7D75</MachineType>
              <SerialNumber>J123ABC</SerialNumber>
            </System>
        "#;
        assert_eq!(
            extract_serial_from_inventory(xml).as_deref(),
            Some("J123ABC")
        );
        assert_eq!(
            extract_machine_type_from_inventory(xml).as_deref(),
            Some("7D75")
        );
    }

    #[test]
    fn extracts_serial_from_key_value() {
        let text = "Serial Number: K987XYZ\nHostname: node1\n";
        assert_eq!(
            extract_serial_from_inventory(text).as_deref(),
            Some("K987XYZ")
        );
    }

    #[test]
    fn ignores_placeholder_serial() {
        let xml = "<SerialNumber>N/A</SerialNumber>";
        assert!(extract_serial_from_inventory(xml).is_none());
    }
}
