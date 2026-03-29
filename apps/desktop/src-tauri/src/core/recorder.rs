use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::{JoinHandle, sleep};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::storage::{NewKeyframe, NewRawEvent, NewSession, Storage, StorageError};

#[derive(Debug)]
pub enum RecorderError {
    BinaryNotFound(PathBuf),
    Io(std::io::Error),
    Json(serde_json::Error),
    Storage(StorageError),
    Protocol(String),
}

impl std::fmt::Display for RecorderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BinaryNotFound(path) => write!(f, "recorder binary not found at {}", path.display()),
            Self::Io(error) => write!(f, "io error: {}", error),
            Self::Json(error) => write!(f, "json error: {}", error),
            Self::Storage(error) => write!(f, "storage error: {}", error),
            Self::Protocol(message) => write!(f, "protocol error: {}", message),
        }
    }
}

impl std::error::Error for RecorderError {}

impl From<std::io::Error> for RecorderError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for RecorderError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<StorageError> for RecorderError {
    fn from(value: StorageError) -> Self {
        Self::Storage(value)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecorderStatus {
    pub active: bool,
    pub session_external_id: Option<String>,
    pub session_row_id: Option<i64>,
    pub event_count: i64,
    pub frame_count: i64,
    pub permissions: BTreeMap<String, bool>,
    pub recorder_binary: String,
}

pub struct RecorderCoordinator {
    storage: Storage,
    binary_path: PathBuf,
    process: Option<RecorderProcess>,
}

const BRIDGE_START_TIMEOUT: Duration = Duration::from_secs(3);
const BRIDGE_STOP_TIMEOUT: Duration = Duration::from_secs(2);

struct RecorderProcess {
    child: Child,
    stdin: ChildStdin,
    ingest_thread: JoinHandle<Result<(), RecorderError>>,
    stderr_thread: JoinHandle<()>,
    diagnostics: Arc<Mutex<BridgeDiagnostics>>,
    state: Arc<Mutex<ActiveCapture>>,
}

struct ActiveCapture {
    session_row_id: i64,
    session_external_id: String,
    next_sequence: i64,
    event_count: i64,
    frame_count: i64,
    app_transition_count: i64,
    ax_snapshot_count: i64,
    last_error: Option<String>,
    permissions: BTreeMap<String, bool>,
}

#[derive(Debug, Deserialize)]
struct BridgeMessage {
    kind: String,
    payload: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct BridgeCommand<'a> {
    command: &'a str,
}

#[derive(Debug, Default)]
struct BridgeDiagnostics {
    telemetry_tail: VecDeque<String>,
    stderr_tail: VecDeque<String>,
    malformed_stdout_lines: usize,
    helper_exit: Option<String>,
}

impl BridgeDiagnostics {
    fn push_telemetry(&mut self, entry: String) {
        push_bounded(&mut self.telemetry_tail, entry);
    }

    fn push_stderr(&mut self, line: String) {
        push_bounded(&mut self.stderr_tail, line);
    }

    fn note_malformed_stdout(&mut self) {
        self.malformed_stdout_lines += 1;
    }

    fn record_exit_status(&mut self, status: ExitStatus) {
        self.helper_exit = Some(match status.code() {
            Some(code) => format!("exit {}", code),
            None => "terminated by signal".to_string(),
        });
    }

    fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(exit) = &self.helper_exit {
            parts.push(format!("helper {}", exit));
        }
        if self.malformed_stdout_lines > 0 {
            parts.push(format!("malformed_stdout_lines={}", self.malformed_stdout_lines));
        }
        if !self.telemetry_tail.is_empty() {
            parts.push(format!(
                "telemetry=[{}]",
                self.telemetry_tail.iter().cloned().collect::<Vec<_>>().join(" | ")
            ));
        }
        if !self.stderr_tail.is_empty() {
            parts.push(format!(
                "stderr=[{}]",
                self.stderr_tail.iter().cloned().collect::<Vec<_>>().join(" | ")
            ));
        }

        if parts.is_empty() {
            "no bridge diagnostics".to_string()
        } else {
            parts.join("; ")
        }
    }
}

impl RecorderCoordinator {
    pub fn new(storage: Storage, binary_path: PathBuf) -> Self {
        Self {
            storage,
            binary_path,
            process: None,
        }
    }

