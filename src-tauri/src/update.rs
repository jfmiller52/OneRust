//! Per-host firmware update workflow.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

use crate::lenovo::onecli_compare;
use crate::logutil::{log_path_for, HostLogger};
use crate::redfish::{RedfishClient, StageResult};

/// Target host specification (optional credential override).
#[derive(Debug, Clone)]
pub struct TargetHost {
    pub ip: String,
    pub user: Option<String>,
    pub pass: Option<String>,
}

/// Options for reboot + post-update verification.
#[derive(Debug, Clone)]
pub struct UpdateOptions {
    /// Issue Redfish ComputerSystem.Reset after staging (default true).
    pub reboot_after_stage: bool,
    /// Reset type: ForceRestart (default) or GracefulRestart.
    pub reset_type: String,
    /// Run OneCLI compare after reboot to confirm no updates remain (default true).
    pub verify_with_compare: bool,
    /// How long to wait for the host to come back after reboot.
    pub reboot_timeout: Duration,
}

impl Default for UpdateOptions {
    fn default() -> Self {
        Self {
            reboot_after_stage: true,
            reset_type: "ForceRestart".into(),
            verify_with_compare: true,
            reboot_timeout: Duration::from_secs(90 * 60),
        }
    }
}

/// Parse a host token: `ip`, `user@ip`, or `user:pass@ip`.
pub fn parse_host_token(token: &str) -> Result<TargetHost> {
    let token = token.trim();
    if token.is_empty() {
        bail!("empty host token");
    }
    if let Some((creds, ip)) = token.rsplit_once('@') {
        if let Some((user, pass)) = creds.split_once(':') {
            Ok(TargetHost {
                ip: ip.to_string(),
                user: Some(user.to_string()),
                pass: Some(pass.to_string()),
            })
        } else {
            Ok(TargetHost {
                ip: ip.to_string(),
                user: Some(creds.to_string()),
                pass: None,
            })
        }
    } else {
        Ok(TargetHost {
            ip: token.to_string(),
            user: None,
            pass: None,
        })
    }
}

/// Parse mixed IP list text (comma / whitespace / newline separated).
pub fn parse_hosts_text(text: &str) -> Result<Vec<TargetHost>> {
    let mut hosts = Vec::new();
    for part in text.split(|c: char| c == ',' || c.is_whitespace()) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        hosts.push(parse_host_token(part)?);
    }
    if hosts.is_empty() {
        bail!("no host IPs provided");
    }
    Ok(hosts)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostOutcome {
    /// Staged, rebooted, and OneCLI compare reports no remaining updates.
    Verified { serial: String, detail: String },
    /// Staged only (reboot/verify disabled or skipped).
    Staged { serial: String, detail: String },
    Failed { serial: String, reason: String },
    Skipped { serial: String, reason: String },
}

fn package_dir_for_bundle(bundle: &Path) -> PathBuf {
    let looks_like_file = bundle.is_file()
        || bundle
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| {
                e.eq_ignore_ascii_case("zip")
                    || e.eq_ignore_ascii_case("uxz")
                    || e.eq_ignore_ascii_case("exe")
            });
    if looks_like_file {
        bundle
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    } else {
        bundle.to_path_buf()
    }
}

