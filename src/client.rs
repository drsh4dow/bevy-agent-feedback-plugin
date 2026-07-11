//! Rust client for the v3 agent feedback protocol.

#[cfg(test)]
use crate::session::PROTOCOL_VERSION;
use crate::session::unix_ms;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    error::Error,
    fmt::{self, Display, Formatter},
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Client configuration.
#[derive(Clone, Debug)]
pub struct AgentClientConfig {
    /// Protocol file written by the Bevy plugin.
    pub protocol_file: PathBuf,
    /// TCP connect/read/write timeout.
    pub timeout: Duration,
    /// Optional replayable transcript path.
    pub transcript_file: Option<PathBuf>,
    /// OCR settings used by `ocr_*` and text assertions.
    pub ocr: OcrOptions,
}

impl Default for AgentClientConfig {
    fn default() -> Self {
        Self {
            protocol_file: std::env::var_os("BEVY_FEEDBACK_PROTOCOL")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("target/agent-feedback/agent-feedback.json")),
            timeout: Duration::from_secs(10),
            transcript_file: std::env::var_os("BEVY_FEEDBACK_TRANSCRIPT").map(PathBuf::from),
            ocr: OcrOptions::default(),
        }
    }
}

/// Tesseract OCR settings.
#[derive(Clone, Debug)]
pub struct OcrOptions {
    /// Path to the `tesseract` executable.
    pub tesseract: PathBuf,
    /// Tesseract language code, for example `eng`.
    pub language: String,
    /// OCR subprocess timeout.
    pub timeout: Duration,
}

impl Default for OcrOptions {
    fn default() -> Self {
        Self {
            tesseract: std::env::var_os("BEVY_FEEDBACK_TESSERACT")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("tesseract")),
            language: "eng".to_string(),
            timeout: Duration::from_secs(5),
        }
    }
}

/// Completion state for a captured PNG.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureCompletion {
    /// Bevy emitted `ScreenshotCaptured` after render readback.
    ScreenshotCaptured,
}

/// Stable primary-window mode recorded with a capture.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureWindowMode {
    /// A regular window.
    Windowed,
    /// Borderless fullscreen.
    BorderlessFullscreen,
    /// Exclusive fullscreen.
    Fullscreen,
}

/// Primary-window metadata recorded at capture request or completion time.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CaptureWindowInfo {
    /// Logical width used by input coordinates.
    pub logical_width: f32,
    /// Logical height used by input coordinates.
    pub logical_height: f32,
    /// Physical width in PNG pixels.
    pub physical_width: u32,
    /// Physical height in PNG pixels.
    pub physical_height: u32,
    /// Physical-to-logical scale factor.
    pub scale_factor: f32,
    /// Logical cursor position, when available.
    pub cursor_position: Option<[f32; 2]>,
    /// Whether the window was focused.
    pub focused: bool,
    /// Whether the window was visible.
    pub visible: bool,
    /// Window presentation mode.
    pub mode: CaptureWindowMode,
}

/// A captured PNG returned by the plugin.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Capture {
    /// Monotonic capture sequence number.
    pub sequence: u64,
    /// PNG path on disk.
    pub path: PathBuf,
    /// Optional capture label echoed by the plugin.
    pub label: Option<String>,
    /// Plugin app-update counter when the request was admitted.
    pub requested_frame: u64,
    /// Plugin app-update counter when render readback completed.
    pub completed_frame: u64,
    /// Captured PNG width in physical pixels.
    pub image_width: u32,
    /// Captured PNG height in physical pixels.
    pub image_height: u32,
    /// Primary-window metadata retained from request admission.
    pub window_at_request: CaptureWindowInfo,
    /// Primary-window metadata at completion, if the window still existed.
    pub window_at_completion: Option<CaptureWindowInfo>,
    /// Typed render-readback completion state.
    pub completion: CaptureCompletion,
}

/// Pixel-space rectangle used by image assertions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Region {
    /// Left coordinate in pixels.
    pub x: u32,
    /// Top coordinate in pixels.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Immutable limits and timing behavior advertised by the running game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentCapabilities {
    /// Maximum app-update frames accepted by one request.
    pub max_wait_frames: u16,
    /// Maximum abort predicates accepted by one semantic wait.
    pub max_abort_predicates: usize,
    /// Whether deterministic Bevy time is enabled.
    pub deterministic_time: bool,
    /// Maximum deterministic updates accepted by one request.
    pub max_time_advance_steps: u16,
    /// Maximum deterministic duration accepted by one request.
    pub max_time_advance: Duration,
}

