use bevy::{
    app::AppExit,
    prelude::*,
    render::RenderPlugin,
    window::{ExitCondition, WindowResolution},
    winit::WinitPlugin,
};
use bevy_agent_feedback_plugin::{AgentFeedbackConfig, AgentFeedbackPlugin};
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

#[derive(Component)]
struct AgentDriven;

#[derive(Resource)]
struct DemoResult {
    agent: Arc<Mutex<Option<Result<(), String>>>>,
    frames: u32,
}

fn main() {
    let config = AgentFeedbackConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        protocol_file: PathBuf::from(
            "target/agent-feedback/examples/agent-driven/agent-feedback.json",
        ),
        capture_dir: PathBuf::from("target/agent-feedback/examples/agent-driven/captures"),
        max_wait_frames: 600,
        command_timeout: Duration::from_secs(30),
        ..Default::default()
    };
    let protocol_file = config.protocol_file.clone();
    let agent_result = Arc::new(Mutex::new(None));

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "Agent Feedback Self-Driving Demo".into(),
                    resolution: WindowResolution::new(640, 480).with_scale_factor_override(1.0),
                    ..default()
                }),
                exit_condition: ExitCondition::DontExit,
                ..default()
            })
            .set(RenderPlugin {
                synchronous_pipeline_compilation: true,
                ..default()
            })
            .set(WinitPlugin {
                run_on_any_thread: true,
            }),
    )
    .add_plugins(AgentFeedbackPlugin::new(config))
    .insert_resource(DemoResult {
        agent: agent_result.clone(),
        frames: 0,
    })
    .add_systems(Startup, spawn_scene)
    .add_systems(
        Update,
        (move_from_agent_input, finish_when_agent_done).chain(),
    );

    let _agent_thread = thread::spawn(move || {
        let outcome = drive_agent(&protocol_file);
        if let Ok(mut result) = agent_result.lock() {
            *result = Some(outcome);
        }
    });

    app.run();
}

fn spawn_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((
        Camera3d::default(),
        Transform::from_xyz(0.0, 0.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((PointLight::default(), Transform::from_xyz(4.0, 8.0, 4.0)));
    commands.spawn((
        AgentDriven,
        Mesh3d(meshes.add(Sphere::new(0.8))),
        MeshMaterial3d(materials.add(Color::srgb(0.1, 0.4, 1.0))),
        Transform::default(),
    ));
}

fn move_from_agent_input(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mut marker: Single<&mut Transform, With<AgentDriven>>,
) {
    if keyboard_input.pressed(KeyCode::KeyW) {
        marker.translation.x = 1.5;
    }
}

fn finish_when_agent_done(
    mut demo: ResMut<DemoResult>,
    marker: Single<&Transform, With<AgentDriven>>,
    mut app_exit: MessageWriter<AppExit>,
) {
    demo.frames += 1;
    if demo.frames > 600 {
        eprintln!("agent demo timed out");
        app_exit.write(AppExit::Success);
        return;
    }

    let Ok(mut result) = demo.agent.lock() else {
        return;
    };
    let Some(result) = result.take() else {
        return;
    };

    match result {
        Ok(()) if marker.translation.x > 1.0 => {
            println!("agent demo completed");
        }
        Ok(()) => {
            eprintln!("agent completed, but the marker did not move");
        }
        Err(error) => {
            eprintln!("agent failed: {error}");
        }
    }
    app_exit.write(AppExit::Success);
}

fn drive_agent(protocol_file: &Path) -> Result<(), String> {
    let socket_addr = read_socket_addr(protocol_file)?;
    let (mut stream, mut reader) = connect(socket_addr)?;

    send_request(
        &mut stream,
        &mut reader,
        r#"{"id":1,"command":"wait","frames":10}"#,
    )?;
    let before = send_request(&mut stream, &mut reader, r#"{"id":2,"command":"capture"}"#)?;
    println!("before capture: {}", capture_path(&before)?);

    send_request(
        &mut stream,
        &mut reader,
        r#"{"id":3,"command":"key_down","key":"KeyW"}"#,
    )?;
    send_request(
        &mut stream,
        &mut reader,
        r#"{"id":4,"command":"wait","frames":45}"#,
    )?;
    let after = send_request(&mut stream, &mut reader, r#"{"id":5,"command":"capture"}"#)?;
    println!("after capture: {}", capture_path(&after)?);

    send_request(
        &mut stream,
        &mut reader,
        r#"{"id":6,"command":"key_up","key":"KeyW"}"#,
    )?;
    Ok(())
}

fn read_socket_addr(protocol_file: &Path) -> Result<SocketAddr, String> {
    let protocol: Value = serde_json::from_slice(
        &fs::read(protocol_file).map_err(|error| format!("read protocol file: {error}"))?,
    )
    .map_err(|error| format!("parse protocol file: {error}"))?;

    protocol["socket_addr"]
        .as_str()
        .ok_or_else(|| format!("protocol file missing socket_addr: {protocol}"))?
        .parse()
        .map_err(|error| format!("parse socket address: {error}"))
}

fn connect(socket_addr: SocketAddr) -> Result<(TcpStream, BufReader<TcpStream>), String> {
    let stream = TcpStream::connect(socket_addr).map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(30)))
        .map_err(|error| error.to_string())?;
    let reader_stream = stream.try_clone().map_err(|error| error.to_string())?;
    Ok((stream, BufReader::new(reader_stream)))
}

fn send_request(
    stream: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    request: &str,
) -> Result<Value, String> {
    writeln!(stream, "{request}").map_err(|error| error.to_string())?;

    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    if line.is_empty() {
        return Err("agent socket closed before response".to_string());
    }

    let response: Value = serde_json::from_str(&line).map_err(|error| error.to_string())?;
    if response["ok"] != Value::Bool(true) {
        return Err(format!("agent command failed: {response}"));
    }
    Ok(response)
}

fn capture_path(response: &Value) -> Result<&str, String> {
    response["result"]["capture"]["path"]
        .as_str()
        .ok_or_else(|| format!("capture response did not include a path: {response}"))
}
