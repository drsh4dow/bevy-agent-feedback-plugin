#![cfg(feature = "diagnostics")]

use bevy::{
    input::InputSystems,
    prelude::*,
    render::RenderPlugin,
    time::Virtual,
    window::{ExitCondition, PrimaryWindow, WindowResolution},
    winit::WinitPlugin,
};
use bevy_agent_feedback_plugin::{
    AgentFeedbackConfig, AgentFeedbackDiagnosticsPlugin, AgentFeedbackPlugin,
};
use std::{net::SocketAddr, path::PathBuf, sync::mpsc, time::Duration};

const WINDOW_WIDTH: u32 = 640;
const WINDOW_HEIGHT: u32 = 480;
const BUTTON_LEFT: f32 = 160.0;
const BUTTON_TOP: f32 = 150.0;
const BUTTON_WIDTH: f32 = 320.0;
const BUTTON_HEIGHT: f32 = 180.0;
const ADVANCE_NANOSECONDS: u32 = 125_000_000;
const MAX_RECORDED_DELTAS: usize = 8;

#[derive(States, Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
enum WorkflowState {
    #[default]
    Ready,
}

#[derive(Component)]
struct WorkflowButton;

#[derive(Resource)]
struct WorkflowStatus {
    clicked: bool,
    click_count: u32,
    invalid_click_count: u32,
    elapsed_nanoseconds: u32,
    advance_steps: u32,
    deltas: [Duration; MAX_RECORDED_DELTAS],
    delta_overflow: bool,
    deadline: std::time::Instant,
    timed_out: bool,
}

impl WorkflowStatus {
    fn new() -> Self {
        Self {
            clicked: false,
            click_count: 0,
            invalid_click_count: 0,
            elapsed_nanoseconds: 0,
            advance_steps: 0,
            deltas: [Duration::ZERO; MAX_RECORDED_DELTAS],
            delta_overflow: false,
            deadline: std::time::Instant::now() + Duration::from_secs(45),
            timed_out: false,
        }
    }
}
#[derive(Clone, Copy)]
struct WorkflowObservation {
    clicked: bool,
    click_count: u32,
    invalid_click_count: u32,
    elapsed_nanoseconds: u32,
    advance_steps: u32,
    deltas: [Duration; MAX_RECORDED_DELTAS],
    delta_overflow: bool,
    timed_out: bool,
}

#[derive(Resource)]
struct WorkflowReport(Option<mpsc::Sender<WorkflowObservation>>);

#[test]
#[ignore = "requires bevy-feedback CLI, its Python driver, and a graphics-capable environment"]
fn skill_workflow() {
    if skip_without_window_server() {
        return;
    }

    let protocol_file = required_path("BEVY_FEEDBACK_PROTOCOL");
    let capture_dir = required_path("BEVY_FEEDBACK_CAPTURE_DIR");
    let config = AgentFeedbackConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        protocol_file,
        capture_dir,
        max_wait_frames: 300,
        deterministic_time: true,
        max_time_advance_steps: MAX_RECORDED_DELTAS as u16,
        max_time_advance: Duration::from_secs(1),
        command_timeout: Duration::from_secs(30),
        ..Default::default()
    };
    let diagnostics = AgentFeedbackDiagnosticsPlugin::default()
        .with_state::<WorkflowState>()
        .with_marker::<WorkflowButton>()
        .with_resource_field::<WorkflowStatus, bool, _>("clicked", |status| status.clicked)
        .with_resource_field::<WorkflowStatus, u32, _>("elapsed_nanoseconds", |status| {
            status.elapsed_nanoseconds
        })
        .with_resource_field::<WorkflowStatus, u32, _>("advance_steps", |status| {
            status.advance_steps
        });

    let (report_sender, report_receiver) = mpsc::channel();
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "Bundled Python skill workflow".into(),
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
    )
    .init_state::<WorkflowState>()
    .insert_resource(WorkflowStatus::new())
    .insert_resource(WorkflowReport(Some(report_sender)))
    .add_plugins((AgentFeedbackPlugin::new(config), diagnostics))
    .add_systems(Startup, spawn_workflow_scene)
    .add_systems(PreUpdate, consume_named_click.after(InputSystems))
    .add_systems(Update, (record_virtual_time, enforce_wall_timeout))
    .add_systems(Last, report_workflow_result);

    let exit = app.run();
    let status = report_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("workflow app exited without reporting its observations");
    assert_eq!(
        exit,
        AppExit::Success,
        "workflow app did not shut down cleanly"
    );
    assert!(
        !status.timed_out,
        "workflow driver exceeded its 45 second bound"
    );
    assert!(
        status.clicked,
        "same-PreUpdate named click was not consumed"
    );
    assert_eq!(status.click_count, 1, "expected exactly one named click");
    assert_eq!(
        status.invalid_click_count, 0,
        "named click resolved outside the visible workflow target"
    );
    assert!(
        !status.delta_overflow,
        "deterministic delta recording overflowed"
    );
    assert_eq!(
        status.advance_steps, 4,
        "unexpected deterministic step count"
    );
    assert_eq!(
        status.elapsed_nanoseconds, ADVANCE_NANOSECONDS,
        "virtual time did not advance by exactly 125ms"
    );
    assert_eq!(
        &status.deltas[..4],
        &[
            Duration::from_millis(40),
            Duration::from_millis(40),
            Duration::from_millis(40),
            Duration::from_millis(5),
        ],
        "deterministic advancement did not preserve full steps and one final remainder"
    );
}