/// Rust client for the local JSON-lines control socket.
pub struct AgentClient {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
    next_id: u64,
    timeout: Duration,
    transcript: Option<File>,
    last_capture: Option<Capture>,
    ocr: OcrOptions,
    capabilities: AgentCapabilities,
    last_observation: Option<ObservedPredicate>,
}

/// Client failure.
#[derive(Debug)]
pub enum ClientError {
    /// Filesystem, socket, or subprocess I/O failed.
    Io(String),
    /// JSON parsing or formatting failed.
    Json(String),
    /// Protocol discovery failed before connecting.
    Protocol(String),
    /// The game returned an error response.
    Command {
        /// Protocol error code.
        code: String,
        /// Human-readable error message.
        message: String,
        /// Bounded structured context supplied by the game.
        context: Option<Value>,
    },
    /// OCR is not available or the requested language is missing.
    OcrUnavailable(String),
    /// OCR ran but failed unexpectedly.
    Ocr(String),
    /// Visual/text assertion failed.
    Assertion(String),
}

impl Display for ClientError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message)
            | Self::Json(message)
            | Self::Protocol(message)
            | Self::OcrUnavailable(message)
            | Self::Ocr(message)
            | Self::Assertion(message) => formatter.write_str(message),
            Self::Command {
                code,
                message,
                context,
            } => {
                write!(formatter, "command failed [{code}]: {message}")?;
                if let Some(context) = context {
                    write!(formatter, "; context={context}")?;
                }
                Ok(())
            }
        }
    }
}

impl Error for ClientError {}

impl ClientError {
    fn is_semantic_wait_failure(&self) -> bool {
        matches!(
            self,
            Self::Command { code, .. }
                if code == "predicate_timeout" || code == "predicate_aborted"
        )
    }

    fn attach_failure_capture(&mut self, capture: &Capture) {
        let Self::Command { context, .. } = self else {
            return;
        };
        let context = context.get_or_insert_with(|| json!({}));
        if let Some(object) = context.as_object_mut()
            && let Ok(capture) = serde_json::to_value(capture)
        {
            object.insert("failure_capture".to_string(), capture);
        }
    }
}

impl From<io::Error> for ClientError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

impl From<serde_json::Error> for ClientError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error.to_string())
    }
}

impl AgentClient {
    /// Connects using a protocol file path.
    pub fn connect(protocol_file: impl AsRef<Path>) -> Result<Self, ClientError> {
        Self::with_config(AgentClientConfig {
            protocol_file: protocol_file.as_ref().to_path_buf(),
            ..Default::default()
        })
    }

    /// Connects using explicit configuration.
    pub fn with_config(config: AgentClientConfig) -> Result<Self, ClientError> {
        let protocol = read_protocol(&config.protocol_file)?;
        if protocol.max_wait_frames == 0 {
            return Err(ClientError::Protocol(
                "protocol advertises zero max_wait_frames".to_string(),
            ));
        }
        if protocol.max_abort_predicates == 0 {
            return Err(ClientError::Protocol(
                "protocol advertises zero max_abort_predicates".to_string(),
            ));
        }
        if protocol.max_time_advance_steps == 0 {
            return Err(ClientError::Protocol(
                "protocol advertises zero max_time_advance_steps".to_string(),
            ));
        }
        let max_time_advance = Duration::try_from_secs_f64(protocol.max_time_advance_seconds)
            .map_err(|_| {
                ClientError::Protocol(
                    "protocol max_time_advance_seconds must be finite and positive".to_string(),
                )
            })?;
        if max_time_advance.is_zero() {
            return Err(ClientError::Protocol(
                "protocol advertises zero max_time_advance_seconds".to_string(),
            ));
        }
        let stream = TcpStream::connect_timeout(&protocol.socket_addr, config.timeout)
            .map_err(|error| socket_error(error, &protocol.socket_addr))?;
        stream.set_read_timeout(Some(config.timeout))?;
        stream.set_write_timeout(Some(config.timeout))?;
        let reader = BufReader::new(stream.try_clone()?);
        let transcript = match &config.transcript_file {
            Some(path) => {
                if let Some(parent) = path.parent()
                    && !parent.as_os_str().is_empty()
                {
                    fs::create_dir_all(parent)?;
                }
                Some(OpenOptions::new().create(true).append(true).open(path)?)
            }
            None => None,
        };
        Ok(Self {
            stream,
            reader,
            next_id: 1,
            timeout: config.timeout,
            transcript,
            last_capture: None,
            ocr: config.ocr,
            capabilities: AgentCapabilities {
                max_wait_frames: protocol.max_wait_frames,
                max_abort_predicates: protocol.max_abort_predicates,
                deterministic_time: protocol.deterministic_time,
                max_time_advance_steps: protocol.max_time_advance_steps,
                max_time_advance,
            },
            last_observation: None,
        })
    }

