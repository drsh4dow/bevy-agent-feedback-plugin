use bevy::prelude::*;
use bevy::render::view::window::screenshot::{Screenshot, ScreenshotCaptured};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[derive(Clone, Debug, Resource)]
pub struct AgentFeedbackConfig {
    pub bind_addr: SocketAddr,
    pub protocol_file: PathBuf,
    pub capture_dir: PathBuf,
    pub max_pending_commands: usize,
    pub max_wait_frames: u16,
    pub max_captures: usize,
    pub command_timeout: Duration,
}

impl Default for AgentFeedbackConfig {
    fn default() -> Self {
        Self {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 15712)),
            protocol_file: PathBuf::from("agent-feedback.json"),
            capture_dir: PathBuf::from("agent-feedback-captures"),
            max_pending_commands: 32,
            max_wait_frames: 300,
            max_captures: 32,
            command_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Default)]
pub struct AgentFeedbackPlugin {
    config: AgentFeedbackConfig,
}

impl AgentFeedbackPlugin {
    pub fn new(config: AgentFeedbackConfig) -> Self {
        Self { config }
    }
}

impl Plugin for AgentFeedbackPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(self.config.clone())
            .init_resource::<AgentFeedbackState>()
            .add_systems(
                PreUpdate,
                (tick_pending_waits, drain_agent_requests)
                    .chain()
                    .after(bevy::input::InputSystems),
            );

        match start_runtime(&self.config) {
            Ok((runtime, socket_addr)) => {
                if let Err(error) = write_protocol_file(&self.config, socket_addr) {
                    log::error!("failed to write agent protocol file: {error}");
                }
                app.insert_resource(runtime);
            }
            Err(error) => log::error!("failed to start agent feedback socket: {error}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum AgentCommand {
    KeyDown(KeyCode),
    KeyUp(KeyCode),
    MouseDown(MouseButton),
    MouseUp(MouseButton),
    Wait { frames: u16 },
    Capture,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct CaptureInfo {
    sequence: u64,
    path: String,
}

#[derive(Resource)]
struct AgentFeedbackRuntime {
    requests: Mutex<Receiver<AgentRequest>>,
    running: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for AgentFeedbackRuntime {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        let Ok(mut thread) = self.thread.lock() else {
            return;
        };
        if let Some(thread) = thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Resource, Default)]
struct AgentFeedbackState {
    next_capture: u64,
    latest_capture: Option<CaptureInfo>,
    pending_waits: VecDeque<PendingWait>,
    captures: VecDeque<PathBuf>,
}

struct PendingWait {
    id: Value,
    frames_left: u16,
    responder: SyncSender<AgentResponse>,
}

struct AgentRequest {
    id: Value,
    command: AgentCommand,
    responder: SyncSender<AgentResponse>,
}

#[derive(Clone, Debug, PartialEq)]
struct AgentRequestBody {
    id: Value,
    command: AgentCommand,
}

#[derive(Debug, Deserialize)]
struct WireRequest {
    id: Value,
    #[serde(flatten)]
    command: WireCommand,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum WireCommand {
    KeyDown { key: KeyCode },
    KeyUp { key: KeyCode },
    MouseDown { button: MouseButton },
    MouseUp { button: MouseButton },
    Wait { frames: Option<u16> },
    Capture,
}

#[derive(Debug, Serialize)]
struct AgentResponse {
    id: Value,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<AgentResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<AgentError>,
}

#[derive(Debug, Serialize)]
struct AgentResult {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_capture: Option<CaptureInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capture: Option<CaptureInfo>,
}

#[derive(Debug, Serialize)]
struct AgentError {
    code: &'static str,
    message: String,
}

impl AgentResponse {
    fn ok(
        id: Value,
        status: &'static str,
        latest_capture: Option<CaptureInfo>,
        capture: Option<CaptureInfo>,
    ) -> Self {
        Self {
            id,
            ok: true,
            result: Some(AgentResult {
                status,
                latest_capture,
                capture,
            }),
            error: None,
        }
    }

    fn error(id: Value, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(AgentError {
                code,
                message: message.into(),
            }),
        }
    }
}

fn parse_request(line: &str, max_wait_frames: u16) -> Result<AgentRequestBody, String> {
    let request: WireRequest = serde_json::from_str(line).map_err(|error| error.to_string())?;
    let command = match request.command {
        WireCommand::KeyDown { key } => AgentCommand::KeyDown(key),
        WireCommand::KeyUp { key } => AgentCommand::KeyUp(key),
        WireCommand::MouseDown { button } => AgentCommand::MouseDown(button),
        WireCommand::MouseUp { button } => AgentCommand::MouseUp(button),
        WireCommand::Capture => AgentCommand::Capture,
        WireCommand::Wait { frames } => {
            let frames = frames.unwrap_or(1);
            if frames == 0 || frames > max_wait_frames {
                return Err(format!(
                    "frames must be between 1 and {max_wait_frames}, got {frames}"
                ));
            }
            AgentCommand::Wait { frames }
        }
    };

    Ok(AgentRequestBody {
        id: request.id,
        command,
    })
}

fn start_runtime(config: &AgentFeedbackConfig) -> io::Result<(AgentFeedbackRuntime, SocketAddr)> {
    let listener = TcpListener::bind(config.bind_addr)?;
    listener.set_nonblocking(true)?;
    let socket_addr = listener.local_addr()?;
    let (sender, receiver) = sync_channel(config.max_pending_commands.max(1));
    let running = Arc::new(AtomicBool::new(true));
    let server_running = running.clone();
    let command_timeout = config.command_timeout;
    let max_wait_frames = config.max_wait_frames;
    let thread = thread::Builder::new()
        .name("bevy-agent-feedback".to_string())
        .spawn(move || {
            server_loop(
                listener,
                sender,
                server_running,
                command_timeout,
                max_wait_frames,
            )
        })?;

    Ok((
        AgentFeedbackRuntime {
            requests: Mutex::new(receiver),
            running,
            thread: Mutex::new(Some(thread)),
        },
        socket_addr,
    ))
}

fn server_loop(
    listener: TcpListener,
    sender: SyncSender<AgentRequest>,
    running: Arc<AtomicBool>,
    command_timeout: Duration,
    max_wait_frames: u16,
) {
    while running.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                handle_client(stream, &sender, &running, command_timeout, max_wait_frames)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                log::warn!("agent feedback socket accept failed: {error}");
                thread::sleep(Duration::from_millis(250));
            }
        }
    }
}

