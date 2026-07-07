//! Rust client for the v2 agent feedback protocol.

use crate::session::{PROTOCOL_VERSION, unix_ms};
use image::GenericImageView;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    error::Error,
    fmt::{self, Display, Formatter},
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Client configuration.
#[derive(Clone, Debug)]
pub struct AgentClientConfig {
    /// Protocol file written by the Bevy plugin.
    pub protocol_file: PathBuf,
    /// TCP connect/read/write timeout.
    pub timeout: Duration,
    /// Optional request-only transcript path.
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
    transcript: Option<File>,
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
            transcript,
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
        if let Some(transcript) = &mut self.transcript {
            writeln!(transcript, "{line}")?;
            transcript.flush()?;
        }
        writeln!(self.stream, "{line}")?;
        self.stream.flush()?;

        let mut response = String::new();
        self.reader.read_line(&mut response)?;
        if response.is_empty() {
            return Err(ClientError::Io(
                "agent socket closed before response".to_string(),
            ));
        }
        let response: Value = serde_json::from_str(&response)?;
        if response["ok"] == Value::Bool(true) {
            return Ok(response);
        }
        let code = response["error"]["code"]
            .as_str()
            .unwrap_or("command_failed")
            .to_string();
        let message = response["error"]["message"]
            .as_str()
            .unwrap_or("game returned an error")
            .to_string();
        Err(ClientError::Command { code, message })
    }

    /// Replays request-only JSON-lines from disk.
    pub fn replay_jsonl(&mut self, path: impl AsRef<Path>) -> Result<Vec<Value>, ClientError> {
        let file = File::open(path)?;
        let mut responses = Vec::new();
        for line in BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            responses.push(self.request(serde_json::from_str(&line)?)?);
        }
        Ok(responses)
    }

    /// Queries primary-window metadata.
    pub fn window_info(&mut self) -> Result<Value, ClientError> {
        self.request(json!({"command": "window_info"}))
    }

    /// Captures the primary window as a PNG.
    pub fn capture(&mut self) -> Result<Capture, ClientError> {
        let response = self.request(json!({"command": "capture"}))?;
        capture_from_response(&response)
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
    Ok(Capture {
        sequence,
        path: PathBuf::from(path),
    })
}

fn image_error(error: image::ImageError) -> ClientError {
    ClientError::Assertion(error.to_string())
}

fn diff_images(
    a: &image::DynamicImage,
    b: &image::DynamicImage,
    region: Option<Region>,
) -> Result<u64, ClientError> {
    if a.dimensions() != b.dimensions() {
        return Err(ClientError::Assertion(format!(
            "image dimensions differ: {:?} vs {:?}",
            a.dimensions(),
            b.dimensions()
        )));
    }
    let region = region.unwrap_or(Region {
        x: 0,
        y: 0,
        width: a.width(),
        height: a.height(),
    });
    validate_region(a, region)?;
    let mut changed = 0;
    for y in region.y..region.y + region.height {
        for x in region.x..region.x + region.width {
            if a.get_pixel(x, y) != b.get_pixel(x, y) {
                changed += 1;
            }
        }
    }
    Ok(changed)
}

