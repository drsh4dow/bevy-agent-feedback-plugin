use bevy::{
    app::AppExit,
    prelude::*,
    render::RenderPlugin,
    window::{ExitCondition, WindowResolution},
    winit::WinitPlugin,
};
#[cfg(feature = "diagnostics")]
use bevy_agent_feedback_plugin::AgentFeedbackDiagnosticsPlugin;
use bevy_agent_feedback_plugin::{
    AgentFeedbackConfig, AgentFeedbackPlugin,
    client::{AgentClient, AgentClientConfig, Capture, CaptureCompletion, CaptureWindowMode},
};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const REQUESTED_WINDOW_WIDTH: u32 = 640;
const REQUESTED_WINDOW_HEIGHT: u32 = 480;
const LONG_FRAME_RUN: u16 = 240;
const TRANSITION_AFTER_REQUEST_FRAMES: u16 = 160;
const MAX_FIXTURE_FRAMES: u32 = 1_800;
const RED: [u8; 3] = [220, 36, 42];
const BLUE: [u8; 3] = [35, 92, 220];
const BACKGROUND: [u8; 3] = [12, 18, 28];

#[test]
#[ignore = "requires a graphics-capable environment"]
fn stable_pattern_preserves_metadata_and_pixels_across_a_long_frame_run() {
    if skip_without_window_server() {
        return;
    }

    let root = artifact_root();
    eprintln!("stable-pattern artifacts: {}", root.display());
    let config = agent_config(&root);
    let client_finished = Arc::new(AtomicBool::new(false));
    let transition_requested = Arc::new(AtomicBool::new(false));

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "Agent feedback stable pattern".into(),
                    resolution: WindowResolution::new(
                        REQUESTED_WINDOW_WIDTH,
                        REQUESTED_WINDOW_HEIGHT,
                    )
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
    .insert_resource(ClearColor(Color::srgb_u8(
        BACKGROUND[0],
        BACKGROUND[1],
        BACKGROUND[2],
    )))
    .insert_resource(PatternProgress::default())
    .insert_resource(ClientFinished(client_finished.clone()))
    .insert_resource(TransitionRequested(transition_requested.clone()))
    .add_plugins(AgentFeedbackPlugin::new(config.clone()))
    .add_systems(Startup, spawn_stable_pattern)
    .add_systems(Update, (transition_pattern, finish_fixture));
    #[cfg(feature = "diagnostics")]
    app.add_plugins(AgentFeedbackDiagnosticsPlugin::default());

    let protocol_file = config.protocol_file.clone();
    let client = thread::spawn(move || {
        let result = drive_stable_pattern(&protocol_file, transition_requested);
        client_finished.store(true, Ordering::Release);
        result
    });

    let exit = app.run();
    drop(app);
    let client_result = client
        .join()
        .unwrap_or_else(|_| Err("stable-pattern client panicked".to_string()));
    if let Err(error) = client_result {
        panic!("stable-pattern client failed: {error}");
    }
    assert_eq!(exit, AppExit::Success);
}

#[derive(Component, Clone, Copy)]
enum PatternBlock {
    Left,
    Right,
}

#[derive(Resource, Default)]
struct PatternProgress {
    requested_frames: u16,
    transitioned: bool,
}

#[derive(Resource)]
struct ClientFinished(Arc<AtomicBool>);

#[derive(Resource)]
struct TransitionRequested(Arc<AtomicBool>);

fn spawn_stable_pattern(mut commands: Commands) {
    commands.spawn(Camera2d);
    commands.spawn((
        PatternBlock::Left,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(0.0),
            top: Val::Percent(0.0),
            width: Val::Percent(50.0),
            height: Val::Percent(100.0),
            ..default()
        },
        BackgroundColor(Color::srgb_u8(RED[0], RED[1], RED[2])),
    ));
    commands.spawn((
        PatternBlock::Right,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(0.0),
            width: Val::Percent(50.0),
            height: Val::Percent(100.0),
            ..default()
        },
        BackgroundColor(Color::srgb_u8(BLUE[0], BLUE[1], BLUE[2])),
    ));
}

