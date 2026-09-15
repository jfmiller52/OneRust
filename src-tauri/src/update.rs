//! Per-host firmware update workflow.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Semaphore;

use crate::logutil::{log_path_for, HostLogger};
use crate::redfish::{RedfishClient, StageResult};

/// Target host specification (optional credential override).
#[derive(Debug, Clone)]
pub struct TargetHost {
    pub ip: String,
    pub user: Option<String>,
    pub pass: Option<String>,
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
    Staged { serial: String, detail: String },
    Failed { serial: String, reason: String },
    Skipped { serial: String, reason: String },
}

/// Run update for one BMC.
pub async fn update_one_host(
    host: &TargetHost,
    default_user: &str,
    default_pass: &str,
    insecure_tls: bool,
    bundles_by_mt: &HashMap<String, PathBuf>,
    logs_dir: &Path,
) -> HostOutcome {
    let user = host.user.as_deref().unwrap_or(default_user);
    let pass = host.pass.as_deref().unwrap_or(default_pass);

    // Temporary log until serial is known
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

    // Re-open logger under serial name if different
    let final_path = log_path_for(logs_dir, Some(&serial), &host.ip);
    let log = if final_path != provisional {
        // Copy note into serial log
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

    match client.stage_bundle_onreset(bundle, &log).await {
        Ok(StageResult::Staged { task_state, job_uri }) => {
            let detail = match job_uri {
                Some(j) => format!("TaskState={task_state}; job={j}"),
                None => format!("TaskState={task_state}"),
            };
            log.info(&format!("STAGED (OnReset): {detail}"));
            log.info("Firmware will apply on the next host power reset. No reboot was performed.");
            HostOutcome::Staged { serial, detail }
        }
        Ok(StageResult::Failed { reason }) => {
            log.error(&format!("FAILED: {reason}"));
            HostOutcome::Failed { serial, reason }
        }
        Err(e) => {
            log.error(&format!("FAILED: {e:#}"));
            HostOutcome::Failed {
                serial,
                reason: format!("{e:#}"),
            }
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
    on_progress: Option<ProgressCallback>,
) -> Vec<(String, HostOutcome)> {
    let sem = Arc::new(Semaphore::new(concurrency.max(1)));
    let bundles = Arc::new(bundles_by_mt);
    let logs_dir = Arc::new(logs_dir);
    let default_user = Arc::new(default_user);
    let default_pass = Arc::new(default_pass);

    let mut handles = Vec::new();
    for host in hosts {
        let sem = Arc::clone(&sem);
        let bundles = Arc::clone(&bundles);
        let logs_dir = Arc::clone(&logs_dir);
        let default_user = Arc::clone(&default_user);
        let default_pass = Arc::clone(&default_pass);
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
            if let Some(ref cb) = on_progress {
                cb(
                    host.ip.clone(),
                    "running".into(),
                    None,
                    "Connecting to BMC…".into(),
                );
            }
            let outcome = update_one_host(
                &host,
                &default_user,
                &default_pass,
                insecure_tls,
                &bundles,
                &logs_dir,
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
}
