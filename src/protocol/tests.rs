use super::commands::{ComparisonOperator, TargetKind, TargetSelector};
use super::*;
use serde_json::{Value, json};
use std::time::Duration;
mod discovery;
mod serialization;

const TEST_LIMITS: ParseLimits = ParseLimits {
    max_wait_frames: 7,
    max_action_steps: 3,
    max_time_advance_steps: 4,
    max_time_advance: Duration::from_secs(3),
};

fn parse_value(request: Value) -> AgentCommand {
    parse_request_with_limits(&request.to_string(), TEST_LIMITS)
        .expect("valid protocol request")
        .command
}

fn assert_invalid_argument(request: Value, message: &str) {
    let id = request["id"].clone();
    let error = parse_request_with_limits(&request.to_string(), TEST_LIMITS)
        .expect_err("invalid protocol argument");
    assert_eq!(error.id, id);
    assert_eq!(error.code, "invalid_argument");
    assert_eq!(error.message, message);
}

fn assert_invalid_argument_wire(request: &str, message: &str) {
    let id = serde_json::from_str::<Value>(request).expect("request JSON")["id"].clone();
    let error =
        parse_request_with_limits(request, TEST_LIMITS).expect_err("invalid protocol argument");
    assert_eq!(
        (error.id, error.code, error.message),
        (id, "invalid_argument", message.to_string())
    );
}

#[test]
fn parses_new_commands_defaults_and_legacy_v2_wait() {
    let cases = vec![
        (
            "legacy wait",
            json!({"id": 1, "command": "wait", "frames": 3}),
            AgentCommand::Wait { frames: 3 },
        ),
        (
            "wait seconds default budget",
            json!({"id": 2, "command": "wait_seconds", "seconds": 0.25}),
            AgentCommand::WaitSeconds {
                duration: Duration::from_millis(250),
                max_frames: 7,
            },
        ),
        (
            "advance time default step",
            json!({"id": 3, "command": "advance_time", "seconds": 0.25}),
            AgentCommand::AdvanceTime {
                duration: Duration::from_millis(250),
                step: None,
            },
        ),
        (
            "capture after frames default label",
            json!({"id": 4, "command": "capture_after_frames", "frames": 2}),
            AgentCommand::CaptureAfterFrames {
                frames: 2,
                label: None,
            },
        ),
        (
            "target info defaults",
            json!({"id": 5, "command": "target_info", "target": {"name": "Play"}}),
            AgentCommand::TargetInfo {
                target: TargetSelector::Name("Play".to_string()),
                kind: TargetKind::Any,
                camera: None,
            },
        ),
        (
            "click target defaults",
            json!({
                "id": 6,
                "command": "click_target",
                "target": {"accessibility_label": "Start game"}
            }),
            AgentCommand::ClickTarget {
                target: TargetSelector::AccessibilityLabel("Start game".to_string()),
                kind: TargetKind::Any,
                camera: None,
                button: MouseButton::Left,
                frames: 1,
            },
        ),
        (
            "resource info defaults",
            json!({"id": 7, "command": "resource_info"}),
            AgentCommand::ResourceInfo {
                resource: None,
                field: None,
            },
        ),
        (
            "evaluate predicate",
            json!({
                "id": 8,
                "command": "evaluate_predicate",
                "predicate": {"type": "state_equals", "state": "GameState", "value": "Playing"}
            }),
            AgentCommand::EvaluatePredicate {
                predicate: Predicate::StateEquals {
                    state: "GameState".to_string(),
                    value: DiagnosticValue::String("Playing".to_string()),
                },
            },
        ),
        (
            "wait for default budget",
            json!({
                "id": 9,
                "command": "wait_for",
                "predicate": {"type": "target_absent", "target": {"marker": "Loading"}}
            }),
            AgentCommand::WaitFor {
                predicate: Predicate::TargetAbsent {
                    target: TargetSelector::Marker("Loading".to_string()),
                    kind: TargetKind::Any,
                    camera: None,
                },
                max_frames: 7,
            },
        ),
    ];
    for (name, request, expected) in cases {
        assert_eq!(parse_value(request), expected, "{name}");
    }
}

