use std::collections::VecDeque;
use std::fmt::{Display, Formatter};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

#[derive(Debug)]
pub enum RunnerError {
    Io(std::io::Error),
    InvalidProtocol(String),
    Remote {
        code: String,
        message: String,
        retryable: bool,
    },
    Timeout {
        operation: &'static str,
        stderr_tail: String,
    },
    Bridge(String),
}

impl RunnerError {
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Io(_) | Self::Timeout { .. } => true,
            Self::Remote { retryable, .. } => *retryable,
            Self::InvalidProtocol(_) | Self::Bridge(_) => false,
        }
    }
}

impl Display for RunnerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "runner io error: {}", error),
            Self::InvalidProtocol(message) => write!(f, "runner protocol error: {}", message),
            Self::Remote { code, message, .. } => {
                write!(f, "runner remote error {}: {}", code, message)
            }
            Self::Timeout {
                operation,
                stderr_tail,
            } => {
                if stderr_tail.is_empty() {
                    write!(f, "runner {} timed out", operation)
                } else {
                    write!(f, "runner {} timed out: {}", operation, stderr_tail)
                }
            }
            Self::Bridge(message) => write!(f, "runner bridge error: {}", message),
        }
    }
}

impl std::error::Error for RunnerError {}

impl From<std::io::Error> for RunnerError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Clone, Debug)]
pub struct RunnerStepRequest {
    pub workflow_id: String,
    pub outer_run_id: String,
    pub step_index: usize,
    pub attempt: u32,
    pub step: Value,
}

#[derive(Clone, Debug)]
pub struct RunnerStepResult {
    pub result: Value,
}

pub trait RunnerStepExecutor {
    fn execute_step(
        &mut self,
        request: &RunnerStepRequest,
        timeout: Duration,
    ) -> Result<RunnerStepResult, RunnerError>;
}

pub struct RunnerBridge {
    child: Child,
    stdin: ChildStdin,
    stdout_rx: Receiver<String>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
}

impl RunnerBridge {
    pub fn spawn(binary_path: &Path) -> Result<Self, RunnerError> {
        let mut child = Command::new(binary_path)
            .arg("--bridge")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| RunnerError::Bridge("runner stdin unavailable".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RunnerError::Bridge("runner stdout unavailable".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| RunnerError::Bridge("runner stderr unavailable".to_string()))?;

        let (stdout_tx, stdout_rx) = mpsc::channel();
        let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(12)));
        let stderr_tail_reader = Arc::clone(&stderr_tail);

        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) if !line.trim().is_empty() => {
                        if stdout_tx.send(line).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });

        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let Ok(mut tail) = stderr_tail_reader.lock() else {
                    break;
                };
                if tail.len() >= 12 {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        });

        Ok(Self {
            child,
            stdin,
            stdout_rx,
            stderr_tail,
        })
    }

    fn request_id(request: &RunnerStepRequest) -> String {
        format!(
            "req_step_{}_attempt_{}",
            request.step_index, request.attempt
        )
    }

    fn helper_run_id(request: &RunnerStepRequest) -> String {
        format!(
            "{}_step_{}_attempt_{}",
            request.outer_run_id, request.step_index, request.attempt
        )
    }

    fn stderr_summary(&self) -> String {
        let Ok(tail) = self.stderr_tail.lock() else {
            return String::new();
        };
        tail.iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

impl RunnerStepExecutor for RunnerBridge {
    fn execute_step(
        &mut self,
        request: &RunnerStepRequest,
        timeout: Duration,
    ) -> Result<RunnerStepResult, RunnerError> {
        let request_id = Self::request_id(request);
        let request_json = json!({
            "id": request_id,
            "type": "run_workflow",
            "payload": {
                "workflow_id": request.workflow_id,
                "run_id": Self::helper_run_id(request),
                "steps": [request.step.clone()],
            }
        });

        writeln!(self.stdin, "{}", request_json)?;
        self.stdin.flush()?;

        let deadline = Instant::now() + timeout;
        let mut reply_seen = false;
        let mut last_step_result: Option<Value> = None;
        let mut last_step_error: Option<RunnerError> = None;

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(RunnerError::Timeout {
                    operation: "step execution",
                    stderr_tail: self.stderr_summary(),
                });
            }

            let wait_for = remaining.min(Duration::from_millis(50));
            match self.stdout_rx.recv_timeout(wait_for) {
                Ok(line) => {
                    let message: Value = serde_json::from_str(&line).map_err(|error| {
                        RunnerError::InvalidProtocol(format!("invalid json from runner: {}", error))
                    })?;

                    if message.get("id").and_then(Value::as_str) == Some(request_id.as_str()) {
                        reply_seen = true;
                        if !message.get("ok").and_then(Value::as_bool).unwrap_or(false) {
                            return Err(parse_reply_error(&message).unwrap_or_else(|| {
                                RunnerError::InvalidProtocol(
                                    "runner returned an error reply without details".to_string(),
                                )
                            }));
                        }
                        continue;
                    }

                    let Some(event_type) = message.get("type").and_then(Value::as_str) else {
                        continue;
                    };
                    let payload = message.get("payload").cloned().unwrap_or_else(|| json!({}));

                    match event_type {
                        "step_finished" => {
                            if payload.get("ok").and_then(Value::as_bool).unwrap_or(false) {
                                last_step_result = Some(
                                    payload.get("result").cloned().unwrap_or_else(|| json!({})),
                                );
                            } else {
                                last_step_error =
                                    Some(parse_event_error(&payload).unwrap_or_else(|| {
                                        RunnerError::InvalidProtocol(
                                            "runner step_finished event missing error payload"
                                                .to_string(),
                                        )
                                    }));
                            }
                        }
                        "run_completed" => {
                            if !reply_seen {
                                return Err(RunnerError::InvalidProtocol(
                                    "runner completed a step before sending a reply".to_string(),
                                ));
                            }
                            return Ok(RunnerStepResult {
                                result: last_step_result.unwrap_or_else(|| json!({})),
                            });
                        }
                        "run_failed" => {
                            return Err(last_step_error.take().unwrap_or_else(|| {
                                parse_event_error(&payload).unwrap_or_else(|| {
                                    RunnerError::InvalidProtocol(
                                        "runner run_failed event missing error payload".to_string(),
                                    )
                                })
                            }));
                        }
                        _ => {}
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Some(status) = self.child.try_wait()? {
                        return Err(RunnerError::Bridge(format!(
                            "runner helper exited with status {}{}",
                            status,
                            format_stderr_suffix(&self.stderr_summary()),
                        )));
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(RunnerError::Bridge(format!(
                        "runner stdout disconnected{}",
                        format_stderr_suffix(&self.stderr_summary()),
                    )));
                }
            }
        }
    }
}

impl Drop for RunnerBridge {
    fn drop(&mut self) {
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn parse_reply_error(message: &Value) -> Option<RunnerError> {
    parse_error_payload(message.get("error"))
}

fn parse_event_error(payload: &Value) -> Option<RunnerError> {
    parse_error_payload(payload.get("error"))
}

fn parse_error_payload(payload: Option<&Value>) -> Option<RunnerError> {
    let payload = payload?;
    Some(RunnerError::Remote {
        code: payload
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN")
            .to_string(),
        message: payload
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("runner reported an unknown error")
            .to_string(),
        retryable: payload
            .get("retryable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn format_stderr_suffix(stderr_tail: &str) -> String {
    if stderr_tail.is_empty() {
        String::new()
    } else {
        format!(": {}", stderr_tail)
    }
}