    pub fn status(&mut self) -> Result<RecorderStatus, RecorderError> {
        let permissions = match &self.process {
            Some(process) => process
                .state
                .lock()
                .map_err(|_| RecorderError::Protocol("recorder state mutex poisoned".to_string()))?
                .permissions
                .clone(),
            None => self.permissions()?,
        };

        Ok(match &self.process {
            Some(process) => {
                let state = process
                    .state
                    .lock()
                    .map_err(|_| RecorderError::Protocol("recorder state mutex poisoned".to_string()))?;

                RecorderStatus {
                    active: true,
                    session_external_id: Some(state.session_external_id.clone()),
                    session_row_id: Some(state.session_row_id),
                    event_count: state.event_count,
                    frame_count: state.frame_count,
                    permissions,
                    recorder_binary: self.binary_path.display().to_string(),
                }
            }
            None => RecorderStatus {
                active: false,
                session_external_id: None,
                session_row_id: None,
                event_count: 0,
                frame_count: 0,
                permissions,
                recorder_binary: self.binary_path.display().to_string(),
            },
        })
    }

    pub fn start_capture(&mut self) -> Result<RecorderStatus, RecorderError> {
        if self.process.is_some() {
            return self.status();
        }

        if !self.binary_path.exists() {
            return Err(RecorderError::BinaryNotFound(self.binary_path.clone()));
        }

        let mut child = Command::new(&self.binary_path)
            .arg("--bridge")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdin = child.stdin.take().ok_or_else(|| RecorderError::Protocol("missing stdin".to_string()))?;
        let stdout = child.stdout.take().ok_or_else(|| RecorderError::Protocol("missing stdout".to_string()))?;
        let stderr = child.stderr.take().ok_or_else(|| RecorderError::Protocol("missing stderr".to_string()))?;
        let diagnostics = Arc::new(Mutex::new(BridgeDiagnostics::default()));
        let events_rx = spawn_bridge_reader(stdout, diagnostics.clone());
        let stderr_thread = spawn_stderr_reader(stderr, diagnostics.clone());

        let permissions_message = wait_for_message(
            &events_rx,
            &mut child,
            diagnostics.clone(),
            "permissions",
            BRIDGE_START_TIMEOUT,
        )?;
        let permissions: BTreeMap<String, bool> = serde_json::from_value(permissions_message.payload)?;

        let session_message = wait_for_message(
            &events_rx,
            &mut child,
            diagnostics.clone(),
            "capture_started",
            BRIDGE_START_TIMEOUT,
        )?;
        let session_payload: CaptureStartedPayload = serde_json::from_value(session_message.payload)?;
        if !session_payload.ok {
            drop(stdin);
            let _ = child.kill();
            let _ = child.wait();
            return Err(RecorderError::Protocol(
                format!(
                    "{} ({})",
                    session_payload
                    .error
                    .unwrap_or_else(|| "capture_start_failed".to_string()),
                    bridge_summary(&diagnostics)
                ),
            ));
        }
        let session_external_id = session_payload
            .session_id
            .ok_or_else(|| RecorderError::Protocol(format!(
                "capture_started missing session_id ({})",
                bridge_summary(&diagnostics)
            )))?;
        let started_at_ms = session_payload
            .started_at
            .ok_or_else(|| RecorderError::Protocol(format!(
                "capture_started missing started_at ({})",
                bridge_summary(&diagnostics)
            )))?;
        let session_row_id = self.storage.insert_session(&NewSession {
            external_id: session_external_id.clone(),
            label: Some("Bridge capture".to_string()),
            started_at_ms,
            status: "recording".to_string(),
        })?;

        let state = Arc::new(Mutex::new(ActiveCapture {
            session_row_id,
            session_external_id,
            next_sequence: 0,
            event_count: 0,
            frame_count: 0,
            app_transition_count: 0,
            ax_snapshot_count: 0,
            last_error: None,
            permissions,
        }));
        let ingest_thread = spawn_ingest_thread(self.storage.clone(), state.clone(), events_rx);

        self.process = Some(RecorderProcess {
            child,
            stdin,
            ingest_thread,
            stderr_thread,
            diagnostics,
            state,
        });

        self.status()
    }

