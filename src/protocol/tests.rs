use super::*;
use std::{net::SocketAddr, time::Duration};

#[test]
fn parses_case_insensitive_input_names() {
    let request = parse_request(
        r#"{"id":1,"command":"click","x":12,"y":34,"button":"right"}"#,
        10,
        10,
    )
    .expect("valid request");

    assert_eq!(request.id, Value::from(1));
    assert_eq!(
        request.command,
        AgentCommand::Click {
            position: Vec2::new(12.0, 34.0),
            button: MouseButton::Right,
            frames: 1,
        }
    );

    let request = parse_request(r#"{"id":2,"command":"key_tap","key":"keyw"}"#, 10, 10)
        .expect("valid request");
    assert_eq!(
        request.command,
        AgentCommand::KeyHold {
            key: KeyCode::KeyW,
            frames: 1,
        }
    );
}

#[test]
fn invalid_names_suggest_close_values() {
    let error = parse_request(
        r#"{"id":1,"command":"mouse_down","button":"rigth"}"#,
        10,
        10,
    )
    .expect_err("invalid button");
    assert_eq!(error.id, Value::from(1));
    assert!(error.message.contains("Right"));

    let error = parse_request(
        r#"{"id":"bad-scroll","command":"scroll","lines":1,"unit":"lien"}"#,
        10,
        10,
    )
    .expect_err("invalid scroll unit");
    assert_eq!(error.id, Value::from("bad-scroll"));
    assert!(error.message.contains("Line"));

    let error = parse_request(r#"{"id":3,"command":"key_tap","key":"keyww"}"#, 10, 10)
        .expect_err("invalid key");
    assert_eq!(error.id, Value::from(3));
    assert!(error.message.contains("KeyW"));
}

#[test]
fn parses_capture_label() {
    let request = parse_request(
        r#"{"id":"shot","command":"capture","label":"boss-Intro_1"}"#,
        10,
        10,
    )
    .expect("valid labeled capture");

    assert_eq!(request.id, Value::from("shot"));
    assert_eq!(
        request.command,
        AgentCommand::Capture {
            label: Some("boss-Intro_1".to_string()),
        }
    );
}

#[test]
fn rejects_invalid_capture_labels_as_invalid_arguments() {
    for label in [
        "",
        "has space",
        "slash/name",
        "12345678901234567890123456789012345678901",
    ] {
        let request = json!({
            "id": label,
            "command": "capture",
            "label": label,
        })
        .to_string();
        let error = parse_request(&request, 10, 10).expect_err("invalid capture label");

        assert_eq!(error.id, Value::from(label));
        assert_eq!(error.code, "invalid_argument");
        assert_eq!(
            error.message,
            "capture label must match [A-Za-z0-9_-]{1,40}"
        );
    }
}

#[test]
fn rejects_wait_commands_outside_the_frame_bound() {
    let error = parse_request(r#"{"id":"slow","command":"wait","frames":11}"#, 10, 10)
        .expect_err("frame bound should be enforced");

    assert_eq!(error.id, Value::from("slow"));
    assert!(error.message.contains("frames"));
}

#[test]
fn writes_v2_protocol_with_session_metadata() {
    let root =
        std::env::temp_dir().join(format!("bevy-agent-protocol-{}", crate::session::unix_ms()));
    let config = AgentFeedbackConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        protocol_file: root.join("agent.json"),
        capture_dir: root.join("captures"),
        command_timeout: Duration::from_secs(2),
        ..Default::default()
    };
    let session = AgentFeedbackSession::new(&config);

    write_protocol_file(&config, &session, SocketAddr::from(([127, 0, 0, 1], 12345)))
        .expect("protocol");

    let protocol: Value =
        serde_json::from_slice(&fs::read(&config.protocol_file).unwrap()).expect("protocol json");
    assert_eq!(protocol["protocol"], PROTOCOL_VERSION);
    assert_eq!(protocol["session_id"], session.session_id);
    assert_eq!(protocol["max_wait_frames"], config.max_wait_frames);
    assert!(session.heartbeat_file.exists());
    let _ = fs::remove_dir_all(root);
}
