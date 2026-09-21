//! Apply OneCLI XML/INI "blueprints" (RAID, BMC/UEFI settings, firmware compare).
//!
//! A single `.ini` / `.txt` file may contain both UEFI/BMC settings and RAID policy;
//! OneRust splits the parts and runs the matching OneCLI commands in order.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::Semaphore;

use crate::lenovo::{ensure_onecli, onecli_compare, onecli_identify, onecli_power_restart};
use crate::logutil::sanitize_serial;
use crate::update::TargetHost;

/// Which OneCLI actions a blueprint file should trigger (can be combined).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BlueprintPlan {
    pub raid: bool,
    pub config_settings: bool,
    pub config_batch: bool,
    pub firmware_xml: bool,
}

impl BlueprintPlan {
    pub fn as_str(&self) -> String {
        let mut parts = Vec::new();
        if self.firmware_xml {
            parts.push("firmware-xml");
        }
        if self.config_settings {
            parts.push("config-settings");
        }
        if self.config_batch {
            parts.push("config-batch");
        }
        if self.raid {
            parts.push("raid");
        }
        if parts.is_empty() {
            "unknown".into()
        } else {
            parts.join("+")
        }
    }

    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        if self.firmware_xml {
            parts.push("Firmware compare (XML)");
        }
        if self.config_settings {
            parts.push("BMC/UEFI settings");
        }
        if self.config_batch {
            parts.push("Config batch");
        }
        if self.raid {
            parts.push("RAID policy");
        }
        if parts.is_empty() {
            "Blueprint".into()
        } else {
            parts.join(" + ")
        }
    }

    pub fn needs_package_dir(&self) -> bool {
        self.firmware_xml
    }

    pub fn is_empty(&self) -> bool {
        !self.raid && !self.config_settings && !self.config_batch && !self.firmware_xml
    }
}

#[derive(Debug, Clone)]
pub struct BlueprintClassify {
    pub plan: BlueprintPlan,
    pub label: String,
    pub detail: String,
    pub needs_package_dir: bool,
}

/// Read and classify a blueprint path (RAID / settings / batch may combine).
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
    let plan = classify_blueprint_content(&ext, &content)?;
    Ok(BlueprintClassify {
        label: plan.label(),
        detail: classify_detail(&plan, &content),
        needs_package_dir: plan.needs_package_dir(),
        plan,
    })
}

fn classify_detail(plan: &BlueprintPlan, content: &str) -> String {
    let parts = split_blueprint_parts(content);
    let mut bits = Vec::new();
    if plan.config_settings {
        let n = parts
            .settings
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count();
        bits.push(format!("{n} setting line(s)"));
    }
    if plan.config_batch {
        let sets = parts
            .batch
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count();
        bits.push(format!("{sets} set command(s)"));
    }
    if plan.raid {
        let vols = parts
            .raid
            .lines()
            .filter(|l| {
                let t = l.trim().to_ascii_lowercase();
                t.starts_with('[') && t.contains("ctrl") && t.contains("vol")
            })
            .count();
        if vols > 0 {
            bits.push(format!("{vols} RAID volume section(s)"));
        } else {
            bits.push("RAID sections".into());
        }
    }
    if plan.firmware_xml {
        let pkgs = content.to_ascii_lowercase().matches("<package").count();
        if pkgs > 0 {
            bits.push(format!("{pkgs} <Package> node(s)"));
        } else {
            bits.push("Firmware compare XML".into());
        }
    }
    if bits.is_empty() {
        "Blueprint".into()
    } else {
        bits.join(" · ")
    }
}

