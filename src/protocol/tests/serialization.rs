use super::super::*;
use serde_json::{Value, json};

#[test]
fn serializes_full_structured_error_context() {
    let window = WindowInfo {
        logical_width: 640.0,
        logical_height: 360.0,
        physical_width: 1280,
        physical_height: 720,
        scale_factor: 2.0,
        cursor_position: Some([12.5, 8.0]),
        focused: true,
        visible: true,
        mode: StableWindowMode::Windowed,
    };
    let response = AgentResponse::contextual_error(
        Value::from("timed-out"),
        "predicate_timeout",
        "predicate did not match",
        AgentErrorContext {
            latest_capture: Some(CaptureInfo {
                sequence: 4,
                path: "captures/frame-4.png".to_string(),
                label: Some("failure".to_string()),
                requested_frame: 40,
                completed_frame: 42,
                image_width: 1280,
                image_height: 720,
                window_at_request: window.clone(),
                window_at_completion: Some(window),
                completion: CaptureCompletion::ScreenshotCaptured,
            }),
            snapshot: Some(AgentSnapshot {
                frame: 42,
                game_time_secs: 1.25,
                window: None,
                mouse_position: None,
                pressed_keys: Vec::new(),
                pressed_buttons: Vec::new(),
            }),
            observed_predicate: Some(ObservedPredicate {
                predicate: Predicate::MarkerCount {
                    marker: "Enemy".to_string(),
                    min: Some(5),
                    max: None,
                },
                outcome: PredicateOutcome::Indeterminate,
                value: None,
                count: Some(3),
                count_is_lower_bound: true,
            }),
            ecs_summary: Some(EcsSummaryContext {
                entity_count: 100,
                entity_count_is_lower_bound: true,
                component_count: 12,
                archetype_count: 5,
            }),
            timing: Some(AgentTimingContext {
                state: Some("frozen"),
                reason: Some("frame_budget_exhausted"),
                ..Default::default()
            }),
            diagnostic: Some(DiagnosticErrorContext {
                reason: Some("target_search_truncated".to_string()),
                limit: Some(100),
                registered: vec!["Score".to_string()],
                ..Default::default()
            }),
        },
    );
    let value = serde_json::to_value(response).expect("structured error serialization");
    assert_eq!(
        &value,
        &json!({
            "id": "timed-out",
            "ok": false,
            "error": {
                "code": "predicate_timeout",
                "message": "predicate did not match",
                "context": {
                    "latest_capture": {
                        "sequence": 4, "path": "captures/frame-4.png", "label": "failure",
                        "requested_frame": 40, "completed_frame": 42,
                        "image_width": 1280, "image_height": 720,
                        "window_at_request": {
                            "logical_width": 640.0, "logical_height": 360.0,
                            "physical_width": 1280, "physical_height": 720,
                            "scale_factor": 2.0, "cursor_position": [12.5, 8.0],
                            "focused": true, "visible": true, "mode": "windowed"
                        },
                        "window_at_completion": {
                            "logical_width": 640.0, "logical_height": 360.0,
                            "physical_width": 1280, "physical_height": 720,
                            "scale_factor": 2.0, "cursor_position": [12.5, 8.0],
                            "focused": true, "visible": true, "mode": "windowed"
                        },
                        "completion": "screenshot_captured"
                    },
                    "snapshot": {
                        "frame": 42, "game_time_secs": 1.25,
                        "pressed_keys": [], "pressed_buttons": []
                    },
                    "observed_predicate": {
                        "predicate": {
                            "type": "marker_count", "marker": "Enemy", "min": 5, "max": null
                        },
                        "outcome": "indeterminate", "count": 3, "count_is_lower_bound": true
                    },
                    "ecs_summary": {
                        "entity_count": 100, "entity_count_is_lower_bound": true,
                        "component_count": 12, "archetype_count": 5
                    },
                    "timing": {"state": "frozen", "reason": "frame_budget_exhausted"},
                    "diagnostic": {
                        "reason": "target_search_truncated",
                        "limit": 100, "registered": ["Score"]
                    }
                }
            }
        })
    );
}
