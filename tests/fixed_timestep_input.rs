mod support;

use bevy::prelude::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::Sender,
};
use support::{Probe, finish_probe};

#[test]
#[ignore = "requires a graphics-capable environment"]
fn fixed_timestep_accepts_agent_input_and_capture() {
    support::run_agent_render_test(
        "fixed-timestep-input",
        "Fixed timestep agent feedback test",
        KeyCode::KeyW,
        add_to_app,
    );
}

#[derive(Component)]
struct AgentDriven;

fn add_to_app(app: &mut App, capture_done: Arc<AtomicBool>, result: Sender<Result<(), String>>) {
    app.insert_resource(Probe {
        capture_done,
        result: Some(result),
        max_frames: 600,
    })
    .add_systems(Startup, spawn_scene)
    .add_systems(FixedUpdate, move_from_agent_input)
    .add_systems(Update, finish_when_moved);
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
        MeshMaterial3d(materials.add(Color::srgb(1.0, 0.2, 0.1))),
        Transform::default(),
    ));
}

fn move_from_agent_input(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mut marker: Single<&mut Transform, With<AgentDriven>>,
    time: Res<Time<Fixed>>,
) {
    if keyboard_input.pressed(KeyCode::KeyW) {
        marker.translation.x += 4.0 * time.delta_secs();
    }
}

fn finish_when_moved(
    mut probe: ResMut<Probe>,
    marker: Single<&Transform, With<AgentDriven>>,
    mut app_exit: MessageWriter<AppExit>,
    mut frames: Local<u32>,
) {
    if probe.result.is_none() {
        return;
    }

    *frames += 1;
    let capture_done = probe.capture_done.load(Ordering::Relaxed);
    if marker.translation.x > 1.0 && capture_done {
        finish_probe(&mut probe, &mut app_exit, Ok(()));
    } else if *frames > probe.max_frames {
        finish_probe(
            &mut probe,
            &mut app_exit,
            Err(format!(
                "marker x was {}, capture_done={}",
                marker.translation.x, capture_done
            )),
        );
    }
}