/// Classify blueprint contents. Settings + RAID in one file is supported.
pub fn classify_blueprint_content(ext: &str, content: &str) -> Result<BlueprintPlan> {
    let lower = content.to_ascii_lowercase();
    let trimmed = content.trim_start();

    if ext == "xml" || trimmed.starts_with("<?xml") || trimmed.starts_with('<') {
        if lower.contains("<package")
            || lower.contains("compareresult")
            || lower.contains("onecli-update")
            || lower.contains("updatecompare")
            || lower.contains("<packages")
        {
            return Ok(BlueprintPlan {
                firmware_xml: true,
                ..BlueprintPlan::default()
            });
        }
        bail!(
            "XML blueprint is not a recognized OneCLI update-compare file. \
             Export compare XML via OneCLI update compare, or use an INI/TXT settings+RAID file."
        );
    }

    let parts = split_blueprint_parts(content);
    let mut plan = BlueprintPlan {
        raid: !parts.raid.trim().is_empty()
            || (ext == "ini" && lower.contains("raid_level")),
        config_settings: !parts.settings.trim().is_empty(),
        config_batch: !parts.batch.trim().is_empty(),
        firmware_xml: false,
    };

    // Comment-only .ini with raid_level mentioned in comments still maps to RAID intent.
    if plan.is_empty() && ext == "ini" {
        plan.raid = true;
    }

    if plan.is_empty() {
        bail!("Could not classify blueprint contents as RAID, settings, batch, or firmware XML");
    }
    Ok(plan)
}

#[derive(Debug, Default)]
struct BlueprintParts {
    settings: String,
    batch: String,
    raid: String,
}

/// Split a unified blueprint into OneCLI-ready fragments.
fn split_blueprint_parts(content: &str) -> BlueprintParts {
    let mut parts = BlueprintParts::default();
    let mut in_raid = false;

    for line in content.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();

        if trimmed.starts_with('[') {
            in_raid = is_raid_section_header(trimmed);
            if in_raid {
                parts.raid.push_str(line);
                parts.raid.push('\n');
            }
            continue;
        }

        if in_raid {
            parts.raid.push_str(line);
            parts.raid.push('\n');
            continue;
        }

        if lower.starts_with("set ") {
            parts.batch.push_str(line);
            parts.batch.push('\n');
            continue;
        }

        if is_settings_line(trimmed) {
            parts.settings.push_str(line);
            parts.settings.push('\n');
        }
    }

    // Also catch loose raid_level keys if file has no [ctrl] header but is RAID-ish
    if parts.raid.trim().is_empty() {
        let has_raid_keys = content.lines().any(|l| {
            let t = l.trim().to_ascii_lowercase();
            t.starts_with("raid_level=") || t.starts_with("disks=") || t.starts_with("vol_name=")
        });
        if has_raid_keys {
            for line in content.lines() {
                let t = line.trim().to_ascii_lowercase();
                if t.starts_with('#') || t.is_empty() {
                    continue;
                }
                if t.starts_with("set ")
                    || (is_settings_line(line.trim()) && looks_like_uefi_setting(line.trim()))
                {
                    continue;
                }
                if t.contains('=')
                    && (t.starts_with("raid_level")
                        || t.starts_with("disks")
                        || t.starts_with("vol_name")
                        || t.starts_with("hot_spares")
                        || t.starts_with("strip_size")
                        || t.starts_with("write_policy")
                        || t.starts_with("read_policy")
                        || t.starts_with("volume_size")
                        || t.starts_with("global_hot"))
                {
                    parts.raid.push_str(line);
                    parts.raid.push('\n');
                }
            }
        }
    }

    parts
}

fn is_raid_section_header(trimmed: &str) -> bool {
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("[ctrl")
        || lower.starts_with("[broadcom")
        || lower.starts_with("[microchip")
        || lower.starts_with("[marvell")
}

fn is_settings_line(trimmed: &str) -> bool {
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
        return false;
    }
    if trimmed.starts_with('[') {
        return false;
    }
    if trimmed.to_ascii_lowercase().starts_with("set ") {
        return false;
    }
    if !trimmed.contains('=') {
        return false;
    }
    // Exclude RAID policy keys when they appear outside sections (edge case)
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("raid_level=")
        || lower.starts_with("disks=")
        || lower.starts_with("vol_name=")
        || lower.starts_with("hot_spares=")
        || lower.starts_with("strip_size=")
        || lower.starts_with("write_policy=")
        || lower.starts_with("read_policy=")
        || lower.starts_with("io_policy=")
        || lower.starts_with("access_policy=")
        || lower.starts_with("cache_policy=")
        || lower.starts_with("volume_size=")
        || lower.starts_with("global_hot_spares=")
    {
        return false;
    }
    true
}