    pub fn stop_capture(&mut self) -> Result<RecorderStatus, RecorderError> {
        let Some(mut process) = self.process.take() else {
            return self.status();
        };

        send_command(&mut process.stdin, "stop")?;
        drop(process.stdin);

        let stop_deadline = Instant::now() + BRIDGE_STOP_TIMEOUT;
        let exit_status = loop {
            match process.child.try_wait()? {
                Some(status) => break Some(status),
                None if Instant::now() < stop_deadline => sleep(Duration::from_millis(25)),
                None => {
                    let _ = process.child.kill();
                    break process.child.wait().ok();
                }
            }
        };

        if let Some(status) = exit_status {
            if let Ok(mut diagnostics) = process.diagnostics.lock() {
                diagnostics.record_exit_status(status);
            }
        }

        match process.ingest_thread.join() {
            Ok(result) => result?,
            Err(_) => {
                return Err(RecorderError::Protocol(
                    "recorder ingest thread panicked".to_string(),
                ))
            }
        }
        let _ = process.stderr_thread.join();

        if exit_status.is_none() {
            return Err(RecorderError::Protocol(format!(
                "recorder helper did not stop cleanly ({})",
                bridge_summary(&process.diagnostics)
            )));
        }

        let state = process
            .state
            .lock()
            .map_err(|_| RecorderError::Protocol("recorder state mutex poisoned".to_string()))?;

        self.storage
            .complete_session(state.session_row_id, now_ms())
            .map_err(RecorderError::Storage)?;

        Ok(RecorderStatus {
            active: false,
            session_external_id: Some(state.session_external_id.clone()),
            session_row_id: Some(state.session_row_id),
            event_count: state.event_count,
            frame_count: state.frame_count,
            permissions: state.permissions.clone(),
            recorder_binary: self.binary_path.display().to_string(),
        })
    }

    fn permissions(&self) -> Result<BTreeMap<String, bool>, RecorderError> {
        if !self.binary_path.exists() {
            return Ok(BTreeMap::new());
        }

        let output = Command::new(&self.binary_path).arg("--permissions-json").output()?;
        if !output.status.success() {
            return Err(RecorderError::Protocol("failed to read recorder permissions".to_string()));
        }

        let permissions = serde_json::from_slice::<BTreeMap<String, bool>>(&output.stdout)?;
        Ok(permissions)
    }
}

#[derive(Debug, Deserialize)]
struct CaptureStartedPayload {
    ok: bool,
    session_id: Option<String>,
    started_at: Option<u64>,
    error: Option<String>,
}

fn spawn_bridge_reader(stdout: ChildStdout, diagnostics: Arc<Mutex<BridgeDiagnostics>>) -> Receiver<BridgeMessage> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<BridgeMessage>(&line) {
                Ok(message) => {
                    let _ = tx.send(message);
                }
                Err(_) => {
                    if let Ok(mut bridge_diagnostics) = diagnostics.lock() {
                        bridge_diagnostics.note_malformed_stdout();
                        bridge_diagnostics.push_stderr(format!("malformed stdout: {}", line));
                    }
                }
            }
        }
    });
    rx
}

fn spawn_stderr_reader(stderr: ChildStderr, diagnostics: Arc<Mutex<BridgeDiagnostics>>) -> JoinHandle<()> {
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(mut bridge_diagnostics) = diagnostics.lock() {
                bridge_diagnostics.push_stderr(line);
            }
        }
    })
}

fn spawn_ingest_thread(
    storage: Storage,
    state: Arc<Mutex<ActiveCapture>>,
    rx: Receiver<BridgeMessage>,
) -> JoinHandle<Result<(), RecorderError>> {
    thread::spawn(move || {
        while let Ok(message) = rx.recv() {
            if message.kind != "event" {
                continue;
            }

            let event_type = message
                .payload
                .get("type")
                .and_then(|value| value.as_str())
                .ok_or_else(|| RecorderError::Protocol("event envelope missing type".to_string()))?
                .to_string();

            let mut state_guard = state
                .lock()
                .map_err(|_| RecorderError::Protocol("recorder state mutex poisoned".to_string()))?;

            storage.insert_raw_event(&NewRawEvent {
                session_id: state_guard.session_row_id,
                sequence: state_guard.next_sequence,
                event_type: event_type.clone(),
                event_json: serde_json::to_string(&message.payload)?,
                recorded_at_ms: message
                    .payload
                    .get("ts")
                    .and_then(|value| value.as_u64())
                    .unwrap_or_else(now_ms),
            })?;

            state_guard.next_sequence += 1;
            state_guard.event_count += 1;
            if event_type == "frontmost_app_changed" {
                state_guard.app_transition_count += 1;
            }
            if event_type == "ax_snapshot" {
                state_guard.ax_snapshot_count += 1;
            }
            if event_type == "screen_frame" {
                if let Some(keyframe) = extract_keyframe(&message.payload, state_guard.session_row_id) {
                    let _ = storage.insert_keyframe(&keyframe);
                }
                state_guard.frame_count += 1;
            }
            if event_type == "bridge_error" {
                state_guard.last_error = message
                    .payload
                    .get("payload")
                    .and_then(|value| value.get("message"))
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned);
            }
            storage.update_session_summary(
                state_guard.session_row_id,
                state_guard.app_transition_count,
                state_guard.ax_snapshot_count,
                state_guard.frame_count,
                state_guard.last_error.as_deref(),
            )?;
        }

        Ok(())
    })
}