#[test]
fn validates_duration_step_and_frame_boundaries() {
    for (request, expected) in [
        (
            json!({"id": "short", "command": "wait_seconds", "seconds": 0.000000001}),
            AgentCommand::WaitSeconds {
                duration: Duration::from_nanos(1),
                max_frames: 7,
            },
        ),
        (
            json!({
                "id": "short-step",
                "command": "advance_time",
                "seconds": 0.000000001,
                "step_seconds": 0.000000002
            }),
            AgentCommand::AdvanceTime {
                duration: Duration::from_nanos(1),
                step: Some(Duration::from_nanos(2)),
            },
        ),
        (
            json!({
                "id": "caps",
                "command": "advance_time",
                "seconds": 3.0,
                "step_seconds": 0.75
            }),
            AgentCommand::AdvanceTime {
                duration: Duration::from_secs(3),
                step: Some(Duration::from_millis(750)),
            },
        ),
    ] {
        assert_eq!(parse_value(request), expected);
    }
    for request in [
        json!({"id": 1, "command": "wait_seconds", "seconds": 1, "max_frames": 7}),
        json!({"id": 2, "command": "capture_after_frames", "frames": 7}),
        json!({"id": 3, "command": "click_target", "target": {"name": "Play"}, "frames": 7}),
        json!({
            "id": 4,
            "command": "wait_for",
            "predicate": {"type": "target_exists", "target": {"name": "Play"}},
            "max_frames": 7
        }),
    ] {
        parse_request_with_limits(&request.to_string(), TEST_LIMITS)
            .expect("advertised frame cap is inclusive");
    }

    for case in [
        r#"{"id":"zero-seconds","command":"advance_time","seconds":0}|seconds must be finite and positive"#,
        r#"{"id":"negative-seconds","command":"wait_seconds","seconds":-0.5}|seconds must be finite and positive"#,
        r#"{"id":"short-seconds","command":"advance_time","seconds":1e-12}|seconds must not round to zero duration"#,
        r#"{"id":"duration-cap","command":"advance_time","seconds":3.000000001}|seconds must not exceed 3"#,
        r#"{"id":"duration-overflow","command":"advance_time","seconds":1e300}|seconds is outside the supported duration range"#,
        r#"{"id":"zero-step","command":"advance_time","seconds":1,"step_seconds":0}|step_seconds must be finite and positive"#,
        r#"{"id":"negative-step","command":"advance_time","seconds":1,"step_seconds":-1}|step_seconds must be finite and positive"#,
        r#"{"id":"short-step","command":"advance_time","seconds":1,"step_seconds":1e-12}|step_seconds must not round to zero duration"#,
        r#"{"id":"step-cap","command":"advance_time","seconds":3,"step_seconds":0.5}|advance requires 6 steps, exceeding max_time_advance_steps 4"#,
        r#"{"id":"wait-zero","command":"wait_seconds","seconds":1,"max_frames":0}|max_frames must be between 1 and 7, got 0"#,
        r#"{"id":"wait-cap","command":"wait_seconds","seconds":1,"max_frames":8}|max_frames must be between 1 and 7, got 8"#,
        r#"{"id":"capture-zero","command":"capture_after_frames","frames":0}|frames must be between 1 and 7, got 0"#,
        r#"{"id":"capture-cap","command":"capture_after_frames","frames":8}|frames must be between 1 and 7, got 8"#,
        r#"{"id":"click-zero","command":"click_target","target":{"name":"Play"},"frames":0}|frames must be between 1 and 7, got 0"#,
        r#"{"id":"click-cap","command":"click_target","target":{"name":"Play"},"frames":8}|frames must be between 1 and 7, got 8"#,
        r#"{"id":"predicate-zero","command":"wait_for","predicate":{"type":"target_exists","target":{"name":"Play"}},"max_frames":0}|max_frames must be between 1 and 7, got 0"#,
        r#"{"id":"predicate-cap","command":"wait_for","predicate":{"type":"target_exists","target":{"name":"Play"}},"max_frames":8}|max_frames must be between 1 and 7, got 8"#,
    ] {
        let (request, message) = case.split_once('|').expect("case delimiter");
        assert_invalid_argument_wire(request, message);
    }

    let error = parse_request_with_limits(
        r#"{"id":"nonfinite","command":"advance_time","seconds":1e400}"#,
        TEST_LIMITS,
    )
    .expect_err("nonfinite JSON number");
    assert_eq!((error.id, error.code), (Value::Null, "invalid_request"));
    assert!(error.message.contains("number out of range"));
    for (wire, id, integer_type) in [
        (
            r#"{"id":"frame-overflow","command":"capture_after_frames","frames":65536}"#,
            "frame-overflow",
            "u16",
        ),
        (
            r#"{"id":"count-overflow","command":"evaluate_predicate","predicate":{"type":"marker_count","marker":"Enemy","min":4294967296}}"#,
            "count-overflow",
            "u32",
        ),
    ] {
        let error =
            parse_request_with_limits(wire, TEST_LIMITS).expect_err("wire integer overflow");
        assert_eq!(&error.id, &Value::from(id));
        assert_eq!(error.code, "invalid_request");
        assert!(error.message.contains(integer_type), "{error:?}");
    }
}

