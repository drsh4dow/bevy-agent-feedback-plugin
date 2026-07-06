use bevy::{prelude::*, window::WindowResolution};
use bevy_agent_feedback_plugin::{AgentFeedbackConfig, AgentFeedbackPlugin};
use std::{net::SocketAddr, path::PathBuf};

#[derive(Component)]
struct AgentDriven;

fn main() {
    let config = AgentFeedbackConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        protocol_file: PathBuf::from("target/agent-feedback/examples/minimal/agent-feedback.json"),
        capture_dir: PathBuf::from("target/agent-feedback/examples/minimal/captures"),
        ..Default::default()
    };

    println!("agent protocol: {}", config.protocol_file.display());

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Agent Feedback Minimal".into(),
                resolution: WindowResolution::new(640, 480).with_scale_factor_override(1.0),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(AgentFeedbackPlugin::new(config))
        .add_systems(Startup, spawn_scene)
        .add_systems(Update, move_from_agent_input)
        .run();
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
