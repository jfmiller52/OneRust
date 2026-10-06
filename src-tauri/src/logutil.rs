//! Per-serial (or per-IP) file logger for firmware update progress.

use anyhow::{Context, Result};
use chrono::Local;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Optional live sink for each log line (UI console, etc.).
pub type LineSink = Arc<dyn Fn(String) + Send + Sync>;

/// Sanitize a serial number for use as a log filename.
pub fn sanitize_serial(serial: &str) -> String {
    let cleaned: String = serial
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
        "unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Build a log path under `logs_dir` for a serial, or `unknown-<ip>` when missing.
pub fn log_path_for(logs_dir: &Path, serial: Option<&str>, ip: &str) -> PathBuf {
    let name = match serial {
        Some(s) if !s.trim().is_empty() => sanitize_serial(s),
        _ => format!("unknown-{}", sanitize_serial(ip)),
    };
    logs_dir.join(format!("{name}.log"))
}

/// Thread-safe append-only log writer for one host.
#[derive(Clone)]
pub struct HostLogger {
    file: Arc<Mutex<File>>,
    #[allow(dead_code)]
    pub path: PathBuf,
    sink: Option<LineSink>,
}

impl HostLogger {
    #[allow(dead_code)]
    pub fn create(path: PathBuf) -> Result<Self> {
        Self::create_with_sink(path, None)
    }

    pub fn create_with_sink(path: PathBuf, sink: Option<LineSink>) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create log dir {}", parent.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open log {}", path.display()))?;
        Ok(Self {
            file: Arc::new(Mutex::new(file)),
            path,
            sink,
        })
    }

    pub fn info(&self, msg: &str) {
        self.write("INFO", msg);
    }

    pub fn warn(&self, msg: &str) {
        self.write("WARN", msg);
    }

    pub fn error(&self, msg: &str) {
        self.write("ERROR", msg);
    }

    /// Append a raw console/OneCLI line (no level tag).
    pub fn out(&self, msg: &str) {
        let msg = msg.trim_end_matches(['\r', '\n']);
        if msg.is_empty() {
            return;
        }
        let ts = Local::now().format("%Y-%m-%d %H:%M:%S");
        let line = format!("[{ts}] {msg}\n");
        self.emit(&line);
    }

    /// Callback that forwards OneCLI stdout/stderr into this logger (and UI sink).
    pub fn pipe(&self) -> LineSink {
        let this = self.clone();
        Arc::new(move |line: String| this.out(&line))
    }

    fn write(&self, level: &str, msg: &str) {
        let ts = Local::now().format("%Y-%m-%d %H:%M:%S");
        let line = format!("[{ts}] [{level}] {msg}\n");
        self.emit(&line);
    }

    fn emit(&self, line: &str) {
        if let Ok(mut f) = self.file.lock() {
            let _ = f.write_all(line.as_bytes());
            let _ = f.flush();
        }
        if let Some(sink) = &self.sink {
            sink(line.trim_end_matches('\n').to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_bad_chars() {
        assert_eq!(sanitize_serial("J1A2/B3"), "J1A2_B3");
        assert_eq!(sanitize_serial("  "), "unknown");
        assert_eq!(sanitize_serial("ABC-123"), "ABC-123");
    }

    #[test]
    fn unknown_ip_log_name() {
        let p = log_path_for(Path::new("logs"), None, "192.168.1.10");
        assert_eq!(p, Path::new("logs").join("unknown-192.168.1.10.log"));
    }

    #[test]
    fn serial_log_name() {
        let p = log_path_for(Path::new("logs"), Some("J12345"), "1.2.3.4");
        assert_eq!(p, Path::new("logs").join("J12345.log"));
    }
}