/// Run update for one BMC: stage → reboot → verify via OneCLI compare.
pub async fn update_one_host(
    host: &TargetHost,
    default_user: &str,
    default_pass: &str,
    insecure_tls: bool,
    bundles_by_mt: &HashMap<String, PathBuf>,
    logs_dir: &Path,
    options: &UpdateOptions,
    on_progress: Option<&ProgressCallback>,
) -> HostOutcome {
    let notify = |status: &str, serial: Option<&str>, detail: &str| {
        if let Some(cb) = on_progress {
            cb(
                host.ip.clone(),
                status.to_string(),
                serial.map(str::to_string),
                detail.to_string(),
            );
        }
    };

    let user = host.user.as_deref().unwrap_or(default_user);
    let pass = host.pass.as_deref().unwrap_or(default_pass);

    let provisional = log_path_for(logs_dir, None, &host.ip);
    let log = match HostLogger::create(provisional.clone()) {
        Ok(l) => l,
        Err(e) => {
            return HostOutcome::Failed {
                serial: format!("unknown-{}", host.ip),
                reason: format!("cannot open log: {e:#}"),
            };
        }
    };
    log.info(&format!("Connecting to {} as {user}", host.ip));
    notify("running", None, "Connecting to BMC…");

    let client = match RedfishClient::new(&host.ip, user, pass, insecure_tls) {
        Ok(c) => c,
        Err(e) => {
            log.error(&format!("client build failed: {e:#}"));
            return HostOutcome::Failed {
                serial: format!("unknown-{}", host.ip),
                reason: e.to_string(),
            };
        }
    };

    let identity = match client.identify(&log).await {
        Ok(id) => id,
        Err(e) => {
            log.error(&format!("identify failed: {e:#}"));
            return HostOutcome::Failed {
                serial: format!("unknown-{}", host.ip),
                reason: format!("identify: {e:#}"),
            };
        }
    };

    let serial = if identity.serial.is_empty() {
        format!("unknown-{}", host.ip)
    } else {
        identity.serial.clone()
    };

    let final_path = log_path_for(logs_dir, Some(&serial), &host.ip);
    let log = if final_path != provisional {
        match HostLogger::create(final_path) {
            Ok(l) => {
                l.info(&format!("(continued from provisional log for {})", host.ip));
                l
            }
            Err(_) => log,
        }
    } else {
        log
    };

    let Some(mt) = identity.machine_type.clone() else {
        let reason = "could not determine machine type from Redfish identity".to_string();
        log.error(&reason);
        return HostOutcome::Failed { serial, reason };
    };

    let Some(bundle) = bundles_by_mt.get(&mt) else {
        let reason = format!(
            "machine type {mt} was not selected / no bundle downloaded for this host"
        );
        log.warn(&reason);
        return HostOutcome::Skipped { serial, reason };
    };

    log.info(&format!("Using bundle {}", bundle.display()));
    if let Err(e) = client.log_inventory(&log).await {
        log.warn(&format!("inventory snapshot failed: {e:#}"));
    }

    notify("staging", Some(&serial), "Uploading Update Bundle (OnReset)…");
    let stage = match client.stage_bundle_onreset(bundle, &log).await {
        Ok(StageResult::Staged { task_state, job_uri }) => {
            let detail = match &job_uri {
                Some(j) => format!("TaskState={task_state}; job={j}"),
                None => format!("TaskState={task_state}"),
            };
            log.info(&format!("STAGED (OnReset): {detail}"));
            (task_state, job_uri, detail)
        }
        Ok(StageResult::Failed { reason }) => {
            log.error(&format!("FAILED: {reason}"));
            return HostOutcome::Failed { serial, reason };
        }
        Err(e) => {
            log.error(&format!("FAILED: {e:#}"));
            return HostOutcome::Failed {
                serial,
                reason: format!("{e:#}"),
            };
        }
    };
    let (_task_state, job_uri, stage_detail) = stage;

    if !options.reboot_after_stage {
        log.info("Reboot disabled — leaving firmware staged for OnReset");
        return HostOutcome::Staged {
            serial,
            detail: stage_detail,
        };
    }

    notify(
        "rebooting",
        Some(&serial),
        &format!("Issuing {}…", options.reset_type),
    );
    if let Err(e) = client.reset_host(&options.reset_type, &log).await {
        log.error(&format!("reset failed: {e:#}"));
        return HostOutcome::Failed {
            serial,
            reason: format!("staged OK but reset failed: {e:#}"),
        };
    }

    let _ = client
        .wait_until_unreachable(&log, Duration::from_secs(5 * 60))
        .await;

    notify(
        "applying",
        Some(&serial),
        "Waiting for host to return after firmware apply…",
    );
    if let Err(e) = client
        .wait_until_ready(&log, options.reboot_timeout)
        .await
    {
        log.error(&format!("host did not return: {e:#}"));
        return HostOutcome::Failed {
            serial,
            reason: format!("reboot/apply wait failed: {e:#}"),
        };
    }

    if let Some(ref job) = job_uri {
        notify("applying", Some(&serial), "Waiting for update job to finish…");
        match client
            .wait_for_job(job, &log, Duration::from_secs(60 * 60))
            .await
        {
            Ok(()) => log.info("Update job completed"),
            Err(e) => {
                // Job may have been cleaned up after reboot; continue to compare.
                log.warn(&format!("job monitor: {e:#} (continuing to verify)"));
            }
        }
    }

    if !options.verify_with_compare {
        let detail = format!("{stage_detail}; rebooted; verify skipped");
        log.info(&detail);
        return HostOutcome::Staged { serial, detail };
    }

    notify(
        "verifying",
        Some(&serial),
        "OneCLI compare — checking for remaining updates…",
    );
    let package_dir = package_dir_for_bundle(bundle);
    let compare_out = logs_dir.join("compare").join(&serial);
    match onecli_compare(user, pass, &host.ip, &package_dir, &compare_out).await {
        Ok(cmp) => {
            log.info(&cmp.summary);
            log.info(&format!("compare output: {}", cmp.output_dir.display()));
            if cmp.packages_needed == 0 {
                let detail = format!(
                    "{stage_detail}; rebooted; verified — no additional updates needed"
                );
                log.info("VERIFIED");
                notify("verified", Some(&serial), &detail);
                HostOutcome::Verified { serial, detail }
            } else {
                let reason = format!(
                    "after reboot, {} still recommend update(s): {}",
                    cmp.packages_needed, cmp.summary
                );
                log.error(&reason);
                HostOutcome::Failed { serial, reason }
            }
        }
        Err(e) => {
            let reason = format!("reboot OK but compare failed: {e:#}");
            log.error(&reason);
            HostOutcome::Failed { serial, reason }
        }
    }
}

