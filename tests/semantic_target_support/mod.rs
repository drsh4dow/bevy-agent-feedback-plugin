use bevy::{
    a11y::AccessibilityNode,
    camera::{Viewport, primitives::Aabb},
    prelude::*,
    render::RenderPlugin,
    window::{CursorMoved, ExitCondition, WindowResolution},
    winit::WinitPlugin,
};
use bevy_agent_feedback_plugin::{
    AgentFeedbackConfig, AgentFeedbackDiagnosticsPlugin, AgentFeedbackPlugin,
};
use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Sender},
    },
    thread,
    time::Duration,
};

const EPSILON: f32 = 0.05;
const WINDOW_WIDTH: u32 = 640;
const WINDOW_HEIGHT: u32 = 480;
const HALF_WIDTH: u32 = WINDOW_WIDTH / 2;

type RenderDriver = fn(&Path, &AtomicBool, &mpsc::Receiver<RenderGeometry>) -> Result<(), String>;

#[derive(Clone, Copy, Debug)]
pub(crate) struct RenderGeometry {
    pub(crate) left_ui_viewport: Rect,
    pub(crate) right_ui_viewport: Rect,
    pub(crate) world_viewport: Rect,
    pub(crate) near_viewport: Rect,
}

pub(crate) fn run_rendered_contract(drive_semantic_targets: RenderDriver) {
    if skip_without_window_server() {
        return;
    }

    let root = artifact_root();
    let _ = fs::remove_dir_all(&root);
    let config = AgentFeedbackConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        protocol_file: root.join("agent-feedback.json"),
        capture_dir: root.join("captures"),
        max_wait_frames: 600,
        command_timeout: Duration::from_secs(30),
        ..Default::default()
    };
    let protocol_file = config.protocol_file.clone();
    let client_done = Arc::new(AtomicBool::new(false));
    let (result_sender, result_receiver) = mpsc::channel();
    let geometry_requested = Arc::new(AtomicBool::new(false));
    let (geometry_sender, geometry_receiver) = mpsc::channel();

    let mut app = App::new();
    add_render_plugins(&mut app);
    app.add_plugins((
        AgentFeedbackPlugin::new(config),
        AgentFeedbackDiagnosticsPlugin::default()
            .with_resource_field::<GameplayPostcondition, _, _>("accepted", |value| value.accepted),
    ))
    .init_resource::<GameplayPostcondition>()
    .insert_resource(RenderProbe {
        client_done: client_done.clone(),
        result: Some(result_sender),
        max_frames: 1_800,
    })
    .insert_resource(GeometryProbe {
        requested: geometry_requested.clone(),
        result: Some(geometry_sender),
    })
    .init_resource::<ClickObservation>()
    .add_systems(Startup, spawn_semantic_scene)
    .add_systems(
        Update,
        (observe_click_then_move, finish_render_test).chain(),
    )
    .add_systems(Last, publish_render_geometry);

    let client = thread::spawn(move || {
        let result =
            drive_semantic_targets(&protocol_file, &geometry_requested, &geometry_receiver);
        client_done.store(true, Ordering::Release);
        result
    });
    let exit = app.run();

    let app_result = result_receiver
        .recv_timeout(Duration::from_secs(1))
        .unwrap_or_else(|error| Err(format!("app exited without a test result: {error}")));
    let client_result = client
        .join()
        .unwrap_or_else(|_| Err("semantic target client panicked".to_string()));
    if let Err(error) = client_result {
        panic!("semantic target client failed: {error}");
    }
    if let Err(error) = app_result {
        panic!("semantic target fixture failed: {error}");
    }
    assert_eq!(exit, AppExit::Success);
}

#[derive(Component)]
struct MovingTarget;

#[derive(Component)]
struct LeftUiCameraFixture;

#[derive(Component)]
struct RightUiCameraFixture;

#[derive(Component)]
struct WorldOrthoCameraFixture;

#[derive(Component)]
struct NearCameraFixture;

#[derive(Resource, Default)]
struct GameplayPostcondition {
    accepted: bool,
}

#[derive(Resource, Default)]
struct ClickObservation {
    saw_press: bool,
    cursor: Vec2,
    resolved_center: Vec2,
}

#[derive(Resource)]
struct RenderProbe {
    client_done: Arc<AtomicBool>,
    result: Option<Sender<Result<(), String>>>,
    max_frames: u32,
}

#[derive(Resource)]
struct GeometryProbe {
    requested: Arc<AtomicBool>,
    result: Option<Sender<RenderGeometry>>,
}