fn image_has_color(
    path: &Path,
    color: [u8; 3],
    region: Region,
    tolerance: u8,
) -> Result<bool, ClientError> {
    let image = image::ImageReader::open(path)?
        .decode()
        .map_err(image_error)?;
    validate_region(&image, region)?;
    for y in region.y..region.y + region.height {
        for x in region.x..region.x + region.width {
            let pixel = image.get_pixel(x, y);
            if close(pixel[0], color[0], tolerance)
                && close(pixel[1], color[1], tolerance)
                && close(pixel[2], color[2], tolerance)
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn validate_region(image: &image::DynamicImage, region: Region) -> Result<(), ClientError> {
    let x_end = region
        .x
        .checked_add(region.width)
        .ok_or_else(|| ClientError::Assertion("region x + width overflowed u32".to_string()))?;
    let y_end = region
        .y
        .checked_add(region.height)
        .ok_or_else(|| ClientError::Assertion("region y + height overflowed u32".to_string()))?;
    if region.width == 0 || region.height == 0 || x_end > image.width() || y_end > image.height() {
        return Err(ClientError::Assertion(format!(
            "region {:?} is outside image {}x{}",
            region,
            image.width(),
            image.height()
        )));
    }
    Ok(())
}

fn close(actual: u8, expected: u8, tolerance: u8) -> bool {
    actual.abs_diff(expected) <= tolerance
}

fn run_tesseract(options: &OcrOptions, path: &Path) -> Result<String, ClientError> {
    let mut child = Command::new(&options.tesseract)
        .arg(path)
        .arg("stdout")
        .arg("-l")
        .arg(&options.language)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            ClientError::OcrUnavailable(format!(
                "tesseract unavailable at {}: {error}",
                options.tesseract.display()
            ))
        })?;

    let start = Instant::now();
    while start.elapsed() < options.timeout {
        match child.try_wait() {
            Ok(Some(_)) => {
                let output = child.wait_with_output()?;
                if output.status.success() {
                    return String::from_utf8(output.stdout)
                        .map_err(|error| ClientError::Ocr(error.to_string()));
                }
                return Err(ClientError::OcrUnavailable(
                    String::from_utf8_lossy(&output.stderr).trim().to_string(),
                ));
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(error) => return Err(ClientError::Ocr(error.to_string())),
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    Err(ClientError::OcrUnavailable(format!(
        "tesseract timed out after {} ms",
        options.timeout.as_millis()
    )))
}

fn normalize_text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

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
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};

    #[test]
    fn image_diff_counts_changed_pixels() {
        let root = std::env::temp_dir().join(format!("bevy-agent-client-{}", unix_ms()));
        fs::create_dir_all(&root).expect("temp root");
        let a = root.join("a.png");
        let b = root.join("b.png");
        ImageBuffer::<Rgba<u8>, _>::from_pixel(2, 2, Rgba([0, 0, 0, 255]))
            .save(&a)
            .expect("save a");
        let mut changed = ImageBuffer::<Rgba<u8>, _>::from_pixel(2, 2, Rgba([0, 0, 0, 255]));
        changed.put_pixel(1, 1, Rgba([255, 0, 0, 255]));
        changed.save(&b).expect("save b");

        assert_eq!(AgentClient::pixel_diff(&a, &b).expect("diff"), 1);
        assert_eq!(
            AgentClient::region_diff(
                &a,
                &b,
                Region {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
            )
            .expect("region diff"),
            0
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_v1_protocol_files() {
        let root = std::env::temp_dir().join(format!("bevy-agent-client-protocol-{}", unix_ms()));
        fs::create_dir_all(&root).expect("temp root");
        let protocol = root.join("agent.json");
        fs::write(
            &protocol,
            json!({
                "protocol": "bevy-agent-feedback/1",
                "socket_addr": "127.0.0.1:1",
                "pid": std::process::id(),
                "heartbeat_file": root.join("heartbeat"),
                "stale_after_ms": 1000,
            })
            .to_string(),
        )
        .expect("protocol");

        let error = read_protocol(&protocol).expect_err("v1 should be rejected");
        assert!(error.to_string().contains("expected bevy-agent-feedback/2"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_unknown_protocol_files() {
        let root = std::env::temp_dir().join(format!("bevy-agent-client-unknown-{}", unix_ms()));
        fs::create_dir_all(&root).expect("temp root");
        let protocol = root.join("agent.json");
        fs::write(&protocol, "{}").expect("protocol");

        let error = read_protocol(&protocol).expect_err("unknown protocol");
        assert!(error.to_string().contains("missing protocol"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_stale_heartbeat() {
        let root = std::env::temp_dir().join(format!("bevy-agent-client-stale-{}", unix_ms()));
        fs::create_dir_all(&root).expect("temp root");
        let heartbeat = root.join("heartbeat");
        fs::write(&heartbeat, "1").expect("heartbeat");
        let protocol = root.join("agent.json");
        fs::write(
            &protocol,
            json!({
                "protocol": PROTOCOL_VERSION,
                "socket_addr": "127.0.0.1:1",
                "pid": std::process::id(),
                "heartbeat_file": heartbeat,
                "stale_after_ms": 1,
            })
            .to_string(),
        )
        .expect("protocol");

        let error = read_protocol(&protocol).expect_err("stale heartbeat");
        assert!(error.to_string().contains("protocol stale"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ocr_missing_binary_is_unavailable() {
        let root = std::env::temp_dir().join(format!("bevy-agent-client-ocr-{}", unix_ms()));
        fs::create_dir_all(&root).expect("temp root");
        let image = root.join("text.png");
        ImageBuffer::<Rgba<u8>, _>::from_pixel(2, 2, Rgba([255, 255, 255, 255]))
            .save(&image)
            .expect("save image");
        let config = OcrOptions {
            tesseract: root.join("missing-tesseract"),
            ..Default::default()
        };

        let error = run_tesseract(&config, &image).expect_err("missing binary");
        assert!(matches!(error, ClientError::OcrUnavailable(_)));
        let _ = fs::remove_dir_all(root);
    }
}
