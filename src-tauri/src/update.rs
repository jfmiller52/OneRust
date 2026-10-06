//! Per-host firmware update workflow (OneCLI only).

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

use crate::lenovo::{
    discover_lxpm_package_ids, lxpm_packages_needing_update, onecli_compare, onecli_flash_bundle,
    onecli_identify, onecli_power_restart, onecli_reboot_bmc, onecli_wait_bmc_ready,
    sleep_interruptible, FlashSelect, BMC_SPACE_RETRY_WAIT_SECS,
};
use crate::logutil::{log_path_for, HostLogger, LineSink};

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
    /// Issue OneCLI power restart after staging (default true).
    pub reboot_after_stage: bool,
    /// Reset type: ForceRestart (default) or GracefulRestart.
    pub reset_type: String,
    /// Run OneCLI compare after reboot to confirm no updates remain (default true).
    pub verify_with_compare: bool,
    /// How long to wait for the BMC to answer after reboot.
    pub reboot_timeout: Duration,
    /// Firmware apply time for OneCLI bundle flash (default OnReset).
    pub applytime: String,
}

impl Default for UpdateOptions {
    fn default() -> Self {
        Self {
            reboot_after_stage: true,
            reset_type: "ForceRestart".into(),
            verify_with_compare: true,
            reboot_timeout: Duration::from_secs(90 * 60),
            applytime: "OnReset".into(),
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

/// Run update for one BMC via OneCLI: identify → flash bundle → reboot → compare.
pub async fn update_one_host(
    host: &TargetHost,
    default_user: &str,
    default_pass: &str,
    never_check_trust: bool,
    bundles_by_mt: &HashMap<String, PathBuf>,
    logs_dir: &Path,
    options: &UpdateOptions,
    on_progress: Option<&ProgressCallback>,
    on_console: Option<&ConsoleCallback>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> HostOutcome {
    use crate::cancel::JobCancel;

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

    if cancel.is_some_and(JobCancel::is_cancelled) {
        return HostOutcome::Skipped {
            serial: format!("unknown-{}", host.ip),
            reason: "cancelled".into(),
        };
    }

    let user = host.user.as_deref().unwrap_or(default_user);
    let pass = host.pass.as_deref().unwrap_or(default_pass);

    let line_sink: Option<LineSink> = on_console.map(|cb| {
        let ip = host.ip.clone();
        let cb = cb.clone();
        Arc::new(move |line: String| cb(ip.clone(), line)) as LineSink
    });

    let provisional = log_path_for(logs_dir, None, &host.ip);
    let log = match HostLogger::create_with_sink(provisional.clone(), line_sink.clone()) {
        Ok(l) => l,
        Err(e) => {
            return HostOutcome::Failed {
                serial: format!("unknown-{}", host.ip),
                reason: format!("cannot open log: {e:#}"),
            };
        }
    };
    log.info(&format!("Connecting to {} as {user} via OneCLI", host.ip));
    notify("running", None, "OneCLI inventory — identifying host…");

    let identify_dir = logs_dir.join("identify").join(
        crate::logutil::sanitize_serial(&host.ip),
    );
    let identity = match onecli_identify(
        user,
        pass,
        &host.ip,
        &identify_dir,
        never_check_trust,
        Some(log.pipe()),
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            log.error(&format!("identify failed: {e:#}"));
            return HostOutcome::Failed {
                serial: format!("unknown-{}", host.ip),
                reason: format!("OneCLI identify: {e:#}"),
            };
        }
    };

    let serial = identity.serial.clone();
    let final_path = log_path_for(logs_dir, Some(&serial), &host.ip);
    let log = if final_path != provisional {
        match HostLogger::create_with_sink(final_path, line_sink.clone()) {
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
        let reason = "could not determine machine type from OneCLI inventory".to_string();
        log.error(&reason);
        return HostOutcome::Failed { serial, reason };
    };
    log.info(&format!("Identity serial={serial} mt={mt}"));

    let Some(bundle) = bundles_by_mt.get(&mt) else {
        let reason = format!(
            "machine type {mt} was not selected / no bundle downloaded for this host"
        );
        log.warn(&reason);
        return HostOutcome::Skipped { serial, reason };
    };

    log.info(&format!("Using bundle {}", bundle.display()));
    let package_dir = package_dir_for_bundle(bundle);

    let lxpm_ids = match discover_lxpm_package_ids(&package_dir).await {
        Ok(ids) => ids,
        Err(e) => {
            log.warn(&format!("LXPM package discovery failed ({e:#}); flashing full bundle"));
            Vec::new()
        }
    };
    if lxpm_ids.is_empty() {
        log.info("No LXPM/driver packages found in bundle — single flash");
    } else {
        log.info(&format!(
            "LXPM/driver packages (one-at-a-time after bulk): {}",
            lxpm_ids.join(", ")
        ));
    }

    // 1) Bulk-flash everything except LXPM/drivers (avoids BMC space exhaustion).
    let flash_dir = logs_dir.join("flash").join(&serial);
    let exclude = FlashSelect {
        exclude_ids: &lxpm_ids,
        include_ids: &[],
    };
    let bulk_label = if lxpm_ids.is_empty() {
        "OneCLI update flash --bundle (OnReset)…".to_string()
    } else {
        format!(
            "OneCLI update flash --bundle excluding {} LXPM/driver package(s)…",
            lxpm_ids.len()
        )
    };
    notify("staging", Some(&serial), &bulk_label);
    let mut stage_detail = match flash_with_space_retry(
        user,
        pass,
        &host.ip,
        &package_dir,
        &flash_dir,
        &options.applytime,
        never_check_trust,
        exclude,
        &log,
        &notify,
        Some(&serial),
        cancel,
    )
    .await
    {
        Ok(detail) => detail,
        Err(FlashStepError::Skipped { reason }) => {
            return HostOutcome::Skipped { serial, reason };
        }
        Err(FlashStepError::Failed { reason }) => {
            log.error(&reason);
            return HostOutcome::Failed { serial, reason };
        }
    };

    // Reboot after non-LXPM staging when enabled (applies OnReset payload).
    if options.reboot_after_stage {
        if let Err(e) = reboot_host_and_wait(
            user,
            pass,
            &host.ip,
            &serial,
            options,
            never_check_trust,
            logs_dir,
            &log,
            &notify,
            cancel,
            "after non-LXPM flash",
        )
        .await
        {
            return e.into_outcome(serial);
        }
    } else if lxpm_ids.is_empty() {
        log.info("Reboot disabled — leaving firmware staged for OnReset");
        return HostOutcome::Staged {
            serial,
            detail: stage_detail,
        };
    } else {
        log.warn(
            "Reboot disabled, but LXPM/driver packages will still flash one-at-a-time without host restarts",
        );
    }

    // 2) Flash each needed LXPM/driver package individually, rebooting between them.
    if !lxpm_ids.is_empty() {
        let lxpm_needed = resolve_lxpm_to_flash(
            user,
            pass,
            &host.ip,
            &package_dir,
            &logs_dir.join("compare").join(&serial).join("pre-lxpm"),
            never_check_trust,
            &lxpm_ids,
            &log,
        )
        .await;

        if lxpm_needed.is_empty() {
            log.info("Compare reports no LXPM/driver updates needed");
        } else {
            log.info(&format!(
                "Flashing {} LXPM/driver package(s) one at a time",
                lxpm_needed.len()
            ));
        }

        for (idx, pkg_id) in lxpm_needed.iter().enumerate() {
            if cancel.is_some_and(crate::cancel::JobCancel::is_cancelled) {
                return HostOutcome::Skipped {
                    serial,
                    reason: "cancelled".into(),
                };
            }
            let n = idx + 1;
            let total = lxpm_needed.len();
            notify(
                "staging",
                Some(&serial),
                &format!("LXPM/driver {n}/{total}: {pkg_id}"),
            );
            log.info(&format!("Flashing LXPM/driver {n}/{total}: {pkg_id}"));
            let step_dir = logs_dir
                .join("flash")
                .join(&serial)
                .join("lxpm")
                .join(format!("{n:02}_{}", sanitize_dir_component(pkg_id)));
            let include = [pkg_id.clone()];
            match flash_with_space_retry(
                user,
                pass,
                &host.ip,
                &package_dir,
                &step_dir,
                &options.applytime,
                never_check_trust,
                FlashSelect {
                    exclude_ids: &[],
                    include_ids: &include,
                },
                &log,
                &notify,
                Some(&serial),
                cancel,
            )
            .await
            {
                Ok(detail) => {
                    stage_detail = format!("{stage_detail}; lxpm {n}/{total} {pkg_id}: {detail}");
                }
                Err(FlashStepError::Skipped { reason }) => {
                    return HostOutcome::Skipped { serial, reason };
                }
                Err(FlashStepError::Failed { reason }) => {
                    log.error(&reason);
                    return HostOutcome::Failed { serial, reason };
                }
            }

            if options.reboot_after_stage {
                if let Err(e) = reboot_host_and_wait(
                    user,
                    pass,
                    &host.ip,
                    &serial,
                    options,
                    never_check_trust,
                    logs_dir,
                    &log,
                    &notify,
                    cancel,
                    &format!("after LXPM/driver {n}/{total}"),
                )
                .await
                {
                    return e.into_outcome(serial);
                }
            }
        }
    }

    if !options.reboot_after_stage {
        log.info("Reboot disabled — leaving remaining firmware staged for OnReset");
        return HostOutcome::Staged {
            serial,
            detail: stage_detail,
        };
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
    let compare_out = logs_dir.join("compare").join(&serial).join("final");
    match onecli_compare(
        user,
        pass,
        &host.ip,
        &package_dir,
        &compare_out,
        never_check_trust,
        Some(log.pipe()),
    )
    .await
    {
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

enum FlashStepError {
    Failed { reason: String },
    Skipped { reason: String },
}

impl FlashStepError {
    fn into_outcome(self, serial: String) -> HostOutcome {
        match self {
            Self::Failed { reason } => HostOutcome::Failed { serial, reason },
            Self::Skipped { reason } => HostOutcome::Skipped { serial, reason },
        }
    }
}

fn sanitize_dir_component(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_');
    if trimmed.is_empty() {
        "pkg".into()
    } else {
        trimmed.chars().take(80).collect()
    }
}

async fn resolve_lxpm_to_flash(
    user: &str,
    pass: &str,
    ip: &str,
    package_dir: &Path,
    compare_dir: &Path,
    never_check_trust: bool,
    discovered: &[String],
    log: &HostLogger,
) -> Vec<String> {
    match onecli_compare(
        user,
        pass,
        ip,
        package_dir,
        compare_dir,
        never_check_trust,
        Some(log.pipe()),
    )
    .await
    {
        Ok(cmp) => {
            log.info(&cmp.summary);
            match lxpm_packages_needing_update(compare_dir).await {
                Ok(needed) if !needed.is_empty() => needed,
                Ok(_) => Vec::new(),
                Err(e) => {
                    log.warn(&format!(
                        "Could not parse LXPM needs from compare ({e:#}); flashing all discovered LXPM packages"
                    ));
                    discovered.to_vec()
                }
            }
        }
        Err(e) => {
            log.warn(&format!(
                "Pre-LXPM compare failed ({e:#}); flashing all discovered LXPM packages"
            ));
            discovered.to_vec()
        }
    }
}

async fn flash_with_space_retry<N>(
    user: &str,
    pass: &str,
    ip: &str,
    package_dir: &Path,
    flash_dir: &Path,
    applytime: &str,
    never_check_trust: bool,
    select: FlashSelect<'_>,
    log: &HostLogger,
    notify: &N,
    serial: Option<&str>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<String, FlashStepError>
where
    N: Fn(&str, Option<&str>, &str) + Sync,
{
    match onecli_flash_bundle(
        user,
        pass,
        ip,
        package_dir,
        flash_dir,
        applytime,
        never_check_trust,
        Some(log.pipe()),
        select,
    )
    .await
    {
        Ok(detail) => {
            log.info(&detail);
            Ok(detail)
        }
        Err(e) if e.is_bmc_space_exhausted() => {
            log.warn(&format!("{e}"));
            notify(
                "rebooting",
                serial,
                &format!(
                    "BMC space full (exit {:?}) — rebooting BMC, waiting {BMC_SPACE_RETRY_WAIT_SECS}s, then retrying flash…",
                    e.code
                ),
            );
            let bmc_reboot_dir = flash_dir.join("rebootbmc");
            if let Err(reboot_err) = onecli_reboot_bmc(
                user,
                pass,
                ip,
                &bmc_reboot_dir,
                never_check_trust,
                Some(log.pipe()),
            )
            .await
            {
                return Err(FlashStepError::Failed {
                    reason: format!(
                        "flash failed (BMC space) and BMC reboot failed: {reboot_err:#}; original: {e}"
                    ),
                });
            }
            log.info(&format!(
                "BMC reboot issued; waiting {BMC_SPACE_RETRY_WAIT_SECS}s before flash retry"
            ));
            if let Err(wait_err) =
                wait_with_heartbeats(BMC_SPACE_RETRY_WAIT_SECS, cancel, log).await
            {
                let reason = format!("{wait_err:#}");
                if reason.contains("cancelled") {
                    return Err(FlashStepError::Skipped { reason });
                }
                return Err(FlashStepError::Failed { reason });
            }
            if let Err(ready_err) = onecli_wait_bmc_ready(
                user,
                pass,
                ip,
                Duration::from_secs(10 * 60),
                never_check_trust,
                cancel,
                Some(log.pipe()),
            )
            .await
            {
                let reason = format!("{ready_err:#}");
                if reason.contains("cancelled") {
                    return Err(FlashStepError::Skipped { reason });
                }
                return Err(FlashStepError::Failed {
                    reason: format!(
                        "BMC did not return after space-retry reboot: {ready_err:#}"
                    ),
                });
            }

            notify(
                "staging",
                serial,
                "Retrying OneCLI update flash after BMC reboot…",
            );
            let flash_retry_dir = flash_dir.join("retry");
            match onecli_flash_bundle(
                user,
                pass,
                ip,
                package_dir,
                &flash_retry_dir,
                applytime,
                never_check_trust,
                Some(log.pipe()),
                select,
            )
            .await
            {
                Ok(detail) => {
                    let detail = format!(
                        "{detail} (after BMC reboot + {BMC_SPACE_RETRY_WAIT_SECS}s wait for exit {:?})",
                        e.code
                    );
                    log.info(&detail);
                    Ok(detail)
                }
                Err(retry_err) => Err(FlashStepError::Failed {
                    reason: format!(
                        "flash retry after BMC reboot failed: {retry_err}; first error: {e}"
                    ),
                }),
            }
        }
        Err(e) => Err(FlashStepError::Failed {
            reason: e.to_string(),
        }),
    }
}

async fn reboot_host_and_wait<N>(
    user: &str,
    pass: &str,
    ip: &str,
    serial: &str,
    options: &UpdateOptions,
    never_check_trust: bool,
    logs_dir: &Path,
    log: &HostLogger,
    notify: &N,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    reason: &str,
) -> Result<(), FlashStepError>
where
    N: Fn(&str, Option<&str>, &str) + Sync,
{
    notify(
        "rebooting",
        Some(serial),
        &format!("OneCLI misc power ({}) — {reason}…", options.reset_type),
    );
    let power_dir = logs_dir.join("power").join(serial).join(sanitize_dir_component(reason));
    if let Err(e) = onecli_power_restart(
        user,
        pass,
        ip,
        &options.reset_type,
        &power_dir,
        never_check_trust,
        Some(log.pipe()),
    )
    .await
    {
        return Err(FlashStepError::Failed {
            reason: format!("staged OK but reset failed ({reason}): {e:#}"),
        });
    }

    notify(
        "applying",
        Some(serial),
        &format!("Waiting for BMC to return ({reason})…"),
    );
    if let Err(e) = onecli_wait_bmc_ready(
        user,
        pass,
        ip,
        options.reboot_timeout,
        never_check_trust,
        cancel,
        Some(log.pipe()),
    )
    .await
    {
        let msg = format!("{e:#}");
        if msg.contains("cancelled") {
            return Err(FlashStepError::Skipped { reason: msg });
        }
        return Err(FlashStepError::Failed {
            reason: format!("reboot/apply wait failed ({reason}): {e:#}"),
        });
    }
    Ok(())
}

/// Progress callback: (ip, status, serial, detail)
pub type ProgressCallback = Arc<dyn Fn(String, String, Option<String>, String) + Send + Sync>;

/// Live console callback: (ip, line)
pub type ConsoleCallback = Arc<dyn Fn(String, String) + Send + Sync>;

async fn wait_with_heartbeats(
    secs: u64,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    log: &HostLogger,
) -> anyhow::Result<()> {
    const CHUNK: u64 = 30;
    let mut elapsed = 0u64;
    while elapsed < secs {
        let chunk = (secs - elapsed).min(CHUNK);
        sleep_interruptible(chunk, cancel).await?;
        elapsed += chunk;
        if elapsed < secs {
            log.info(&format!("… waiting {elapsed}s / {secs}s"));
        }
    }
    Ok(())
}

/// Fan-out concurrent updates with a semaphore limit.
pub async fn run_concurrent(
    hosts: Vec<TargetHost>,
    default_user: String,
    default_pass: String,
    never_check_trust: bool,
    bundles_by_mt: HashMap<String, PathBuf>,
    logs_dir: PathBuf,
    concurrency: usize,
    options: UpdateOptions,
    on_progress: Option<ProgressCallback>,
    on_console: Option<ConsoleCallback>,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Vec<(String, HostOutcome)> {
    use crate::cancel::JobCancel;

    let sem = Arc::new(Semaphore::new(concurrency.max(1)));
    let bundles = Arc::new(bundles_by_mt);
    let logs_dir = Arc::new(logs_dir);
    let default_user = Arc::new(default_user);
    let default_pass = Arc::new(default_pass);
    let options = Arc::new(options);

    let mut handles = Vec::new();
    for host in hosts {
        if cancel.as_ref().is_some_and(|c| JobCancel::is_cancelled(c)) {
            handles.push(tokio::spawn(async move {
                (
                    host.ip.clone(),
                    HostOutcome::Skipped {
                        serial: format!("unknown-{}", host.ip),
                        reason: "cancelled".into(),
                    },
                )
            }));
            continue;
        }
        let sem = Arc::clone(&sem);
        let bundles = Arc::clone(&bundles);
        let logs_dir = Arc::clone(&logs_dir);
        let default_user = Arc::clone(&default_user);
        let default_pass = Arc::clone(&default_pass);
        let options = Arc::clone(&options);
        let on_progress = on_progress.clone();
        let on_console = on_console.clone();
        let cancel = cancel.clone();
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
                never_check_trust,
                &bundles,
                &logs_dir,
                &options,
                on_progress.as_ref(),
                on_console.as_ref(),
                cancel.as_deref(),
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
