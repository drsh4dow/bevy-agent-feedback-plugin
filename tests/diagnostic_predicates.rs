#![cfg(feature = "diagnostics")]

#[allow(dead_code)]
mod controls_support;

use bevy::prelude::*;
use bevy_agent_feedback_plugin::AgentFeedbackDiagnosticsPlugin;
use controls_support::{agent_app, connect, send};
use serde_json::json;
use std::fs;

#[derive(States, Clone, Debug, Default, Eq, Hash, PartialEq)]
enum GamePhase {
    #[default]
    Loading,
    Ready,
}

#[derive(Resource)]
struct SessionStats {
    score: f64,
    ready: bool,
}

#[derive(Component)]
struct VisibleMarker;

fn diagnostic_app(name: &str) -> (App, bevy_agent_feedback_plugin::AgentFeedbackConfig) {
    let (mut app, config) = agent_app(name);
    app.insert_resource(State::new(GamePhase::Ready));
    app.insert_resource(SessionStats {
        score: 7.5,
        ready: true,
    });
    app.add_plugins(
        AgentFeedbackDiagnosticsPlugin::default()
            .with_state::<GamePhase>()
            .with_marker::<VisibleMarker>()
            .with_resource_field::<SessionStats, _, _>("score", |stats| stats.score)
            .with_resource_field::<SessionStats, _, _>("ready", |stats| stats.ready)
            .with_resource_field::<SessionStats, _, _>("invalid", |_| f64::NAN),
    );
    (app, config)
}