fn handle_client(
    mut stream: TcpStream,
    sender: &SyncSender<AgentRequest>,
    running: &AtomicBool,
    command_timeout: Duration,
    max_wait_frames: u16,
) {
    if stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .is_err()
    {
        return;
    }
    if stream.set_write_timeout(Some(command_timeout)).is_err() {
        return;
    }

    let mut pending = Vec::with_capacity(1024);
    let mut buf = [0_u8; 1024];
    while running.load(Ordering::Relaxed) {
        match stream.read(&mut buf) {
            Ok(0) => return,
            Ok(read) => {
                if pending.len() + read > 8192 {
                    let _ = write_response(
                        &mut stream,
                        &AgentResponse::error(
                            Value::Null,
                            "line_too_long",
                            "request exceeds 8192 bytes",
                        ),
                    );
                    return;
                }
                pending.extend_from_slice(&buf[..read]);
                while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
                    let line: Vec<u8> = pending.drain(..=newline).collect();
                    let line = String::from_utf8_lossy(&line);
                    let line = line.trim();
                    if !line.is_empty() {
                        let _ = handle_line(
                            line,
                            &mut stream,
                            sender,
                            command_timeout,
                            max_wait_frames,
                        );
                    }
                }
            }
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.kind() == io::ErrorKind::TimedOut => {}
            Err(_) => return,
        }
    }
}

fn handle_line(
    line: &str,
    stream: &mut TcpStream,
    sender: &SyncSender<AgentRequest>,
    command_timeout: Duration,
    max_wait_frames: u16,
) -> io::Result<()> {
    let request = match parse_request(line, max_wait_frames) {
        Ok(request) => request,
        Err(error) => {
            return write_response(
                stream,
                &AgentResponse::error(Value::Null, "invalid_request", error),
            );
        }
    };

    let id = request.id.clone();
    let (response_sender, response_receiver) = sync_channel(1);
    let agent_request = AgentRequest {
        id: request.id,
        command: request.command,
        responder: response_sender,
    };

    match sender.try_send(agent_request) {
        Ok(()) => match response_receiver.recv_timeout(command_timeout) {
            Ok(response) => write_response(stream, &response),
            Err(_) => write_response(
                stream,
                &AgentResponse::error(id, "timeout", "game did not answer in time"),
            ),
        },
        Err(TrySendError::Full(request)) => write_response(
            stream,
            &AgentResponse::error(request.id, "queue_full", "game command queue is full"),
        ),
        Err(TrySendError::Disconnected(request)) => write_response(
            stream,
            &AgentResponse::error(request.id, "closed", "game command queue is closed"),
        ),
    }
}

fn write_response(stream: &mut TcpStream, response: &AgentResponse) -> io::Result<()> {
    serde_json::to_writer(&mut *stream, response).map_err(io::Error::other)?;
    stream.write_all(b"\n")?;
    stream.flush()
}

