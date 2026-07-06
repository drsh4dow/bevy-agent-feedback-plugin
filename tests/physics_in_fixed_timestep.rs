mod support;

use bevy::{app::AppExit, prelude::*};
use bevy_agent_feedback_plugin::AgentFeedbackPlugin;
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

#[test]
#[ignore = "requires a graphics-capable environment"]
fn fixed_timestep_example_accepts_agent_input_and_capture() {
    if support::skip_without_window_server() {
        return;
    }

    let root = support::artifact_root("fixed-timestep");
    eprintln!("agent feedback artifacts: {}", root.display());
    let config = support::agent_config(&root);
    let capture_done = Arc::new(AtomicBool::new(false));
    let (result_sender, result_receiver) = mpsc::channel();

    let mut app = App::new();
    support::add_render_plugins(&mut app, "Fixed timestep agent feedback test");
    app.add_plugins(AgentFeedbackPlugin::new(config.clone()));
    physics_in_fixed_timestep::add_to_app(&mut app, capture_done.clone(), result_sender);

    let socket_addr = support::socket_addr(&config);
    let client = thread::spawn(move || drive_fixed_timestep(socket_addr, capture_done));
    let exit = app.run();

    let app_result = result_receiver
        .recv_timeout(Duration::from_secs(1))
        .unwrap_or_else(|error| Err(format!("app exited without a test result: {error}")));
    let client_result = client
        .join()
        .unwrap_or_else(|_| Err("agent client panicked".to_string()));
    if let Err(error) = client_result {
        panic!("agent client failed: {error}");
    }
    if let Err(error) = app_result {
        panic!("fixed timestep test failed: {error}");
    }
    assert_eq!(exit, AppExit::Success);
}

