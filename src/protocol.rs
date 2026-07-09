mod commands;
mod discovery;

pub use commands::DiagnosticValue;
#[cfg(test)]
pub(crate) use commands::parse_request;
pub(crate) use commands::{AgentCommand, ParseLimits, Predicate, parse_request_with_limits};
#[cfg(feature = "diagnostics")]
pub(crate) use commands::{ComparisonOperator, TargetKind, TargetSelector};
pub(crate) use discovery::write_protocol_file;

use bevy::{prelude::*, window::WindowMode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StableWindowMode {
    Windowed,
    BorderlessFullscreen,
    Fullscreen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CaptureCompletion {
    ScreenshotCaptured,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct CaptureInfo {
    pub(crate) sequence: u64,
    pub(crate) path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) label: Option<String>,
    pub(crate) requested_frame: u64,
    pub(crate) completed_frame: u64,
    pub(crate) image_width: u32,
    pub(crate) image_height: u32,
    pub(crate) window_at_request: WindowInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) window_at_completion: Option<WindowInfo>,
    pub(crate) completion: CaptureCompletion,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct WindowInfo {
    pub(crate) logical_width: f32,
    pub(crate) logical_height: f32,
    pub(crate) physical_width: u32,
    pub(crate) physical_height: u32,
    pub(crate) scale_factor: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cursor_position: Option<[f32; 2]>,
    pub(crate) focused: bool,
    pub(crate) visible: bool,
    pub(crate) mode: StableWindowMode,
}

impl WindowInfo {
    pub(crate) fn from_window(window: &Window) -> Self {
        Self {
            logical_width: window.width(),
            logical_height: window.height(),
            physical_width: window.physical_width(),
            physical_height: window.physical_height(),
            scale_factor: window.scale_factor(),
            cursor_position: window
                .cursor_position()
                .map(|position| [position.x, position.y]),
            focused: window.focused,
            visible: window.visible,
            mode: match window.mode {
                WindowMode::Windowed => StableWindowMode::Windowed,
                WindowMode::BorderlessFullscreen(_) => StableWindowMode::BorderlessFullscreen,
                WindowMode::Fullscreen(_, _) => StableWindowMode::Fullscreen,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct AgentSnapshot {
    pub(crate) frame: u64,
    pub(crate) game_time_secs: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<WindowInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mouse_position: Option<[f32; 2]>,
    pub(crate) pressed_keys: Vec<String>,
    pub(crate) pressed_buttons: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PredicateOutcome {
    Matched,
    NotMatched,
    Indeterminate,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ObservedPredicate {
    pub(crate) predicate: Predicate,
    pub(crate) outcome: PredicateOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) value: Option<DiagnosticValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) count: Option<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) count_is_lower_bound: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EcsSummaryContext {
    pub(crate) entity_count: usize,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) entity_count_is_lower_bound: bool,
    pub(crate) component_count: usize,
    pub(crate) archetype_count: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct DiagnosticErrorContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) entity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) resource: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) candidates: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) registered: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct AgentErrorContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) latest_capture: Option<CaptureInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) snapshot: Option<AgentSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) observed_predicate: Option<ObservedPredicate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ecs_summary: Option<EcsSummaryContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) timing: Option<AgentTimingContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) diagnostic: Option<DiagnosticErrorContext>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct AgentTimingContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) state: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) requested_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) observed_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) frames: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) expected_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) actual_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) step_count: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct AgentTimingResult {
    pub(crate) start_seconds: f64,
    pub(crate) end_seconds: f64,
    pub(crate) actual_seconds: f64,
    pub(crate) step_count: u16,
}

#[derive(Debug, Serialize)]
pub(crate) struct AgentResponse {
    id: Value,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<AgentResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<AgentError>,
}

#[derive(Debug, Serialize)]
struct AgentResult {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_capture: Option<CaptureInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capture: Option<CaptureInfo>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    snapshot: Option<AgentSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<Value>,
}

#[derive(Debug, Serialize)]
struct AgentError {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<AgentErrorContext>,
}

impl AgentResponse {
    pub(crate) fn ok(
        id: Value,
        status: &'static str,
        latest_capture: Option<CaptureInfo>,
        snapshot: Option<AgentSnapshot>,
    ) -> Self {
        Self::result(id, status, latest_capture, None, snapshot, None)
    }

    pub(crate) fn captured(
        id: Value,
        capture: CaptureInfo,
        snapshot: Option<AgentSnapshot>,
    ) -> Self {
        Self::result(
            id,
            "captured",
            Some(capture.clone()),
            Some(capture),
            snapshot,
            None,
        )
    }

    #[cfg(feature = "diagnostics")]
    pub(crate) fn details(id: Value, status: &'static str, details: Value) -> Self {
        Self::result(id, status, None, None, None, Some(details))
    }
    pub(crate) fn details_with_context(
        id: Value,
        status: &'static str,
        latest_capture: Option<CaptureInfo>,
        snapshot: AgentSnapshot,
        details: Value,
    ) -> Self {
        Self::result(
            id,
            status,
            latest_capture,
            None,
            Some(snapshot),
            Some(details),
        )
    }

    pub(crate) fn timing(
        id: Value,
        latest_capture: Option<CaptureInfo>,
        snapshot: AgentSnapshot,
        timing: AgentTimingResult,
    ) -> Self {
        Self::result(
            id,
            "advanced_time",
            latest_capture,
            None,
            Some(snapshot),
            Some(serde_json::to_value(timing).expect("timing result is serializable")),
        )
    }

    pub(crate) fn error(id: Value, code: &'static str, message: impl Into<String>) -> Self {
        Self::contextual_error(id, code, message, AgentErrorContext::default())
    }

    pub(crate) fn contextual_error(
        id: Value,
        code: &'static str,
        message: impl Into<String>,
        context: AgentErrorContext,
    ) -> Self {
        let context = (context != AgentErrorContext::default()).then_some(context);
        Self {
            id,
            ok: false,
            result: None,
            error: Some(AgentError {
                code,
                message: message.into(),
                context,
            }),
        }
    }

    fn result(
        id: Value,
        status: &'static str,
        latest_capture: Option<CaptureInfo>,
        capture: Option<CaptureInfo>,
        snapshot: Option<AgentSnapshot>,
        details: Option<Value>,
    ) -> Self {
        Self {
            id,
            ok: true,
            result: Some(AgentResult {
                status,
                latest_capture,
                capture,
                snapshot,
                details,
            }),
            error: None,
        }
    }
}

#[cfg(test)]
mod tests;