    /// Sends a raw JSON request and returns the raw JSON response.
    pub fn request(&mut self, mut request: Value) -> Result<Value, ClientError> {
        let Some(object) = request.as_object_mut() else {
            return Err(ClientError::Protocol(
                "request must be a JSON object".to_string(),
            ));
        };
        object
            .entry("id".to_string())
            .or_insert_with(|| Value::from(self.next_id));
        self.next_id = self.next_id.saturating_add(1);

        let line = serde_json::to_string(&request)?;
        let ts_ms = u64::try_from(unix_ms()).unwrap_or(u64::MAX);
        let start = Instant::now();
        writeln!(self.stream, "{line}")?;
        self.stream.flush()?;

        let mut response = String::new();
        if let Err(error) = self.reader.read_line(&mut response) {
            return Err(self.read_error(error));
        }
        if response.is_empty() {
            return Err(ClientError::Io(
                "agent socket closed before response".to_string(),
            ));
        }
        let response: Value = serde_json::from_str(&response)?;
        if let Some(transcript) = &mut self.transcript {
            serde_json::to_writer(
                &mut *transcript,
                &json!({
                    "ts_ms": ts_ms,
                    "duration_ms": u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                    "request": request,
                    "response": response,
                }),
            )?;
            writeln!(transcript)?;
            transcript.flush()?;
        }
        self.record_response_context(&response);
        if response["ok"] == Value::Bool(true) {
            return Ok(response);
        }
        let code = response["error"]["code"]
            .as_str()
            .unwrap_or("command_failed")
            .to_string();
        let mut message = response["error"]["message"]
            .as_str()
            .unwrap_or("game returned an error")
            .to_string();
        if code == "timeout" {
            message = self.with_last_capture(message);
        }
        let context = response["error"].get("context").cloned();
        Err(ClientError::Command {
            code,
            message,
            context,
        })
    }

    /// Replays request-only or transcript-envelope JSON-lines from disk.
    pub fn replay_jsonl(&mut self, path: impl AsRef<Path>) -> Result<Vec<Value>, ClientError> {
        let file = File::open(path)?;
        let lines = BufReader::new(file)
            .lines()
            .collect::<Result<Vec<_>, _>>()?;
        let mut responses = Vec::new();
        for line in lines {
            if line.trim().is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(&line)?;
            let request = value.get("request").cloned().unwrap_or(value);
            responses.push(self.request(request)?);
        }
        Ok(responses)
    }

    /// Queries primary-window metadata.
    pub fn window_info(&mut self) -> Result<Value, ClientError> {
        self.request(json!({"command": "window_info"}))
    }

    /// Moves the cursor in logical window coordinates.
    pub fn cursor_move(&mut self, x: f32, y: f32) -> Result<Value, ClientError> {
        self.request(json!({"command": "cursor_move", "x": x, "y": y}))
    }

    /// Presses a physical key code.
    pub fn key_down(&mut self, key: &str) -> Result<Value, ClientError> {
        self.request(json!({"command": "key_down", "key": key}))
    }

    /// Releases a physical key code.
    pub fn key_up(&mut self, key: &str) -> Result<Value, ClientError> {
        self.request(json!({"command": "key_up", "key": key}))
    }

    /// Presses a mouse button.
    pub fn mouse_down(&mut self, button: &str) -> Result<Value, ClientError> {
        self.request(json!({"command": "mouse_down", "button": button}))
    }

    /// Releases a mouse button.
    pub fn mouse_up(&mut self, button: &str) -> Result<Value, ClientError> {
        self.request(json!({"command": "mouse_up", "button": button}))
    }

    /// Scrolls by line units.
    pub fn scroll(&mut self, lines: f32) -> Result<Value, ClientError> {
        self.request(json!({"command": "scroll", "lines": lines}))
    }

    /// Clicks at logical window coordinates.
    pub fn click(&mut self, x: f32, y: f32, button: &str) -> Result<Value, ClientError> {
        self.request(json!({"command": "click", "x": x, "y": y, "button": button}))
    }