fn add_render_plugins(app: &mut App) {
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "Semantic target rendered contract".into(),
                    resolution: WindowResolution::new(WINDOW_WIDTH, WINDOW_HEIGHT)
                        .with_scale_factor_override(1.0),
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
    );
}

fn camera_viewport(x: u32, order: isize) -> Camera {
    Camera {
        order,
        viewport: Some(Viewport {
            physical_position: UVec2::new(x, 0),
            physical_size: UVec2::new(HALF_WIDTH, WINDOW_HEIGHT),
            ..default()
        }),
        ..default()
    }
}

fn spawn_semantic_scene(mut commands: Commands) {
    let left_ui_camera = commands
        .spawn((
            Name::new("LeftUiCamera"),
            LeftUiCameraFixture,
            Camera2d,
            camera_viewport(0, 0),
        ))
        .id();
    let right_ui_camera = commands
        .spawn((
            Name::new("RightUiCamera"),
            RightUiCameraFixture,
            Camera2d,
            camera_viewport(HALF_WIDTH, 1),
        ))
        .id();
    commands.spawn((
        Name::new("WorldOrthoCamera"),
        WorldOrthoCameraFixture,
        Camera2d,
        camera_viewport(0, 2),
    ));
    commands.spawn((
        Name::new("NearCamera"),
        NearCameraFixture,
        Camera3d::default(),
        camera_viewport(HALF_WIDTH, 3),
    ));

    commands
        .spawn((
            Node {
                width: px(HALF_WIDTH),
                height: px(WINDOW_HEIGHT),
                ..default()
            },
            UiTargetCamera(left_ui_camera),
        ))
        .with_children(|root| {
            root.spawn((
                Name::new("RotatedScaled"),
                Node {
                    position_type: PositionType::Absolute,
                    left: px(100),
                    top: px(100),
                    width: px(80),
                    height: px(40),
                    ..default()
                },
                UiTransform {
                    translation: Val2::ZERO,
                    scale: Vec2::new(2.0, 0.5),
                    rotation: Rot2::degrees(90.0),
                },
                BackgroundColor(Color::srgb(0.9, 0.2, 0.2)),
            ));

            root.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: px(250),
                    top: px(300),
                    width: px(50),
                    height: px(60),
                    overflow: Overflow::clip(),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.1, 0.1, 0.1)),
            ))
            .with_children(|clip| {
                clip.spawn((
                    Name::new("OverflowChild"),
                    Node {
                        position_type: PositionType::Absolute,
                        left: px(30),
                        top: px(20),
                        width: px(60),
                        height: px(50),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.2, 0.8, 0.2)),
                ));
            });

            let mut accessibility = AccessibilityNode::default();
            accessibility.set_label("Launch Mission");
            root.spawn((
                Name::new("AccessibilityFixture"),
                accessibility,
                Node {
                    position_type: PositionType::Absolute,
                    left: px(20),
                    top: px(220),
                    width: px(100),
                    height: px(30),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.2, 0.2, 0.9)),
            ));

            root.spawn((
                Name::new("MovingButton"),
                MovingTarget,
                Node {
                    position_type: PositionType::Absolute,
                    left: px(40),
                    top: px(400),
                    width: px(60),
                    height: px(30),
                    ..default()
                },
                UiTransform::IDENTITY,
                BackgroundColor(Color::srgb(0.9, 0.8, 0.1)),
            ));
        });

    commands
        .spawn((
            Node {
                width: px(HALF_WIDTH),
                height: px(WINDOW_HEIGHT),
                ..default()
            },
            UiTargetCamera(right_ui_camera),
        ))
        .with_children(|root| {
            root.spawn((
                Name::new("RightViewportTarget"),
                Node {
                    position_type: PositionType::Absolute,
                    left: px(25),
                    top: px(35),
                    width: px(70),
                    height: px(30),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.2, 0.8, 0.9)),
            ));
        });

    commands.spawn((
        Name::new("TransformedAabb"),
        Aabb {
            center: Vec3A::ZERO,
            half_extents: Vec3A::new(20.0, 10.0, 1.0),
        },
        Transform {
            translation: Vec3::new(30.0, -40.0, 0.0),
            rotation: Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
            scale: Vec3::new(2.0, 3.0, 1.0),
        },
        Visibility::Visible,
    ));
    commands.spawn((
        Name::new("NearPlaneAabb"),
        Aabb {
            center: Vec3A::ZERO,
            half_extents: Vec3A::new(0.05, 0.05, 0.5),
        },
        Transform::from_xyz(0.0, 0.0, -0.2),
        Visibility::Visible,
    ));
    commands.spawn((
        Name::new("HiddenWorld"),
        Transform::from_xyz(0.0, 0.0, 0.0),
        Visibility::Hidden,
    ));
    commands.spawn((
        Name::new("OffscreenWorld"),
        Transform::from_xyz(1_000.0, 0.0, 0.0),
        Visibility::Visible,
    ));
    for x in [-20.0, 20.0] {
        commands.spawn((
            Name::new("DuplicateWorld"),
            Transform::from_xyz(x, 0.0, 0.0),
            Visibility::Visible,
        ));
    }
}

