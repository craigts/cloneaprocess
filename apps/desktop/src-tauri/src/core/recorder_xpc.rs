use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr::NonNull;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

#[repr(C)]
struct XpcClientOpaque {
    _private: [u8; 0],
}

type XpcEventCallback = extern "C" fn(*const c_char, *mut c_void);
type XpcReplyCallback = extern "C" fn(*const c_char, *mut c_void);
type XpcErrorCallback = extern "C" fn(c_int, *const c_char, *mut c_void);

#[repr(C)]
#[derive(Clone, Copy)]
pub enum RecorderXpcTransportKind {
    MachService = 0,
    BundledService = 1,
}

unsafe extern "C" {
    fn xpc_client_create(
        service_name: *const c_char,
        connection_kind: RecorderXpcTransportKind,
        on_error: Option<XpcErrorCallback>,
        user_data: *mut c_void,
    ) -> *mut XpcClientOpaque;
    fn xpc_client_connect(client: *mut XpcClientOpaque) -> c_int;
    fn xpc_client_disconnect(client: *mut XpcClientOpaque);
    fn xpc_client_destroy(client: *mut XpcClientOpaque);

    fn xpc_recorder_ping(
        client: *mut XpcClientOpaque,
        cb: Option<XpcReplyCallback>,
        user_data: *mut c_void,
    ) -> c_int;
    fn xpc_recorder_get_permissions(
        client: *mut XpcClientOpaque,
        cb: Option<XpcReplyCallback>,
        user_data: *mut c_void,
    ) -> c_int;
    fn xpc_recorder_begin_capture(
        client: *mut XpcClientOpaque,
        json_config: *const c_char,
        cb: Option<XpcReplyCallback>,
        user_data: *mut c_void,
    ) -> c_int;
    fn xpc_recorder_end_capture(
        client: *mut XpcClientOpaque,
        session_id: *const c_char,
        cb: Option<XpcReplyCallback>,
        user_data: *mut c_void,
    ) -> c_int;
    fn xpc_recorder_subscribe_events(
        client: *mut XpcClientOpaque,
        cb: Option<XpcEventCallback>,
        user_data: *mut c_void,
    ) -> c_int;
    fn xpc_recorder_unsubscribe_events(
        client: *mut XpcClientOpaque,
        cb: Option<XpcReplyCallback>,
        user_data: *mut c_void,
    ) -> c_int;
}

#[derive(Debug)]
pub enum RecorderXpcError {
    InvalidArgs(&'static str),
    NotConnected,
    SendFailed,
    Timeout(&'static str),
    Bridge(String),
    Json(serde_json::Error),
    Nul(std::ffi::NulError),
}

impl std::fmt::Display for RecorderXpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidArgs(op) => write!(f, "invalid args for {}", op),
            Self::NotConnected => write!(f, "xpc client is not connected"),
            Self::SendFailed => write!(f, "xpc send failed"),
            Self::Timeout(op) => write!(f, "timed out waiting for {}", op),
            Self::Bridge(message) => write!(f, "{}", message),
            Self::Json(error) => write!(f, "json error: {}", error),
            Self::Nul(error) => write!(f, "nul error: {}", error),
        }
    }
}

impl std::error::Error for RecorderXpcError {}

impl From<serde_json::Error> for RecorderXpcError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<std::ffi::NulError> for RecorderXpcError {
    fn from(value: std::ffi::NulError) -> Self {
        Self::Nul(value)
    }
}

struct ErrorState {
    tx: Sender<String>,
}

struct ReplyState {
    tx: Sender<String>,
}

struct EventState {
    tx: Sender<Value>,
}

pub struct RecorderXpcClient {
    raw: NonNull<XpcClientOpaque>,
    error_rx: Mutex<Receiver<String>>,
    error_state_ptr: *mut ErrorState,
    event_state_ptr: Option<*mut EventState>,
}

unsafe impl Send for RecorderXpcClient {}

impl RecorderXpcClient {
    pub fn connect(
        service_name: &str,
        transport_kind: RecorderXpcTransportKind,
    ) -> Result<Self, RecorderXpcError> {
        let (error_tx, error_rx) = mpsc::channel();
        let error_state = Box::new(ErrorState { tx: error_tx });
        let error_state_ptr = Box::into_raw(error_state);
        let service_name = CString::new(service_name)?;
        let raw = unsafe {
            xpc_client_create(
                service_name.as_ptr(),
                transport_kind,
                Some(xpc_error_callback),
                error_state_ptr.cast(),
            )
        };
        let Some(raw) = NonNull::new(raw) else {
            unsafe {
                drop(Box::from_raw(error_state_ptr));
            }
            return Err(RecorderXpcError::InvalidArgs("xpc_client_create"));
        };

        let client = Self {
            raw,
            error_rx: Mutex::new(error_rx),
            error_state_ptr,
            event_state_ptr: None,
        };

        let code = unsafe { xpc_client_connect(client.raw.as_ptr()) };
        if code != 0 {
            return Err(map_return_code("xpc_client_connect", code));
        }

        Ok(client)
    }

