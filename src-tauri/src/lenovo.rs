//! Firmware acquisition via Lenovo XClarity Essentials OneCLI.

use anyhow::{anyhow, bail, Context, Result};
use reqwest::Client;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::fs;
use tokio::process::Command;

const USER_AGENT: &str = "OneRust/0.1";

/// HTTP client (kept for shared use; firmware acquire uses OneCLI).
pub fn download_client() -> Result<Client> {
    Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(1800))
        .connect_timeout(Duration::from_secs(30))
        .build()
        .context("build HTTP client")
}

/// Expected OneCLI install directory: `<app_dir>/OneCLI`.
pub fn onecli_dir_candidates() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.join("OneCLI"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let beside_exe = parent.join("OneCLI");
            if !dirs.iter().any(|d| d == &beside_exe) {
                dirs.push(beside_exe);
            }
            // During `cargo run` / `tauri dev`, exe is often under target/*/ — also try repo root.
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

/// Locate `OneCli.exe` under the app's `OneCLI` folder.
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
    // Prefer OneCLI/OneCli.exe directly, then search recursively.
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

/// Resolve OneCLI from `<app>/OneCLI` (no download).
pub async fn ensure_onecli() -> Result<PathBuf> {
    if let Some(existing) = find_onecli().await {
        return Ok(existing);
    }
    let tried = onecli_dir_candidates()
        .into_iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "OneCLI not found. Unzip Lenovo OneCLI into a folder named OneCLI next to the app \
         (looked in: {tried})"
    )
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

/// Locate an existing ZIP under firmware/<mt>/ (prefer filenames that look like bundles).
pub async fn find_local_bundle(firmware_root: &Path, mt: &str) -> Result<Option<PathBuf>> {
    let dir = firmware_root.join(mt.to_uppercase());
    if !dir.is_dir() {
        return Ok(None);
    }

    let mut zips: Vec<(u64, PathBuf)> = Vec::new();
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
            zips.push((meta.len(), path));
        }
    }

    if zips.is_empty() {
        return Ok(None);
    }

    zips.sort_by(|a, b| {
        let a_bundle = is_bundle_name(&a.1);
        let b_bundle = is_bundle_name(&b.1);
        b_bundle.cmp(&a_bundle).then(b.0.cmp(&a.0))
    });
    Ok(zips.into_iter().next().map(|(_, p)| p))
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
    _client: &Client,
    mt: &str,
    firmware_root: &Path,
    offline_only: bool,
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

    if let Some(local) = find_local_bundle(firmware_root, mt).await? {
        return Ok(local);
    }

    let onecli = ensure_onecli().await?;
    onecli_acquire(&onecli, mt, firmware_root).await
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
    }
}