#[test]
fn public_state_and_resource_reads_drive_exact_predicate_outcomes() {
    let (mut app, config) = diagnostic_app("diagnostic-state-resource");
    let mut stream = connect(&config);

    let state_info = send(
        &mut app,
        &mut stream,
        &json!({"id": "state-info", "command": "state_info"}).to_string(),
    );
    assert_eq!(state_info["ok"], true);
    assert_eq!(state_info["result"]["status"], "ok");
    assert_eq!(
        state_info["result"]["details"]["states"][0]["value"],
        "Ready"
    );

    let resource_info = send(
        &mut app,
        &mut stream,
        &json!({
            "id": "resource-info",
            "command": "resource_info",
            "resource": "SessionStats",
            "field": "score"
        })
        .to_string(),
    );
    assert_eq!(resource_info["ok"], true);
    assert_eq!(resource_info["result"]["status"], "resource_info");
    assert_eq!(
        resource_info["result"]["details"],
        json!({
            "scope": "field",
            "resource": "SessionStats",
            "field": "score",
            "value": 7.5
        })
    );

    let cases = [
        (
            "state-match",
            json!({"type": "state_equals", "state": "GamePhase", "value": "Ready"}),
            "matched",
            json!("Ready"),
        ),
        (
            "state-miss",
            json!({"type": "state_equals", "state": "GamePhase", "value": "Loading"}),
            "not_matched",
            json!("Ready"),
        ),
        (
            "resource-match",
            json!({
                "type": "resource_field",
                "resource": "SessionStats",
                "field": "score",
                "operator": "gte",
                "value": 7.5
            }),
            "matched",
            json!(7.5),
        ),
        (
            "resource-miss",
            json!({
                "type": "resource_field",
                "resource": "SessionStats",
                "field": "score",
                "operator": "lt",
                "value": 7.5
            }),
            "not_matched",
            json!(7.5),
        ),
    ];
    for (id, predicate, outcome, observed_value) in cases {
        let response = send(
            &mut app,
            &mut stream,
            &json!({
                "id": id,
                "command": "evaluate_predicate",
                "predicate": predicate
            })
            .to_string(),
        );
        assert_eq!(response["ok"], true, "case {id}: {response}");
        assert_eq!(response["result"]["status"], "predicate_evaluated");
        assert_eq!(response["result"]["details"]["outcome"], outcome);
        assert_eq!(
            response["result"]["details"]["value"], observed_value,
            "case {id}"
        );
        assert_eq!(
            response["result"]["details"]["predicate"], predicate,
            "the observation must identify the predicate that produced it"
        );
    }

    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn scalar_reader_and_ordering_type_failures_have_structured_diagnostics() {
    let (mut app, config) = diagnostic_app("diagnostic-scalar-errors");
    let mut stream = connect(&config);

    let type_mismatch = send(
        &mut app,
        &mut stream,
        &json!({
            "id": "type-mismatch",
            "command": "evaluate_predicate",
            "predicate": {
                "type": "resource_field",
                "resource": "SessionStats",
                "field": "ready",
                "operator": "gt",
                "value": 0
            }
        })
        .to_string(),
    );
    assert_eq!(type_mismatch["ok"], false);
    assert_eq!(type_mismatch["error"]["code"], "comparison_type_mismatch");
    assert_eq!(
        type_mismatch["error"]["message"],
        "ordering comparisons require numeric observed and expected values"
    );
    assert_eq!(
        type_mismatch["error"]["context"]["diagnostic"],
        json!({"resource": "SessionStats", "field": "ready"})
    );

    let invalid_scalar = send(
        &mut app,
        &mut stream,
        &json!({
            "id": "invalid-reader",
            "command": "evaluate_predicate",
            "predicate": {
                "type": "resource_field",
                "resource": "SessionStats",
                "field": "invalid",
                "operator": "eq",
                "value": 0
            }
        })
        .to_string(),
    );
    assert_eq!(invalid_scalar["ok"], false);
    assert_eq!(invalid_scalar["error"]["code"], "diagnostic_value_invalid");
    assert_eq!(
        invalid_scalar["error"]["message"],
        "diagnostic numeric values must be finite"
    );
    assert_eq!(
        invalid_scalar["error"]["context"]["diagnostic"],
        json!({
            "reason": "diagnostic numeric values must be finite",
            "resource": "SessionStats",
            "field": "invalid"
        })
    );

    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn marker_counts_distinguish_absence_presence_and_truncated_uncertainty() {
    let (mut app, config) = diagnostic_app("diagnostic-markers");
    let mut stream = connect(&config);

    let absent = send(
        &mut app,
        &mut stream,
        &json!({
            "id": "absent",
            "command": "evaluate_predicate",
            "predicate": {
                "type": "marker_count",
                "marker": "VisibleMarker",
                "max": 0
            }
        })
        .to_string(),
    );
    assert_eq!(absent["result"]["details"]["outcome"], "matched");
    assert_eq!(absent["result"]["details"]["count"], 0);
    assert!(
        absent["result"]["details"]
            .get("count_is_lower_bound")
            .is_none()
    );

    for _ in 0..257 {
        app.world_mut().spawn(VisibleMarker);
    }
    let cases = [
        ("present", Some(1), None, "matched"),
        ("impossible-upper-bound", None, Some(256), "not_matched"),
        ("uncertain-lower-bound", Some(300), None, "indeterminate"),
    ];
    for (id, min, max, outcome) in cases {
        let response = send(
            &mut app,
            &mut stream,
            &json!({
                "id": id,
                "command": "evaluate_predicate",
                "predicate": {
                    "type": "marker_count",
                    "marker": "VisibleMarker",
                    "min": min,
                    "max": max
                }
            })
            .to_string(),
        );
        assert_eq!(response["ok"], true, "case {id}: {response}");
        assert_eq!(response["result"]["details"]["outcome"], outcome);
        assert_eq!(response["result"]["details"]["count"], 257);
        assert_eq!(response["result"]["details"]["count_is_lower_bound"], true);
    }

    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn public_target_errors_preserve_bounded_miss_ambiguity_and_truncation_context() {
    let (mut app, config) = diagnostic_app("diagnostic-target-errors");
    let mut stream = connect(&config);
    let target_request = |id: &str| {
        json!({
            "id": id,
            "command": "target_info",
            "target": {"name": "Duplicate"},
            "kind": "any"
        })
        .to_string()
    };

    let missing = send(&mut app, &mut stream, &target_request("missing"));
    assert_eq!(missing["ok"], false);
    assert_eq!(missing["error"]["code"], "target_not_found");
    assert_eq!(
        missing["error"]["context"]["diagnostic"],
        json!({}),
        "the public context intentionally omits unbounded scan internals"
    );

    let first = app.world_mut().spawn(Name::new("Duplicate")).id();
    let second = app.world_mut().spawn(Name::new("Duplicate")).id();
    let ambiguous = send(&mut app, &mut stream, &target_request("ambiguous"));
    assert_eq!(ambiguous["ok"], false);
    assert_eq!(ambiguous["error"]["code"], "ambiguous_target");
    assert_eq!(
        ambiguous["error"]["context"]["diagnostic"]["candidates"],
        json!([format!("{first:?}"), format!("{second:?}")])
    );

    for _ in 0..255 {
        app.world_mut().spawn(Name::new("Filler"));
    }
    let truncated = send(&mut app, &mut stream, &target_request("truncated"));
    assert_eq!(truncated["ok"], false);
    assert_eq!(truncated["error"]["code"], "target_search_truncated");
    assert_eq!(
        truncated["error"]["context"]["diagnostic"],
        json!({
            "limit": 256,
            "candidates": [format!("{first:?}"), format!("{second:?}")]
        })
    );

    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}