fn looks_like_uefi_setting(trimmed: &str) -> bool {
    let name = trimmed.split('=').next().unwrap_or("").trim();
    name.contains('.') || name.chars().any(|c| c.is_ascii_uppercase())
}

#[derive(Debug, Clone)]
pub struct BlueprintApplyOptions {
    pub package_dir: Option<PathBuf>,
    pub logs_dir: PathBuf,
    pub never_check_trust: bool,
    pub applytime: String,
    /// After a successful apply, restart the host (default true).
    pub reboot_after_apply: bool,
    /// ForceRestart (default) or GracefulRestart.
    pub reset_type: String,
}

impl Default for BlueprintApplyOptions {
    fn default() -> Self {
        Self {
            package_dir: None,
            logs_dir: PathBuf::from("logs"),
            never_check_trust: true,
            applytime: "OnReset".into(),
            reboot_after_apply: true,
            reset_type: "ForceRestart".into(),
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
    plan: BlueprintPlan,
    hosts: Vec<TargetHost>,
    username: String,
    password: String,
    concurrency: usize,
    opts: BlueprintApplyOptions,
    on_progress: Option<BlueprintProgressCb>,
) -> Result<Vec<BlueprintHostResult>> {
    if plan.firmware_xml {
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
        let plan = plan.clone();
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
                    format!("Applying {}…", plan.label()),
                );
            }

            let result = apply_blueprint_one(
                &onecli,
                &blueprint,
                &plan,
                user,
                pass,
                &ip,
                &opts,
                on_progress.as_ref(),
            )
            .await;

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
                Err((serial, e)) => {
                    let detail = format!("{e:#}");
                    if let Some(cb) = &on_progress {
                        cb(
                            ip.clone(),
                            "failed".into(),
                            if serial.is_empty() {
                                None
                            } else {
                                Some(serial.clone())
                            },
                            detail.clone(),
                        );
                    }
                    BlueprintHostResult {
                        ip,
                        serial,
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
    plan: &BlueprintPlan,
    user: &str,
    pass: &str,
    ip: &str,
    opts: &BlueprintApplyOptions,
    on_progress: Option<&BlueprintProgressCb>,
) -> Result<(String, String), (String, anyhow::Error)> {
    let fallback = format!("unknown-{}", sanitize_serial(ip));
    let provisional = opts.logs_dir.join("blueprint").join(&fallback);
    tokio::fs::create_dir_all(&provisional)
        .await
        .map_err(|e| {
            (
                String::new(),
                anyhow::Error::new(e).context(format!("create {}", provisional.display())),
            )
        })?;

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

    let map_err = |e: anyhow::Error| (serial.clone(), e);

    let out_dir = opts.logs_dir.join("blueprint").join(&serial);
    tokio::fs::create_dir_all(&out_dir)
        .await
        .map_err(|e| {
            map_err(anyhow::Error::new(e).context(format!("create {}", out_dir.display())))
        })?;

    let content = tokio::fs::read_to_string(blueprint)
        .await
        .map_err(|e| {
            map_err(anyhow::Error::new(e).context(format!("read {}", blueprint.display())))
        })?;
    let parts = split_blueprint_parts(&content);
    let parts_dir = out_dir.join("_parts");
    tokio::fs::create_dir_all(&parts_dir)
        .await
        .map_err(|e| {
            map_err(anyhow::Error::new(e).context(format!("create {}", parts_dir.display())))
        })?;

    let bmc = format!("{user}:{pass}@{ip}");
    let mut done = Vec::new();

    // Order: settings → batch → RAID → firmware XML
    if plan.config_settings {
        let path = parts_dir.join("settings.txt");
        let body = if parts.settings.trim().is_empty() {
            // Fallback: use whole file if classification said settings-only
            content.clone()
        } else {
            parts.settings.clone()
        };
        tokio::fs::write(&path, &body)
            .await
            .map_err(|e| {
                map_err(anyhow::Error::new(e).context(format!("write {}", path.display())))
            })?;
        run_onecli(
            onecli,
            &[
                "config",
                "replicate",
                "--bmc",
                &bmc,
                "--file",
                &path.to_string_lossy(),
                "--quiet",
                "--output",
                &out_dir.to_string_lossy(),
            ],
            opts.never_check_trust,
            &out_dir,
            "config-replicate",
        )
        .await
        .map_err(map_err)?;
        done.push("config replicate");
    }

    if plan.config_batch {
        let path = parts_dir.join("batch.txt");
        let body = if parts.batch.trim().is_empty() {
            content.clone()
        } else {
            parts.batch.clone()
        };
        tokio::fs::write(&path, &body)
            .await
            .map_err(|e| {
                map_err(anyhow::Error::new(e).context(format!("write {}", path.display())))
            })?;
        run_onecli(
            onecli,
            &[
                "config",
                "batch",
                "--bmc",
                &bmc,
                "--file",
                &path.to_string_lossy(),
                "--quiet",
                "--output",
                &out_dir.to_string_lossy(),
            ],
            opts.never_check_trust,
            &out_dir,
            "config-batch",
        )
        .await
        .map_err(map_err)?;
        done.push("config batch");
    }

    if plan.raid {
        let path = parts_dir.join("raid.ini");
        let body = if parts.raid.trim().is_empty() {
            content.clone()
        } else {
            parts.raid.clone()
        };
        tokio::fs::write(&path, &body)
            .await
            .map_err(|e| {
                map_err(anyhow::Error::new(e).context(format!("write {}", path.display())))
            })?;
        run_onecli(
            onecli,
            &[
                "misc",
                "raid",
                "add",
                "--bmc",
                &bmc,
                "--file",
                &path.to_string_lossy(),
                "--force",
                "--quiet",
                "--output",
                &out_dir.to_string_lossy(),
            ],
            opts.never_check_trust,
            &out_dir,
            "raid-add",
        )
        .await
        .map_err(map_err)?;
        done.push("raid add");
    }

    if plan.firmware_xml {
        let package_dir = opts
            .package_dir
            .as_ref()
            .ok_or_else(|| {
                map_err(anyhow::anyhow!("package directory required for firmware XML"))
            })?;
        run_onecli(
            onecli,
            &[
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
            ],
            opts.never_check_trust,
            &out_dir,
            "flash-comparexml",
        )
        .await
        .map_err(map_err)?;
        done.push("flash --comparexml");
    }

    if opts.reboot_after_apply {
        if let Some(cb) = on_progress {
            cb(
                ip.to_string(),
                "rebooting".into(),
                Some(serial.clone()),
                format!("OneCLI misc power ({})…", opts.reset_type),
            );
        }
        let power_dir = opts.logs_dir.join("power").join(&serial);
        onecli_power_restart(
            user,
            pass,
            ip,
            &opts.reset_type,
            &power_dir,
            opts.never_check_trust,
        )
        .await
        .map_err(|e| {
            map_err(
                e.context(format!(
                    "applied OK but restart failed ({})",
                    opts.reset_type
                )),
            )
        })?;
        done.push("restart");
    }

    Ok((
        serial,
        format!("{} applied ({})", plan.label(), done.join(", ")),
    ))
}

async fn run_onecli(
    onecli: &Path,
    args: &[&str],
    never_check_trust: bool,
    out_dir: &Path,
    log_stem: &str,
) -> Result<()> {
    let mut cmd = Command::new(onecli);
    cmd.args(args)
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
    let _ = tokio::fs::write(out_dir.join(format!("{log_stem}-stdout.txt")), &stdout).await;
    let _ = tokio::fs::write(out_dir.join(format!("{log_stem}-stderr.txt")), &stderr).await;

    if !output.status.success() {
        bail!(
            "OneCLI {} failed (exit {:?}): {}",
            log_stem,
            output.status.code(),
            truncate(&format!("{stdout}\n{stderr}"), 900)
        );
    }
    Ok(())
}

/// Build a plan from a kind-override string (single action or `a+b` form).
pub fn plan_from_override(s: &str) -> Result<BlueprintPlan> {
    let mut plan = BlueprintPlan::default();
    for part in s.split(|c| c == '+' || c == ',' || c == '|') {
        match part.trim().to_ascii_lowercase().as_str() {
            "" => {}
            "raid" => plan.raid = true,
            "config-settings" | "settings" | "replicate" => plan.config_settings = true,
            "config-batch" | "batch" => plan.config_batch = true,
            "firmware-xml" | "firmware" | "xml" => plan.firmware_xml = true,
            other => bail!("Unknown blueprint kind '{other}'"),
        }
    }
    if plan.is_empty() {
        bail!("Empty blueprint kind override");
    }
    Ok(plan)
}

/// Verify a blueprint against many hosts (read-only OneCLI checks).
pub async fn verify_blueprint_concurrent(
    blueprint: PathBuf,
    plan: BlueprintPlan,
    hosts: Vec<TargetHost>,
    username: String,
    password: String,
    concurrency: usize,
    opts: BlueprintApplyOptions,
    on_progress: Option<BlueprintProgressCb>,
) -> Result<Vec<BlueprintHostResult>> {
    if plan.firmware_xml {
        let dir = opts
            .package_dir
            .as_ref()
            .context("Firmware XML blueprints require a package directory for verify")?;
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
        let plan = plan.clone();
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
                    "verifying".into(),
                    None,
                    format!("Verifying {}…", plan.label()),
                );
            }

            let result =
                verify_blueprint_one(&onecli, &blueprint, &plan, user, pass, &ip, &opts).await;

            match result {
                Ok((serial, outcome, detail)) => {
                    if let Some(cb) = &on_progress {
                        cb(
                            ip.clone(),
                            outcome.clone(),
                            Some(serial.clone()),
                            detail.clone(),
                        );
                    }
                    BlueprintHostResult {
                        ip,
                        serial,
                        outcome,
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
        results.push(j.await.context("blueprint verify task join")?);
    }
    Ok(results)
}

async fn verify_blueprint_one(
    onecli: &Path,
    blueprint: &Path,
    plan: &BlueprintPlan,
    user: &str,
    pass: &str,
    ip: &str,
    opts: &BlueprintApplyOptions,
) -> Result<(String, String, String)> {
    let fallback = format!("unknown-{}", sanitize_serial(ip));
    let provisional = opts.logs_dir.join("blueprint").join(&fallback).join("verify");
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

    let out_dir = opts.logs_dir.join("blueprint").join(&serial).join("verify");
    tokio::fs::create_dir_all(&out_dir)
        .await
        .with_context(|| format!("create {}", out_dir.display()))?;

    let content = tokio::fs::read_to_string(blueprint)
        .await
        .with_context(|| format!("read {}", blueprint.display()))?;
    let parts = split_blueprint_parts(&content);
    let parts_dir = out_dir.join("_parts");
    tokio::fs::create_dir_all(&parts_dir).await?;

    let bmc = format!("{user}:{pass}@{ip}");
    let mut ok_bits = Vec::new();
    let mut bad_bits = Vec::new();

    // Settings + batch (as Setting=Value) via config compare --file
    let mut compare_body = String::new();
    if plan.config_settings && !parts.settings.trim().is_empty() {
        compare_body.push_str(&parts.settings);
        compare_body.push('\n');
    } else if plan.config_settings {
        // settings-only file without successful split
        for line in content.lines() {
            if is_settings_line(line.trim()) {
                compare_body.push_str(line);
                compare_body.push('\n');
            }
        }
    }
    if plan.config_batch {
        compare_body.push_str(&batch_lines_to_settings(&parts.batch));
    }

    if !compare_body.trim().is_empty() {
        let path = parts_dir.join("compare-settings.txt");
        tokio::fs::write(&path, &compare_body).await?;
        match verify_config_compare(
            onecli,
            &bmc,
            &path,
            &out_dir,
            opts.never_check_trust,
        )
        .await
        {
            Ok(()) => ok_bits.push("settings match".into()),
            Err(e) => bad_bits.push(format!("settings: {e:#}")),
        }
    }

    if plan.raid {
        let raid_body = if parts.raid.trim().is_empty() {
            content.clone()
        } else {
            parts.raid.clone()
        };
        match verify_raid_policy(onecli, &bmc, &raid_body, &out_dir, opts.never_check_trust)
            .await
        {
            Ok(msg) => ok_bits.push(msg),
            Err(e) => bad_bits.push(format!("raid: {e:#}")),
        }
    }

    if plan.firmware_xml {
        let package_dir = opts
            .package_dir
            .as_ref()
            .context("package directory required for firmware verify")?;
        match onecli_compare(
            user,
            pass,
            ip,
            package_dir,
            &out_dir.join("compare"),
            opts.never_check_trust,
        )
        .await
        {
            Ok(cmp) if cmp.packages_needed == 0 => {
                ok_bits.push("firmware up to date".into());
            }
            Ok(cmp) => {
                bad_bits.push(format!(
                    "firmware: {} package(s) still recommended",
                    cmp.packages_needed
                ));
            }
            Err(e) => bad_bits.push(format!("firmware: {e:#}")),
        }
    }

    if ok_bits.is_empty() && bad_bits.is_empty() {
        bail!("Nothing to verify in this blueprint");
    }

    if bad_bits.is_empty() {
        Ok((
            serial,
            "verified".into(),
            format!("Verified — {}", ok_bits.join("; ")),
        ))
    } else if ok_bits.is_empty() {
        Ok((
            serial,
            "mismatch".into(),
            format!("Mismatch — {}", bad_bits.join("; ")),
        ))
    } else {
        Ok((
            serial,
            "mismatch".into(),
            format!(
                "Partial — ok: {}; bad: {}",
                ok_bits.join("; "),
                bad_bits.join("; ")
            ),
        ))
    }
}

async fn verify_config_compare(
    onecli: &Path,
    bmc: &str,
    settings_file: &Path,
    out_dir: &Path,
    never_check_trust: bool,
) -> Result<()> {
    let mut cmd = Command::new(onecli);
    cmd.args([
        "config",
        "compare",
        "--bmc",
        bmc,
        "--file",
        &settings_file.to_string_lossy(),
        "--quiet",
        "--output",
        &out_dir.to_string_lossy(),
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
    let _ = tokio::fs::write(out_dir.join("config-compare-stdout.txt"), &stdout).await;
    let _ = tokio::fs::write(out_dir.join("config-compare-stderr.txt"), &stderr).await;

    // Also scrape any compare result files OneCLI dropped
    let mut blob = combined.clone();
    if let Ok(extra) = collect_dir_text(out_dir).await {
        blob.push_str(&extra);
    }

    if config_compare_reports_mismatch(&blob) {
        bail!("{}", truncate(&blob, 500));
    }
    if !output.status.success() {
        // Non-zero without clear mismatch text — still treat as failure
        bail!(
            "exit {:?} — {}",
            output.status.code(),
            truncate(&combined, 400)
        );
    }
    Ok(())
}

fn config_compare_reports_mismatch(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    const MARKERS: &[&str] = &[
        "not match",
        "does not match",
        "don't match",
        "mismatch",
        "different",
        "not equal",
        "incorrect",
        "unexpected value",
    ];
    // "compare result: same" / "identical" / "match" alone are OK
    if lower.contains("no difference")
        || lower.contains("all match")
        || lower.contains("identical")
        || lower.contains("matches the file")
    {
        return false;
    }
    MARKERS.iter().any(|m| lower.contains(m))
}

async fn verify_raid_policy(
    onecli: &Path,
    bmc: &str,
    raid_body: &str,
    out_dir: &Path,
    never_check_trust: bool,
) -> Result<String> {
    let expected = raid_expectations(raid_body);
    if expected.is_empty() {
        bail!("no RAID expectations found in blueprint (need raid_level / vol_name)");
    }

    let mut cmd = Command::new(onecli);
    cmd.args([
        "misc",
        "raid",
        "show",
        "--bmc",
        bmc,
        "--quiet",
        "--output",
        &out_dir.to_string_lossy(),
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
    let mut blob = format!("{stdout}\n{stderr}\n");
    if let Ok(extra) = collect_dir_text(out_dir).await {
        blob.push_str(&extra);
    }
    let _ = tokio::fs::write(out_dir.join("raid-show-stdout.txt"), &stdout).await;
    let _ = tokio::fs::write(out_dir.join("raid-show-stderr.txt"), &stderr).await;

    if !output.status.success() && blob.trim().is_empty() {
        bail!(
            "raid show failed (exit {:?}): {}",
            output.status.code(),
            truncate(&blob, 400)
        );
    }

    let lower = blob.to_ascii_lowercase();
    let mut missing = Vec::new();
    for exp in &expected {
        // Single token (vol_name): must appear. Multi-token (raid level forms): any.
        let ok = if exp.tokens.len() == 1 {
            lower.contains(&exp.tokens[0].to_ascii_lowercase())
        } else {
            exp.tokens
                .iter()
                .any(|t| lower.contains(&t.to_ascii_lowercase()))
        };
        if !ok {
            missing.push(exp.label.clone());
        }
    }

    if missing.is_empty() {
        Ok(format!("RAID matches ({} volume check(s))", expected.len()))
    } else {
        bail!("missing/unmatched: {}", missing.join(", "));
    }
}

#[derive(Debug)]
struct RaidExpectation {
    label: String,
    tokens: Vec<String>,
}

fn raid_expectations(raid_body: &str) -> Vec<RaidExpectation> {
    let mut out = Vec::new();
    let mut section: Option<String> = None;
    let mut raid_level: Option<String> = None;
    let mut vol_name: Option<String> = None;

    let flush = |section: &Option<String>,
                 level: &Option<String>,
                 vol: &Option<String>,
                 out: &mut Vec<RaidExpectation>| {
        if let Some(v) = vol {
            out.push(RaidExpectation {
                label: v.clone(),
                tokens: vec![v.clone()],
            });
            return;
        }
        if let Some(l) = level {
            out.push(RaidExpectation {
                label: section
                    .clone()
                    .unwrap_or_else(|| format!("raid{l}")),
                // Any of these forms in raid show output is enough (checked as OR).
                tokens: vec![
                    format!("raid {l}"),
                    format!("raid{l}"),
                    format!("raid-{l}"),
                ],
            });
        }
    };

    for line in raid_body.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with('[') {
            flush(&section, &raid_level, &vol_name, &mut out);
            section = Some(trimmed.trim_matches(|c| c == '[' || c == ']').to_string());
            raid_level = None;
            vol_name = None;
            continue;
        }
        if let Some(rest) = lower.strip_prefix("raid_level=") {
            raid_level = Some(rest.trim().to_string());
        } else if let Some(rest) = lower.strip_prefix("vol_name=") {
            vol_name = Some(rest.trim().to_string());
        }
    }
    flush(&section, &raid_level, &vol_name, &mut out);
    out
}

fn batch_lines_to_settings(batch: &str) -> String {
    let mut out = String::new();
    for line in batch.lines() {
        let t = line.trim();
        if !t.to_ascii_lowercase().starts_with("set ") {
            continue;
        }
        let rest = t[4..].trim();
        // set SettingName value…  → SettingName=value…
        if let Some((name, value)) = rest.split_once(char::is_whitespace) {
            out.push_str(name.trim());
            out.push('=');
            out.push_str(value.trim());
            out.push('\n');
        }
    }
    out
}

async fn collect_dir_text(dir: &Path) -> Result<String> {
    let mut out = String::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&current).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_dir() {
                // Don't recurse into nested verify loops endlessly — one level of files is enough
                if path.file_name().and_then(|n| n.to_str()) == Some("_parts") {
                    continue;
                }
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
                || name.ends_with(".log")
                || name.ends_with(".html")
            {
                if let Ok(text) = tokio::fs::read_to_string(&path).await {
                    out.push_str(&text);
                    out.push('\n');
                }
            }
        }
    }
    Ok(out)
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
        let plan = classify_blueprint_content("ini", ini).unwrap();
        assert!(plan.raid);
        assert!(!plan.config_settings);
    }

    #[test]
    fn classifies_batch() {
        let batch = "set IMM.LoginId.5 USERID5\nset IMM.LoginId.6 USERID6\n";
        let plan = classify_blueprint_content("txt", batch).unwrap();
        assert!(plan.config_batch);
        assert!(!plan.raid);
    }

    #[test]
    fn classifies_settings() {
        let settings = "UEFI.BootMode=UEFI Mode\nIMM.HostName1=node01\n";
        let plan = classify_blueprint_content("txt", settings).unwrap();
        assert!(plan.config_settings);
        assert!(!plan.raid);
    }

    #[test]
    fn classifies_combined_settings_and_raid() {
        let file = r#"
# BMC / UEFI
UEFI.BootMode=UEFI Mode
IMM.HostName1=node01

# RAID
[ctrl1-vol0]
disks=0,1
raid_level=1
vol_name=os
"#;
        let plan = classify_blueprint_content("ini", file).unwrap();
        assert!(plan.config_settings);
        assert!(plan.raid);
        assert!(!plan.config_batch);

        let parts = split_blueprint_parts(file);
        assert!(parts.settings.contains("UEFI.BootMode"));
        assert!(parts.raid.contains("[ctrl1-vol0]"));
        assert!(parts.raid.contains("raid_level=1"));
        assert!(!parts.raid.contains("UEFI.BootMode"));
        assert!(!parts.settings.contains("raid_level"));
    }

    #[test]
    fn classifies_compare_xml() {
        let xml = r#"<?xml version="1.0"?>
<Packages>
  <Package><Name>UEFI</Name><CompareResult>Update</CompareResult></Package>
</Packages>"#;
        let plan = classify_blueprint_content("xml", xml).unwrap();
        assert!(plan.firmware_xml);
    }

    #[test]
    fn rejects_unrelated_xml() {
        let xml = r#"<?xml version="1.0"?><asu><item>x</item></asu>"#;
        assert!(classify_blueprint_content("xml", xml).is_err());
    }

    #[test]
    fn batch_to_settings_conversion() {
        let batch = "set IMM.LoginId.5 USERID5\nset UEFI.BootMode UEFI Mode\n";
        let settings = batch_lines_to_settings(batch);
        assert!(settings.contains("IMM.LoginId.5=USERID5"));
        assert!(settings.contains("UEFI.BootMode=UEFI Mode"));
    }

    #[test]
    fn raid_expectations_from_vol_name() {
        let raid = "[ctrl1-vol0]\nraid_level=1\nvol_name=os\n";
        let exp = raid_expectations(raid);
        assert_eq!(exp.len(), 1);
        assert_eq!(exp[0].label, "os");
    }

    #[test]
    fn plan_override_combined() {
        let plan = plan_from_override("settings+raid").unwrap();
        assert!(plan.config_settings && plan.raid);
    }
}
