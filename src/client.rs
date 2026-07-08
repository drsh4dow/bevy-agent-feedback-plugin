//! Rust client for the v2 agent feedback protocol.

use crate::session::{PROTOCOL_VERSION, unix_ms};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    error::Error,
    fmt::{self, Display, Formatter},
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    net::{SocketAddr, TcpStream},
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

/// A captured PNG returned by the plugin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Capture {
    /// Monotonic capture sequence number.
    pub sequence: u64,
    /// PNG path on disk.
    pub path: PathBuf,
    /// Optional capture label echoed by the plugin.
    pub label: Option<String>,
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

/// Rust client for the local JSON-lines control socket.
pub struct AgentClient {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
    next_id: u64,
    timeout: Duration,
    transcript: Option<File>,
    last_capture: Option<PathBuf>,
    ocr: OcrOptions,
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
            Self::Command { code, message } => {
                write!(formatter, "command failed [{code}]: {message}")
            }
        }
    }
}

impl Error for ClientError {}

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
        Err(ClientError::Command { code, message })
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

    /// Captures the primary window as a PNG.
    pub fn capture(&mut self) -> Result<Capture, ClientError> {
        self.capture_with_label(None)
    }

    /// Captures the primary window as a labeled PNG.
    pub fn capture_labeled(&mut self, label: &str) -> Result<Capture, ClientError> {
        self.capture_with_label(Some(label))
    }

    fn capture_with_label(&mut self, label: Option<&str>) -> Result<Capture, ClientError> {
        let response = match label {
            Some(label) => self.request(json!({"command": "capture", "label": label}))?,
            None => self.request(json!({"command": "capture"}))?,
        };
        let capture = capture_from_response(&response)?;
        self.last_capture = Some(capture.path.clone());
        Ok(capture)
    }

    /// Waits for `frames` Bevy frames.
    pub fn wait(&mut self, frames: u16) -> Result<Value, ClientError> {
        self.request(json!({"command": "wait", "frames": frames}))
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
            Some(path) => format!("{message}; last captured frame: {}", path.display()),
            None => message,
        }
    }

    /// Counts differing pixels across two equally-sized images.
    pub fn pixel_diff(a: impl AsRef<Path>, b: impl AsRef<Path>) -> Result<u64, ClientError> {
        let a = image::ImageReader::open(a)?.decode().map_err(image_error)?;
        let b = image::ImageReader::open(b)?.decode().map_err(image_error)?;
        diff_images(&a, &b, None)
    }

    /// Counts differing pixels inside a region across two equally-sized images.
    pub fn region_diff(
        a: impl AsRef<Path>,
        b: impl AsRef<Path>,
        region: Region,
    ) -> Result<u64, ClientError> {
        let a = image::ImageReader::open(a)?.decode().map_err(image_error)?;
        let b = image::ImageReader::open(b)?.decode().map_err(image_error)?;
        diff_images(&a, &b, Some(region))
    }

    /// Asserts that two screenshots differ by at least `min_pixels`.
    pub fn assert_changed(
        a: impl AsRef<Path>,
        b: impl AsRef<Path>,
        min_pixels: u64,
    ) -> Result<(), ClientError> {
        let changed = Self::pixel_diff(&a, &b)?;
        if changed >= min_pixels {
            return Ok(());
        }
        Err(ClientError::Assertion(format!(
            "screenshots changed {changed} pixels, expected at least {min_pixels}: {} and {}",
            a.as_ref().display(),
            b.as_ref().display()
        )))
    }

    /// Asserts that two screenshots differ by at least `min_pixels` inside `region`.
    pub fn assert_region_changed(
        a: impl AsRef<Path>,
        b: impl AsRef<Path>,
        region: Region,
        min_pixels: u64,
    ) -> Result<(), ClientError> {
        let changed = Self::region_diff(&a, &b, region)?;
        if changed >= min_pixels {
            return Ok(());
        }
        Err(ClientError::Assertion(format!(
            "region {:?} changed {changed} pixels, expected at least {min_pixels}: {} and {}",
            region,
            a.as_ref().display(),
            b.as_ref().display()
        )))
    }

    /// Asserts that a color appears at least `min_pixels` times.
    pub fn assert_color_present(
        path: impl AsRef<Path>,
        color: [u8; 3],
        region: Option<Region>,
        tolerance: u8,
        min_pixels: u64,
    ) -> Result<(), ClientError> {
        let found = color_pixel_count(path.as_ref(), color, region, tolerance)?;
        if found >= min_pixels {
            return Ok(());
        }
        Err(ClientError::Assertion(format!(
            "color {:?} found {found} pixels, expected at least {min_pixels}: {}",
            color,
            path.as_ref().display()
        )))
    }

    /// Captures until the screenshot differs from `before`.
    pub fn wait_until_changed(
        &mut self,
        before: impl AsRef<Path>,
        frames_per_poll: u16,
        attempts: u16,
    ) -> Result<Capture, ClientError> {
        for _ in 0..attempts {
            self.wait(frames_per_poll)?;
            let capture = self.capture()?;
            if Self::pixel_diff(before.as_ref(), &capture.path)? > 0 {
                return Ok(capture);
            }
        }
        Err(ClientError::Assertion(
            "screenshot did not change".to_string(),
        ))
    }

    /// Captures until a color appears in a pixel region.
    pub fn wait_until_color(
        &mut self,
        color: [u8; 3],
        region: Region,
        tolerance: u8,
        frames_per_poll: u16,
        attempts: u16,
    ) -> Result<Capture, ClientError> {
        for _ in 0..attempts {
            self.wait(frames_per_poll)?;
            let capture = self.capture()?;
            if image_has_color(&capture.path, color, region, tolerance)? {
                return Ok(capture);
            }
        }
        Err(ClientError::Assertion("color did not appear".to_string()))
    }

    /// Runs Tesseract OCR on an image.
    pub fn ocr_image(&self, path: impl AsRef<Path>) -> Result<String, ClientError> {
        run_tesseract(&self.ocr, path.as_ref())
    }

    /// Crops a region and runs Tesseract OCR on it.
    pub fn ocr_region(
        &self,
        path: impl AsRef<Path>,
        region: Region,
    ) -> Result<String, ClientError> {
        let image = image::ImageReader::open(path)?
            .decode()
            .map_err(image_error)?;
        validate_region(&image, region)?;
        let cropped = image.crop_imm(region.x, region.y, region.width, region.height);
        let temp = std::env::temp_dir().join(format!(
            "bevy-feedback-ocr-{}-{}.png",
            std::process::id(),
            unix_ms()
        ));
        cropped.save(&temp).map_err(image_error)?;
        let result = self.ocr_image(&temp);
        let _ = fs::remove_file(temp);
        result
    }

    /// Asserts that OCR output contains `expected`.
    pub fn assert_text(&self, path: impl AsRef<Path>, expected: &str) -> Result<(), ClientError> {
        let text = normalize_text(&self.ocr_image(path)?);
        let expected = normalize_text(expected);
        if text.contains(&expected) {
            Ok(())
        } else {
            Err(ClientError::Assertion(format!(
                "OCR text did not contain '{expected}': {text}"
            )))
        }
    }

    /// Captures until OCR output contains `expected`.
    pub fn wait_until_text(
        &mut self,
        expected: &str,
        frames_per_poll: u16,
        attempts: u16,
    ) -> Result<Capture, ClientError> {
        for _ in 0..attempts {
            self.wait(frames_per_poll)?;
            let capture = self.capture()?;
            match self.assert_text(&capture.path, expected) {
                Ok(()) => return Ok(capture),
                Err(ClientError::Assertion(_)) => {}
                Err(error) => return Err(error),
            }
        }
        Err(ClientError::Assertion(format!(
            "text '{expected}' did not appear"
        )))
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

#[derive(Debug, Deserialize)]
struct ProtocolFile {
    socket_addr: SocketAddr,
    pid: u32,
    heartbeat_file: PathBuf,
    stale_after_ms: u64,
}

fn read_protocol(path: &Path) -> Result<ProtocolFile, ClientError> {
    let bytes = fs::read(path).map_err(|error| {
        ClientError::Protocol(format!(
            "failed to read protocol file {}: {error}",
            path.display()
        ))
    })?;
    let value: Value = serde_json::from_slice(&bytes)?;
    let Some(version) = value["protocol"].as_str() else {
        return Err(ClientError::Protocol(format!(
            "unknown protocol file {}; missing protocol, expected {PROTOCOL_VERSION}",
            path.display()
        )));
    };
    if version != PROTOCOL_VERSION {
        return Err(ClientError::Protocol(format!(
            "unsupported protocol '{version}'; expected {PROTOCOL_VERSION}"
        )));
    }
    let protocol: ProtocolFile = serde_json::from_value(value)?;
    if !process_alive(protocol.pid) {
        return Err(ClientError::Protocol(format!(
            "protocol stale: process {} is not alive",
            protocol.pid
        )));
    }
    let heartbeat = fs::read_to_string(&protocol.heartbeat_file).map_err(|error| {
        ClientError::Protocol(format!(
            "protocol stale: failed to read heartbeat {}: {error}",
            protocol.heartbeat_file.display()
        ))
    })?;
    let heartbeat_ms = heartbeat.trim().parse::<u128>().map_err(|error| {
        ClientError::Protocol(format!("protocol stale: heartbeat is invalid: {error}"))
    })?;
    let age = unix_ms().saturating_sub(heartbeat_ms);
    if age > u128::from(protocol.stale_after_ms) {
        return Err(ClientError::Protocol(format!(
            "protocol stale: heartbeat is {age} ms old, stale after {} ms",
            protocol.stale_after_ms
        )));
    }
    Ok(protocol)
}

fn socket_error(error: io::Error, socket_addr: &SocketAddr) -> ClientError {
    if error.kind() == io::ErrorKind::ConnectionRefused {
        ClientError::Protocol(format!(
            "socket refused at {socket_addr}; game probably exited"
        ))
    } else {
        ClientError::Io(format!("connect {socket_addr}: {error}"))
    }
}

fn capture_from_response(response: &Value) -> Result<Capture, ClientError> {
    let capture = &response["result"]["capture"];
    let sequence = capture["sequence"].as_u64().ok_or_else(|| {
        ClientError::Protocol(format!("capture response missing sequence: {response}"))
    })?;
    let path = capture["path"].as_str().ok_or_else(|| {
        ClientError::Protocol(format!("capture response missing path: {response}"))
    })?;
    let label = capture["label"].as_str().map(str::to_string);
    Ok(Capture {
        sequence,
        path: PathBuf::from(path),
        label,
    })
}

mod image_assertions;
use image_assertions::{
    color_pixel_count, diff_images, image_error, image_has_color, normalize_text, run_tesseract,
    validate_region,
};

fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        Path::new("/proc").join(pid.to_string()).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

#[cfg(test)]
mod tests;