    pub fn ping(&self, timeout: Duration) -> Result<String, RecorderXpcError> {
        self.call_reply(timeout, "recorder ping", |cb, user_data| unsafe {
            xpc_recorder_ping(self.raw.as_ptr(), cb, user_data)
        })
    }

    pub fn get_permissions(&self, timeout: Duration) -> Result<String, RecorderXpcError> {
        self.call_reply(timeout, "recorder getPermissions", |cb, user_data| unsafe {
            xpc_recorder_get_permissions(self.raw.as_ptr(), cb, user_data)
        })
    }

    pub fn begin_capture(
        &self,
        config: &Value,
        timeout: Duration,
    ) -> Result<String, RecorderXpcError> {
        let config = CString::new(serde_json::to_string(config)?)?;
        self.call_reply(
            timeout,
            "recorder beginCapture",
            move |cb, user_data| unsafe {
                xpc_recorder_begin_capture(self.raw.as_ptr(), config.as_ptr(), cb, user_data)
            },
        )
    }

    pub fn end_capture(
        &self,
        session_id: &str,
        timeout: Duration,
    ) -> Result<String, RecorderXpcError> {
        let session_id = CString::new(session_id)?;
        self.call_reply(
            timeout,
            "recorder endCapture",
            move |cb, user_data| unsafe {
                xpc_recorder_end_capture(self.raw.as_ptr(), session_id.as_ptr(), cb, user_data)
            },
        )
    }

    pub fn subscribe_events(&mut self) -> Result<Receiver<Value>, RecorderXpcError> {
        if self.event_state_ptr.is_some() {
            return Err(RecorderXpcError::Bridge(
                "recorder events already subscribed".to_string(),
            ));
        }

        let (tx, rx) = mpsc::channel();
        let event_state = Box::new(EventState { tx });
        let event_state_ptr = Box::into_raw(event_state);
        let code = unsafe {
            xpc_recorder_subscribe_events(
                self.raw.as_ptr(),
                Some(xpc_event_callback),
                event_state_ptr.cast(),
            )
        };
        if code != 0 {
            unsafe {
                drop(Box::from_raw(event_state_ptr));
            }
            return Err(map_return_code("recorder subscribeEvents", code));
        }

        self.event_state_ptr = Some(event_state_ptr);
        Ok(rx)
    }

    pub fn unsubscribe_events(&mut self, timeout: Duration) -> Result<String, RecorderXpcError> {
        let result = self.call_reply(
            timeout,
            "recorder unsubscribeEvents",
            |cb, user_data| unsafe {
                xpc_recorder_unsubscribe_events(self.raw.as_ptr(), cb, user_data)
            },
        );

        if let Some(event_state_ptr) = self.event_state_ptr.take() {
            unsafe {
                drop(Box::from_raw(event_state_ptr));
            }
        }

        result
    }

    fn call_reply<F>(
        &self,
        timeout: Duration,
        op: &'static str,
        invoke: F,
    ) -> Result<String, RecorderXpcError>
    where
        F: FnOnce(Option<XpcReplyCallback>, *mut c_void) -> c_int,
    {
        self.clear_pending_errors();
        let (reply_tx, reply_rx) = mpsc::channel();
        let reply_state_ptr = Box::into_raw(Box::new(ReplyState { tx: reply_tx }));
        let code = invoke(Some(xpc_reply_callback), reply_state_ptr.cast());
        if code != 0 {
            unsafe {
                drop(Box::from_raw(reply_state_ptr));
            }
            return Err(map_return_code(op, code));
        }

        let deadline = Instant::now() + timeout;
        loop {
            match reply_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(reply) => return Ok(reply),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if let Some(error) = self.try_take_error() {
                        return Err(RecorderXpcError::Bridge(error));
                    }
                    return Err(RecorderXpcError::Bridge(format!(
                        "{} reply callback disconnected",
                        op
                    )));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Some(error) = self.try_take_error() {
                        return Err(RecorderXpcError::Bridge(error));
                    }
                    if Instant::now() >= deadline {
                        return Err(RecorderXpcError::Timeout(op));
                    }
                }
            }
        }
    }

    fn clear_pending_errors(&self) {
        let Ok(rx) = self.error_rx.lock() else {
            return;
        };
        while rx.try_recv().is_ok() {}
    }

    fn try_take_error(&self) -> Option<String> {
        let Ok(rx) = self.error_rx.lock() else {
            return Some("xpc error channel poisoned".to_string());
        };
        rx.try_recv().ok()
    }
}

