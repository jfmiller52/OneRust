//! Redfish client for Lenovo XCC2/XCC3 firmware staging.

use anyhow::{anyhow, bail, Context, Result};
use reqwest::multipart::{Form, Part};
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;
use tokio::fs::File;
use tokio_util::io::ReaderStream;

use crate::catalog::extract_machine_type;
use crate::logutil::HostLogger;

/// Identity information read from a BMC.
#[derive(Debug, Clone)]
pub struct HostIdentity {
    pub serial: String,
    #[allow(dead_code)]
    pub model: Option<String>,
    #[allow(dead_code)]
    pub sku: Option<String>,
    #[allow(dead_code)]
    pub hostname: Option<String>,
    pub machine_type: Option<String>,
}

/// Outcome of an OnReset staging attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageResult {
    /// Task is Pending (or Completed) — bundle staged for apply on next reset.
    Staged { task_state: String, job_uri: Option<String> },
    Failed { reason: String },
}

/// Interpret a Redfish Task JSON for OnReset apply-time success.
pub fn interpret_onreset_task(task: &Value) -> StageResult {
    let state = task
        .get("TaskState")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();
    let job_uri = extract_job_uri(task);

    match state.as_str() {
        "Pending" | "Completed" => StageResult::Staged {
            task_state: state,
            job_uri,
        },
        "Exception" | "Cancelled" | "Interrupted" => {
            let msgs = collect_messages(task);
            StageResult::Failed {
                reason: format!("TaskState={state}: {msgs}"),
            }
        }
        other => StageResult::Failed {
            reason: format!("unexpected TaskState={other}"),
        },
    }
}

fn extract_job_uri(task: &Value) -> Option<String> {
    let messages = task.get("Messages")?.as_array()?;
    for msg in messages {
        if let Some(args) = msg.get("MessageArgs").and_then(|a| a.as_array()) {
            for arg in args {
                if let Some(s) = arg.as_str() {
                    if s.contains("JobService") {
                        return Some(s.to_string());
                    }
                }
            }
        }
        if let Some(m) = msg.get("Message").and_then(|v| v.as_str()) {
            if let Some(start) = m.find("/redfish/v1/JobService") {
                let rest = &m[start..];
                let end = rest
                    .find(|c: char| c == '\'' || c == '"' || c.is_whitespace())
                    .unwrap_or(rest.len());
                return Some(rest[..end].to_string());
            }
        }
    }
    None
}

fn collect_messages(task: &Value) -> String {
    task.get("Messages")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("Message").and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default()
}

/// BMC Redfish session wrapper.
pub struct RedfishClient {
    client: Client,
    base: String,
    user: String,
    pass: String,
}

