use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::{sleep, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::core::recorder_xpc::{RecorderXpcClient, RecorderXpcTransportKind};
use crate::core::trace::{normalize_raw_event, NormalizedRawEvent};
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
            Self::BinaryNotFound(path) => {
                write!(f, "recorder binary not found at {}", path.display())
            }
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
    pub transport_mode: RecorderTransportMode,
    pub transport_target: String,
    pub transport_ready: bool,
    pub transport_error: Option<String>,
    pub protocol_version: Option<u32>,
    pub protocol_min: Option<u32>,
    pub protocol_capabilities: Vec<String>,
}

pub struct RecorderCoordinator {
    storage: Storage,
    transport: RecorderTransportConfig,
    session: Option<ActiveRecorderSession>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecorderTransportMode {
    SubprocessBridge,
    XpcMachService,
    XpcBundledService,
}

#[derive(Clone, Debug)]
pub enum RecorderTransportConfig {
    SubprocessBridge { binary_path: PathBuf },
    XpcMachService { service_name: String },
    XpcBundledService { service_name: String },
}

impl RecorderTransportConfig {
    pub fn subprocess_bridge(binary_path: PathBuf) -> Self {
        Self::SubprocessBridge { binary_path }
    }

    pub fn xpc_service(service_name: String) -> Self {
        Self::XpcMachService { service_name }
    }

    pub fn xpc_bundle_service(service_name: String) -> Self {
        Self::XpcBundledService { service_name }
    }

    fn mode(&self) -> RecorderTransportMode {
        match self {
            Self::SubprocessBridge { .. } => RecorderTransportMode::SubprocessBridge,
            Self::XpcMachService { .. } => RecorderTransportMode::XpcMachService,
            Self::XpcBundledService { .. } => RecorderTransportMode::XpcBundledService,
        }
    }

    fn target(&self) -> String {
        match self {
            Self::SubprocessBridge { binary_path } => binary_path.display().to_string(),
            Self::XpcMachService { service_name } => service_name.clone(),
            Self::XpcBundledService { service_name } => service_name.clone(),
        }
    }

    fn recorder_binary(&self) -> String {
        match self {
            Self::SubprocessBridge { binary_path } => binary_path.display().to_string(),
            Self::XpcMachService { .. } => String::new(),
            Self::XpcBundledService { .. } => String::new(),
        }
    }

    fn is_ready(&self) -> bool {
        match self {
            Self::SubprocessBridge { binary_path } => binary_path.exists(),
            Self::XpcMachService { .. } => false,
            Self::XpcBundledService { .. } => false,
        }
    }