fn required_path(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("{name} must be injected by bevy-feedback run"))
}

fn skip_without_window_server() -> bool {
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("skipping rendered Bevy test: DISPLAY/WAYLAND_DISPLAY is not set");
        return true;
    }
    false
}

fn spawn_workflow_scene(mut commands: Commands) {
    commands.spawn((Camera2d, Name::new("WorkflowCamera")));
    commands.spawn((
        WorkflowButton,
        Name::new("WorkflowButton"),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(BUTTON_LEFT),
            top: Val::Px(BUTTON_TOP),
            width: Val::Px(BUTTON_WIDTH),
            height: Val::Px(BUTTON_HEIGHT),
            ..default()
        },
        BackgroundColor(Color::srgb_u8(20, 80, 220)),
    ));
}

fn consume_named_click(
    buttons: Res<ButtonInput<MouseButton>>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut status: ResMut<WorkflowStatus>,
    mut button_color: Query<&mut BackgroundColor, With<WorkflowButton>>,
) {
    if !buttons.just_pressed(MouseButton::Left) {
        return;
    }
    status.click_count = status.click_count.saturating_add(1);
    let inside_target = window.cursor_position().is_some_and(|position| {
        (BUTTON_LEFT..=BUTTON_LEFT + BUTTON_WIDTH).contains(&position.x)
            && (BUTTON_TOP..=BUTTON_TOP + BUTTON_HEIGHT).contains(&position.y)
    });
    if !inside_target {
        status.invalid_click_count = status.invalid_click_count.saturating_add(1);
        return;
    }
    status.clicked = true;
    if let Ok(mut color) = button_color.single_mut() {
        color.0 = Color::srgb_u8(20, 210, 70);
    }
}

fn record_virtual_time(time: Res<Time<Virtual>>, mut status: ResMut<WorkflowStatus>) {
    let delta = time.delta();
    if !delta.is_zero() {
        let index = status.advance_steps as usize;
        if index < status.deltas.len() {
            status.deltas[index] = delta;
        } else {
            status.delta_overflow = true;
        }
        status.advance_steps = status.advance_steps.saturating_add(1);
    }
    match u32::try_from(time.elapsed().as_nanos()) {
        Ok(elapsed) => status.elapsed_nanoseconds = elapsed,
        Err(_) => status.delta_overflow = true,
    }
}

fn enforce_wall_timeout(mut status: ResMut<WorkflowStatus>, mut app_exit: MessageWriter<AppExit>) {
    if !status.timed_out && std::time::Instant::now() >= status.deadline {
        status.timed_out = true;
        app_exit.write(AppExit::error());
    }
}

fn report_workflow_result(
    mut exits: MessageReader<AppExit>,
    status: Res<WorkflowStatus>,
    mut report: ResMut<WorkflowReport>,
) {
    if exits.read().next().is_none() {
        return;
    }
    let Some(sender) = report.0.take() else {
        return;
    };
    let _ = sender.send(WorkflowObservation {
        clicked: status.clicked,
        click_count: status.click_count,
        invalid_click_count: status.invalid_click_count,
        elapsed_nanoseconds: status.elapsed_nanoseconds,
        advance_steps: status.advance_steps,
        deltas: status.deltas,
        delta_overflow: status.delta_overflow,
        timed_out: status.timed_out,
    });
}
