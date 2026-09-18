//! Apply OneCLI XML/INI "blueprints" (RAID, BMC/UEFI settings, firmware compare).

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::Semaphore;

use crate::lenovo::{ensure_onecli, onecli_identify};
use crate::logutil::sanitize_serial;
use crate::update::TargetHost;

/// How a blueprint file should be applied through OneCLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlueprintKind {
    /// Hardware/software RAID policy INI → `misc raid add`
    Raid,
    /// Saved `Setting=Value` file → `config replicate`
    ConfigSettings,
    /// Batch of `set …` lines → `config batch`
    ConfigBatch,
    /// OneCLI update-compare XML → `update flash --comparexml`
    FirmwareCompareXml,
}

impl BlueprintKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Raid => "raid",
            Self::ConfigSettings => "config-settings",
            Self::ConfigBatch => "config-batch",
            Self::FirmwareCompareXml => "firmware-xml",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Raid => "RAID policy (INI)",
            Self::ConfigSettings => "BMC/UEFI settings",
            Self::ConfigBatch => "Config batch",
            Self::FirmwareCompareXml => "Firmware compare (XML)",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BlueprintClassify {
    pub kind: BlueprintKind,
    pub label: String,
    pub detail: String,
    pub needs_package_dir: bool,
}

/// Read and classify a blueprint path as RAID / settings / batch / firmware XML.
pub async fn classify_blueprint_file(path: &Path) -> Result<BlueprintClassify> {
    if !path.is_file() {
        bail!("Blueprint file not found: {}", path.display());
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(ext.as_str(), "ini" | "xml" | "txt") {
        bail!("Blueprint must be .ini, .xml, or .txt (got .{ext})");
    }

    let content = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("read {}", path.display()))?;
    let kind = classify_blueprint_content(&ext, &content)?;
    Ok(BlueprintClassify {
        kind,
        label: kind.label().to_string(),
        detail: classify_detail(kind, &content),
        needs_package_dir: kind == BlueprintKind::FirmwareCompareXml,
    })
}

fn classify_detail(kind: BlueprintKind, content: &str) -> String {
    match kind {
        BlueprintKind::Raid => {
            let vols = content
                .lines()
                .filter(|l| {
                    let t = l.trim();
                    t.starts_with('[') && t.contains("ctrl") && t.contains("vol")
                })
                .count();
            if vols > 0 {
                format!("{vols} volume section(s) detected")
            } else {
                "RAID controller sections detected".into()
            }
        }
        BlueprintKind::ConfigBatch => {
            let sets = content
                .lines()
                .filter(|l| l.trim().to_ascii_lowercase().starts_with("set "))
                .count();
            format!("{sets} set command(s)")
        }
        BlueprintKind::ConfigSettings => {
            let keys = content
                .lines()
                .filter(|l| {
                    let t = l.trim();
                    !t.is_empty() && !t.starts_with('#') && t.contains('=')
                })
                .count();
            format!("{keys} setting line(s)")
        }
        BlueprintKind::FirmwareCompareXml => {
            let pkgs = content.to_ascii_lowercase().matches("<package").count();
            if pkgs > 0 {
                format!("{pkgs} <Package> node(s)")
            } else {
                "Firmware compare XML".into()
            }
        }
    }
}

pub fn classify_blueprint_content(ext: &str, content: &str) -> Result<BlueprintKind> {
    let lower = content.to_ascii_lowercase();
    let trimmed = content.trim_start();

    if ext == "xml" || trimmed.starts_with("<?xml") || trimmed.starts_with('<') {
        if lower.contains("<package")
            || lower.contains("compareresult")
            || lower.contains("onecli-update")
            || lower.contains("updatecompare")
            || lower.contains("<packages")
        {
            return Ok(BlueprintKind::FirmwareCompareXml);
        }
        bail!(
            "XML blueprint is not a recognized OneCLI update-compare file. \
             Export compare XML via OneCLI update compare, or use an INI/TXT settings or RAID file."
        );
    }

    // Batch files: lines of `set Setting value`
    let set_lines = content.lines().filter(|l| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with('#') && t.to_ascii_lowercase().starts_with("set ")
    });
    if set_lines.count() > 0 {
        return Ok(BlueprintKind::ConfigBatch);
    }

    // RAID policy: [ctrlN] / [ctrlN-volM] sections
    let has_raid_section = content.lines().any(|l| {
        let t = l.trim().to_ascii_lowercase();
        t.starts_with("[ctrl") || t.starts_with("[broadcom") || t.starts_with("[microchip")
    });
    if has_raid_section || (ext == "ini" && lower.contains("raid_level")) {
        return Ok(BlueprintKind::Raid);
    }

    // Settings file from `config save`: Setting.Name=value
    let setting_lines = content.lines().filter(|l| {
        let t = l.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with(';') {
            return false;
        }
        t.contains('=') && !t.starts_with('[')
    });
    if setting_lines.count() > 0 {
        return Ok(BlueprintKind::ConfigSettings);
    }

    if ext == "ini" {
        // Empty/comment-only sample INI still treated as RAID template intent.
        return Ok(BlueprintKind::Raid);
    }

    bail!("Could not classify blueprint contents as RAID, settings, batch, or firmware XML");
}