fn transition_pattern(
    requested: Res<TransitionRequested>,
    mut progress: ResMut<PatternProgress>,
    mut blocks: Query<(&PatternBlock, &mut BackgroundColor)>,
) {
    if progress.transitioned || !requested.0.load(Ordering::Acquire) {
        return;
    }

    progress.requested_frames = progress.requested_frames.saturating_add(1);
    if progress.requested_frames < TRANSITION_AFTER_REQUEST_FRAMES {
        return;
    }

    for (block, mut color) in &mut blocks {
        color.0 = match block {
            PatternBlock::Left => Color::srgb_u8(BLUE[0], BLUE[1], BLUE[2]),
            PatternBlock::Right => Color::srgb_u8(RED[0], RED[1], RED[2]),
        };
    }
    progress.transitioned = true;
}

fn finish_fixture(
    finished: Res<ClientFinished>,
    mut frames: Local<u32>,
    mut app_exit: MessageWriter<AppExit>,
) {
    *frames = frames.saturating_add(1);
    if finished.0.load(Ordering::Acquire) {
        app_exit.write(AppExit::Success);
    } else if *frames >= MAX_FIXTURE_FRAMES {
        app_exit.write(AppExit::error());
    }
}

fn drive_stable_pattern(
    protocol_file: &Path,
    transition_requested: Arc<AtomicBool>,
) -> Result<(), String> {
    let mut client = AgentClient::with_config(AgentClientConfig {
        protocol_file: protocol_file.to_path_buf(),
        timeout: Duration::from_secs(30),
        ..Default::default()
    })
    .map_err(|error| error.to_string())?;

    let before = client
        .wait_until_first_capture()
        .map_err(|error| error.to_string())?;
    expect_client_metadata(&client, &before, None, 1)?;
    let before_pixels = read_pixels(&before.path)?;
    expect_pattern(&before_pixels, RED, BLUE, "before long frame run")?;

    transition_requested.store(true, Ordering::Release);
    let after = client
        .capture_after_frames(LONG_FRAME_RUN, Some("after_long_run"))
        .map_err(|error| error.to_string())?;
    expect_client_metadata(
        &client,
        &after,
        Some("after_long_run"),
        u64::from(LONG_FRAME_RUN),
    )?;
    let after_pixels = read_pixels(&after.path)?;
    expect_pattern(&after_pixels, BLUE, RED, "after long frame run")?;

    let retained = client
        .capture_labeled("retained")
        .map_err(|error| error.to_string())?;
    expect_client_metadata(&client, &retained, Some("retained"), 0)?;

    if before.path.exists() {
        return Err(format!(
            "retention kept oldest capture after the configured two-file limit: {}",
            before.path.display()
        ));
    }
    if !after.path.is_file() || !retained.path.is_file() {
        return Err(format!(
            "retention removed a recent capture: after={}, retained={}",
            after.path.display(),
            retained.path.display()
        ));
    }
    if after.sequence != before.sequence + 1 || retained.sequence != after.sequence + 1 {
        return Err(format!(
            "capture sequence was not monotonic: before={}, after={}, retained={}",
            before.sequence, after.sequence, retained.sequence
        ));
    }

    Ok(())
}

fn expect_client_metadata(
    client: &AgentClient,
    capture: &Capture,
    expected_label: Option<&str>,
    minimum_frame_delta: u64,
) -> Result<(), String> {
    if client.last_capture_info() != Some(capture) {
        return Err("Rust client did not retain the complete most recent Capture metadata".into());
    }
    if capture.label.as_deref() != expected_label {
        return Err(format!(
            "capture label was {:?}, expected {expected_label:?}",
            capture.label
        ));
    }
    let frame_delta = capture
        .completed_frame
        .checked_sub(capture.requested_frame)
        .ok_or_else(|| {
            format!(
                "completion frame {} preceded request frame {}",
                capture.completed_frame, capture.requested_frame
            )
        })?;
    if frame_delta < minimum_frame_delta {
        return Err(format!(
            "capture completed after {frame_delta} frames, expected at least {minimum_frame_delta}"
        ));
    }
    if capture.completion != CaptureCompletion::ScreenshotCaptured {
        return Err(format!(
            "capture completion marker was {:?}",
            capture.completion
        ));
    }
    if capture.image_width == 0 || capture.image_height == 0 {
        return Err("capture metadata reported zero image dimensions".to_string());
    }
    if capture.window_at_request.physical_width == 0
        || capture.window_at_request.physical_height == 0
        || capture.window_at_request.mode != CaptureWindowMode::Windowed
    {
        return Err(format!(
            "request window metadata did not describe a visible-size windowed surface: {:?}",
            capture.window_at_request
        ));
    }
    let completion_window = capture
        .window_at_completion
        .as_ref()
        .ok_or_else(|| "rendered capture unexpectedly lost its completion window".to_string())?;
    if completion_window.physical_width != capture.image_width
        || completion_window.physical_height != capture.image_height
    {
        return Err(format!(
            "completion window dimensions {}x{} disagreed with capture dimensions {}x{}",
            completion_window.physical_width,
            completion_window.physical_height,
            capture.image_width,
            capture.image_height
        ));
    }

    let dimensions = image::image_dimensions(&capture.path).map_err(|error| error.to_string())?;
    if dimensions != (capture.image_width, capture.image_height) {
        return Err(format!(
            "persisted PNG dimensions {dimensions:?} disagreed with capture metadata {}x{}",
            capture.image_width, capture.image_height
        ));
    }
    Ok(())
}