fn tick_pending_waits(mut state: ResMut<AgentFeedbackState>) {
    let latest_capture = state.latest_capture.clone();
    state.pending_waits.retain_mut(|wait| {
        wait.frames_left -= 1;
        if wait.frames_left > 0 {
            return true;
        }

        let _ = wait.responder.send(AgentResponse::ok(
            wait.id.clone(),
            "waited",
            latest_capture.clone(),
            None,
        ));
        false
    });
}

fn drain_agent_requests(
    mut commands: Commands,
    runtime: Option<Res<AgentFeedbackRuntime>>,
    config: Res<AgentFeedbackConfig>,
    mut state: ResMut<AgentFeedbackState>,
    key_input: Option<ResMut<ButtonInput<KeyCode>>>,
    mouse_input: Option<ResMut<ButtonInput<MouseButton>>>,
) {
    let Some(runtime) = runtime else {
        return;
    };
    let mut requests = Vec::new();
    let receiver = match runtime.requests.lock() {
        Ok(receiver) => receiver,
        Err(_) => {
            log::error!("agent feedback command queue lock was poisoned");
            return;
        }
    };
    for _ in 0..config.max_pending_commands.max(1) {
        match receiver.try_recv() {
            Ok(request) => requests.push(request),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => return,
        }
    }
    drop(receiver);

    let mut key_input = key_input;
    let mut mouse_input = mouse_input;
    for request in requests {
        let AgentRequest {
            id,
            command,
            responder,
        } = request;
        match command {
            AgentCommand::KeyDown(key) => match key_input.as_deref_mut() {
                Some(keys) => {
                    keys.press(key);
                    let _ = responder.send(AgentResponse::ok(
                        id,
                        "ok",
                        state.latest_capture.clone(),
                        None,
                    ));
                }
                None => {
                    let _ = responder.send(AgentResponse::error(
                        id,
                        "missing_input",
                        "ButtonInput<KeyCode> resource is missing",
                    ));
                }
            },
            AgentCommand::KeyUp(key) => match key_input.as_deref_mut() {
                Some(keys) => {
                    keys.release(key);
                    let _ = responder.send(AgentResponse::ok(
                        id,
                        "ok",
                        state.latest_capture.clone(),
                        None,
                    ));
                }
                None => {
                    let _ = responder.send(AgentResponse::error(
                        id,
                        "missing_input",
                        "ButtonInput<KeyCode> resource is missing",
                    ));
                }
            },
            AgentCommand::MouseDown(button) => match mouse_input.as_deref_mut() {
                Some(buttons) => {
                    buttons.press(button);
                    let _ = responder.send(AgentResponse::ok(
                        id,
                        "ok",
                        state.latest_capture.clone(),
                        None,
                    ));
                }
                None => {
                    let _ = responder.send(AgentResponse::error(
                        id,
                        "missing_input",
                        "ButtonInput<MouseButton> resource is missing",
                    ));
                }
            },
            AgentCommand::MouseUp(button) => match mouse_input.as_deref_mut() {
                Some(buttons) => {
                    buttons.release(button);
                    let _ = responder.send(AgentResponse::ok(
                        id,
                        "ok",
                        state.latest_capture.clone(),
                        None,
                    ));
                }
                None => {
                    let _ = responder.send(AgentResponse::error(
                        id,
                        "missing_input",
                        "ButtonInput<MouseButton> resource is missing",
                    ));
                }
            },
            AgentCommand::Wait { frames } => {
                if state.pending_waits.len() >= config.max_pending_commands.max(1) {
                    let _ = responder.send(AgentResponse::error(
                        id,
                        "queue_full",
                        "too many pending wait commands",
                    ));
                } else {
                    state.pending_waits.push_back(PendingWait {
                        id,
                        frames_left: frames,
                        responder,
                    });
                }
            }
            AgentCommand::Capture => {
                if let Err(error) = fs::create_dir_all(&config.capture_dir) {
                    let _ = responder.send(AgentResponse::error(
                        id,
                        "capture_dir",
                        format!("failed to create capture directory: {error}"),
                    ));
                    continue;
                }

                let sequence = state.next_capture;
                state.next_capture += 1;
                let path = config
                    .capture_dir
                    .join(format!("capture-{sequence:06}.png"));
                let capture = CaptureInfo {
                    sequence,
                    path: path.to_string_lossy().into_owned(),
                };
                let max_captures = config.max_captures.max(1);

                commands.spawn(Screenshot::primary_window()).observe(
                    move |screenshot: On<ScreenshotCaptured>,
                          mut state: ResMut<AgentFeedbackState>| {
                        let response = match save_capture(&screenshot.image, &path) {
                            Ok(()) => {
                                state.latest_capture = Some(capture.clone());
                                state.captures.push_back(path.clone());
                                while state.captures.len() > max_captures {
                                    if let Some(old_capture) = state.captures.pop_front() {
                                        let _ = fs::remove_file(old_capture);
                                    }
                                }
                                AgentResponse::ok(
                                    id.clone(),
                                    "captured",
                                    Some(capture.clone()),
                                    Some(capture.clone()),
                                )
                            }
                            Err(error) => AgentResponse::error(
                                id.clone(),
                                "capture_failed",
                                format!("failed to save capture: {error}"),
                            ),
                        };
                        let _ = responder.send(response);
                    },
                );
            }
        }
    }
}