fn observe_click_then_move(
    mouse: Res<ButtonInput<MouseButton>>,
    mut cursor_moved: MessageReader<CursorMoved>,
    mut moving: Single<(&ComputedNode, &UiGlobalTransform, &mut UiTransform), With<MovingTarget>>,
    mut observation: ResMut<ClickObservation>,
    mut motion_frame: Local<u32>,
) {
    let (node, global, transform) = &mut *moving;
    let resolved_center =
        global.affine().transform_point2(Vec2::ZERO) * node.inverse_scale_factor();
    let mut closest_cursor = None;
    let mut closest_distance = f32::INFINITY;
    for event in cursor_moved.read().take(64) {
        let distance = event.position.distance(resolved_center);
        if distance < closest_distance {
            closest_cursor = Some(event.position);
            closest_distance = distance;
        }
    }
    if mouse.just_pressed(MouseButton::Left) && !observation.saw_press {
        observation.saw_press = true;
        observation.resolved_center = resolved_center;
        observation.cursor = closest_cursor.unwrap_or(Vec2::splat(f32::NAN));
    }

    *motion_frame = motion_frame.wrapping_add(1);
    transform.translation = Val2::px((*motion_frame % 97) as f32, 0.0);
}

fn publish_render_geometry(
    mut probe: ResMut<GeometryProbe>,
    left_ui: Single<&Camera, With<LeftUiCameraFixture>>,
    right_ui: Single<&Camera, With<RightUiCameraFixture>>,
    world: Single<&Camera, With<WorldOrthoCameraFixture>>,
    near: Single<&Camera, With<NearCameraFixture>>,
) {
    if probe.result.is_none() || !probe.requested.load(Ordering::Acquire) {
        return;
    }
    let (
        Some(left_ui_viewport),
        Some(right_ui_viewport),
        Some(world_viewport),
        Some(near_viewport),
    ) = (
        left_ui.logical_viewport_rect(),
        right_ui.logical_viewport_rect(),
        world.logical_viewport_rect(),
        near.logical_viewport_rect(),
    )
    else {
        return;
    };

    let geometry = RenderGeometry {
        left_ui_viewport,
        right_ui_viewport,
        world_viewport,
        near_viewport,
    };
    if let Some(sender) = probe.result.take() {
        let _ = sender.send(geometry);
    }
}

fn finish_render_test(
    mut probe: ResMut<RenderProbe>,
    observation: Res<ClickObservation>,
    mut app_exit: MessageWriter<AppExit>,
    mut frames: Local<u32>,
) {
    if probe.result.is_none() {
        return;
    }
    *frames += 1;

    let result = if probe.client_done.load(Ordering::Acquire) {
        if !observation.saw_press {
            Some(Err(
                "named click completed without a MouseButton::Left press".to_string(),
            ))
        } else if observation.cursor.distance(observation.resolved_center) > EPSILON {
            Some(Err(format!(
                "named click was not atomic: cursor {:?}, moving target center {:?}",
                observation.cursor, observation.resolved_center
            )))
        } else {
            Some(Ok(()))
        }
    } else if *frames >= probe.max_frames {
        Some(Err(format!(
            "semantic target client did not finish within {} rendered updates",
            probe.max_frames
        )))
    } else {
        None
    };

    if let Some(result) = result {
        let success = result.is_ok();
        if let Some(sender) = probe.result.take() {
            let _ = sender.send(result);
        }
        app_exit.write(if success {
            AppExit::Success
        } else {
            AppExit::error()
        });
    }
}

fn artifact_root() -> PathBuf {
    std::env::var_os("AGENT_FEEDBACK_ARTIFACT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/agent-feedback"))
        .join("semantic-target")
}

fn skip_without_window_server() -> bool {
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("skipping rendered Bevy test: DISPLAY/WAYLAND_DISPLAY is not set");
        return true;
    }

    false
}
