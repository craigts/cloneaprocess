use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

struct RecorderProcess {
    child: Child,
    stdin: ChildStdin,
    ingest_thread: JoinHandle<Result<(), RecorderError>>,
    state: Arc<Mutex<ActiveCapture>>,
}

struct ActiveCapture {
    session_row_id: i64,
    session_external_id: String,
    next_sequence: i64,
    event_count: i64,
    frame_count: i64,
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
            .stderr(Stdio::inherit())
            .spawn()?;

        let stdin = child.stdin.take().ok_or_else(|| RecorderError::Protocol("missing stdin".to_string()))?;
        let stdout = child.stdout.take().ok_or_else(|| RecorderError::Protocol("missing stdout".to_string()))?;
        let events_rx = spawn_bridge_reader(stdout);

        let permissions_message = wait_for_message(&events_rx, "permissions", BRIDGE_START_TIMEOUT)?;
        let permissions: BTreeMap<String, bool> = serde_json::from_value(permissions_message.payload)?;

        let session_message = wait_for_message(&events_rx, "capture_started", BRIDGE_START_TIMEOUT)?;
        let session_payload: CaptureStartedPayload = serde_json::from_value(session_message.payload)?;
        if !session_payload.ok {
            drop(stdin);
            let _ = child.kill();
            let _ = child.wait();
            return Err(RecorderError::Protocol(
                session_payload
                    .error
                    .unwrap_or_else(|| "capture_start_failed".to_string()),
            ));
        }
        let session_external_id = session_payload
            .session_id
            .ok_or_else(|| RecorderError::Protocol("capture_started missing session_id".to_string()))?;
        let started_at_ms = session_payload
            .started_at
            .ok_or_else(|| RecorderError::Protocol("capture_started missing started_at".to_string()))?;
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
            permissions,
        }));
        let ingest_thread = spawn_ingest_thread(self.storage.clone(), state.clone(), events_rx);

        self.process = Some(RecorderProcess {
            child,
            stdin,
            ingest_thread,
            state,
        });

        self.status()
    }

    pub fn stop_capture(&mut self) -> Result<RecorderStatus, RecorderError> {
        let Some(mut process) = self.process.take() else {
            return self.status();
        };

        send_command(&mut process.stdin, "stop")?;
        let _ = process.child.wait();
        match process.ingest_thread.join() {
            Ok(result) => result?,
            Err(_) => {
                return Err(RecorderError::Protocol(
                    "recorder ingest thread panicked".to_string(),
                ))
            }
        }

        let state = process
            .state
            .lock()
            .map_err(|_| RecorderError::Protocol("recorder state mutex poisoned".to_string()))?;

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

fn spawn_bridge_reader(stdout: std::process::ChildStdout) -> Receiver<BridgeMessage> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }

            if let Ok(message) = serde_json::from_str::<BridgeMessage>(&line) {
                let _ = tx.send(message);
            }
        }
    });
    rx
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
            if event_type == "screen_frame" {
                if let Some(keyframe) = extract_keyframe(&message.payload, state_guard.session_row_id) {
                    let _ = storage.insert_keyframe(&keyframe);
                }
                state_guard.frame_count += 1;
            }
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
    expected_kind: &str,
    timeout: Duration,
) -> Result<BridgeMessage, RecorderError> {
    loop {
        let message = rx
            .recv_timeout(timeout)
            .map_err(|_| RecorderError::Protocol(format!("timed out waiting for {}", expected_kind)))?;
        if message.kind == expected_kind {
            return Ok(message);
        }
        if message.kind == "error" {
            return Err(RecorderError::Protocol(message.payload.to_string()));
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