/// Progress callback: (ip, status, serial, detail)
pub type ProgressCallback = Arc<dyn Fn(String, String, Option<String>, String) + Send + Sync>;

/// Fan-out concurrent updates with a semaphore limit.
pub async fn run_concurrent(
    hosts: Vec<TargetHost>,
    default_user: String,
    default_pass: String,
    insecure_tls: bool,
    bundles_by_mt: HashMap<String, PathBuf>,
    logs_dir: PathBuf,
    concurrency: usize,
    options: UpdateOptions,
    on_progress: Option<ProgressCallback>,
) -> Vec<(String, HostOutcome)> {
    let sem = Arc::new(Semaphore::new(concurrency.max(1)));
    let bundles = Arc::new(bundles_by_mt);
    let logs_dir = Arc::new(logs_dir);
    let default_user = Arc::new(default_user);
    let default_pass = Arc::new(default_pass);
    let options = Arc::new(options);

    let mut handles = Vec::new();
    for host in hosts {
        let sem = Arc::clone(&sem);
        let bundles = Arc::clone(&bundles);
        let logs_dir = Arc::clone(&logs_dir);
        let default_user = Arc::clone(&default_user);
        let default_pass = Arc::clone(&default_pass);
        let options = Arc::clone(&options);
        let on_progress = on_progress.clone();
        let ip = host.ip.clone();
        handles.push(tokio::spawn(async move {
            let _permit = match sem.acquire().await {
                Ok(p) => p,
                Err(_) => {
                    return (
                        ip,
                        HostOutcome::Failed {
                            serial: "unknown".into(),
                            reason: "semaphore closed".into(),
                        },
                    );
                }
            };
            let outcome = update_one_host(
                &host,
                &default_user,
                &default_pass,
                insecure_tls,
                &bundles,
                &logs_dir,
                &options,
                on_progress.as_ref(),
            )
            .await;
            (ip, outcome)
        }));
    }

    let mut results = Vec::new();
    for h in handles {
        match h.await {
            Ok(pair) => results.push(pair),
            Err(e) => results.push((
                "unknown".into(),
                HostOutcome::Failed {
                    serial: "unknown".into(),
                    reason: format!("task join: {e}"),
                },
            )),
        }
    }
    results
}

/// Load hosts from a text file.
#[allow(dead_code)]
pub async fn load_hosts_file(path: &Path) -> Result<Vec<TargetHost>> {
    let text = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("read {}", path.display()))?;
    parse_hosts_text(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_ip() {
        let h = parse_host_token("10.0.0.5").unwrap();
        assert_eq!(h.ip, "10.0.0.5");
        assert!(h.user.is_none());
    }

    #[test]
    fn parse_user_pass_ip() {
        let h = parse_host_token("admin:secret@10.0.0.5").unwrap();
        assert_eq!(h.ip, "10.0.0.5");
        assert_eq!(h.user.as_deref(), Some("admin"));
        assert_eq!(h.pass.as_deref(), Some("secret"));
    }

    #[test]
    fn parse_list() {
        let hosts = parse_hosts_text("1.1.1.1, 2.2.2.2\n3.3.3.3").unwrap();
        assert_eq!(hosts.len(), 3);
    }

    #[test]
    fn package_dir_from_zip() {
        let p = package_dir_for_bundle(Path::new("firmware/7D76/bundle.zip"));
        assert!(p.ends_with("7D76"));
    }
}