#[test]
fn validates_selectors_scalars_ranges_and_comparison_operators() {
    let selector_cases = [
        (
            json!({"id": 1, "command": "target_info", "target": {"name": "n".repeat(128)}}),
            TargetSelector::Name("n".repeat(128)),
        ),
        (
            json!({"id": 2, "command": "target_info", "target": {"accessibility_label": "é".repeat(64)}}),
            TargetSelector::AccessibilityLabel("é".repeat(64)),
        ),
        (
            json!({"id": 3, "command": "target_info", "target": {"marker": "Clickable"}}),
            TargetSelector::Marker("Clickable".to_string()),
        ),
    ];
    for (request, expected) in selector_cases {
        let AgentCommand::TargetInfo { target, .. } = parse_value(request) else {
            panic!("target_info did not parse to TargetInfo");
        };
        assert_eq!(target, expected);
    }

    for (wire, expected) in [
        ("eq", ComparisonOperator::Eq),
        ("ne", ComparisonOperator::Ne),
        ("lt", ComparisonOperator::Lt),
        ("lte", ComparisonOperator::Lte),
        ("gt", ComparisonOperator::Gt),
        ("gte", ComparisonOperator::Gte),
    ] {
        let command = parse_value(json!({
            "id": wire,
            "command": "evaluate_predicate",
            "predicate": {
                "type": "resource_field",
                "resource": "Score",
                "field": "points",
                "operator": wire,
                "value": 10
            }
        }));
        let AgentCommand::EvaluatePredicate {
            predicate: Predicate::ResourceField { operator, .. },
        } = command
        else {
            panic!("resource predicate did not parse to ResourceField");
        };
        assert_eq!(operator, expected, "{wire}");
    }
    assert_eq!(
        parse_value(json!({
            "id": "zero-range",
            "command": "evaluate_predicate",
            "predicate": {"type": "marker_count", "marker": "Enemy", "min": 0, "max": 0}
        })),
        AgentCommand::EvaluatePredicate {
            predicate: Predicate::MarkerCount {
                marker: "Enemy".to_string(),
                min: Some(0),
                max: Some(0),
            },
        }
    );

    for (request, message) in [
        (
            json!({"id": "missing", "command": "target_info", "target": {}}),
            "target must contain exactly one of name, accessibility_label, or marker",
        ),
        (
            json!({"id": "multiple", "command": "target_info", "target": {"name": "Play", "marker": "Clickable"}}),
            "target must contain exactly one of name, accessibility_label, or marker",
        ),
        (
            json!({"id": "empty", "command": "target_info", "target": {"name": ""}}),
            "target.name must contain 1..=128 UTF-8 bytes",
        ),
        (
            json!({"id": "long", "command": "target_info", "target": {"name": "x".repeat(129)}}),
            "target.name must contain 1..=128 UTF-8 bytes",
        ),
        (
            json!({"id": "camera", "command": "target_info", "target": {"name": "Play"}, "camera": ""}),
            "camera must contain 1..=128 UTF-8 bytes",
        ),
        (
            json!({"id": "resource", "command": "resource_info", "resource": ""}),
            "resource must contain 1..=128 UTF-8 bytes",
        ),
        (
            json!({
                "id": "ordering",
                "command": "evaluate_predicate",
                "predicate": {"type": "resource_field", "resource": "Score", "field": "points", "operator": "lt", "value": false}
            }),
            "ordering comparisons require a numeric value",
        ),
        (
            json!({
                "id": "array",
                "command": "evaluate_predicate",
                "predicate": {"type": "state_equals", "state": "GameState", "value": [1]}
            }),
            "value must be a null, boolean, finite number, or string scalar",
        ),
        (
            json!({
                "id": "long-scalar",
                "command": "evaluate_predicate",
                "predicate": {"type": "state_equals", "state": "GameState", "value": "x".repeat(1025)}
            }),
            "value must contain 1..=1024 UTF-8 bytes",
        ),
        (
            json!({
                "id": "missing-range",
                "command": "evaluate_predicate",
                "predicate": {"type": "marker_count", "marker": "Enemy"}
            }),
            "marker_count requires min, max, or both",
        ),
        (
            json!({
                "id": "inverted-range",
                "command": "evaluate_predicate",
                "predicate": {"type": "marker_count", "marker": "Enemy", "min": 2, "max": 1}
            }),
            "marker_count min must be <= max, got 2 > 1",
        ),
    ] {
        assert_invalid_argument(request, message);
    }
    for wire in [
        r#"{"id":"unknown-selector","command":"target_info","target":{"name":"Play","entity":"42"}}"#,
        r#"{"id":"unknown-kind","command":"target_info","target":{"name":"Play"},"kind":"screen"}"#,
        r#"{"id":"unknown-operator","command":"evaluate_predicate","predicate":{"type":"resource_field","resource":"Score","field":"points","operator":"approximately","value":10}}"#,
    ] {
        let error =
            parse_request_with_limits(wire, TEST_LIMITS).expect_err("unknown wire vocabulary");
        assert_eq!(error.code, "invalid_request");
        assert_ne!(error.id, Value::Null);
    }
}

