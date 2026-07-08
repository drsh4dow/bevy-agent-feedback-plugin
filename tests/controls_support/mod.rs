use bevy::{
    app::AppExit,
    input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseWheel},
    prelude::*,
    render::view::window::screenshot::Screenshot,
    window::{FileDragAndDrop, Ime, PrimaryWindow},
};
use bevy_agent_feedback_plugin::{AgentFeedbackConfig, AgentFeedbackPlugin};
use serde_json::Value;
use std::{
    fs,
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Resource, Default)]
pub(crate) struct ObservedControls {
    pub(crate) text: String,
    pub(crate) motion_delta: Vec2,
    pub(crate) scroll_delta: Vec2,
    pub(crate) scroll_y: f32,
    pub(crate) dropped_file: Option<PathBuf>,
}

#[derive(Resource, Default)]
pub(crate) struct ObservedExits {
    pub(crate) count: usize,
}

pub(crate) fn observe_controls(
    mut observed: ResMut<ObservedControls>,
    mut ime: MessageReader<Ime>,
    mut mouse_wheel: MessageReader<MouseWheel>,
    mut file_drag_drop: MessageReader<FileDragAndDrop>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
) {
    if motion.delta != Vec2::ZERO {
        observed.motion_delta = motion.delta;
    }
    if scroll.delta != Vec2::ZERO {
        observed.scroll_delta = scroll.delta;
    }
    for event in ime.read() {
        if let Ime::Commit { value, .. } = event {
            observed.text.push_str(value);
        }
    }
    for event in mouse_wheel.read() {
        observed.scroll_y += event.y;
    }
    for event in file_drag_drop.read() {
        if let FileDragAndDrop::DroppedFile { path_buf, .. } = event {
            observed.dropped_file = Some(path_buf.clone());
        }
    }
}

pub(crate) fn agent_app(name: &str) -> (App, AgentFeedbackConfig) {
    agent_app_with_config(name, None)
}

pub(crate) fn agent_app_with_idle_shutdown(
    name: &str,
    idle_after: Duration,
) -> (App, AgentFeedbackConfig) {
    agent_app_with_config(name, Some(idle_after))
}

fn agent_app_with_config(
    name: &str,
    idle_shutdown_after: Option<Duration>,
) -> (App, AgentFeedbackConfig) {
    let root = temp_root(name);
    let config = AgentFeedbackConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        protocol_file: root.join("agent.json"),
        capture_dir: root.join("captures"),
        command_timeout: Duration::from_secs(2),
        idle_shutdown_after,
        ..Default::default()
    };
    let mut app = App::new();
    app.add_plugins(bevy::input::InputPlugin);
    app.world_mut().spawn((
        Window {
            resolution: bevy::window::WindowResolution::new(640, 480)
                .with_scale_factor_override(1.0),
            ..default()
        },
        PrimaryWindow,
    ));
    app.add_plugins(AgentFeedbackPlugin::new(config.clone()));
    (app, config)
}

fn temp_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "bevy-agent-feedback-{name}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos()
    ))
}

pub(crate) fn heartbeat_path(config: &AgentFeedbackConfig) -> PathBuf {
    let protocol: Value = serde_json::from_slice(
        &fs::read(&config.protocol_file).expect("protocol file should be written"),
    )
    .expect("protocol file should be JSON");
    PathBuf::from(
        protocol["heartbeat_file"]
            .as_str()
            .expect("protocol should expose heartbeat file"),
    )
}

pub(crate) fn screenshot_entity(app: &mut App) -> Entity {
    for _ in 0..100 {
        app.update();
        let entity = {
            let world = app.world_mut();
            let mut query = world.query_filtered::<Entity, With<Screenshot>>();
            query.iter(world).next()
        };
        if let Some(entity) = entity {
            return entity;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("capture command did not spawn a screenshot entity");
}

pub(crate) fn connect(config: &AgentFeedbackConfig) -> TcpStream {
    let protocol: Value = serde_json::from_slice(
        &fs::read(&config.protocol_file).expect("protocol file should be written"),
    )
    .expect("protocol file should be JSON");
    let stream = TcpStream::connect(
        protocol["socket_addr"]
            .as_str()
            .expect("protocol should expose socket address"),
    )
    .expect("agent socket should accept local connections");
    stream.set_nonblocking(true).expect("nonblocking stream");
    stream
}

pub(crate) fn update_for(app: &mut App, duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        app.update();
        thread::sleep(Duration::from_millis(10));
    }
}

pub(crate) fn observe_app_exit(
    mut observed: ResMut<ObservedExits>,
    mut app_exit: MessageReader<AppExit>,
) {
    observed.count += app_exit.read().count();
}

pub(crate) fn send_raw(stream: &mut TcpStream, request: &str) {
    writeln!(stream, "{request}").expect("send agent command");
}

pub(crate) fn send(app: &mut App, stream: &mut TcpStream, request: &str) -> Value {
    send_raw(stream, request);
    read_response_while_updating(app, stream)
}

pub(crate) fn send_ok(app: &mut App, stream: &mut TcpStream, request: &str) -> Value {
    let response = send(app, stream, request);
    assert_eq!(response["ok"], Value::Bool(true));
    response
}

pub(crate) fn read_response_while_updating(app: &mut App, stream: &mut TcpStream) -> Value {
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