fn extract_keyframe(payload: &serde_json::Value, session_id: i64) -> Option<NewKeyframe> {
    let event_payload = payload.get("payload")?.as_object()?;
    let frame_id = event_payload.get("frame_id")?.as_str()?.to_string();
    let relative_path = event_payload.get("path")?.as_str()?.to_string();

    Some(NewKeyframe {
        session_id,
        frame_id,
        relative_path,
        sha256: None,
    })
}

fn wait_for_message(
    rx: &Receiver<BridgeMessage>,
    child: &mut Child,
    diagnostics: Arc<Mutex<BridgeDiagnostics>>,
    expected_kind: &str,
    timeout: Duration,
) -> Result<BridgeMessage, RecorderError> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if let Ok(mut bridge_diagnostics) = diagnostics.lock() {
                bridge_diagnostics.record_exit_status(status);
            }
            return Err(RecorderError::Protocol(format!(
                "helper exited while waiting for {} ({})",
                expected_kind,
                bridge_summary(&diagnostics)
            )));
        }

        let remaining = timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return Err(RecorderError::Protocol(format!(
                "timed out waiting for {} ({})",
                expected_kind,
                bridge_summary(&diagnostics)
            )));
        }

        let recv_timeout = remaining.min(Duration::from_millis(100));
        let message = match rx.recv_timeout(recv_timeout) {
            Ok(message) => message,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(RecorderError::Protocol(format!(
                    "bridge disconnected while waiting for {} ({})",
                    expected_kind,
                    bridge_summary(&diagnostics)
                )))
            }
        };
        if message.kind == expected_kind {
            return Ok(message);
        }
        if message.kind == "error" {
            return Err(RecorderError::Protocol(format!(
                "{} ({})",
                message.payload,
                bridge_summary(&diagnostics)
            )));
        }
        if message.kind == "telemetry" {
            if let Ok(mut bridge_diagnostics) = diagnostics.lock() {
                bridge_diagnostics.push_telemetry(compact_json(&message.payload));
            }
        }
    }
}

fn send_command(stdin: &mut ChildStdin, command: &str) -> Result<(), RecorderError> {
    let payload = serde_json::to_vec(&BridgeCommand { command })?;
    stdin.write_all(&payload)?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn bridge_summary(diagnostics: &Arc<Mutex<BridgeDiagnostics>>) -> String {
    diagnostics
        .lock()
        .map(|bridge_diagnostics| bridge_diagnostics.summary())
        .unwrap_or_else(|_| "bridge diagnostics unavailable".to_string())
}

fn push_bounded(buffer: &mut VecDeque<String>, entry: String) {
    const LIMIT: usize = 8;
    if buffer.len() == LIMIT {
        buffer.pop_front();
    }
    buffer.push_back(entry);
}

fn compact_json(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<invalid json>".to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::process::Command;

    use super::{bridge_summary, push_bounded, BridgeDiagnostics};

    #[test]
    fn bridge_summary_is_bounded_and_informative() {
        let diagnostics = std::sync::Arc::new(std::sync::Mutex::new(BridgeDiagnostics::default()));
        {
            let mut guard = diagnostics.lock().expect("diagnostics lock should succeed");
            for index in 0..10 {
                guard.push_telemetry(format!("telemetry-{}", index));
                guard.push_stderr(format!("stderr-{}", index));
            }
            guard.note_malformed_stdout();
        }

        let summary = bridge_summary(&diagnostics);
        assert!(summary.contains("malformed_stdout_lines=1"));
        assert!(summary.contains("telemetry-9"));
        assert!(!summary.contains("telemetry-0"));
        assert!(summary.contains("stderr-9"));
        assert!(!summary.contains("stderr-0"));
    }

    #[test]
    fn push_bounded_keeps_recent_entries() {
        let mut buffer = VecDeque::new();
        for index in 0..10 {
            push_bounded(&mut buffer, index.to_string());
        }

        let values = buffer.into_iter().collect::<Vec<_>>();
        assert_eq!(values.first().map(String::as_str), Some("2"));
        assert_eq!(values.last().map(String::as_str), Some("9"));
    }

    #[test]
    fn exit_status_summary_includes_code() {
        let status = Command::new("sh")
            .arg("-c")
            .arg("exit 17")
            .status()
            .expect("status should succeed");
        let mut diagnostics = BridgeDiagnostics::default();
        diagnostics.record_exit_status(status);

        assert!(diagnostics.summary().contains("helper exit 17"));
    }
}