#[test]
fn parses_case_insensitive_input_names() {
    for (wire, expected) in [
        (
            r#"{"id":1,"command":"click","x":12,"y":34,"button":"right"}"#,
            AgentCommand::Click {
                position: Vec2::new(12.0, 34.0),
                button: MouseButton::Right,
                frames: 1,
            },
        ),
        (
            r#"{"id":2,"command":"key_tap","key":"keyw"}"#,
            AgentCommand::KeyHold {
                key: KeyCode::KeyW,
                frames: 1,
            },
        ),
    ] {
        assert_eq!(
            parse_request(wire, 10, 10).expect("valid request").command,
            expected
        );
    }
}

#[test]
fn invalid_names_suggest_close_values() {
    for (wire, id, suggestion) in [
        (
            r#"{"id":1,"command":"mouse_down","button":"rigth"}"#,
            json!(1),
            "Right",
        ),
        (
            r#"{"id":"bad-scroll","command":"scroll","lines":1,"unit":"lien"}"#,
            json!("bad-scroll"),
            "Line",
        ),
        (
            r#"{"id":3,"command":"key_tap","key":"keyww"}"#,
            json!(3),
            "KeyW",
        ),
    ] {
        let error = parse_request(wire, 10, 10).expect_err("invalid input name");
        assert_eq!(error.id, id);
        assert!(error.message.contains(suggestion), "{error:?}");
    }
}

#[test]
fn validates_capture_labels() {
    assert_eq!(
        parse_request(
            r#"{"id":"shot","command":"capture","label":"boss-Intro_1"}"#,
            10,
            10,
        )
        .expect("valid label")
        .command,
        AgentCommand::Capture {
            label: Some("boss-Intro_1".to_string()),
        }
    );
    for label in ["", "has space", "slash/name", &"x".repeat(41)] {
        let request = json!({"id": label, "command": "capture", "label": label}).to_string();
        let error = parse_request(&request, 10, 10).expect_err("invalid capture label");
        assert_eq!(
            (error.id, error.code),
            (Value::from(label), "invalid_argument")
        );
        assert_eq!(
            error.message,
            "capture label must match [A-Za-z0-9_-]{1,40}"
        );
    }
}
