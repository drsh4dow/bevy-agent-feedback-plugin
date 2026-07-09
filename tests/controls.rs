mod controls_support;

use bevy::{
    asset::RenderAssetUsages,
    prelude::*,
    render::{
        render_resource::{Extent3d, TextureDimension, TextureFormat},
        view::window::screenshot::ScreenshotCaptured,
    },
    window::PrimaryWindow,
};
use controls_support::*;
use serde_json::Value;
use std::{fs, path::PathBuf, thread, time::Duration};

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

    assert_eq!(
        response["result"]["mouse_position"],
        Value::Array(vec![Value::from(320.0), Value::from(240.0)])
    );
    assert_eq!(
        response["result"]["window"]["cursor_position"],
        serde_json::json!([320.0, 240.0])
    );
    let mut windows = app
        .world_mut()
        .query_filtered::<&Window, With<PrimaryWindow>>();
    assert_eq!(
        windows.single(app.world()).unwrap().cursor_position(),
        Some(Vec2::new(320.0, 240.0))
    );
    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn second_cursor_move_uses_agent_logical_position() {
    let (mut app, config) = agent_app("second-cursor-move");
    let mut stream = connect(&config);

    send_ok(
        &mut app,
        &mut stream,
        r#"{"id":1,"command":"cursor_move","x":100,"y":100}"#,
    );
    let response = send_ok(
        &mut app,
        &mut stream,
        r#"{"id":2,"command":"cursor_move","x":110,"y":105}"#,
    );

    assert_eq!(
        response["result"]["mouse_position"],
        Value::Array(vec![Value::from(110.0), Value::from(105.0)])
    );
    assert_eq!(response["result"]["pressed_keys"], Value::Array(Vec::new()));
    assert_eq!(
        response["result"]["pressed_buttons"],
        Value::Array(Vec::new())
    );

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
    let mut windows = app
        .world_mut()
        .query_filtered::<&Window, With<PrimaryWindow>>();
    assert_eq!(
        windows.single(app.world()).unwrap().cursor_position(),
        Some(Vec2::new(320.0, 240.0))
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
    assert_eq!(
        response["error"]["message"],
        "point [999,10] outside logical window 640x480"
    );
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
        response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("outside logical window 100x100")),
        "{}",
        response["error"]["message"]
    );
    assert!(
        !app.world()
            .resource::<ButtonInput<MouseButton>>()
            .pressed(MouseButton::Left)
    );
    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn invalid_request_preserves_id_for_validation_errors() {
    let (mut app, config) = agent_app("invalid-request-id");
    let mut stream = connect(&config);

    let response = send(
        &mut app,
        &mut stream,
        r#"{"id":"bad-button","command":"click","x":10,"y":10,"button":"rigth"}"#,
    );

    assert_eq!(response["ok"], Value::Bool(false));
    assert_eq!(response["id"], "bad-button");
    assert_eq!(response["error"]["code"], "invalid_argument");
    assert!(
        response["error"]["message"]
            .as_str()
            .expect("error message should be a string")
            .contains("Right")
    );
    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn malformed_json_returns_null_id() {
    let (mut app, config) = agent_app("malformed-json-null-id");
    let mut stream = connect(&config);

    let response = send(
        &mut app,
        &mut stream,
        r#"{"id":"bad-json","command":"click""#,
    );

    assert_eq!(response["ok"], Value::Bool(false));
    assert_eq!(response["id"], Value::Null);
    assert_eq!(response["error"]["code"], "invalid_request");
    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn labeled_capture_uses_label_in_filename_and_response() {
    let (mut app, config) = agent_app("labeled-capture");
    let mut stream = connect(&config);

    send_raw(
        &mut stream,
        r#"{"id":"capture-hud","command":"capture","label":"hud_1"}"#,
    );
    let screenshot = screenshot_entity(&mut app);
    app.world_mut().trigger(ScreenshotCaptured {
        entity: screenshot,
        image: bevy::image::Image::new_fill(
            Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            &[0, 0, 0, 255],
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::default(),
        ),
    });

    let response = read_response_while_updating(&mut app, &mut stream);
    let expected_path = config.capture_dir.join("capture-000000-hud_1.png");

    assert_eq!(response["ok"], Value::Bool(true));
    assert_eq!(response["id"], "capture-hud");
    assert_eq!(response["result"]["status"], "captured");
    assert_eq!(response["result"]["capture"]["sequence"], Value::from(0));
    assert_eq!(response["result"]["capture"]["label"], "hud_1");
    assert_eq!(response["result"]["latest_capture"]["label"], "hud_1");
    assert_eq!(
        PathBuf::from(
            response["result"]["capture"]["path"]
                .as_str()
                .expect("capture response should include a path")
        ),
        expected_path
    );
    assert!(expected_path.exists());
    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn capture_without_primary_window_returns_missing_window() {
    let (mut app, config) = agent_app("capture-missing-window");
    let mut stream = connect(&config);
    let mut windows = app
        .world_mut()
        .query_filtered::<Entity, With<PrimaryWindow>>();
    let window_entity = windows.single(app.world()).expect("primary window");
    app.world_mut().despawn(window_entity);

    let response = send(&mut app, &mut stream, r#"{"id":1,"command":"capture"}"#);

    assert_eq!(response["ok"], Value::Bool(false));
    assert_eq!(response["error"]["code"], "missing_window");
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
        assert_eq!(
            response["error"]["message"],
            "point [999,10] outside logical window 640x480"
        );

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