    fn xpc_transport_kind(&self) -> Option<RecorderXpcTransportKind> {
        match self {
            Self::SubprocessBridge { .. } => None,
            Self::XpcMachService { .. } => Some(RecorderXpcTransportKind::MachService),
            Self::XpcBundledService { .. } => Some(RecorderXpcTransportKind::BundledService),
        }
    }
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

struct XpcRecorderSession {
    client: RecorderXpcClient,
    ingest_thread: JoinHandle<Result<(), RecorderError>>,
    state: Arc<Mutex<ActiveCapture>>,
}

enum ActiveRecorderSession {
    Subprocess(RecorderProcess),
    Xpc(XpcRecorderSession),
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

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
struct RecorderProtocolHandshake {
    protocol_version: u32,
    protocol_min: u32,
    capabilities: Vec<String>,
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
            parts.push(format!(
                "malformed_stdout_lines={}",
                self.malformed_stdout_lines
            ));
        }
        if !self.telemetry_tail.is_empty() {
            parts.push(format!(
                "telemetry=[{}]",
                self.telemetry_tail
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" | ")
            ));
        }
        if !self.stderr_tail.is_empty() {
            parts.push(format!(
                "stderr=[{}]",
                self.stderr_tail
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" | ")
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
    pub fn new(storage: Storage, transport: RecorderTransportConfig) -> Self {
        Self {
            storage,
            transport,
            session: None,
        }
    }

    pub fn status(&mut self) -> Result<RecorderStatus, RecorderError> {
        let (permissions, permissions_error) = match (&self.transport, &self.session) {
            (_, Some(session)) => (
                active_session_state(session)?
                    .lock()
                    .map_err(|_| {
                        RecorderError::Protocol("recorder state mutex poisoned".to_string())
                    })?
                    .permissions
                    .clone(),
                None,
            ),
            (RecorderTransportConfig::SubprocessBridge { .. }, None) => (self.permissions()?, None),
            (
                RecorderTransportConfig::XpcMachService { .. }
                | RecorderTransportConfig::XpcBundledService { .. },
                None,
            ) => match self.permissions() {
                Ok(permissions) => (permissions, None),
                Err(error) => (BTreeMap::new(), Some(error.to_string())),
            },
        };
        let (protocol_handshake, handshake_error) = match self.protocol_handshake() {
            Ok(handshake) => (Some(handshake), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let transport_mode = self.transport.mode();
        let transport_target = self.transport.target();
        let transport_ready = match &self.transport {
            RecorderTransportConfig::SubprocessBridge { .. } => self.transport.is_ready(),
            RecorderTransportConfig::XpcMachService { .. }
            | RecorderTransportConfig::XpcBundledService { .. } => protocol_handshake.is_some(),
        };
        let transport_error = handshake_error.or(permissions_error);
        let recorder_binary = self.transport.recorder_binary();

        Ok(match &self.session {
            Some(session) => {
                let state = active_session_state(session)?.lock().map_err(|_| {
                    RecorderError::Protocol("recorder state mutex poisoned".to_string())
                })?;

                RecorderStatus {
                    active: true,
                    session_external_id: Some(state.session_external_id.clone()),
                    session_row_id: Some(state.session_row_id),
                    event_count: state.event_count,
                    frame_count: state.frame_count,
                    permissions,
                    recorder_binary,
                    transport_mode,
                    transport_target,
                    transport_ready,
                    transport_error,
                    protocol_version: protocol_handshake
                        .as_ref()
                        .map(|handshake| handshake.protocol_version),
                    protocol_min: protocol_handshake
                        .as_ref()
                        .map(|handshake| handshake.protocol_min),
                    protocol_capabilities: protocol_handshake
                        .as_ref()
                        .map(|handshake| handshake.capabilities.clone())
                        .unwrap_or_default(),
                }
            }
            None => RecorderStatus {
                active: false,
                session_external_id: None,
                session_row_id: None,
                event_count: 0,
                frame_count: 0,
                permissions,
                recorder_binary,
                transport_mode,
                transport_target,
                transport_ready,
                transport_error,
                protocol_version: protocol_handshake
                    .as_ref()
                    .map(|handshake| handshake.protocol_version),
                protocol_min: protocol_handshake
                    .as_ref()
                    .map(|handshake| handshake.protocol_min),
                protocol_capabilities: protocol_handshake
                    .as_ref()
                    .map(|handshake| handshake.capabilities.clone())
                    .unwrap_or_default(),
            },
        })
    }

    pub fn start_capture(&mut self) -> Result<RecorderStatus, RecorderError> {
        if self.session.is_some() {
            return self.status();
        }

        match &self.transport {
            RecorderTransportConfig::SubprocessBridge { binary_path } => {
                if !binary_path.exists() {
                    return Err(RecorderError::BinaryNotFound(binary_path.clone()));
                }
                self.start_subprocess_capture(binary_path.clone())?;
            }
            RecorderTransportConfig::XpcMachService { service_name } => {
                self.start_xpc_capture(
                    service_name.clone(),
                    RecorderXpcTransportKind::MachService,
                )?;
            }
            RecorderTransportConfig::XpcBundledService { service_name } => {
                self.start_xpc_capture(
                    service_name.clone(),
                    RecorderXpcTransportKind::BundledService,
                )?;
            }
        }

        self.status()
    }

    pub fn stop_capture(&mut self) -> Result<RecorderStatus, RecorderError> {
        let Some(session) = self.session.take() else {
            return self.status();
        };

        let stopped_status = match session {
            ActiveRecorderSession::Subprocess(process) => self.stop_subprocess_capture(process)?,
            ActiveRecorderSession::Xpc(session) => self.stop_xpc_capture(session)?,
        };

        Ok(stopped_status)
    }

    fn start_subprocess_capture(&mut self, binary_path: PathBuf) -> Result<(), RecorderError> {
        let mut child = Command::new(&binary_path)
            .arg("--bridge")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| RecorderError::Protocol("missing stdin".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RecorderError::Protocol("missing stdout".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| RecorderError::Protocol("missing stderr".to_string()))?;
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
        let permissions: BTreeMap<String, bool> =
            serde_json::from_value(permissions_message.payload)?;

        let session_message = wait_for_message(
            &events_rx,
            &mut child,
            diagnostics.clone(),
            "capture_started",
            BRIDGE_START_TIMEOUT,
        )?;
        let session_payload: CaptureStartedPayload =
            serde_json::from_value(session_message.payload)?;
        if !session_payload.ok {
            drop(stdin);
            let _ = child.kill();
            let _ = child.wait();
            return Err(RecorderError::Protocol(format!(
                "{} ({})",
                session_payload
                    .error
                    .unwrap_or_else(|| "capture_start_failed".to_string()),
                bridge_summary(&diagnostics)
            )));
        }
        let (state, ingest_thread) = self.build_active_capture(
            "Bridge capture".to_string(),
            permissions,
            session_payload,
            wrap_bridge_events(events_rx),
        )?;

        self.session = Some(ActiveRecorderSession::Subprocess(RecorderProcess {
            child,
            stdin,
            ingest_thread,
            stderr_thread,
            diagnostics,
            state,
        }));
        Ok(())
    }

    fn start_xpc_capture(
        &mut self,
        service_name: String,
        transport_kind: RecorderXpcTransportKind,
    ) -> Result<(), RecorderError> {
        let mut client = RecorderXpcClient::connect(&service_name, transport_kind)
            .map_err(|error| RecorderError::Protocol(error.to_string()))?;

        let _ = client
            .ping(BRIDGE_START_TIMEOUT)
            .map_err(|error| RecorderError::Protocol(error.to_string()))?;
        let permissions: BTreeMap<String, bool> = serde_json::from_str(
            &client
                .get_permissions(BRIDGE_START_TIMEOUT)
                .map_err(|error| RecorderError::Protocol(error.to_string()))?,
        )?;

        let events_rx = client
            .subscribe_events()
            .map_err(|error| RecorderError::Protocol(error.to_string()))?;
        let session_payload: CaptureStartedPayload = serde_json::from_str(
            &client
                .begin_capture(&json!({}), BRIDGE_START_TIMEOUT)
                .map_err(|error| RecorderError::Protocol(error.to_string()))?,
        )?;
        if !session_payload.ok {
            let _ = client.unsubscribe_events(BRIDGE_STOP_TIMEOUT);
            return Err(RecorderError::Protocol(
                session_payload
                    .error
                    .unwrap_or_else(|| "capture_start_failed".to_string()),
            ));
        }

        let (state, ingest_thread) = self.build_active_capture(
            "XPC capture".to_string(),
            permissions,
            session_payload,
            events_rx,
        )?;

        self.session = Some(ActiveRecorderSession::Xpc(XpcRecorderSession {
            client,
            ingest_thread,
            state,
        }));
        Ok(())
    }

    fn build_active_capture(
        &self,
        label: String,
        permissions: BTreeMap<String, bool>,
        session_payload: CaptureStartedPayload,
        events_rx: Receiver<Value>,
    ) -> Result<
        (
            Arc<Mutex<ActiveCapture>>,
            JoinHandle<Result<(), RecorderError>>,
        ),
        RecorderError,
    > {
        let session_external_id = session_payload.session_id.ok_or_else(|| {
            RecorderError::Protocol("capture_started missing session_id".to_string())
        })?;
        let started_at_ms = session_payload.started_at.ok_or_else(|| {
            RecorderError::Protocol("capture_started missing started_at".to_string())
        })?;
        let session_row_id = self.storage.insert_session(&NewSession {
            external_id: session_external_id.clone(),
            label: Some(label),
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
        Ok((state, ingest_thread))
    }

    fn stop_subprocess_capture(
        &self,
        mut process: RecorderProcess,
    ) -> Result<RecorderStatus, RecorderError> {
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

        self.build_stopped_status(&state)
    }

    fn stop_xpc_capture(
        &self,
        mut session: XpcRecorderSession,
    ) -> Result<RecorderStatus, RecorderError> {
        let session_external_id = session
            .state
            .lock()
            .map_err(|_| RecorderError::Protocol("recorder state mutex poisoned".to_string()))?
            .session_external_id
            .clone();

        let _reply = session
            .client
            .end_capture(&session_external_id, BRIDGE_STOP_TIMEOUT)
            .map_err(|error| RecorderError::Protocol(error.to_string()))?;
        let _ = session.client.unsubscribe_events(BRIDGE_STOP_TIMEOUT);

        match session.ingest_thread.join() {
            Ok(result) => result?,
            Err(_) => {
                return Err(RecorderError::Protocol(
                    "recorder ingest thread panicked".to_string(),
                ))
            }
        }

        let state = session
            .state
            .lock()
            .map_err(|_| RecorderError::Protocol("recorder state mutex poisoned".to_string()))?;

        self.build_stopped_status(&state)
    }

    fn build_stopped_status(&self, state: &ActiveCapture) -> Result<RecorderStatus, RecorderError> {
        self.storage
            .complete_session(state.session_row_id, now_ms())
            .map_err(RecorderError::Storage)?;

        let protocol_handshake = self.protocol_handshake().ok();
        let transport_ready = match &self.transport {
            RecorderTransportConfig::SubprocessBridge { .. } => self.transport.is_ready(),
            RecorderTransportConfig::XpcMachService { .. }
            | RecorderTransportConfig::XpcBundledService { .. } => protocol_handshake.is_some(),
        };

        Ok(RecorderStatus {
            active: false,
            session_external_id: Some(state.session_external_id.clone()),
            session_row_id: Some(state.session_row_id),
            event_count: state.event_count,
            frame_count: state.frame_count,
            permissions: state.permissions.clone(),
            recorder_binary: self.transport.recorder_binary(),
            transport_mode: self.transport.mode(),
            transport_target: self.transport.target(),
            transport_ready,
            transport_error: None,
            protocol_version: protocol_handshake
                .as_ref()
                .map(|handshake| handshake.protocol_version),
            protocol_min: protocol_handshake
                .as_ref()
                .map(|handshake| handshake.protocol_min),
            protocol_capabilities: protocol_handshake
                .map(|handshake| handshake.capabilities)
                .unwrap_or_default(),
        })
    }

    fn permissions(&self) -> Result<BTreeMap<String, bool>, RecorderError> {
        match &self.transport {
            RecorderTransportConfig::SubprocessBridge { binary_path } => {
                if !binary_path.exists() {
                    return Ok(BTreeMap::new());
                }

                let output = Command::new(binary_path)
                    .arg("--permissions-json")
                    .output()?;
                if !output.status.success() {
                    return Err(RecorderError::Protocol(
                        "failed to read recorder permissions".to_string(),
                    ));
                }

                let permissions = serde_json::from_slice::<BTreeMap<String, bool>>(&output.stdout)?;
                Ok(permissions)
            }
            RecorderTransportConfig::XpcMachService { service_name }
            | RecorderTransportConfig::XpcBundledService { service_name } => {
                let client = RecorderXpcClient::connect(
                    service_name,
                    self.transport
                        .xpc_transport_kind()
                        .expect("xpc transport kind"),
                )
                .map_err(|error| RecorderError::Protocol(error.to_string()))?;
                let permissions = serde_json::from_str::<BTreeMap<String, bool>>(
                    &client
                        .get_permissions(BRIDGE_START_TIMEOUT)
                        .map_err(|error| RecorderError::Protocol(error.to_string()))?,
                )?;
                Ok(permissions)
            }
        }
    }

    fn protocol_handshake(&self) -> Result<RecorderProtocolHandshake, RecorderError> {
        match &self.transport {
            RecorderTransportConfig::SubprocessBridge { binary_path } => {
                if !binary_path.exists() {
                    return Err(RecorderError::BinaryNotFound(binary_path.clone()));
                }

                let output = Command::new(binary_path).arg("--protocol-json").output()?;
                if !output.status.success() {
                    return Err(RecorderError::Protocol(
                        "failed to read recorder protocol metadata".to_string(),
                    ));
                }

                let handshake =
                    serde_json::from_slice::<RecorderProtocolHandshake>(&output.stdout)?;
                Ok(handshake)
            }
            RecorderTransportConfig::XpcMachService { service_name }
            | RecorderTransportConfig::XpcBundledService { service_name } => {
                let client = RecorderXpcClient::connect(
                    service_name,
                    self.transport
                        .xpc_transport_kind()
                        .expect("xpc transport kind"),
                )
                .map_err(|error| RecorderError::Protocol(error.to_string()))?;
                let handshake = serde_json::from_str::<RecorderProtocolHandshake>(
                    &client
                        .ping(BRIDGE_START_TIMEOUT)
                        .map_err(|error| RecorderError::Protocol(error.to_string()))?,
                )?;
                Ok(handshake)
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct CaptureStartedPayload {
    ok: bool,
    session_id: Option<String>,
    started_at: Option<u64>,
    error: Option<String>,
}

fn spawn_bridge_reader(
    stdout: ChildStdout,
    diagnostics: Arc<Mutex<BridgeDiagnostics>>,
) -> Receiver<BridgeMessage> {
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

fn wrap_bridge_events(rx: Receiver<BridgeMessage>) -> Receiver<Value> {
    let (tx, wrapped_rx) = mpsc::channel();
    thread::spawn(move || {
        while let Ok(message) = rx.recv() {
            if message.kind != "event" {
                continue;
            }
            let _ = tx.send(message.payload);
        }
    });
    wrapped_rx
}

fn spawn_stderr_reader(
    stderr: ChildStderr,
    diagnostics: Arc<Mutex<BridgeDiagnostics>>,
) -> JoinHandle<()> {
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
    rx: Receiver<Value>,
) -> JoinHandle<Result<(), RecorderError>> {
    thread::spawn(move || {
        while let Ok(payload) = rx.recv() {
            let normalized =
                normalize_raw_event(None, &payload, now_ms()).map_err(RecorderError::Protocol)?;
            let event_type = normalized.event_type.clone();
            let event_json = normalized.event_json.clone();
            let bridge_error_message = if event_type == "bridge_error" {
                serde_json::from_str::<serde_json::Value>(&event_json)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("payload")
                            .and_then(|payload| payload.get("message"))
                            .and_then(|value| value.as_str())
                            .map(ToOwned::to_owned)
                    })
            } else {
                None
            };

            let mut state_guard = state.lock().map_err(|_| {
                RecorderError::Protocol("recorder state mutex poisoned".to_string())
            })?;

            storage.insert_raw_event(&NewRawEvent {
                session_id: state_guard.session_row_id,
                sequence: state_guard.next_sequence,
                event_type: event_type.clone(),
                event_json,
                recorded_at_ms: normalized.recorded_at_ms,
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
                if let Some(keyframe) = extract_keyframe(&normalized, state_guard.session_row_id) {
                    let _ = storage.insert_keyframe(&keyframe);
                }
                state_guard.frame_count += 1;
            }
            if event_type == "bridge_error" {
                state_guard.last_error = bridge_error_message;
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

fn active_session_state(
    session: &ActiveRecorderSession,
) -> Result<&Arc<Mutex<ActiveCapture>>, RecorderError> {
    Ok(match session {
        ActiveRecorderSession::Subprocess(process) => &process.state,
        ActiveRecorderSession::Xpc(session) => &session.state,
    })
}

fn extract_keyframe(event: &NormalizedRawEvent, session_id: i64) -> Option<NewKeyframe> {
    let payload: serde_json::Value = serde_json::from_str(&event.event_json).ok()?;
    let event_payload = payload.get("payload")?.as_object()?;
    let frame_id = event_payload.get("frameId")?.as_str()?.to_string();
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

    use serde_json::json;

    use super::{
        bridge_summary, push_bounded, BridgeDiagnostics, RecorderProtocolHandshake,
        RecorderTransportConfig, RecorderTransportMode,
    };

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

    #[test]
    fn protocol_handshake_deserializes_expected_shape() {
        let handshake: RecorderProtocolHandshake = serde_json::from_value(json!({
            "protocol_version": 1,
            "protocol_min": 1,
            "capabilities": ["event_stream", "permissions"],
        }))
        .expect("handshake should deserialize");

        assert_eq!(handshake.protocol_version, 1);
        assert_eq!(handshake.protocol_min, 1);
        assert_eq!(handshake.capabilities, vec!["event_stream", "permissions"]);
    }

    #[test]
    fn xpc_transport_reports_expected_identity() {
        let transport =
            RecorderTransportConfig::xpc_service("com.cloneaprocess.recorder.dev".to_string());
        let bundled_transport =
            RecorderTransportConfig::xpc_bundle_service("com.cloneaprocess.recorder".to_string());

        assert_eq!(transport.mode(), RecorderTransportMode::XpcMachService);
        assert_eq!(transport.target(), "com.cloneaprocess.recorder.dev");
        assert!(!transport.is_ready());
        assert!(transport.recorder_binary().is_empty());

        assert_eq!(
            bundled_transport.mode(),
            RecorderTransportMode::XpcBundledService
        );
        assert_eq!(bundled_transport.target(), "com.cloneaprocess.recorder");
        assert!(!bundled_transport.is_ready());
        assert!(bundled_transport.recorder_binary().is_empty());
    }
}
