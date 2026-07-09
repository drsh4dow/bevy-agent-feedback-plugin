use super::super::*;
use crate::{
    AgentFeedbackConfig,
    session::{AgentFeedbackSession, PROTOCOL_VERSION},
};
use serde_json::{Value, json};
use std::{fs, net::SocketAddr, time::Duration};

#[test]
fn writes_complete_v2_discovery_metadata() {
    let root =
        std::env::temp_dir().join(format!("bevy-agent-protocol-{}", crate::session::unix_ms()));
    let config = AgentFeedbackConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        protocol_file: root.join("agent.json"),
        capture_dir: root.join("captures"),
        max_wait_frames: 7,
        max_action_steps: 3,
        deterministic_time: true,
        max_time_advance_steps: 4,
        max_time_advance: Duration::from_secs(3),
        command_timeout: Duration::from_secs(2),
        ..Default::default()
    };
    let session = AgentFeedbackSession::new(&config);
    let address = SocketAddr::from(([127, 0, 0, 1], 12345));
    write_protocol_file(&config, &session, address).expect("protocol");
    let protocol: Value =
        serde_json::from_slice(&fs::read(&config.protocol_file).expect("protocol bytes"))
            .expect("protocol json");

    assert_eq!(protocol.as_object().expect("protocol object").len(), 22);
    for (key, expected) in [
        ("protocol", json!(PROTOCOL_VERSION)),
        ("session_id", json!(session.session_id)),
        ("pid", json!(session.pid)),
        ("started_at_unix_ms", json!(session.started_at_unix_ms)),
        (
            "heartbeat_file",
            json!(session.heartbeat_file.to_string_lossy()),
        ),
        (
            "heartbeat_interval_ms",
            json!(session.heartbeat_interval.as_millis()),
        ),
        ("stale_after_ms", json!(session.stale_after.as_millis())),
        ("socket_addr", json!(address.to_string())),
        ("transport", json!("json-lines-over-tcp")),
        ("clients", json!("single local client at a time")),
        (
            "coordinates",
            json!("logical window pixels, origin at the top-left of the primary window"),
        ),
        ("capture_dir", json!(config.capture_dir.to_string_lossy())),
        ("command_timeout_ms", json!(2000)),
        ("deterministic_time", json!(true)),
        ("max_action_steps", json!(3)),
        ("max_wait_frames", json!(7)),
        ("max_time_advance_steps", json!(4)),
        ("max_time_advance_seconds", json!(3.0)),
        (
            "window_modes",
            json!(["windowed", "borderless_fullscreen", "fullscreen"]),
        ),
        ("capture_completion", json!("screenshot_captured")),
    ] {
        assert_eq!(protocol[key], expected, "{key}");
    }

    let commands = &protocol["commands"];
    let command_names = "key_down,key_up,mouse_down,mouse_up,cursor_move,mouse_motion,mouse_scroll,scroll,click,drag,key_tap,key_hold,release_all_inputs,shutdown,text,file_hover,file_drop,file_cancel,window_info,wait,wait_seconds,advance_time,capture,capture_after_frames,target_info,click_target,resource_info,evaluate_predicate,wait_for,ecs_summary,list_entities,camera_info,state_info,marker_info";
    assert_eq!(commands.as_object().expect("commands object").len(), 34);
    for name in command_names.split(',') {
        assert!(commands.get(name).is_some(), "missing command {name}");
    }
    let json_value = |text| serde_json::from_str::<Value>(text).expect("expected JSON");
    let assert_command = |name: &str, expected| {
        assert_eq!(commands[name], json_value(expected), "{name}");
    };
    assert_command("wait", r#"{"frames":"1..=7; default 1"}"#);
    assert_command(
        "wait_seconds",
        r#"{"seconds":"positive finite f64 converted to nonzero duration","max_frames":"1..=7; default 7"}"#,
    );
    assert_command(
        "advance_time",
        r#"{"seconds":"positive finite duration <= 3 seconds","step_seconds":"optional positive finite duration; ceil(seconds/step_seconds) <= 4; default Time<Fixed>::timestep or 1/60"}"#,
    );
    assert_command(
        "capture_after_frames",
        r#"{"frames":"required 1..=7","label":"optional [A-Za-z0-9_-]{1,40}"}"#,
    );
    let requirement = "diagnostics feature and AgentFeedbackDiagnosticsPlugin";
    assert_command(
        "target_info",
        r#"{"target":{"exactly_one":["name","accessibility_label","marker"],"string_bytes":"1..=128"},"kind":"any|ui|world; default any","camera":"optional exact 1..=128 byte name","requires":"diagnostics feature and AgentFeedbackDiagnosticsPlugin"}"#,
    );
    assert_command(
        "click_target",
        r#"{"target":{"exactly_one":["name","accessibility_label","marker"],"string_bytes":"1..=128"},"kind":"any|ui|world; default any","camera":"optional exact 1..=128 byte name","button":"default Left","frames":"1..=7; default 1","requires":"diagnostics feature and AgentFeedbackDiagnosticsPlugin"}"#,
    );
    assert_command(
        "resource_info",
        r#"{"resource":"optional exact 1..=128 byte key","field":"optional exact 1..=128 byte key","requires":"diagnostics feature and AgentFeedbackDiagnosticsPlugin"}"#,
    );
    let predicates = json_value(
        r#"{"discriminator":"type: state_equals|resource_field|marker_count|target_exists|target_absent","state_equals":{"state":"exact 1..=128 byte key","value":"bounded scalar"},"resource_field":{"resource":"exact 1..=128 byte key","field":"exact 1..=128 byte key","operator":"eq|ne|lt|lte|gt|gte","value":"bounded scalar; ordering requires a number"},"marker_count":{"marker":"exact 1..=128 byte key","min":"optional u32","max":"optional u32; at least one bound is required"},"target_exists":{"target":{"exactly_one":["name","accessibility_label","marker"],"string_bytes":"1..=128"},"kind":"any|ui|world; default any","camera":"optional exact 1..=128 byte name"},"target_absent":{"target":{"exactly_one":["name","accessibility_label","marker"],"string_bytes":"1..=128"},"kind":"any|ui|world; default any","camera":"optional exact 1..=128 byte name"},"scalar":"null, boolean, finite number, or UTF-8 string of 1..=1024 bytes"}"#,
    );
    assert_eq!(
        commands["evaluate_predicate"],
        json!({"predicate": predicates.clone(), "requires": requirement})
    );
    assert_eq!(
        commands["wait_for"],
        json!({"predicate": predicates, "max_frames": "1..=7; default 7", "requires": requirement})
    );
    assert_eq!(
        protocol["examples"],
        json_value(
            r#"[{"id":1,"command":"window_info"},{"id":2,"command":"click","x":320.0,"y":240.0,"button":"left"},{"id":3,"command":"drag","from":[320.0,240.0],"to":[420.0,240.0],"button":"Right","steps":5,"frames":5},{"id":4,"command":"key_tap","key":"keyw"},{"id":5,"command":"capture","label":"default"},{"id":6,"command":"release_all_inputs"},{"id":7,"command":"marker_info"},{"id":8,"command":"shutdown"}]"#
        )
    );
    assert!(session.heartbeat_file.exists());
    let _ = fs::remove_dir_all(root);
}