    /// Drags between logical window coordinates.
    pub fn drag(
        &mut self,
        button: &str,
        from: [f32; 2],
        to: [f32; 2],
        steps: u16,
        frames: u16,
    ) -> Result<Value, ClientError> {
        self.request(json!({
            "command": "drag",
            "button": button,
            "from": from,
            "to": to,
            "steps": steps,
            "frames": frames,
        }))
    }

    /// Taps a key for one frame.
    pub fn key_tap(&mut self, key: &str) -> Result<Value, ClientError> {
        self.request(json!({"command": "key_tap", "key": key}))
    }

    /// Holds a key for `frames` frames, then releases it.
    pub fn key_hold(&mut self, key: &str, frames: u16) -> Result<Value, ClientError> {
        self.request(json!({"command": "key_hold", "key": key, "frames": frames}))
    }

    /// Releases every input held by this agent session.
    pub fn release_all_inputs(&mut self) -> Result<Value, ClientError> {
        self.request(json!({"command": "release_all_inputs"}))
    }

    /// Asks the Bevy app to exit cleanly.
    pub fn shutdown(&mut self) -> Result<Value, ClientError> {
        self.request(json!({"command": "shutdown"}))
    }

    /// Waits until the server closes this control connection.
    pub fn wait_for_disconnect(&mut self) -> Result<(), ClientError> {
        let mut byte = [0_u8; 1];
        match self.reader.read(&mut byte) {
            Ok(0) => Ok(()),
            Ok(_) => Err(ClientError::Protocol(
                "unexpected protocol data after shutdown acknowledgment".to_string(),
            )),
            Err(error) => Err(error.into()),
        }
    }

    fn record_response_context(&mut self, response: &Value) {
        let capture_value = response["result"]
            .get("capture")
            .or_else(|| response["result"].get("latest_capture"))
            .or_else(|| response["error"]["context"].get("latest_capture"));
        if let Some(capture) =
            capture_value.and_then(|value| serde_json::from_value::<Capture>(value.clone()).ok())
        {
            self.last_capture = Some(capture);
        }

        let observation_value = response["result"]
            .get("details")
            .or_else(|| response["error"]["context"].get("observed_predicate"));
        if let Some(observation) = observation_value
            .and_then(|value| serde_json::from_value::<ObservedPredicate>(value.clone()).ok())
        {
            self.last_observation = Some(observation);
        }
    }
    fn read_error(&self, error: io::Error) -> ClientError {
        if error.kind() == io::ErrorKind::TimedOut || error.kind() == io::ErrorKind::WouldBlock {
            ClientError::Io(self.with_last_capture(format!(
                "agent request timed out after {} ms",
                self.timeout.as_millis()
            )))
        } else {
            ClientError::Io(error.to_string())
        }
    }

    fn with_last_capture(&self, message: String) -> String {
        match &self.last_capture {
            Some(capture) => format!("{message}; last captured frame: {}", capture.path.display()),
            None => message,
        }
    }

    fn validate_wait_limit(&self, name: &str, requested: u64) -> Result<(), ClientError> {
        let supported = u64::from(self.capabilities.max_wait_frames);
        if requested > supported {
            return Err(ClientError::Protocol(format!(
                "{name}={requested} exceeds server limit {supported}; configure AgentFeedbackConfig.max_wait_frames or issue explicit bounded requests"
            )));
        }
        Ok(())
    }
}

impl Drop for AgentClient {
    fn drop(&mut self) {
        let _ = writeln!(
            self.stream,
            "{}",
            json!({"id": self.next_id, "command": "release_all_inputs"})
        );
        let _ = self.stream.flush();
    }
}

fn capture_from_response(response: &Value) -> Result<Capture, ClientError> {
    serde_json::from_value(response["result"]["capture"].clone()).map_err(|error| {
        ClientError::Protocol(format!("invalid capture metadata ({error}): {response}"))
    })
}

mod image_assertions;
#[cfg(test)]
use image_assertions::run_tesseract;
mod protocol_file;
use protocol_file::{read_protocol, socket_error};
mod diagnostics;
pub use diagnostics::{
    ComparisonOperator, ObservedPredicate, Predicate, PredicateOutcome, ResolvedTargetKind,
    TargetBounds, TargetInfo, TargetKind, TargetSelector,
};
mod timing;

#[cfg(test)]
mod tests;