impl Drop for RecorderXpcClient {
    fn drop(&mut self) {
        if let Some(event_state_ptr) = self.event_state_ptr.take() {
            unsafe {
                drop(Box::from_raw(event_state_ptr));
            }
        }
        unsafe {
            xpc_client_disconnect(self.raw.as_ptr());
            xpc_client_destroy(self.raw.as_ptr());
            drop(Box::from_raw(self.error_state_ptr));
        }
    }
}

extern "C" fn xpc_error_callback(code: c_int, message: *const c_char, user_data: *mut c_void) {
    if user_data.is_null() {
        return;
    }
    let state = unsafe { &*(user_data.cast::<ErrorState>()) };
    let message = c_string_to_owned(message).unwrap_or_default();
    let _ = state.tx.send(format!("xpc error {}: {}", code, message));
}

extern "C" fn xpc_reply_callback(reply_json: *const c_char, user_data: *mut c_void) {
    if user_data.is_null() {
        return;
    }
    let state = unsafe { Box::from_raw(user_data.cast::<ReplyState>()) };
    let reply = c_string_to_owned(reply_json).unwrap_or_else(|| "{}".to_string());
    let _ = state.tx.send(reply);
}

extern "C" fn xpc_event_callback(event_json: *const c_char, user_data: *mut c_void) {
    if user_data.is_null() {
        return;
    }
    let state = unsafe { &*(user_data.cast::<EventState>()) };
    let Some(event_json) = c_string_to_owned(event_json) else {
        return;
    };
    let Ok(event) = serde_json::from_str::<Value>(&event_json) else {
        return;
    };
    let _ = state.tx.send(event);
}

fn c_string_to_owned(value: *const c_char) -> Option<String> {
    if value.is_null() {
        return None;
    }
    Some(
        unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned(),
    )
}

fn map_return_code(op: &'static str, code: c_int) -> RecorderXpcError {
    match code {
        -1 => RecorderXpcError::InvalidArgs(op),
        -2 => RecorderXpcError::NotConnected,
        -3 => RecorderXpcError::SendFailed,
        _ => RecorderXpcError::Bridge(format!("{} failed with code {}", op, code)),
    }
}

#[cfg(test)]
mod tests {
    use super::RecorderXpcClient;
    use super::RecorderXpcTransportKind;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::Duration;

    fn packaged_app_executable() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("release")
            .join("bundle")
            .join("macos")
            .join("Clone a Process.app")
            .join("Contents")
            .join("MacOS")
            .join("cloneaprocess-desktop")
    }

    #[test]
    #[ignore = "requires a running recorder XPC mach service"]
    fn dev_xpc_client_can_ping_and_fetch_permissions() {
        let service_name = std::env::var("CLONEAPROCESS_RECORDER_XPC_SERVICE")
            .unwrap_or_else(|_| "com.cloneaprocess.recorder.dev".to_string());
        let client =
            RecorderXpcClient::connect(&service_name, RecorderXpcTransportKind::MachService)
                .expect("xpc client should connect");

        let ping = client
            .ping(Duration::from_secs(3))
            .expect("ping should succeed");
        let handshake: serde_json::Value =
            serde_json::from_str(&ping).expect("ping should return json");
        assert_eq!(
            handshake
                .get("protocol_version")
                .and_then(|value| value.as_u64()),
            Some(1)
        );

        let permissions = client
            .get_permissions(Duration::from_secs(3))
            .expect("permissions should succeed");
        let permissions: BTreeMap<String, bool> =
            serde_json::from_str(&permissions).expect("permissions should deserialize");
        assert!(permissions.contains_key("accessibility"));
        assert!(permissions.contains_key("screenRecording"));
    }

    #[test]
    #[ignore = "requires a packaged desktop app bundle and permission to launch the bundled XPC service"]
    fn packaged_app_probe_can_ping_bundled_recorder_service() {
        let executable = packaged_app_executable();
        assert!(
            executable.exists(),
            "packaged app executable missing at {}",
            executable.display()
        );

        let output = Command::new(&executable)
            .arg("--probe-recorder-xpc")
            .env("CLONEAPROCESS_RECORDER_TRANSPORT", "xpc_bundle_service")
            .env(
                "CLONEAPROCESS_RECORDER_XPC_SERVICE",
                "com.cloneaprocess.recorder",
            )
            .output()
            .expect("probe command should launch");

        assert!(
            output.status.success(),
            "probe failed: {}",
            String::from_utf8_lossy(&output.stdout)
        );

        let payload: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("probe output should be json");
        assert_eq!(
            payload.get("ok").and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            payload.get("transport").and_then(|value| value.as_str()),
            Some("xpc_bundled_service")
        );
        assert_eq!(
            payload
                .get("protocol")
                .and_then(|value| value.get("protocol_version"))
                .and_then(|value| value.as_u64()),
            Some(1)
        );
        assert!(payload
            .get("permissions")
            .and_then(|value| value.get("accessibility"))
            .is_some());
    }
}