struct CapturedPixels {
    width: u32,
    height: u32,
    rgba: image::RgbaImage,
}

fn read_pixels(path: &Path) -> Result<CapturedPixels, String> {
    let rgba = image::ImageReader::open(path)
        .map_err(|error| error.to_string())?
        .decode()
        .map_err(|error| error.to_string())?
        .to_rgba8();
    Ok(CapturedPixels {
        width: rgba.width(),
        height: rgba.height(),
        rgba,
    })
}

fn expect_pattern(
    image: &CapturedPixels,
    expected_left: [u8; 3],
    expected_right: [u8; 3],
    phase: &str,
) -> Result<(), String> {
    if image.width < 8 || image.height < 4 {
        return Err(format!(
            "{phase} capture was too small for stable regions: {}x{}",
            image.width, image.height
        ));
    }
    let eighth_width = image.width / 8;
    let quarter_height = image.height / 4;
    expect_region_color(
        image,
        PixelRegion::new(
            eighth_width,
            quarter_height,
            eighth_width * 2,
            quarter_height * 2,
        ),
        expected_left,
        phase,
    )?;
    expect_region_color(
        image,
        PixelRegion::new(
            eighth_width * 5,
            quarter_height,
            eighth_width * 2,
            quarter_height * 2,
        ),
        expected_right,
        phase,
    )?;
    Ok(())
}

#[derive(Clone, Copy)]
struct PixelRegion {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl PixelRegion {
    const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

fn expect_region_color(
    image: &CapturedPixels,
    region: PixelRegion,
    expected: [u8; 3],
    phase: &str,
) -> Result<(), String> {
    let right = region
        .x
        .checked_add(region.width)
        .ok_or_else(|| "pixel region x overflowed".to_string())?;
    let bottom = region
        .y
        .checked_add(region.height)
        .ok_or_else(|| "pixel region y overflowed".to_string())?;
    if right > image.width || bottom > image.height {
        return Err(format!(
            "{phase} region ({}, {}) {}x{} exceeded image {}x{}",
            region.x, region.y, region.width, region.height, image.width, image.height
        ));
    }

    let mut matching = 0_u32;
    for y in region.y..bottom {
        for x in region.x..right {
            let pixel = image.rgba.get_pixel(x, y).0;
            if pixel[..3]
                .iter()
                .zip(expected)
                .all(|(actual, expected)| actual.abs_diff(expected) <= 12)
            {
                matching = matching.saturating_add(1);
            }
        }
    }
    let total = region.width.saturating_mul(region.height);
    let minimum_matching = total.saturating_mul(95) / 100;
    if matching < minimum_matching {
        return Err(format!(
            "{phase} region ({}, {}) {}x{} had {matching}/{total} pixels near rgb({},{},{}), expected at least {minimum_matching}",
            region.x, region.y, region.width, region.height, expected[0], expected[1], expected[2]
        ));
    }
    Ok(())
}

fn agent_config(root: &Path) -> AgentFeedbackConfig {
    AgentFeedbackConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        protocol_file: root.join("agent-feedback.json"),
        capture_dir: root.join("captures"),
        max_wait_frames: 600,
        max_captures: 2,
        command_timeout: Duration::from_secs(30),
        ..Default::default()
    }
}

fn artifact_root() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after unix epoch")
        .as_nanos();
    std::env::var_os("AGENT_FEEDBACK_ARTIFACT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/agent-feedback"))
        .join(format!("stable-pattern-{}-{nonce}", std::process::id()))
}

fn skip_without_window_server() -> bool {
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("skipping rendered stable-pattern test: DISPLAY/WAYLAND_DISPLAY is not set");
        return true;
    }

    false
}
