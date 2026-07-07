use crate::{
    config::AgentFeedbackConfig,
    protocol::{AgentCommand, AgentResponse, parse_request, write_protocol_file},
    session::AgentFeedbackSession,
};
use bevy::prelude::*;
use serde_json::Value;
use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub(crate) struct AgentFeedbackRuntimePlugin;

impl Plugin for AgentFeedbackRuntimePlugin {
    fn build(&self, app: &mut App) {
        let config = app.world().resource::<AgentFeedbackConfig>().clone();
        let session = AgentFeedbackSession::new(&config);
        match start_runtime(&config, session.clone()) {
            Ok((runtime, socket_addr)) => {
                if let Err(error) = write_protocol_file(&config, &session, socket_addr) {
                    log::error!("failed to write agent protocol file: {error}");
                }
                app.insert_resource(session).insert_resource(runtime);
            }
            Err(error) => log::error!("failed to start agent feedback socket: {error}"),
        }
    }
}

#[derive(Resource)]
pub(crate) struct AgentFeedbackRuntime {
    pub(crate) requests: Mutex<Receiver<AgentRequest>>,
    pub(crate) release_on_disconnect: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
    session: AgentFeedbackSession,
    protocol_file: PathBuf,
}

impl Drop for AgentFeedbackRuntime {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        let Ok(mut thread) = self.thread.lock() else {
            self.session.cleanup(&self.protocol_file);
            return;
        };
        if let Some(thread) = thread.take() {
            let _ = thread.join();
        }
        self.session.cleanup(&self.protocol_file);
    }
}

pub(crate) struct AgentRequest {
    pub(crate) id: Value,
    pub(crate) command: AgentCommand,
    pub(crate) responder: SyncSender<AgentResponse>,
}

fn start_runtime(
    config: &AgentFeedbackConfig,
    session: AgentFeedbackSession,
) -> io::Result<(AgentFeedbackRuntime, SocketAddr)> {
    let listener = TcpListener::bind(config.bind_addr)?;
    listener.set_nonblocking(true)?;
    let socket_addr = listener.local_addr()?;
    let (sender, receiver) = sync_channel(config.max_pending_commands.max(1));
    let running = Arc::new(AtomicBool::new(true));
    let release_on_disconnect = Arc::new(AtomicBool::new(false));
    let server_running = running.clone();
    let server_release_on_disconnect = release_on_disconnect.clone();
    let command_timeout = config.command_timeout;
    let max_wait_frames = config.max_wait_frames;
    let max_action_steps = config.max_action_steps;
    let thread = thread::Builder::new()
        .name("bevy-agent-feedback".to_string())
        .spawn(move || {
            server_loop(
                listener,
                sender,
                server_running,
                command_timeout,
                max_wait_frames,
                max_action_steps,
                server_release_on_disconnect,
            )
        })?;

    Ok((
        AgentFeedbackRuntime {
            requests: Mutex::new(receiver),
            release_on_disconnect,
            running,
            thread: Mutex::new(Some(thread)),
            protocol_file: config.protocol_file.clone(),
            session,
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
    max_action_steps: u16,
    release_on_disconnect: Arc<AtomicBool>,
) {
    while running.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => handle_client(
                stream,
                &sender,
                &running,
                command_timeout,
                max_wait_frames,
                max_action_steps,
                release_on_disconnect.clone(),
            ),
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
    max_action_steps: u16,
    release_on_disconnect: Arc<AtomicBool>,
) {
    let _release_guard = DisconnectReleaseGuard {
        release_on_disconnect,
    };
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
                    if !line.is_empty()
                        && handle_line(
                            line,
                            &mut stream,
                            sender,
                            command_timeout,
                            max_wait_frames,
                            max_action_steps,
                        )
                        .is_err()
                    {
                        return;
                    }
                }
            }
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.kind() == io::ErrorKind::TimedOut => {}
            Err(error) => {
                let _ = write_response(
                    &mut stream,
                    &AgentResponse::error(Value::Null, "socket_error", error.to_string()),
                );
                return;
            }
        }
    }
}

fn handle_line(
    line: &str,
    stream: &mut TcpStream,
    sender: &SyncSender<AgentRequest>,
    command_timeout: Duration,
    max_wait_frames: u16,
    max_action_steps: u16,
) -> io::Result<()> {
    let request = match parse_request(line, max_wait_frames, max_action_steps) {
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
        Ok(()) => wait_for_response(stream, response_receiver, id, command_timeout),
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

fn wait_for_response(
    stream: &mut TcpStream,
    response_receiver: Receiver<AgentResponse>,
    id: Value,
    command_timeout: Duration,
) -> io::Result<()> {
    let start = Instant::now();
    while start.elapsed() < command_timeout {
        match response_receiver.recv_timeout(Duration::from_millis(20)) {
            Ok(response) => return write_response(stream, &response),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if socket_closed(stream) {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "client disconnected while command was pending",
                    ));
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return write_response(
                    stream,
                    &AgentResponse::error(id, "closed", "game command queue is closed"),
                );
            }
        }
    }
    write_response(
        stream,
        &AgentResponse::error(
            id,
            "timeout",
            format!(
                "game did not answer within {} ms",
                command_timeout.as_millis()
            ),
        ),
    )
}

fn socket_closed(stream: &TcpStream) -> bool {
    let mut byte = [0_u8; 1];
    match stream.peek(&mut byte) {
        Ok(0) => true,
        Ok(_) => false,
        Err(error)
            if error.kind() == io::ErrorKind::WouldBlock
                || error.kind() == io::ErrorKind::TimedOut =>
        {
            false
        }
        Err(_) => true,
    }
}

fn write_response(stream: &mut TcpStream, response: &AgentResponse) -> io::Result<()> {
    serde_json::to_writer(&mut *stream, response).map_err(io::Error::other)?;
    stream.write_all(b"\n")?;
    stream.flush()
}

struct DisconnectReleaseGuard {
    release_on_disconnect: Arc<AtomicBool>,
}

impl Drop for DisconnectReleaseGuard {
    fn drop(&mut self) {
        self.release_on_disconnect.store(true, Ordering::Relaxed);
    }
}
