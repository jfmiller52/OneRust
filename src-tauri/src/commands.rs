//! Tauri command handlers for the OneRust GUI.

use crate::blueprint::{
    apply_blueprint_concurrent, classify_blueprint_file, plan_from_override,
    verify_blueprint_concurrent, BlueprintApplyOptions,
};
use crate::cancel::JobCancel;
use crate::catalog::{
    machine_types_from_selection, model_name_for_mt, MODELS,
};
use crate::lenovo::acquire_bundle_for_mt;
use crate::update::{parse_hosts_text, run_concurrent, HostOutcome, TargetHost, UpdateOptions};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub index: usize,
    pub name: String,
    pub machine_types: Vec<String>,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathsInfo {
    pub firmware_dir: String,
    pub logs_dir: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub machine_type: String,
    pub model_name: String,
    pub status: String,
    pub path: Option<String>,
    pub error: Option<String>,
    pub index: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostProgress {
    pub ip: String,
    pub serial: Option<String>,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostConsoleLine {
    pub ip: String,
    pub line: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostResultDto {
    pub ip: String,
    pub serial: String,
    pub outcome: String,
    pub detail: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadRequest {
    pub machine_types: Vec<String>,
    pub firmware_dir: String,
    pub offline: bool,
    #[serde(default)]
    pub force_reacquire: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRequest {
    pub hosts_text: String,
    pub username: String,
    pub password: String,
    pub concurrency: usize,
    pub firmware_dir: String,
    pub logs_dir: String,
    pub offline: bool,
    pub verify_bmc_tls: bool,
    pub machine_types: Vec<String>,
    /// Optional map of MT -> local zip path overrides
    pub bundle_overrides: Option<HashMap<String, String>>,
    /// After staging, reboot the host so OnReset firmware applies (default true).
    #[serde(default = "default_true")]
    pub reboot_after_stage: bool,
    /// ForceRestart (default) or GracefulRestart.
    #[serde(default = "default_reset_type")]
    pub reset_type: String,
    /// After reboot, run OneCLI compare and require zero remaining updates (default true).
    #[serde(default = "default_true")]
    pub verify_with_compare: bool,
}

fn default_true() -> bool {
    true
}

fn default_reset_type() -> String {
    "ForceRestart".into()
}

#[tauri::command]
pub fn list_models() -> Vec<ModelInfo> {
    MODELS
        .iter()
        .enumerate()
        .map(|(index, m)| ModelInfo {
            index,
            name: m.name.to_string(),
            machine_types: m.machine_types.iter().map(|s| (*s).to_string()).collect(),
            label: format!("{} ({})", m.name, m.machine_types.join(", ")),
        })
        .collect()
}

#[tauri::command]
pub fn resolve_machine_types(
    selected_indices: Vec<usize>,
    extra_machine_types: Vec<String>,
) -> Vec<String> {
    machine_types_from_selection(&selected_indices, &extra_machine_types)
}

#[tauri::command]
pub fn parse_hosts(hosts_text: String) -> Result<Vec<String>, String> {
    let hosts = parse_hosts_text(&hosts_text).map_err(|e| e.to_string())?;
    Ok(hosts.into_iter().map(|h| h.ip).collect())
}

#[tauri::command]
pub fn default_paths() -> PathsInfo {
    PathsInfo {
        firmware_dir: "firmware".into(),
        logs_dir: "logs".into(),
    }
}

#[tauri::command]
pub async fn ensure_onecli_ready(app: AppHandle) -> Result<String, String> {
    let _ = app.emit(
        "download-progress",
        DownloadProgress {
            machine_type: "ONECLI".into(),
            model_name: "Lenovo OneCLI".into(),
            status: "checking".into(),
            path: None,
            error: None,
            index: 0,
            total: 1,
        },
    );

    if crate::lenovo::find_onecli().await.is_none() {
        let _ = app.emit(
            "download-progress",
            DownloadProgress {
                machine_type: "ONECLI".into(),
                model_name: "Lenovo OneCLI".into(),
                status: "downloading".into(),
                path: Some(crate::lenovo::preferred_onecli_dir().display().to_string()),
                error: None,
                index: 0,
                total: 1,
            },
        );
    }

    match crate::lenovo::ensure_onecli().await {
        Ok(path) => {
            let path_str = path.display().to_string();
            let _ = app.emit(
                "download-progress",
                DownloadProgress {
                    machine_type: "ONECLI".into(),
                    model_name: "Lenovo OneCLI".into(),
                    status: "ready".into(),
                    path: Some(path_str.clone()),
                    error: None,
                    index: 0,
                    total: 1,
                },
            );
            Ok(path_str)
        }
        Err(e) => {
            let err = format!("{e:#}");
            let _ = app.emit(
                "download-progress",
                DownloadProgress {
                    machine_type: "ONECLI".into(),
                    model_name: "Lenovo OneCLI".into(),
                    status: "error".into(),
                    path: None,
                    error: Some(err.clone()),
                    index: 0,
                    total: 1,
                },
            );
            Err(err)
        }
    }
}

#[tauri::command]
pub async fn download_bundles(
    app: AppHandle,
    request: DownloadRequest,
) -> Result<HashMap<String, String>, String> {
    let firmware_dir = PathBuf::from(&request.firmware_dir);
    let total = request.machine_types.len();
    let mut map = HashMap::new();

    for (index, mt) in request.machine_types.iter().enumerate() {
        let model_name = model_name_for_mt(mt)
            .unwrap_or("Custom")
            .to_string();
        let _ = app.emit(
            "download-progress",
            DownloadProgress {
                machine_type: mt.clone(),
                model_name: model_name.clone(),
                status: "downloading".into(),
                path: None,
                error: None,
                index,
                total,
            },
        );

        match acquire_bundle_for_mt(mt, &firmware_dir, request.offline, request.force_reacquire)
            .await
        {
            Ok(path) => {
                let path_str = path.display().to_string();
                let _ = app.emit(
                    "download-progress",
                    DownloadProgress {
                        machine_type: mt.clone(),
                        model_name,
                        status: "ready".into(),
                        path: Some(path_str.clone()),
                        error: None,
                        index,
                        total,
                    },
                );
                map.insert(mt.clone(), path_str);
            }
            Err(e) => {
                let err = format!("{e:#}");
                let _ = app.emit(
                    "download-progress",
                    DownloadProgress {
                        machine_type: mt.clone(),
                        model_name,
                        status: "error".into(),
                        path: None,
                        error: Some(err.clone()),
                        index,
                        total,
                    },
                );
                return Err(format!("Failed to acquire bundle for {mt}: {err}"));
            }
        }
    }

    Ok(map)
}

#[tauri::command]
pub async fn start_updates(
    app: AppHandle,
    cancel: State<'_, JobCancel>,
    request: UpdateRequest,
) -> Result<Vec<HostResultDto>, String> {
    let hosts: Vec<TargetHost> =
        parse_hosts_text(&request.hosts_text).map_err(|e| e.to_string())?;

    if hosts.is_empty() {
        return Err("No host IPs provided".into());
    }
    if request.username.trim().is_empty() {
        return Err("Username is required".into());
    }
    if request.password.is_empty() {
        return Err("Password is required".into());
    }

    let firmware_dir = PathBuf::from(&request.firmware_dir);
    let logs_dir = PathBuf::from(&request.logs_dir);
    std::fs::create_dir_all(&logs_dir).map_err(|e| e.to_string())?;

    let mut bundles_by_mt: HashMap<String, PathBuf> = HashMap::new();

    if let Some(overrides) = &request.bundle_overrides {
        for (mt, path) in overrides {
            bundles_by_mt.insert(mt.clone(), PathBuf::from(path));
        }
    }

    for mt in &request.machine_types {
        if bundles_by_mt.contains_key(mt) {
            continue;
        }
        let path = acquire_bundle_for_mt(mt, &firmware_dir, request.offline, false)
            .await
            .map_err(|e| format!("Bundle for {mt}: {e:#}"))?;
        bundles_by_mt.insert(mt.clone(), path);
    }

    if bundles_by_mt.is_empty() {
        return Err("No firmware bundles available".into());
    }

    let cancel_flag = cancel.begin();

    for h in &hosts {
        let _ = app.emit(
            "host-progress",
            HostProgress {
                ip: h.ip.clone(),
                serial: None,
                status: "queued".into(),
                detail: "Waiting to start…".into(),
            },
        );
    }

    let never_check_trust = !request.verify_bmc_tls;
    let app_progress = app.clone();
    let on_progress: crate::update::ProgressCallback = std::sync::Arc::new(move |ip, status, serial, detail| {
        let _ = app_progress.emit(
            "host-progress",
            HostProgress {
                ip,
                serial,
                status,
                detail,
            },
        );
    });
    let app_console = app.clone();
    let on_console: crate::update::ConsoleCallback =
        std::sync::Arc::new(move |ip, line| {
            let _ = app_console.emit("host-console", HostConsoleLine { ip, line });
        });

    let results = run_concurrent(
        hosts,
        request.username,
        request.password,
        never_check_trust,
        bundles_by_mt,
        logs_dir,
        request.concurrency.max(1),
        UpdateOptions {
            reboot_after_stage: request.reboot_after_stage,
            reset_type: request.reset_type,
            verify_with_compare: request.verify_with_compare,
            ..UpdateOptions::default()
        },
        Some(on_progress),
        Some(on_console),
        Some(cancel_flag),
    )
    .await;

    let mut dtos = Vec::new();
    for (ip, outcome) in results {
        let (serial, status, detail) = match &outcome {
            HostOutcome::Verified { serial, detail } => {
                (serial.clone(), "verified".to_string(), detail.clone())
            }
            HostOutcome::Staged { serial, detail } => {
                (serial.clone(), "staged".to_string(), detail.clone())
            }
            HostOutcome::Failed { serial, reason } => {
                (serial.clone(), "failed".to_string(), reason.clone())
            }
            HostOutcome::Skipped { serial, reason } => {
                (serial.clone(), "skipped".to_string(), reason.clone())
            }
        };

        let _ = app.emit(
            "host-progress",
            HostProgress {
                ip: ip.clone(),
                serial: Some(serial.clone()),
                status: status.clone(),
                detail: detail.clone(),
            },
        );

        dtos.push(HostResultDto {
            ip,
            serial,
            outcome: status,
            detail,
        });
    }

    Ok(dtos)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlueprintInfo {
    pub path: String,
    pub kind: String,
    pub label: String,
    pub detail: String,
    pub needs_package_dir: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlueprintApplyRequest {
    /// Settings / batch / firmware XML blueprint (optional if `raid_path` is set).
    #[serde(default)]
    pub blueprint_path: Option<String>,
    /// Separate RAID policy `.ini` (optional). Applied after settings; one reboot covers both.
    #[serde(default)]
    pub raid_path: Option<String>,
    pub hosts_text: String,
    pub username: String,
    pub password: String,
    pub concurrency: usize,
    pub logs_dir: String,
    pub package_dir: Option<String>,
    pub verify_bmc_tls: bool,
    /// Optional override: raid | config-settings | config-batch | firmware-xml
    /// (combine with `+`, e.g. `settings+raid`)
    pub kind_override: Option<String>,
    #[serde(default = "default_applytime")]
    pub applytime: String,
    /// After apply, restart each host (default true).
    #[serde(default = "default_true")]
    pub reboot_after_apply: bool,
    /// ForceRestart (default) or GracefulRestart.
    #[serde(default = "default_reset_type")]
    pub reset_type: String,
    /// Seconds to wait for BMC after restart (default 5400 = 90 min).
    #[serde(default = "default_reboot_timeout_secs")]
    pub reboot_timeout_secs: u64,
}

fn optional_path(raw: Option<&String>) -> Option<PathBuf> {
    raw.map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

fn default_applytime() -> String {
    "OnReset".into()
}

fn default_reboot_timeout_secs() -> u64 {
    90 * 60
}

#[tauri::command]
pub async fn classify_blueprint(path: String) -> Result<BlueprintInfo, String> {
    let path = PathBuf::from(path);
    let info = classify_blueprint_file(&path)
        .await
        .map_err(|e| format!("{e:#}"))?;
    Ok(BlueprintInfo {
        path: path.display().to_string(),
        kind: info.plan.as_str(),
        label: info.label,
        detail: info.detail,
        needs_package_dir: info.needs_package_dir,
    })
}

fn resolve_blueprint_inputs(
    request: &BlueprintApplyRequest,
) -> Result<(Option<PathBuf>, Option<PathBuf>), String> {
    let blueprint = optional_path(request.blueprint_path.as_ref());
    let raid = optional_path(request.raid_path.as_ref());
    if blueprint.is_none() && raid.is_none() {
        return Err("Choose a settings blueprint and/or a RAID settings file".into());
    }
    if let Some(path) = &blueprint {
        if !path.is_file() {
            return Err(format!("Blueprint file not found: {}", path.display()));
        }
    }
    if let Some(path) = &raid {
        if !path.is_file() {
            return Err(format!("RAID file not found: {}", path.display()));
        }
    }
    Ok((blueprint, raid))
}

async fn resolve_blueprint_plan(
    blueprint: Option<&PathBuf>,
    kind_override: Option<&String>,
) -> Result<crate::blueprint::BlueprintPlan, String> {
    if let Some(over) = kind_override {
        return plan_from_override(over).map_err(|e| format!("{e:#}"));
    }
    if let Some(path) = blueprint {
        return Ok(classify_blueprint_file(path)
            .await
            .map_err(|e| format!("{e:#}"))?
            .plan);
    }
    Ok(crate::blueprint::BlueprintPlan::default())
}

fn blueprint_job_label(plan: &crate::blueprint::BlueprintPlan, has_raid: bool) -> String {
    let mut parts = Vec::new();
    if !plan.is_empty() {
        parts.push(plan.label());
    }
    if has_raid {
        parts.push("RAID policy".into());
    }
    if parts.is_empty() {
        "Blueprint".into()
    } else {
        parts.join(" + ")
    }
}

#[tauri::command]
pub async fn apply_blueprint(
    app: AppHandle,
    cancel: State<'_, JobCancel>,
    request: BlueprintApplyRequest,
) -> Result<Vec<HostResultDto>, String> {
    let (blueprint, raid_path) = resolve_blueprint_inputs(&request)?;
    let plan = resolve_blueprint_plan(blueprint.as_ref(), request.kind_override.as_ref()).await?;
    let label = blueprint_job_label(&plan, raid_path.is_some());

    let hosts = parse_hosts_text(&request.hosts_text).map_err(|e| e.to_string())?;
    if hosts.is_empty() {
        return Err("No host IPs provided".into());
    }
    if request.username.trim().is_empty() {
        return Err("Username is required".into());
    }
    if request.password.is_empty() {
        return Err("Password is required".into());
    }

    let cancel_flag = cancel.begin();

    for h in &hosts {
        let _ = app.emit(
            "host-progress",
            HostProgress {
                ip: h.ip.clone(),
                serial: None,
                status: "queued".into(),
                detail: format!("Waiting to apply {label}…"),
            },
        );
    }

    let app_progress = app.clone();
    let on_progress: crate::blueprint::BlueprintProgressCb =
        std::sync::Arc::new(move |ip, status, serial, detail| {
            let _ = app_progress.emit(
                "host-progress",
                HostProgress {
                    ip,
                    serial,
                    status,
                    detail,
                },
            );
        });

    let results = apply_blueprint_concurrent(
        blueprint,
        plan,
        hosts,
        request.username,
        request.password,
        request.concurrency.max(1),
        BlueprintApplyOptions {
            package_dir: request.package_dir.map(PathBuf::from),
            logs_dir: PathBuf::from(&request.logs_dir),
            never_check_trust: !request.verify_bmc_tls,
            applytime: request.applytime,
            reboot_after_apply: request.reboot_after_apply,
            reset_type: request.reset_type,
            reboot_timeout: Duration::from_secs(request.reboot_timeout_secs.max(60)),
            raid_path,
        },
        Some(on_progress),
        Some(cancel_flag),
    )
    .await
    .map_err(|e| format!("{e:#}"))?;

    let mut dtos = Vec::new();
    for r in results {
        let _ = app.emit(
            "host-progress",
            HostProgress {
                ip: r.ip.clone(),
                serial: if r.serial.is_empty() {
                    None
                } else {
                    Some(r.serial.clone())
                },
                status: r.outcome.clone(),
                detail: r.detail.clone(),
            },
        );
        dtos.push(HostResultDto {
            ip: r.ip,
            serial: r.serial,
            outcome: r.outcome,
            detail: r.detail,
        });
    }
    Ok(dtos)
}

#[tauri::command]
pub async fn verify_blueprint(
    app: AppHandle,
    cancel: State<'_, JobCancel>,
    request: BlueprintApplyRequest,
) -> Result<Vec<HostResultDto>, String> {
    let (blueprint, raid_path) = resolve_blueprint_inputs(&request)?;
    let plan = resolve_blueprint_plan(blueprint.as_ref(), request.kind_override.as_ref()).await?;
    let label = blueprint_job_label(&plan, raid_path.is_some());

    let hosts = parse_hosts_text(&request.hosts_text).map_err(|e| e.to_string())?;
    if hosts.is_empty() {
        return Err("No host IPs provided".into());
    }
    if request.username.trim().is_empty() {
        return Err("Username is required".into());
    }
    if request.password.is_empty() {
        return Err("Password is required".into());
    }

    let cancel_flag = cancel.begin();

    for h in &hosts {
        let _ = app.emit(
            "host-progress",
            HostProgress {
                ip: h.ip.clone(),
                serial: None,
                status: "queued".into(),
                detail: format!("Waiting to verify {label}…"),
            },
        );
    }

    let app_progress = app.clone();
    let on_progress: crate::blueprint::BlueprintProgressCb =
        std::sync::Arc::new(move |ip, status, serial, detail| {
            let _ = app_progress.emit(
                "host-progress",
                HostProgress {
                    ip,
                    serial,
                    status,
                    detail,
                },
            );
        });

    let results = verify_blueprint_concurrent(
        blueprint,
        plan,
        hosts,
        request.username,
        request.password,
        request.concurrency.max(1),
        BlueprintApplyOptions {
            package_dir: request.package_dir.map(PathBuf::from),
            logs_dir: PathBuf::from(&request.logs_dir),
            never_check_trust: !request.verify_bmc_tls,
            applytime: request.applytime,
            reboot_after_apply: false,
            reset_type: request.reset_type,
            reboot_timeout: Duration::from_secs(request.reboot_timeout_secs.max(60)),
            raid_path,
        },
        Some(on_progress),
        Some(cancel_flag),
    )
    .await
    .map_err(|e| format!("{e:#}"))?;

    let mut dtos = Vec::new();
    for r in results {
        let _ = app.emit(
            "host-progress",
            HostProgress {
                ip: r.ip.clone(),
                serial: if r.serial.is_empty() {
                    None
                } else {
                    Some(r.serial.clone())
                },
                status: r.outcome.clone(),
                detail: r.detail.clone(),
            },
        );
        dtos.push(HostResultDto {
            ip: r.ip,
            serial: r.serial,
            outcome: r.outcome,
            detail: r.detail,
        });
    }
    Ok(dtos)
}

#[tauri::command]
pub fn cancel_jobs(cancel: State<'_, JobCancel>) {
    cancel.request_cancel();
}

#[tauri::command]
pub async fn load_hosts_file(path: String) -> Result<String, String> {
    let hosts = crate::update::load_hosts_file(std::path::Path::new(&path))
        .await
        .map_err(|e| format!("{e:#}"))?;
    let lines: Vec<String> = hosts
        .into_iter()
        .map(|h| match (h.user, h.pass) {
            (Some(u), Some(p)) => format!("{u}:{p}@{}", h.ip),
            (Some(u), None) => format!("{u}@{}", h.ip),
            _ => h.ip,
        })
        .collect();
    Ok(lines.join("\n"))
}

#[tauri::command]
pub fn open_path(app: AppHandle, path: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;

    let raw = PathBuf::from(path.trim());
    let resolved = if raw.is_absolute() {
        raw
    } else {
        // Relative paths (e.g. "logs") resolve beside the executable, matching
        // where OneCLI and fleet logs land for installed builds.
        crate::lenovo::preferred_onecli_dir()
            .parent()
            .map(|p| p.join(&raw))
            .unwrap_or(raw)
    };
    std::fs::create_dir_all(&resolved).map_err(|e| e.to_string())?;
    app.opener()
        .open_path(resolved.display().to_string(), None::<&str>)
        .map_err(|e| e.to_string())?;
    Ok(())
}