#[derive(Debug, Clone)]
pub struct BlueprintApplyOptions {
    pub package_dir: Option<PathBuf>,
    pub logs_dir: PathBuf,
    pub never_check_trust: bool,
    pub applytime: String,
}

impl Default for BlueprintApplyOptions {
    fn default() -> Self {
        Self {
            package_dir: None,
            logs_dir: PathBuf::from("logs"),
            never_check_trust: true,
            applytime: "OnReset".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BlueprintHostResult {
    pub ip: String,
    pub serial: String,
    pub outcome: String,
    pub detail: String,
}

/// Progress callback: (ip, status, serial, detail)
pub type BlueprintProgressCb = Arc<dyn Fn(String, String, Option<String>, String) + Send + Sync>;

/// Apply one blueprint file to many hosts concurrently.
pub async fn apply_blueprint_concurrent(
    blueprint: PathBuf,
    kind: BlueprintKind,
    hosts: Vec<TargetHost>,
    username: String,
    password: String,
    concurrency: usize,
    opts: BlueprintApplyOptions,
    on_progress: Option<BlueprintProgressCb>,
) -> Result<Vec<BlueprintHostResult>> {
    if kind == BlueprintKind::FirmwareCompareXml {
        let dir = opts
            .package_dir
            .as_ref()
            .context("Firmware XML blueprints require a package directory (--dir)")?;
        if !dir.exists() {
            bail!("Package directory not found: {}", dir.display());
        }
    }

    let onecli = ensure_onecli().await?;
    tokio::fs::create_dir_all(&opts.logs_dir)
        .await
        .with_context(|| format!("create {}", opts.logs_dir.display()))?;

    let sem = Arc::new(Semaphore::new(concurrency.max(1)));
    let mut joins = Vec::with_capacity(hosts.len());

    for host in hosts {
        let permit = sem.clone().acquire_owned().await?;
        let onecli = onecli.clone();
        let blueprint = blueprint.clone();
        let username = username.clone();
        let password = password.clone();
        let opts = opts.clone();
        let on_progress = on_progress.clone();

        joins.push(tokio::spawn(async move {
            let _permit = permit;
            let ip = host.ip.clone();
            let user = host.user.as_deref().unwrap_or(&username);
            let pass = host.pass.as_deref().unwrap_or(&password);

            if let Some(cb) = &on_progress {
                cb(
                    ip.clone(),
                    "running".into(),
                    None,
                    format!("Applying {}…", kind.label()),
                );
            }

            let result =
                apply_blueprint_one(&onecli, &blueprint, kind, user, pass, &ip, &opts).await;

            match result {
                Ok((serial, detail)) => {
                    if let Some(cb) = &on_progress {
                        cb(
                            ip.clone(),
                            "applied".into(),
                            Some(serial.clone()),
                            detail.clone(),
                        );
                    }
                    BlueprintHostResult {
                        ip,
                        serial,
                        outcome: "applied".into(),
                        detail,
                    }
                }
                Err(e) => {
                    let detail = format!("{e:#}");
                    if let Some(cb) = &on_progress {
                        cb(ip.clone(), "failed".into(), None, detail.clone());
                    }
                    BlueprintHostResult {
                        ip,
                        serial: String::new(),
                        outcome: "failed".into(),
                        detail,
                    }
                }
            }
        }));
    }

    let mut results = Vec::new();
    for j in joins {
        results.push(j.await.context("blueprint task join")?);
    }
    Ok(results)
}

async fn apply_blueprint_one(
    onecli: &Path,
    blueprint: &Path,
    kind: BlueprintKind,
    user: &str,
    pass: &str,
    ip: &str,
    opts: &BlueprintApplyOptions,
) -> Result<(String, String)> {
    let fallback = format!("unknown-{}", sanitize_serial(ip));
    let provisional = opts.logs_dir.join("blueprint").join(&fallback);
    tokio::fs::create_dir_all(&provisional)
        .await
        .with_context(|| format!("create {}", provisional.display()))?;

    let serial = match onecli_identify(
        user,
        pass,
        ip,
        &provisional.join("_inventory"),
        opts.never_check_trust,
    )
    .await
    {
        Ok(id) => id.serial,
        Err(_) => fallback,
    };

    let out_dir = opts.logs_dir.join("blueprint").join(&serial);
    tokio::fs::create_dir_all(&out_dir)
        .await
        .with_context(|| format!("create {}", out_dir.display()))?;

    let bmc = format!("{user}:{pass}@{ip}");
    let mut cmd = Command::new(onecli);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    match kind {
        BlueprintKind::Raid => {
            cmd.args([
                "misc",
                "raid",
                "add",
                "--bmc",
                &bmc,
                "--file",
                &blueprint.to_string_lossy(),
                "--force",
                "--quiet",
                "--output",
                &out_dir.to_string_lossy(),
            ]);
        }
        BlueprintKind::ConfigSettings => {
            cmd.args([
                "config",
                "replicate",
                "--bmc",
                &bmc,
                "--file",
                &blueprint.to_string_lossy(),
                "--quiet",
                "--output",
                &out_dir.to_string_lossy(),
            ]);
        }
        BlueprintKind::ConfigBatch => {
            cmd.args([
                "config",
                "batch",
                "--bmc",
                &bmc,
                "--file",
                &blueprint.to_string_lossy(),
                "--quiet",
                "--output",
                &out_dir.to_string_lossy(),
            ]);
        }
        BlueprintKind::FirmwareCompareXml => {
            let package_dir = opts
                .package_dir
                .as_ref()
                .context("package directory required for firmware XML")?;
            cmd.args([
                "update",
                "flash",
                "--bmc",
                &bmc,
                "--comparexml",
                &blueprint.to_string_lossy(),
                "--dir",
                &package_dir.to_string_lossy(),
                "--bundle",
                "--applytime",
                &opts.applytime,
                "--quiet",
                "--output",
                &out_dir.to_string_lossy(),
            ]);
        }
    }

    if opts.never_check_trust {
        cmd.arg("--never-check-trust");
    }

    let output = cmd
        .output()
        .await
        .with_context(|| format!("spawn {}", onecli.display()))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{stdout}\n{stderr}");

    let _ = tokio::fs::write(out_dir.join("stdout.txt"), &stdout).await;
    let _ = tokio::fs::write(out_dir.join("stderr.txt"), &stderr).await;

    if !output.status.success() {
        bail!(
            "OneCLI {} failed (exit {:?}): {}",
            kind.as_str(),
            output.status.code(),
            truncate(&combined, 900)
        );
    }

    Ok((
        serial,
        format!("{} applied via OneCLI {}", kind.label(), kind.as_str()),
    ))
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
    fn classifies_raid_ini() {
        let ini = r#"
# sample
[ctrl1-vol0]
disks=0,1
raid_level=1
vol_name=os
"#;
        assert_eq!(
            classify_blueprint_content("ini", ini).unwrap(),
            BlueprintKind::Raid
        );
    }

    #[test]
    fn classifies_batch() {
        let batch = "set IMM.LoginId.5 USERID5\nset IMM.LoginId.6 USERID6\n";
        assert_eq!(
            classify_blueprint_content("txt", batch).unwrap(),
            BlueprintKind::ConfigBatch
        );
    }

    #[test]
    fn classifies_settings() {
        let settings = "UEFI.BootMode=UEFI Mode\nIMM.HostName1=node01\n";
        assert_eq!(
            classify_blueprint_content("txt", settings).unwrap(),
            BlueprintKind::ConfigSettings
        );
    }

    #[test]
    fn classifies_compare_xml() {
        let xml = r#"<?xml version="1.0"?>
<Packages>
  <Package><Name>UEFI</Name><CompareResult>Update</CompareResult></Package>
</Packages>"#;
        assert_eq!(
            classify_blueprint_content("xml", xml).unwrap(),
            BlueprintKind::FirmwareCompareXml
        );
    }

    #[test]
    fn rejects_unrelated_xml() {
        let xml = r#"<?xml version="1.0"?><asu><item>x</item></asu>"#;
        assert!(classify_blueprint_content("xml", xml).is_err());
    }
}
