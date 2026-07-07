use bevy::{
    app::AppExit,
    input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseWheel},
    prelude::*,
    window::{FileDragAndDrop, Ime, PrimaryWindow},
};
use bevy_agent_feedback_plugin::{
    AgentFeedbackConfig, AgentFeedbackPlugin,
    client::{AgentClient, AgentClientConfig},
};
use serde_json::Value;
use std::{
    fs,
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[test]
fn socket_key_and_mouse_buttons_update_bevy_input() {
    let (mut app, config) = agent_app("key-mouse-buttons");
    let mut stream = connect(&config);

    send_ok(
        &mut app,
        &mut stream,
        r#"{"id":1,"command":"key_down","key":"KeyW"}"#,
    );
    assert!(
        app.world()
            .resource::<ButtonInput<KeyCode>>()
            .pressed(KeyCode::KeyW)
    );

    send_ok(
        &mut app,
        &mut stream,
        r#"{"id":2,"command":"mouse_down","button":"Left"}"#,
    );
    assert!(
        app.world()
            .resource::<ButtonInput<MouseButton>>()
            .pressed(MouseButton::Left)
    );

    send_ok(
        &mut app,
        &mut stream,
        r#"{"id":3,"command":"mouse_up","button":"Left"}"#,
    );
    assert!(
        !app.world()
            .resource::<ButtonInput<MouseButton>>()
            .pressed(MouseButton::Left)
    );

    send_ok(
        &mut app,
        &mut stream,
        r#"{"id":4,"command":"key_up","key":"KeyW"}"#,
    );
    assert!(
        !app.world()
            .resource::<ButtonInput<KeyCode>>()
            .pressed(KeyCode::KeyW)
    );

    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn socket_cursor_move_updates_window_and_returns_metadata() {
    let (mut app, config) = agent_app("cursor-move");
    let mut stream = connect(&config);

    let response = send_ok(
        &mut app,
        &mut stream,
        r#"{"id":1,"command":"cursor_move","x":320,"y":240}"#,
    );
    assert_eq!(
        response["result"]["window"]["logical_width"],
        Value::from(640.0)
    );

    let (_, window) = app
        .world_mut()
        .query_filtered::<(Entity, &Window), With<PrimaryWindow>>()
        .single(app.world())
        .expect("primary window");
    assert_eq!(window.cursor_position(), Some(Vec2::new(320.0, 240.0)));
    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn socket_text_scroll_and_file_drop_emit_bevy_messages() {
    let (mut app, config) = agent_app("messages");
    app.insert_resource(ObservedControls::default())
        .add_systems(Update, observe_controls);
    let mut stream = connect(&config);

    send_ok(
        &mut app,
        &mut stream,
        r#"{"id":1,"command":"text","value":"hello"}"#,
    );
    send_ok(
        &mut app,
        &mut stream,
        r#"{"id":2,"command":"mouse_motion","dx":5,"dy":-2}"#,
    );
    send_ok(
        &mut app,
        &mut stream,
        r#"{"id":3,"command":"mouse_scroll","y":-1}"#,
    );
    send_ok(
        &mut app,
        &mut stream,
        r#"{"id":4,"command":"file_drop","path":"/tmp/agent-file.txt"}"#,
    );

    let observed = app.world().resource::<ObservedControls>();
    assert_eq!(observed.text, "hello");
    assert_eq!(observed.motion_delta, Vec2::new(5.0, -2.0));
    assert_eq!(observed.scroll_delta, Vec2::new(0.0, -1.0));
    assert_eq!(observed.scroll_y, -1.0);
    assert_eq!(
        observed.dropped_file,
        Some(PathBuf::from("/tmp/agent-file.txt"))
    );
    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn high_level_actions_release_after_requested_frames() {
    let (mut app, config) = agent_app("high-level-actions");
    let mut stream = connect(&config);

    send_ok(
        &mut app,
        &mut stream,
        r#"{"id":1,"command":"key_hold","key":"keyw","frames":2}"#,
    );
    assert!(
        !app.world()
            .resource::<ButtonInput<KeyCode>>()
            .pressed(KeyCode::KeyW)
    );

    send_ok(
        &mut app,
        &mut stream,
        r#"{"id":2,"command":"click","x":320,"y":240,"button":"left","frames":1}"#,
    );
    assert!(
        !app.world()
            .resource::<ButtonInput<MouseButton>>()
            .pressed(MouseButton::Left)
    );
    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn release_all_inputs_releases_tracked_inputs() {
    let (mut app, config) = agent_app("release-all");
    let mut stream = connect(&config);

    send_ok(
        &mut app,
        &mut stream,
        r#"{"id":1,"command":"key_down","key":"KeyW"}"#,
    );
    send_ok(
        &mut app,
        &mut stream,
        r#"{"id":2,"command":"mouse_down","button":"Left"}"#,
    );
    assert!(
        app.world()
            .resource::<ButtonInput<KeyCode>>()
            .pressed(KeyCode::KeyW)
    );
    assert!(
        app.world()
            .resource::<ButtonInput<MouseButton>>()
            .pressed(MouseButton::Left)
    );

    send_ok(
        &mut app,
        &mut stream,
        r#"{"id":3,"command":"release_all_inputs"}"#,
    );
    assert!(
        !app.world()
            .resource::<ButtonInput<KeyCode>>()
            .pressed(KeyCode::KeyW)
    );
    assert!(
        !app.world()
            .resource::<ButtonInput<MouseButton>>()
            .pressed(MouseButton::Left)
    );
    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn drag_rejects_out_of_bounds_target_before_pressing_button() {
    let (mut app, config) = agent_app("drag-target-out-of-bounds");
    let mut stream = connect(&config);

    let response = send(
        &mut app,
        &mut stream,
        r#"{"id":1,"command":"drag","from":[10,10],"to":[999,10],"button":"Left","steps":2,"frames":2}"#,
    );

    assert_eq!(response["ok"], Value::Bool(false));
    assert_eq!(response["error"]["code"], "position_out_of_bounds");
    assert!(
        !app.world()
            .resource::<ButtonInput<MouseButton>>()
            .pressed(MouseButton::Left)
    );
    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn drag_releases_button_when_move_fails_mid_flight() {
    let (mut app, config) = agent_app("drag-mid-flight-failure");
    let mut stream = connect(&config);

    send_raw(
        &mut stream,
        r#"{"id":1,"command":"drag","from":[10,10],"to":[600,10],"button":"Left","steps":2,"frames":10}"#,
    );
    for _ in 0..20 {
        app.update();
        if app
            .world()
            .resource::<ButtonInput<MouseButton>>()
            .pressed(MouseButton::Left)
        {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        app.world()
            .resource::<ButtonInput<MouseButton>>()
            .pressed(MouseButton::Left)
    );

    let (_, mut window) = app
        .world_mut()
        .query_filtered::<(Entity, &mut Window), With<PrimaryWindow>>()
        .single_mut(app.world_mut())
        .expect("primary window");
    window.resolution.set(100.0, 100.0);

    let response = read_response_while_updating(&mut app, &mut stream);
    assert_eq!(response["ok"], Value::Bool(false));
    assert_eq!(response["error"]["code"], "position_out_of_bounds");
    assert!(
        !app.world()
            .resource::<ButtonInput<MouseButton>>()
            .pressed(MouseButton::Left)
    );
    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn diagnostics_without_plugin_returns_clear_error() {
    let (mut app, config) = agent_app("diagnostics-unavailable");
    let mut stream = connect(&config);
    let response = send(&mut app, &mut stream, r#"{"id":1,"command":"ecs_summary"}"#);
    assert_eq!(response["ok"], Value::Bool(false));
    assert_eq!(response["error"]["code"], "diagnostics_unavailable");
    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn shutdown_command_returns_ok() {
    let (mut app, config) = agent_app("shutdown");
    let mut stream = connect(&config);
    send_ok(&mut app, &mut stream, r#"{"id":1,"command":"shutdown"}"#);
    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn disconnect_during_pending_action_releases_inputs() {
    let (mut app, config) = agent_app("disconnect-pending-action");
    let mut stream = connect(&config);
    send_raw(
        &mut stream,
        r#"{"id":1,"command":"key_hold","key":"KeyW","frames":60}"#,
    );

    for _ in 0..20 {
        app.update();
        if app
            .world()
            .resource::<ButtonInput<KeyCode>>()
            .pressed(KeyCode::KeyW)
        {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        app.world()
            .resource::<ButtonInput<KeyCode>>()
            .pressed(KeyCode::KeyW)
    );

    drop(stream);
    for _ in 0..30 {
        app.update();
        if !app
            .world()
            .resource::<ButtonInput<KeyCode>>()
            .pressed(KeyCode::KeyW)
        {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !app.world()
            .resource::<ButtonInput<KeyCode>>()
            .pressed(KeyCode::KeyW)
    );
    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn disconnect_releases_tracked_inputs() {
    let (mut app, config) = agent_app("disconnect-release");
    let mut stream = connect(&config);
    send_ok(
        &mut app,
        &mut stream,
        r#"{"id":1,"command":"key_down","key":"KeyW"}"#,
    );
    assert!(
        app.world()
            .resource::<ButtonInput<KeyCode>>()
            .pressed(KeyCode::KeyW)
    );

    drop(stream);
    for _ in 0..20 {
        app.update();
        if !app
            .world()
            .resource::<ButtonInput<KeyCode>>()
            .pressed(KeyCode::KeyW)
        {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !app.world()
            .resource::<ButtonInput<KeyCode>>()
            .pressed(KeyCode::KeyW)
    );
    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn runtime_drop_removes_live_protocol_files() {
    let config;
    let heartbeat_file;
    {
        let (app, app_config) = agent_app("cleanup");
        config = app_config;
        heartbeat_file = heartbeat_path(&config);
        assert!(config.protocol_file.exists());
        assert!(heartbeat_file.exists());
        drop(app);
    }

    assert!(!config.protocol_file.exists());
    assert!(!heartbeat_file.exists());
    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn rust_client_writes_transcript_envelopes_and_replays_compat_formats() {
    let (mut app, config) = agent_app("rust-client-transcript");
    let transcript_file = config
        .protocol_file
        .parent()
        .expect("protocol parent")
        .join("transcript.jsonl");
    let replay_file = config
        .protocol_file
        .parent()
        .expect("protocol parent")
        .join("replay.jsonl");
    let protocol_file = config.protocol_file.clone();
    let client = thread::spawn({
        let transcript_file = transcript_file.clone();
        let replay_file = replay_file.clone();
        move || -> Result<(), String> {
            let mut client = AgentClient::with_config(AgentClientConfig {
                protocol_file,
                transcript_file: Some(transcript_file),
                ..Default::default()
            })
            .map_err(|error| error.to_string())?;
            client.window_info().map_err(|error| error.to_string())?;
            fs::write(
                &replay_file,
                format!(
                    "{}\n{}\n",
                    serde_json::json!({"command": "window_info"}),
                    serde_json::json!({"request": {"command": "window_info"}}),
                ),
            )
            .map_err(|error| error.to_string())?;
            let responses = client
                .replay_jsonl(&replay_file)
                .map_err(|error| error.to_string())?;
            if responses.len() != 2
                || responses
                    .iter()
                    .any(|response| response["ok"] != Value::Bool(true))
            {
                return Err(format!("unexpected replay responses: {responses:?}"));
            }
            Ok(())
        }
    });
    for _ in 0..100 {
        app.update();
        if client.is_finished() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    client
        .join()
        .expect("client thread")
        .expect("client request");

    let transcript = fs::read_to_string(&transcript_file).expect("transcript");
    let envelopes = transcript
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("transcript line"))
        .collect::<Vec<_>>();
    assert!(envelopes.len() >= 3, "transcript: {transcript}");
    assert!(envelopes[0]["ts_ms"].as_u64().is_some());
    assert!(envelopes[0]["duration_ms"].as_u64().is_some());
    assert_eq!(envelopes[0]["request"]["command"], "window_info");
    assert_eq!(envelopes[0]["response"]["ok"], Value::Bool(true));
    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn rust_client_replay_jsonl_snapshots_transcript_before_appending() {
    let (mut app, config) = agent_app("rust-client-replay-same-transcript");
    let transcript_file = config
        .protocol_file
        .parent()
        .expect("protocol parent")
        .join("transcript.jsonl");
    fs::write(
        &transcript_file,
        format!(
            "{}\n{}\n",
            serde_json::json!({"command": "window_info"}),
            serde_json::json!({"request": {"command": "window_info"}}),
        ),
    )
    .expect("seed transcript");

    let protocol_file = config.protocol_file.clone();
    let client = thread::spawn({
        let transcript_file = transcript_file.clone();
        move || -> Result<(), String> {
            let mut client = AgentClient::with_config(AgentClientConfig {
                protocol_file,
                timeout: Duration::from_millis(250),
                transcript_file: Some(transcript_file.clone()),
                ..Default::default()
            })
            .map_err(|error| error.to_string())?;
            let responses = client
                .replay_jsonl(&transcript_file)
                .map_err(|error| error.to_string())?;
            if responses.len() != 2
                || responses
                    .iter()
                    .any(|response| response["ok"] != Value::Bool(true))
            {
                return Err(format!("unexpected replay responses: {responses:?}"));
            }
            Ok(())
        }
    });

    let finished = update_until(&mut app, Duration::from_secs(2), || client.is_finished());
    if !finished {
        drop(app);
        let _ = client.join();
        panic!("replay_jsonl should stop after the original transcript lines");
    }
    client
        .join()
        .expect("client thread")
        .expect("client replay");

    let transcript = fs::read_to_string(&transcript_file).expect("transcript");
    let envelopes = transcript
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("transcript line"))
        .collect::<Vec<_>>();
    assert_eq!(envelopes.len(), 4, "transcript: {transcript}");
    assert_eq!(envelopes[0]["command"], "window_info");
    assert_eq!(envelopes[1]["request"]["command"], "window_info");
    assert_eq!(envelopes[2]["request"]["command"], "window_info");
    assert_eq!(envelopes[2]["response"]["ok"], Value::Bool(true));
    assert_eq!(envelopes[3]["request"]["command"], "window_info");
    assert_eq!(envelopes[3]["response"]["ok"], Value::Bool(true));
    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn idle_shutdown_refreshes_only_after_accepted_commands() {
    {
        let (mut app, config) =
            agent_app_with_idle_shutdown("idle-rejected-command", Duration::from_millis(1));
        app.insert_resource(ObservedExits::default())
            .add_systems(Update, observe_app_exit);
        let mut stream = connect(&config);

        update_for(&mut app, Duration::from_millis(4000));
        let response = send(
            &mut app,
            &mut stream,
            r#"{"id":1,"command":"cursor_move","x":999,"y":10}"#,
        );
        assert_eq!(response["ok"], Value::Bool(false));
        assert_eq!(response["error"]["code"], "position_out_of_bounds");

        update_for(&mut app, Duration::from_millis(1200));
        assert!(
            app.world().resource::<ObservedExits>().count > 0,
            "rejected commands must not postpone idle shutdown"
        );
        let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
    }

    {
        let (mut app, config) =
            agent_app_with_idle_shutdown("idle-accepted-command", Duration::from_millis(1));
        app.insert_resource(ObservedExits::default())
            .add_systems(Update, observe_app_exit);
        let mut stream = connect(&config);

        update_for(&mut app, Duration::from_millis(4000));
        send_ok(&mut app, &mut stream, r#"{"id":1,"command":"window_info"}"#);

        update_for(&mut app, Duration::from_millis(1200));
        assert_eq!(
            app.world().resource::<ObservedExits>().count,
            0,
            "accepted commands must postpone idle shutdown past the original deadline"
        );
        let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
    }
}

#[derive(Resource, Default)]
struct ObservedControls {
    text: String,
    motion_delta: Vec2,
    scroll_delta: Vec2,
    scroll_y: f32,
    dropped_file: Option<PathBuf>,
}

#[derive(Resource, Default)]
struct ObservedExits {
    count: usize,
}

fn observe_controls(
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

fn agent_app(name: &str) -> (App, AgentFeedbackConfig) {
    agent_app_with_config(name, None)
}

fn agent_app_with_idle_shutdown(name: &str, idle_after: Duration) -> (App, AgentFeedbackConfig) {
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

fn heartbeat_path(config: &AgentFeedbackConfig) -> PathBuf {
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

fn connect(config: &AgentFeedbackConfig) -> TcpStream {
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

fn update_for(app: &mut App, duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        app.update();
        thread::sleep(Duration::from_millis(10));
    }
}

fn update_until(app: &mut App, timeout: Duration, done: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        app.update();
        if done() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    done()
}

fn observe_app_exit(mut observed: ResMut<ObservedExits>, mut app_exit: MessageReader<AppExit>) {
    observed.count += app_exit.read().count();
}

fn send_raw(stream: &mut TcpStream, request: &str) {
    writeln!(stream, "{request}").expect("send agent command");
}

fn send(app: &mut App, stream: &mut TcpStream, request: &str) -> Value {
    send_raw(stream, request);
    read_response_while_updating(app, stream)
}

fn send_ok(app: &mut App, stream: &mut TcpStream, request: &str) -> Value {
    let response = send(app, stream, request);
    assert_eq!(response["ok"], Value::Bool(true));
    response
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