impl RedfishClient {
    pub fn new(ip: &str, user: &str, pass: &str, insecure_tls: bool) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(3600))
            .connect_timeout(Duration::from_secs(30))
            .danger_accept_invalid_certs(insecure_tls)
            .danger_accept_invalid_hostnames(insecure_tls)
            .build()
            .context("build Redfish HTTP client")?;
        let base = if ip.starts_with("http://") || ip.starts_with("https://") {
            ip.trim_end_matches('/').to_string()
        } else {
            format!("https://{ip}")
        };
        Ok(Self {
            client,
            base,
            user: user.to_string(),
            pass: pass.to_string(),
        })
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with("http") {
            path.to_string()
        } else if path.starts_with('/') {
            format!("{}{path}", self.base)
        } else {
            format!("{}/{}", self.base, path)
        }
    }

    async fn get_json(&self, path: &str) -> Result<Value> {
        let resp = self
            .client
            .get(self.url(path))
            .basic_auth(&self.user, Some(&self.pass))
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("GET {path} -> HTTP {status}: {}", truncate(&text, 300));
        }
        serde_json::from_str(&text).with_context(|| format!("parse JSON from {path}"))
    }

    async fn patch_json(&self, path: &str, body: &Value) -> Result<()> {
        let resp = self
            .client
            .patch(self.url(path))
            .basic_auth(&self.user, Some(&self.pass))
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await
            .with_context(|| format!("PATCH {path}"))?;
        let status = resp.status();
        if !(status.is_success() || status == StatusCode::NO_CONTENT || status == StatusCode::ACCEPTED)
        {
            let text = resp.text().await.unwrap_or_default();
            bail!("PATCH {path} -> HTTP {status}: {}", truncate(&text, 300));
        }
        Ok(())
    }

    /// Read Systems/1 identity and derive machine type.
    pub async fn identify(&self, log: &HostLogger) -> Result<HostIdentity> {
        log.info("GET /redfish/v1/Systems/1");
        let sys = self.get_json("/redfish/v1/Systems/1").await?;

        let serial = sys
            .get("SerialNumber")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let model = sys
            .get("Model")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let mut sku = sys
            .get("SKU")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let hostname = sys
            .get("HostName")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        // Oem.Lenovo fallbacks
        if sku.as_ref().map(|s| s.len() < 4).unwrap_or(true) {
            if let Some(oem) = sys.get("Oem").and_then(|o| o.get("Lenovo")) {
                if let Some(s) = oem
                    .get("MachineType")
                    .or_else(|| oem.get("ProductName"))
                    .and_then(|v| v.as_str())
                {
                    sku = Some(s.to_string());
                }
            }
        }

        // Chassis SKU fallback
        if sku.as_ref().map(|s| extract_machine_type(Some(s), None, None).is_none()).unwrap_or(true)
        {
            if let Ok(chassis) = self.get_json("/redfish/v1/Chassis/1").await {
                if let Some(s) = chassis.get("SKU").and_then(|v| v.as_str()) {
                    sku = Some(s.to_string());
                }
            }
        }

        let machine_type = extract_machine_type(
            sku.as_deref(),
            model.as_deref(),
            hostname.as_deref(),
        );

        log.info(&format!(
            "Identity serial={serial} model={} sku={} hostname={} mt={}",
            model.as_deref().unwrap_or("-"),
            sku.as_deref().unwrap_or("-"),
            hostname.as_deref().unwrap_or("-"),
            machine_type.as_deref().unwrap_or("-")
        ));

        Ok(HostIdentity {
            serial,
            model,
            sku,
            hostname,
            machine_type,
        })
    }

    /// Snapshot FirmwareInventory into the log.
    pub async fn log_inventory(&self, log: &HostLogger) -> Result<()> {
        log.info("GET /redfish/v1/UpdateService/FirmwareInventory");
        let coll = self
            .get_json("/redfish/v1/UpdateService/FirmwareInventory")
            .await?;
        let members = coll
            .get("Members")
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default();
        log.info(&format!("FirmwareInventory members: {}", members.len()));
        for m in members.iter().take(40) {
            let id = m.get("@odata.id").and_then(|v| v.as_str()).unwrap_or("");
            if id.is_empty() {
                continue;
            }
            match self.get_json(id).await {
                Ok(item) => {
                    let name = item
                        .get("Name")
                        .or_else(|| item.get("Id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let ver = item
                        .get("Version")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    log.info(&format!("  {name}: {ver}"));
                }
                Err(e) => log.warn(&format!("  skip {id}: {e:#}")),
            }
        }
        Ok(())
    }

    async fn update_service(&self) -> Result<Value> {
        self.get_json("/redfish/v1/UpdateService").await
    }

    async fn set_busy(&self, busy: bool, log: &HostLogger) -> Result<()> {
        let us = self.update_service().await?;
        if us.get("HttpPushUriTargetsBusy").is_none() {
            log.info("HttpPushUriTargetsBusy not present; skipping busy flag");
            return Ok(());
        }
        log.info(&format!("PATCH HttpPushUriTargetsBusy={busy}"));
        self.patch_json(
            "/redfish/v1/UpdateService",
            &json!({ "HttpPushUriTargetsBusy": busy }),
        )
        .await
    }

    /// Multipart push Update Bundle with OnReset apply time; poll until terminal for staging.
    pub async fn stage_bundle_onreset(
        &self,
        bundle_path: &Path,
        log: &HostLogger,
    ) -> Result<StageResult> {
        let us = self.update_service().await?;
        let push_uri = us
            .get("MultipartHttpPushUri")
            .and_then(|v| v.as_str())
            .unwrap_or("/mfwupdate");
        log.info(&format!("MultipartHttpPushUri={push_uri}"));

        self.set_busy(true, log).await?;

        let file_name = bundle_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("bundle.zip")
            .to_string();
        let meta = tokio::fs::metadata(bundle_path)
            .await
            .with_context(|| format!("stat {}", bundle_path.display()))?;
        let file_len = meta.len();
        log.info(&format!(
            "Uploading {} ({file_len} bytes) with OnReset…",
            bundle_path.display()
        ));

        let file = File::open(bundle_path)
            .await
            .with_context(|| format!("open {}", bundle_path.display()))?;
        let stream = ReaderStream::new(file);
        let body = reqwest::Body::wrap_stream(stream);

        let update_params = json!({
            "Targets": [],
            "@Redfish.OperationApplyTime": "OnReset"
        });
        let params_part = Part::text(update_params.to_string())
            .mime_str("application/json")
            .context("params mime")?;
        let file_part = Part::stream_with_length(body, file_len)
            .file_name(file_name)
            .mime_str("application/octet-stream")
            .context("file mime")?;

        let form = Form::new()
            .part("UpdateParameters", params_part)
            .part("UpdateFile", file_part);

        let upload_result = self
            .client
            .post(self.url(push_uri))
            .basic_auth(&self.user, Some(&self.pass))
            .multipart(form)
            .send()
            .await;

        let resp = match upload_result {
            Ok(r) => r,
            Err(e) => {
                let _ = self.set_busy(false, log).await;
                return Err(e).context("multipart firmware upload");
            }
        };

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !(status.is_success() || status == StatusCode::ACCEPTED) {
            let _ = self.set_busy(false, log).await;
            bail!("upload HTTP {status}: {}", truncate(&text, 400));
        }

        let task: Value = serde_json::from_str(&text).unwrap_or_else(|_| {
            json!({ "TaskState": "Unknown", "raw": text })
        });
        let task_uri = task
            .get("@odata.id")
            .and_then(|v| v.as_str())
            .or_else(|| task.get("TaskMonitor").and_then(|v| v.as_str()))
            .map(str::to_string);

        log.info(&format!(
            "Upload accepted; initial TaskState={}",
            task.get("TaskState").and_then(|v| v.as_str()).unwrap_or("?")
        ));

        let result = if let Some(uri) = task_uri {
            self.poll_task_until_staged(&uri, log).await?
        } else {
            interpret_onreset_task(&task)
        };

        // Release busy after staging accepted (Pending/Completed) or failure
        if let Err(e) = self.set_busy(false, log).await {
            log.warn(&format!("failed to clear HttpPushUriTargetsBusy: {e:#}"));
        }

        Ok(result)
    }

    async fn poll_task_until_staged(&self, task_uri: &str, log: &HostLogger) -> Result<StageResult> {
        // OnReset: wait until Pending (staged) or Completed/Exception.
        // Running/New continue polling.
        const MAX_POLLS: u32 = 360; // ~30 min at 5s
        for i in 0..MAX_POLLS {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let task = match self.get_json(task_uri).await {
                Ok(t) => t,
                Err(e) => {
                    log.warn(&format!("poll {task_uri}: {e:#}"));
                    continue;
                }
            };
            let state = task
                .get("TaskState")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let pct = task
                .get("PercentComplete")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if i % 6 == 0 {
                log.info(&format!("Task poll: state={state} percent={pct}"));
                let msgs = collect_messages(&task);
                if !msgs.is_empty() {
                    log.info(&format!("  messages: {msgs}"));
                }
            }
            match state {
                "Pending" | "Completed" | "Exception" | "Cancelled" | "Interrupted" => {
                    return Ok(interpret_onreset_task(&task));
                }
                "New" | "Starting" | "Running" | "Stopping" | "Service" => continue,
                other => {
                    log.warn(&format!("unrecognized TaskState={other}; continuing"));
                }
            }
        }
        Err(anyhow!("timed out polling task {task_uri}"))
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn onreset_pending_is_staged() {
        let task = json!({
            "TaskState": "Pending",
            "Messages": [{
                "Message": "Waiting for reset",
                "MessageArgs": []
            }]
        });
        match interpret_onreset_task(&task) {
            StageResult::Staged { task_state, .. } => assert_eq!(task_state, "Pending"),
            other => panic!("expected Staged, got {other:?}"),
        }
    }

    #[test]
    fn onreset_completed_is_staged() {
        let task = json!({
            "TaskState": "Completed",
            "Messages": [{
                "MessageId": "Update.1.0.OperationTransitionedToJob",
                "Message": "The update operation has transitioned to the job at URI '/redfish/v1/JobService/Jobs/JobR000004-Update'.",
                "MessageArgs": ["/redfish/v1/JobService/Jobs/JobR000004-Update"]
            }]
        });
        match interpret_onreset_task(&task) {
            StageResult::Staged { job_uri, .. } => {
                assert_eq!(
                    job_uri.as_deref(),
                    Some("/redfish/v1/JobService/Jobs/JobR000004-Update")
                );
            }
            other => panic!("expected Staged, got {other:?}"),
        }
    }

    #[test]
    fn onreset_exception_is_failed() {
        let task = json!({
            "TaskState": "Exception",
            "Messages": [{"Message": "Apply failed"}]
        });
        match interpret_onreset_task(&task) {
            StageResult::Failed { reason } => assert!(reason.contains("Apply failed")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}
