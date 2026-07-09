use bevy::{
    asset::RenderAssetUsages,
    prelude::*,
    render::{
        render_resource::{Extent3d, TextureDimension, TextureFormat},
        view::window::screenshot::{Screenshot, ScreenshotCaptured},
    },
    window::PrimaryWindow,
};
#[cfg(feature = "diagnostics")]
use bevy_agent_feedback_plugin::AgentFeedbackDiagnosticsPlugin;
use bevy_agent_feedback_plugin::{AgentFeedbackConfig, AgentFeedbackPlugin};
use serde_json::Value;
use std::{
    fs,
    io::{self, BufRead, BufReader, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MAX_SPAWN_POLLS: usize = 2_000;

#[test]
fn capture_after_one_frame_waits_for_the_next_update_and_readback() {
    let root = temp_root("one-frame-boundary");
    let (mut app, config) = agent_app(&root);
    let mut stream = connect(&config);

    send_raw(
        &mut stream,
        r#"{"id":"one-frame","command":"capture_after_frames","frames":1}"#,
    );
    let (screenshot, spawned_update) = wait_for_screenshot(&mut app, &stream);

    assert_no_response(
        &stream,
        "capture responded before ScreenshotCaptured was emitted",
    );

    let primary_window = {
        let world = app.world_mut();
        let mut windows = world.query_filtered::<Entity, With<PrimaryWindow>>();
        windows.single(world).expect("primary window should exist")
    };
    app.world_mut().despawn(primary_window);
    app.world_mut().trigger(ScreenshotCaptured {
        entity: screenshot,
        image: captured_test_image(),
    });

    let response = read_response(&mut stream);
    let capture = &response["result"]["capture"];
    let requested_frame = capture["requested_frame"]
        .as_u64()
        .expect("capture should include requested_frame");
    let completed_frame = capture["completed_frame"]
        .as_u64()
        .expect("capture should include completed_frame");

    assert_eq!(response["ok"], Value::Bool(true));
    assert_eq!(response["id"], "one-frame");
    assert_eq!(response["result"]["status"], "captured");
    assert_eq!(
        spawned_update,
        requested_frame + 1,
        "frames=1 must spawn on exactly the first update after admission"
    );
    assert!(
        completed_frame >= requested_frame,
        "completion frame {completed_frame} preceded request frame {requested_frame}"
    );
    assert_eq!(capture["completion"], "screenshot_captured");
    assert_eq!(
        capture.get("window_at_completion"),
        None,
        "a vanished primary window must be represented by an omitted nullable completion window"
    );
    assert_eq!(capture["window_at_request"]["physical_width"], 640);
    assert_eq!(capture["window_at_request"]["physical_height"], 480);

    let path = PathBuf::from(
        capture["path"]
            .as_str()
            .expect("capture should include a persisted path"),
    );
    assert!(path.is_file(), "capture response preceded PNG persistence");

    drop(stream);
    drop(app);
    fs::remove_dir_all(root).expect("remove capture test artifacts");
}

#[test]
fn persistence_failure_returns_capture_failed_without_recording_a_capture() {
    let root = temp_root("persistence-failure");
    let (mut app, config) = agent_app(&root);
    let mut stream = connect(&config);

    send_raw(&mut stream, r#"{"id":"save-failure","command":"capture"}"#);
    let (screenshot, _) = wait_for_screenshot(&mut app, &stream);
    assert_no_response(
        &stream,
        "capture responded before the screenshot image could be persisted",
    );

    fs::remove_dir_all(&config.capture_dir).expect("remove admitted capture directory");
    fs::write(&config.capture_dir, b"not a directory")
        .expect("replace capture directory with a regular file");
    app.world_mut().trigger(ScreenshotCaptured {
        entity: screenshot,
        image: captured_test_image(),
    });

    let response = read_response(&mut stream);
    assert_eq!(response["ok"], Value::Bool(false));
    assert_eq!(response["id"], "save-failure");
    assert_eq!(response["error"]["code"], "capture_failed");
    assert!(
        response["error"]["message"]
            .as_str()
            .expect("capture failure should include a message")
            .contains("failed to encode or save capture")
    );
    assert!(
        response["error"]["context"].get("latest_capture").is_none(),
        "a failed first capture must not become latest_capture"
    );
    assert!(
        response["error"]["context"]["snapshot"].is_object(),
        "persistence failures should retain structured game context"
    );

    drop(stream);
    drop(app);
    fs::remove_dir_all(root).expect("remove capture failure artifacts");
}

fn agent_app(root: &Path) -> (App, AgentFeedbackConfig) {
    let config = AgentFeedbackConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        protocol_file: root.join("agent-feedback.json"),
        capture_dir: root.join("captures"),
        max_wait_frames: 8,
        command_timeout: Duration::from_secs(5),
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
    #[cfg(feature = "diagnostics")]
    app.add_plugins(AgentFeedbackDiagnosticsPlugin::default());
    (app, config)
}

fn connect(config: &AgentFeedbackConfig) -> TcpStream {
    let protocol: Value = serde_json::from_slice(
        &fs::read(&config.protocol_file).expect("protocol file should be written"),
    )
    .expect("protocol file should contain JSON");
    let stream = TcpStream::connect(
        protocol["socket_addr"]
            .as_str()
            .expect("protocol should expose socket_addr"),
    )
    .expect("agent socket should accept local connections");
    stream
        .set_nonblocking(true)
        .expect("capture test stream should be nonblocking while checking early responses");
    stream
}

fn send_raw(stream: &mut TcpStream, request: &str) {
    writeln!(stream, "{request}").expect("send capture request");
    stream.flush().expect("flush capture request");
}

fn wait_for_screenshot(app: &mut App, stream: &TcpStream) -> (Entity, u64) {
    for update in 1..=MAX_SPAWN_POLLS {
        app.update();
        let screenshot = {
            let world = app.world_mut();
            let mut screenshots = world.query_filtered::<Entity, With<Screenshot>>();
            screenshots.iter(world).next()
        };
        if let Some(entity) = screenshot {
            assert_no_response(
                stream,
                "capture responded when Screenshot was spawned rather than completed",
            );
            return (
                entity,
                u64::try_from(update).expect("bounded update count should fit u64"),
            );
        }
        assert_no_response(
            stream,
            "capture responded before its scheduled frame boundary",
        );
        thread::sleep(Duration::from_millis(1));
    }
    panic!("capture request did not spawn Screenshot within {MAX_SPAWN_POLLS} app updates");
}

fn assert_no_response(stream: &TcpStream, contract: &str) {
    let mut byte = [0_u8; 1];
    match stream.peek(&mut byte) {
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
        Ok(0) => panic!("{contract}: agent socket closed"),
        Ok(_) => panic!("{contract}"),
        Err(error) => panic!("{contract}: socket peek failed: {error}"),
    }
}

fn read_response(stream: &mut TcpStream) -> Value {
    stream
        .set_nonblocking(false)
        .expect("restore blocking reads for completed response");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("bound capture response read");
    let mut line = String::new();
    BufReader::new(stream.try_clone().expect("clone capture response stream"))
        .read_line(&mut line)
        .expect("read capture response");
    assert!(
        !line.is_empty(),
        "agent socket closed before capture response"
    );
    serde_json::from_str(&line).expect("capture response should be JSON")
}

fn captured_test_image() -> Image {
    Image::new_fill(
        Extent3d {
            width: 2,
            height: 2,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[12, 34, 56, 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    )
}

fn temp_root(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "bevy-agent-feedback-capture-{name}-{}-{nonce}",
        std::process::id()
    ))
}
