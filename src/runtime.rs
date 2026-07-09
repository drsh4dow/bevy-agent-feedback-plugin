use crate::{
    config::AgentFeedbackConfig,
    protocol::{
        AgentCommand, AgentResponse, ParseLimits, parse_request_with_limits, write_protocol_file,
    },
    session::AgentFeedbackSession,
};
use bevy::{app::AppExit, prelude::*};
use serde_json::Value;
use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel},
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
    last_accepted_command: Instant,
    running: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
    session: AgentFeedbackSession,
    protocol_file: PathBuf,
}

impl AgentFeedbackRuntime {
    pub(crate) fn record_accepted_command(&mut self) {
        self.last_accepted_command = Instant::now();
    }
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
    pub(crate) canceled: Arc<AtomicBool>,
}

struct ServerSettings {
    command_timeout: Duration,
    max_wait_frames: u16,
    max_action_steps: u16,
    max_time_advance_steps: u16,
    max_time_advance: Duration,
    release_on_disconnect: Arc<AtomicBool>,
}

pub(crate) fn idle_shutdown(
    config: Res<AgentFeedbackConfig>,
    runtime: Option<ResMut<AgentFeedbackRuntime>>,
    mut app_exit: MessageWriter<AppExit>,
) {
    let Some(idle_after) = config.idle_shutdown_after else {
        return;
    };
    let Some(mut runtime) = runtime else {
        return;
    };
    let idle_after = idle_after.max(Duration::from_secs(5));
    if runtime.last_accepted_command.elapsed() < idle_after {
        return;
    }

    runtime.last_accepted_command = Instant::now();
    log::warn!(
        "agent feedback idle shutdown after {} ms without an accepted command",
        idle_after.as_millis()
    );
    app_exit.write(AppExit::Success);
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
    let last_accepted_command = Instant::now();
    let server_running = running.clone();
    let settings = ServerSettings {
        command_timeout: config.command_timeout,
        max_wait_frames: config.max_wait_frames,
        max_action_steps: config.max_action_steps,
        max_time_advance_steps: config.max_time_advance_steps,
        max_time_advance: config.max_time_advance,
        release_on_disconnect: release_on_disconnect.clone(),
    };
    let thread = thread::Builder::new()
        .name("bevy-agent-feedback".to_string())
        .spawn(move || server_loop(listener, sender, server_running, settings))?;

    Ok((
        AgentFeedbackRuntime {
            requests: Mutex::new(receiver),
            release_on_disconnect,
            last_accepted_command,
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
    settings: ServerSettings,
) {
    while running.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => handle_client(stream, &sender, &running, &settings),
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
    settings: &ServerSettings,
) {
    let _release_guard = DisconnectReleaseGuard {
        release_on_disconnect: settings.release_on_disconnect.clone(),
    };
    if stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .is_err()
    {
        return;
    }
    if stream
        .set_write_timeout(Some(settings.command_timeout))
        .is_err()
    {
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
                    if !line.is_empty() && handle_line(line, &mut stream, sender, settings).is_err()
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

const QUEUE_CLOSED: &str = "game command queue is closed; game likely exited - check game.log";

fn handle_line(
    line: &str,
    stream: &mut TcpStream,
    sender: &SyncSender<AgentRequest>,
    settings: &ServerSettings,
) -> io::Result<()> {
    let request = match parse_request_with_limits(
        line,
        ParseLimits {
            max_wait_frames: settings.max_wait_frames,
            max_action_steps: settings.max_action_steps,
            max_time_advance_steps: settings.max_time_advance_steps,
            max_time_advance: settings.max_time_advance,
        },
    ) {
        Ok(request) => request,
        Err(error) => {
            return write_response(
                stream,
                &AgentResponse::error(error.id, error.code, error.message),
            );
        }
    };

    let id = request.id.clone();
    let (response_sender, response_receiver) = sync_channel(1);
    let canceled = Arc::new(AtomicBool::new(false));
    let agent_request = AgentRequest {
        id: request.id,
        command: request.command,
        responder: response_sender,
        canceled: canceled.clone(),
    };

    match sender.try_send(agent_request) {
        Ok(()) => wait_for_response(
            stream,
            response_receiver,
            id,
            settings.command_timeout,
            &canceled,
        ),
        Err(TrySendError::Full(request)) => write_response(
            stream,
            &AgentResponse::error(request.id, "queue_full", "game command queue is full"),
        ),
        Err(TrySendError::Disconnected(request)) => write_response(
            stream,
            &AgentResponse::error(request.id, "closed", QUEUE_CLOSED),
        ),
    }
}

fn wait_for_response(
    stream: &mut TcpStream,
    response_receiver: Receiver<AgentResponse>,
    id: Value,
    command_timeout: Duration,
    canceled: &AtomicBool,
) -> io::Result<()> {
    let start = Instant::now();
    loop {
        let remaining = command_timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            break;
        }
        match response_receiver.recv_timeout(remaining.min(Duration::from_millis(20))) {
            Ok(response) => return write_response(stream, &response),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if socket_closed(stream) {
                    canceled.store(true, Ordering::Release);
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "client disconnected while command was pending",
                    ));
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return write_response(stream, &AgentResponse::error(id, "closed", QUEUE_CLOSED));
            }
        }
    }
    match response_receiver.try_recv() {
        Ok(response) => write_response(stream, &response),
        Err(TryRecvError::Disconnected) => {
            write_response(stream, &AgentResponse::error(id, "closed", QUEUE_CLOSED))
        }
        Err(TryRecvError::Empty) => {
            canceled.store(true, Ordering::Release);
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
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{BufRead, BufReader};

    #[test]
    fn queued_response_wins_at_zero_deadline() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind local response socket");
        listener
            .set_nonblocking(true)
            .expect("bound local response accept");
        let address = listener.local_addr().expect("local response address");
        let client = TcpStream::connect_timeout(&address, Duration::from_secs(1))
            .expect("connect local response socket");
        let (mut server, _) = listener.accept().expect("accept local response socket");
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("bound response read");
        server
            .set_write_timeout(Some(Duration::from_secs(1)))
            .expect("bound response write");

        let (response_sender, response_receiver) = sync_channel(1);
        response_sender
            .try_send(AgentResponse::ok(json!(17), "queued", None, None))
            .expect("prequeue response");
        let canceled = AtomicBool::new(false);

        wait_for_response(
            &mut server,
            response_receiver,
            json!(17),
            Duration::ZERO,
            &canceled,
        )
        .expect("write prequeued response");

        assert!(!canceled.load(Ordering::Acquire));
        let mut line = String::new();
        BufReader::new(client)
            .read_line(&mut line)
            .expect("read prequeued response");
        assert_eq!(
            serde_json::from_str::<Value>(&line).expect("response JSON"),
            json!({"id": 17, "ok": true, "result": {"status": "queued"}})
        );
    }
}
