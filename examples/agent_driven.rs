use bevy::{
    app::AppExit,
    prelude::*,
    render::RenderPlugin,
    window::{ExitCondition, WindowResolution},
    winit::WinitPlugin,
};
use bevy_agent_feedback_plugin::{AgentFeedbackConfig, AgentFeedbackPlugin, client::AgentClient};
use std::{
    net::SocketAddr,
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
        protocol_file: std::env::var_os("BEVY_FEEDBACK_PROTOCOL")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from("target/agent-feedback/examples/agent-driven/agent-feedback.json")
            }),
        capture_dir: std::env::var_os("BEVY_FEEDBACK_CAPTURE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from("target/agent-feedback/examples/agent-driven/captures")
            }),
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
    let mut client = AgentClient::connect(protocol_file).map_err(|error| error.to_string())?;
    client.wait(10).map_err(|error| error.to_string())?;
    let before = client.capture().map_err(|error| error.to_string())?;
    println!("before capture: {}", before.path.display());

    client
        .key_hold("KeyW", 45)
        .map_err(|error| error.to_string())?;
    let after = client.capture().map_err(|error| error.to_string())?;
    println!("after capture: {}", after.path.display());
    Ok(())
}