fn save_capture(image: &bevy::image::Image, path: &Path) -> io::Result<()> {
    let rgb = image
        .clone()
        .try_into_dynamic()
        .map_err(io::Error::other)?
        .to_rgb8();
    rgb.save_with_format(path, image::ImageFormat::Png)
        .map_err(io::Error::other)
}

fn write_protocol_file(config: &AgentFeedbackConfig, socket_addr: SocketAddr) -> io::Result<()> {
    if let Some(parent) = config.protocol_file.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir_all(&config.capture_dir)?;

    let protocol = json!({
        "protocol": "bevy-agent-feedback/1",
        "socket_addr": socket_addr.to_string(),
        "transport": "json-lines-over-tcp",
        "clients": "single local client at a time",
        "capture_dir": config.capture_dir.to_string_lossy(),
        "command_timeout_ms": config.command_timeout.as_millis(),
        "commands": {
            "key_down": { "key": "Bevy KeyCode string, e.g. KeyW" },
            "key_up": { "key": "Bevy KeyCode string, e.g. KeyW" },
            "mouse_down": { "button": "MouseButton string, e.g. Left" },
            "mouse_up": { "button": "MouseButton string, e.g. Left" },
            "wait": { "frames": format!("1..={}", config.max_wait_frames) },
            "capture": {}
        },
        "examples": [
            { "id": 1, "command": "key_down", "key": "KeyW" },
            { "id": 2, "command": "wait", "frames": 3 },
            { "id": 3, "command": "capture" },
            { "id": 4, "command": "key_up", "key": "KeyW" }
        ]
    });
    let bytes = serde_json::to_vec_pretty(&protocol).map_err(io::Error::other)?;
    fs::write(&config.protocol_file, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_key_down_command() {
        let request = parse_request(r#"{"id":1,"command":"key_down","key":"KeyW"}"#, 10)
            .expect("valid request");

        assert_eq!(request.id, Value::from(1));
        assert_eq!(request.command, AgentCommand::KeyDown(KeyCode::KeyW));
    }

    #[test]
    fn rejects_wait_commands_outside_the_frame_bound() {
        let error = parse_request(r#"{"id":"slow","command":"wait","frames":11}"#, 10)
            .expect_err("frame bound should be enforced");

        assert!(error.contains("frames"));
    }

    #[test]
    fn socket_key_down_updates_bevy_input() {
        let root = std::env::temp_dir().join(format!(
            "bevy-agent-feedback-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock before unix epoch")
                .as_nanos()
        ));
        let config = AgentFeedbackConfig {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            protocol_file: root.join("agent.json"),
            capture_dir: root.join("captures"),
            command_timeout: Duration::from_secs(2),
            ..Default::default()
        };
        let mut app = App::new();
        app.insert_resource(ButtonInput::<KeyCode>::default());
        app.insert_resource(ButtonInput::<MouseButton>::default());
        app.add_plugins(AgentFeedbackPlugin::new(config.clone()));

        let protocol: Value = serde_json::from_slice(
            &fs::read(&config.protocol_file).expect("protocol file should be written"),
        )
        .expect("protocol file should be JSON");
        let mut stream = TcpStream::connect(
            protocol["socket_addr"]
                .as_str()
                .expect("protocol should expose socket address"),
        )
        .expect("agent socket should accept local connections");
        stream.set_nonblocking(true).expect("nonblocking stream");
        stream
            .write_all(
                br#"{"id":1,"command":"key_down","key":"KeyW"}
"#,
            )
            .expect("send key command");

        let response = read_response_while_updating(&mut app, &mut stream);
        assert_eq!(response["ok"], Value::Bool(true));
        assert!(
            app.world()
                .resource::<ButtonInput<KeyCode>>()
                .pressed(KeyCode::KeyW)
        );

        let _ = fs::remove_dir_all(root);
    }

    fn read_response_while_updating(app: &mut App, stream: &mut TcpStream) -> Value {
        let mut bytes = Vec::new();
        let mut buf = [0_u8; 512];
        for _ in 0..100 {
            app.update();
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(read) => {
                    bytes.extend_from_slice(&buf[..read]);
                    if bytes.contains(&b'\n') {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("read failed: {error}"),
            }
            thread::sleep(Duration::from_millis(10));
        }

        assert!(!bytes.is_empty(), "no response from agent socket");
        serde_json::from_slice(bytes.split(|byte| *byte == b'\n').next().unwrap())
            .expect("response should be JSON")
    }
}
