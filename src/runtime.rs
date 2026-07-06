use crate::{
    config::AgentFeedbackConfig,
    protocol::{AgentCommand, AgentResponse, parse_request, write_protocol_file},
};
use bevy::prelude::*;
use serde_json::Value;
use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub(crate) struct AgentFeedbackRuntimePlugin;

impl Plugin for AgentFeedbackRuntimePlugin {
    fn build(&self, app: &mut App) {
        let config = app.world().resource::<AgentFeedbackConfig>().clone();
        match start_runtime(&config) {
            Ok((runtime, socket_addr)) => {
                if let Err(error) = write_protocol_file(&config, socket_addr) {
                    log::error!("failed to write agent protocol file: {error}");
                }
                app.insert_resource(runtime);
            }
            Err(error) => log::error!("failed to start agent feedback socket: {error}"),
        }
    }
}

#[derive(Resource)]
pub(crate) struct AgentFeedbackRuntime {
    pub(crate) requests: Mutex<Receiver<AgentRequest>>,
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

pub(crate) struct AgentRequest {
    pub(crate) id: Value,
    pub(crate) command: AgentCommand,
    pub(crate) responder: SyncSender<AgentResponse>,
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