fn drive_fixed_timestep(
    socket_addr: SocketAddr,
    capture_done: Arc<AtomicBool>,
) -> Result<(), String> {
    let (mut stream, mut reader) = support::connect_agent(socket_addr)?;
    support::send_request(
        &mut stream,
        &mut reader,
        r#"{"id":1,"command":"wait","frames":10}"#,
    )?;
    let before =
        support::send_request(&mut stream, &mut reader, r#"{"id":2,"command":"capture"}"#)?;
    let (before_path, before_pixels) = support::expect_png(&before)?;
    let key_down = support::send_request(
        &mut stream,
        &mut reader,
        r#"{"id":3,"command":"key_down","key":"KeyW"}"#,
    )?;
    support::expect_latest_capture(&key_down, &before_path)?;
    let wait = support::send_request(
        &mut stream,
        &mut reader,
        r#"{"id":4,"command":"wait","frames":45}"#,
    )?;
    support::expect_latest_capture(&wait, &before_path)?;
    let after = support::send_request(&mut stream, &mut reader, r#"{"id":5,"command":"capture"}"#)?;
    let (after_path, after_pixels) = support::expect_png(&after)?;
    if before_pixels == after_pixels {
        return Err(format!(
            "agent captures did not change after input: {} and {}",
            before_path.display(),
            after_path.display()
        ));
    }
    let key_up = support::send_request(
        &mut stream,
        &mut reader,
        r#"{"id":6,"command":"key_up","key":"KeyW"}"#,
    )?;
    support::expect_latest_capture(&key_up, &after_path)?;
    capture_done.store(true, Ordering::Relaxed);
    Ok(())
}

mod physics_in_fixed_timestep {
    use super::support::{Probe, finish_probe};
    use bevy::{color::palettes::tailwind, input::mouse::AccumulatedMouseMotion, prelude::*};
    use std::{
        f32::consts::FRAC_PI_2,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc::Sender,
        },
    };

    pub(super) fn add_to_app(
        app: &mut App,
        capture_done: Arc<AtomicBool>,
        result: Sender<Result<(), String>>,
    ) {
        app.init_resource::<DidFixedTimestepRunThisFrame>()
            .insert_resource(Probe {
                capture_done,
                result: Some(result),
                max_frames: 1_800,
            })
            .add_systems(Startup, (spawn_text, spawn_player, spawn_environment))
            .add_systems(PreUpdate, clear_fixed_timestep_flag)
            .add_systems(FixedPreUpdate, set_fixed_time_step_flag)
            .add_systems(FixedUpdate, advance_physics)
            .add_systems(
                RunFixedMainLoop,
                (
                    (rotate_camera, accumulate_input)
                        .chain()
                        .in_set(RunFixedMainLoopSystems::BeforeFixedMainLoop),
                    (
                        clear_input.run_if(did_fixed_timestep_run_this_frame),
                        interpolate_rendered_transform,
                        translate_camera,
                    )
                        .chain()
                        .in_set(RunFixedMainLoopSystems::AfterFixedMainLoop),
                ),
            )
            .add_systems(Update, finish_when_agent_drove_physics);
    }

    #[derive(Debug, Component, Clone, Copy, PartialEq, Default, Deref, DerefMut)]
    struct AccumulatedInput {
        movement: Vec2,
    }

    #[derive(Debug, Component, Clone, Copy, PartialEq, Default, Deref, DerefMut)]
    struct Velocity(Vec3);

    #[derive(Debug, Component, Clone, Copy, PartialEq, Default, Deref, DerefMut)]
    struct PhysicalTranslation(Vec3);

    #[derive(Debug, Component, Clone, Copy, PartialEq, Default, Deref, DerefMut)]
    struct PreviousPhysicalTranslation(Vec3);

    fn spawn_player(mut commands: Commands) {
        commands.spawn((Camera3d::default(), CameraSensitivity::default()));
        commands.spawn((
            Name::new("Player"),
            Transform::from_scale(Vec3::splat(0.3)),
            AccumulatedInput::default(),
            Velocity::default(),
            PhysicalTranslation::default(),
            PreviousPhysicalTranslation::default(),
        ));
    }

    fn spawn_environment(
        mut commands: Commands,
        mut meshes: ResMut<Assets<Mesh>>,
        mut materials: ResMut<Assets<StandardMaterial>>,
    ) {
        let sphere_material = materials.add(Color::from(tailwind::SKY_200));
        let sphere_mesh = meshes.add(Sphere::new(0.3));
        let spheres_in_x = 6;
        let spheres_in_y = 4;
        let spheres_in_z = 10;
        let distance = 3.0;
        for x in 0..spheres_in_x {
            for y in 0..spheres_in_y {
                for z in 0..spheres_in_z {
                    let translation = Vec3::new(
                        x as f32 * distance - (spheres_in_x as f32 - 1.0) * distance / 2.0,
                        y as f32 * distance - (spheres_in_y as f32 - 1.0) * distance / 2.0,
                        z as f32 * distance - (spheres_in_z as f32 - 1.0) * distance / 2.0,
                    );
                    commands.spawn((
                        Name::new("Sphere"),
                        Transform::from_translation(translation),
                        Mesh3d(sphere_mesh.clone()),
                        MeshMaterial3d(sphere_material.clone()),
                    ));
                }
            }
        }

        commands.spawn((
            DirectionalLight::default(),
            Transform::default().looking_to(Vec3::new(-1.0, -3.0, 0.5), Vec3::Y),
        ));
    }

    fn spawn_text(mut commands: Commands) {
        let font = TextFont {
            font_size: FontSize::Px(25.0),
            ..default()
        };
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                bottom: px(12),
                left: px(12),
                flex_direction: FlexDirection::Column,
                ..default()
            },
            children![
                (Text::new("Move the player with WASD"), font.clone()),
                (Text::new("Rotate the camera with the mouse"), font)
            ],
        ));
    }

    fn rotate_camera(
        accumulated_mouse_motion: Res<AccumulatedMouseMotion>,
        player: Single<(&mut Transform, &CameraSensitivity), With<Camera>>,
    ) {
        let (mut transform, camera_sensitivity) = player.into_inner();
        let delta = accumulated_mouse_motion.delta;

        if delta != Vec2::ZERO {
            let delta_yaw = -delta.x * camera_sensitivity.x;
            let delta_pitch = -delta.y * camera_sensitivity.y;
            let (yaw, pitch, roll) = transform.rotation.to_euler(EulerRot::YXZ);
            let yaw = yaw + delta_yaw;
            const PITCH_LIMIT: f32 = FRAC_PI_2 - 0.01;
            let pitch = (pitch + delta_pitch).clamp(-PITCH_LIMIT, PITCH_LIMIT);
            transform.rotation = Quat::from_euler(EulerRot::YXZ, yaw, pitch, roll);
        }
    }

    #[derive(Debug, Component, Deref, DerefMut)]
    struct CameraSensitivity(Vec2);

    impl Default for CameraSensitivity {
        fn default() -> Self {
            Self(Vec2::new(0.003, 0.002))
        }
    }

    fn accumulate_input(
        keyboard_input: Res<ButtonInput<KeyCode>>,
        player: Single<(&mut AccumulatedInput, &mut Velocity)>,
        camera: Single<&Transform, With<Camera>>,
    ) {
        const SPEED: f32 = 4.0;
        let (mut input, mut velocity) = player.into_inner();
        input.movement = Vec2::ZERO;
        if keyboard_input.pressed(KeyCode::KeyW) {
            input.movement.y += 1.0;
        }
        if keyboard_input.pressed(KeyCode::KeyS) {
            input.movement.y -= 1.0;
        }
        if keyboard_input.pressed(KeyCode::KeyA) {
            input.movement.x -= 1.0;
        }
        if keyboard_input.pressed(KeyCode::KeyD) {
            input.movement.x += 1.0;
        }

        let input_3d = Vec3 {
            x: input.movement.x,
            y: 0.0,
            z: -input.movement.y,
        };
        let rotated_input = camera.rotation * input_3d;
        velocity.0 = rotated_input.clamp_length_max(1.0) * SPEED;
    }

    #[derive(Resource, Debug, Deref, DerefMut, Default)]
    struct DidFixedTimestepRunThisFrame(bool);

    fn clear_fixed_timestep_flag(
        mut did_fixed_timestep_run_this_frame: ResMut<DidFixedTimestepRunThisFrame>,
    ) {
        did_fixed_timestep_run_this_frame.0 = false;
    }

    fn set_fixed_time_step_flag(
        mut did_fixed_timestep_run_this_frame: ResMut<DidFixedTimestepRunThisFrame>,
    ) {
        did_fixed_timestep_run_this_frame.0 = true;
    }

    fn did_fixed_timestep_run_this_frame(
        did_fixed_timestep_run_this_frame: Res<DidFixedTimestepRunThisFrame>,
    ) -> bool {
        did_fixed_timestep_run_this_frame.0
    }

    fn clear_input(mut input: Single<&mut AccumulatedInput>) {
        **input = AccumulatedInput::default();
    }

    fn advance_physics(
        fixed_time: Res<Time<Fixed>>,
        mut query: Query<(
            &mut PhysicalTranslation,
            &mut PreviousPhysicalTranslation,
            &Velocity,
        )>,
    ) {
        for (mut current_physical_translation, mut previous_physical_translation, velocity) in
            query.iter_mut()
        {
            previous_physical_translation.0 = current_physical_translation.0;
            current_physical_translation.0 += velocity.0 * fixed_time.delta_secs();
        }
    }

    fn interpolate_rendered_transform(
        fixed_time: Res<Time<Fixed>>,
        mut query: Query<(
            &mut Transform,
            &PhysicalTranslation,
            &PreviousPhysicalTranslation,
        )>,
    ) {
        for (mut transform, current_physical_translation, previous_physical_translation) in
            query.iter_mut()
        {
            let previous = previous_physical_translation.0;
            let current = current_physical_translation.0;
            let alpha = fixed_time.overstep_fraction();
            transform.translation = previous.lerp(current, alpha);
        }
    }

    fn translate_camera(
        mut camera: Single<&mut Transform, With<Camera>>,
        player: Single<&Transform, (With<AccumulatedInput>, Without<Camera>)>,
    ) {
        camera.translation = player.translation;
    }

    fn finish_when_agent_drove_physics(
        mut probe: ResMut<Probe>,
        player: Single<&PhysicalTranslation, With<AccumulatedInput>>,
        mut app_exit: MessageWriter<AppExit>,
        mut frames: Local<u32>,
    ) {
        if probe.result.is_none() {
            return;
        }

        *frames += 1;
        if player.0.z < -0.1 && probe.capture_done.load(Ordering::Relaxed) {
            finish_probe(&mut probe, &mut app_exit, Ok(()));
        } else if *frames > probe.max_frames {
            let capture_done = probe.capture_done.load(Ordering::Relaxed);
            finish_probe(
                &mut probe,
                &mut app_exit,
                Err(format!(
                    "player physical translation was {:?}, capture_done={}",
                    player.0, capture_done
                )),
            );
        }
    }
}
