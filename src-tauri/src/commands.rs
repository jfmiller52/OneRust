//! Tauri command handlers for the OneRust GUI.

use crate::blueprint::{
    apply_blueprint_concurrent, classify_blueprint_file, plan_from_override,
    verify_blueprint_concurrent, BlueprintApplyOptions,
};
use crate::catalog::{
    machine_types_from_selection, model_name_for_mt, MODELS,
};
use crate::lenovo::acquire_bundle_for_mt;
use crate::update::{parse_hosts_text, run_concurrent, HostOutcome, TargetHost, UpdateOptions};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter};

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

        match acquire_bundle_for_mt(mt, &firmware_dir, request.offline).await {
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
        let path = acquire_bundle_for_mt(mt, &firmware_dir, request.offline)
            .await
            .map_err(|e| format!("Bundle for {mt}: {e:#}"))?;
        bundles_by_mt.insert(mt.clone(), path);
    }

    if bundles_by_mt.is_empty() {
        return Err("No firmware bundles available".into());
    }

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
    pub blueprint_path: String,
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
}

fn default_applytime() -> String {
    "OnReset".into()
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

#[tauri::command]
pub async fn apply_blueprint(
    app: AppHandle,
    request: BlueprintApplyRequest,
) -> Result<Vec<HostResultDto>, String> {
    let path = PathBuf::from(&request.blueprint_path);
    if !path.is_file() {
        return Err(format!("Blueprint file not found: {}", path.display()));
    }

    let plan = if let Some(over) = &request.kind_override {
        plan_from_override(over).map_err(|e| format!("{e:#}"))?
    } else {
        classify_blueprint_file(&path)
            .await
            .map_err(|e| format!("{e:#}"))?
            .plan
    };

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

    for h in &hosts {
        let _ = app.emit(
            "host-progress",
            HostProgress {
                ip: h.ip.clone(),
                serial: None,
                status: "queued".into(),
                detail: format!("Waiting to apply {}…", plan.label()),
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
        path,
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
        },
        Some(on_progress),
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
    request: BlueprintApplyRequest,
) -> Result<Vec<HostResultDto>, String> {
    let path = PathBuf::from(&request.blueprint_path);
    if !path.is_file() {
        return Err(format!("Blueprint file not found: {}", path.display()));
    }

    let plan = if let Some(over) = &request.kind_override {
        plan_from_override(over).map_err(|e| format!("{e:#}"))?
    } else {
        classify_blueprint_file(&path)
            .await
            .map_err(|e| format!("{e:#}"))?
            .plan
    };

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

    for h in &hosts {
        let _ = app.emit(
            "host-progress",
            HostProgress {
                ip: h.ip.clone(),
                serial: None,
                status: "queued".into(),
                detail: format!("Waiting to verify {}…", plan.label()),
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
        path,
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
        },
        Some(on_progress),
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
