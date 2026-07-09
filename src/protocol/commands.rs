mod validation;

use bevy::{input::mouse::MouseScrollUnit, prelude::*};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{path::PathBuf, time::Duration};
use validation::{
    MAX_SELECTOR_BYTES, bounded_frames, bounded_optional, bounded_string, parse_key_code,
    parse_mouse_button, parse_scroll_unit, positive_duration, scalar, validate_capture_label,
    validate_step_count, vec2,
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AgentCommand {
    KeyDown(KeyCode),
    KeyUp(KeyCode),
    MouseDown(MouseButton),
    MouseUp(MouseButton),
    CursorMove {
        position: Vec2,
    },
    MouseMotion {
        delta: Vec2,
    },
    MouseScroll {
        delta: Vec2,
        unit: MouseScrollUnit,
    },
    Text {
        value: String,
    },
    FileHover {
        path: PathBuf,
    },
    FileDrop {
        path: PathBuf,
    },
    FileCancel,
    WindowInfo,
    Wait {
        frames: u16,
    },
    WaitSeconds {
        duration: Duration,
        max_frames: u16,
    },
    AdvanceTime {
        duration: Duration,
        step: Option<Duration>,
    },
    Capture {
        label: Option<String>,
    },
    CaptureAfterFrames {
        frames: u16,
        label: Option<String>,
    },
    ReleaseAllInputs,
    Shutdown,
    Click {
        position: Vec2,
        button: MouseButton,
        frames: u16,
    },
    Drag {
        from: Vec2,
        to: Vec2,
        button: MouseButton,
        steps: u16,
        frames: u16,
    },
    KeyHold {
        key: KeyCode,
        frames: u16,
    },
    TargetInfo {
        target: TargetSelector,
        kind: TargetKind,
        camera: Option<String>,
    },
    ClickTarget {
        target: TargetSelector,
        kind: TargetKind,
        camera: Option<String>,
        button: MouseButton,
        frames: u16,
    },
    ResourceInfo {
        resource: Option<String>,
        field: Option<String>,
    },
    EvaluatePredicate {
        predicate: Predicate,
    },
    WaitFor {
        predicate: Predicate,
        max_frames: u16,
    },
    EcsSummary,
    ListEntities,
    CameraInfo,
    StateInfo,
    MarkerInfo,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TargetKind {
    #[default]
    Any,
    Ui,
    World,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TargetSelector {
    Name(String),
    AccessibilityLabel(String),
    Marker(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ComparisonOperator {
    Eq,
    Ne,
    Lt,
    Lte,
    Gt,
    Gte,
}

impl ComparisonOperator {
    fn is_ordering(self) -> bool {
        matches!(self, Self::Lt | Self::Lte | Self::Gt | Self::Gte)
    }
}

/// Bounded scalar value exposed by registered diagnostic resource fields.
///
/// Runtime diagnostics accept null, booleans, finite numbers, and strings no
/// longer than 1024 UTF-8 bytes. Invalid values are reported as structured
/// diagnostic errors.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DiagnosticValue {
    /// A null diagnostic value.
    Null,
    /// A boolean diagnostic value.
    Bool(bool),
    /// A finite numeric diagnostic value.
    Number(f64),
    /// A bounded UTF-8 diagnostic string.
    String(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Predicate {
    StateEquals {
        state: String,
        value: DiagnosticValue,
    },
    ResourceField {
        resource: String,
        field: String,
        operator: ComparisonOperator,
        value: DiagnosticValue,
    },
    MarkerCount {
        marker: String,
        min: Option<u32>,
        max: Option<u32>,
    },
    TargetExists {
        target: TargetSelector,
        kind: TargetKind,
        camera: Option<String>,
    },
    TargetAbsent {
        target: TargetSelector,
        kind: TargetKind,
        camera: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
struct WireRequest {
    id: Value,
    #[serde(flatten)]
    command: WireCommand,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum WireCommand {
    KeyDown {
        key: String,
    },
    KeyUp {
        key: String,
    },
    MouseDown {
        button: String,
    },
    MouseUp {
        button: String,
    },
    CursorMove {
        x: f32,
        y: f32,
    },
    MouseMotion {
        dx: f32,
        dy: f32,
    },
    MouseScroll {
        #[serde(default)]
        x: f32,
        y: f32,
        unit: Option<String>,
    },
    Text {
        value: String,
    },
    FileHover {
        path: PathBuf,
    },
    FileDrop {
        path: PathBuf,
    },
    FileCancel,
    WindowInfo,
    Wait {
        frames: Option<u16>,
    },
    WaitSeconds {
        seconds: f64,
        max_frames: Option<u16>,
    },
    AdvanceTime {
        seconds: f64,
        step_seconds: Option<f64>,
    },
    Capture {
        label: Option<String>,
    },
    CaptureAfterFrames {
        frames: u16,
        label: Option<String>,
    },
    ReleaseAllInputs,
    Shutdown,
    Click {
        x: f32,
        y: f32,
        button: Option<String>,
        frames: Option<u16>,
    },
    Drag {
        from: [f32; 2],
        to: [f32; 2],
        button: Option<String>,
        steps: Option<u16>,
        frames: Option<u16>,
    },
    Scroll {
        lines: f32,
        #[serde(default)]
        x: f32,
        unit: Option<String>,
    },
    KeyTap {
        key: String,
        frames: Option<u16>,
    },
    KeyHold {
        key: String,
        frames: Option<u16>,
    },
    TargetInfo {
        target: WireTargetSelector,
        #[serde(default)]
        kind: TargetKind,
        camera: Option<String>,
    },
    ClickTarget {
        target: WireTargetSelector,
        #[serde(default)]
        kind: TargetKind,
        camera: Option<String>,
        button: Option<String>,
        frames: Option<u16>,
    },
    ResourceInfo {
        resource: Option<String>,
        field: Option<String>,
    },
    EvaluatePredicate {
        predicate: WirePredicate,
    },
    WaitFor {
        predicate: WirePredicate,
        max_frames: Option<u16>,
    },
    EcsSummary,
    ListEntities,
    CameraInfo,
    StateInfo,
    MarkerInfo,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireTargetSelector {
    name: Option<String>,
    accessibility_label: Option<String>,
    marker: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WirePredicate {
    StateEquals {
        state: String,
        value: Value,
    },
    ResourceField {
        resource: String,
        field: String,
        operator: ComparisonOperator,
        value: Value,
    },
    MarkerCount {
        marker: String,
        min: Option<u32>,
        max: Option<u32>,
    },
    TargetExists {
        target: WireTargetSelector,
        #[serde(default)]
        kind: TargetKind,
        camera: Option<String>,
    },
    TargetAbsent {
        target: WireTargetSelector,
        #[serde(default)]
        kind: TargetKind,
        camera: Option<String>,
    },
}

#[derive(Clone, Copy)]
pub(crate) struct ParseLimits {
    pub(crate) max_wait_frames: u16,
    pub(crate) max_action_steps: u16,
    pub(crate) max_time_advance_steps: u16,
    pub(crate) max_time_advance: Duration,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AgentRequestBody {
    pub(crate) id: Value,
    pub(crate) command: AgentCommand,
}

#[derive(Debug, PartialEq)]
pub(crate) struct ParseRequestError {
    pub(crate) id: Value,
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

pub(crate) fn parse_request_with_limits(
    line: &str,
    limits: ParseLimits,
) -> Result<AgentRequestBody, ParseRequestError> {
    let value = serde_json::from_str::<Value>(line).map_err(|error| ParseRequestError {
        id: Value::Null,
        code: "invalid_request",
        message: error.to_string(),
    })?;
    let id = value.get("id").cloned().unwrap_or(Value::Null);
    let request =
        serde_json::from_value::<WireRequest>(value).map_err(|error| ParseRequestError {
            id,
            code: "invalid_request",
            message: error.to_string(),
        })?;
    let command =
        parse_wire_command(request.command, limits).map_err(|message| ParseRequestError {
            id: request.id.clone(),
            code: "invalid_argument",
            message,
        })?;
    Ok(AgentRequestBody {
        id: request.id,
        command,
    })
}

#[cfg(test)]
pub(crate) fn parse_request(
    line: &str,
    max_wait_frames: u16,
    max_action_steps: u16,
) -> Result<AgentRequestBody, ParseRequestError> {
    parse_request_with_limits(
        line,
        ParseLimits {
            max_wait_frames,
            max_action_steps,
            max_time_advance_steps: 600,
            max_time_advance: Duration::from_secs(10),
        },
    )
}

fn parse_wire_command(command: WireCommand, limits: ParseLimits) -> Result<AgentCommand, String> {
    let command = match command {
        WireCommand::KeyDown { key } => AgentCommand::KeyDown(parse_key_code(&key)?),
        WireCommand::KeyUp { key } => AgentCommand::KeyUp(parse_key_code(&key)?),
        WireCommand::MouseDown { button } => AgentCommand::MouseDown(parse_mouse_button(&button)?),
        WireCommand::MouseUp { button } => AgentCommand::MouseUp(parse_mouse_button(&button)?),
        WireCommand::CursorMove { x, y } => AgentCommand::CursorMove {
            position: vec2("cursor position", x, y)?,
        },
        WireCommand::MouseMotion { dx, dy } => AgentCommand::MouseMotion {
            delta: vec2("mouse motion", dx, dy)?,
        },
        WireCommand::MouseScroll { x, y, unit } => AgentCommand::MouseScroll {
            delta: vec2("mouse scroll", x, y)?,
            unit: parse_scroll_unit(unit.as_deref())?,
        },
        WireCommand::Text { value } => AgentCommand::Text { value },
        WireCommand::FileHover { path } => AgentCommand::FileHover { path },
        WireCommand::FileDrop { path } => AgentCommand::FileDrop { path },
        WireCommand::FileCancel => AgentCommand::FileCancel,
        WireCommand::WindowInfo => AgentCommand::WindowInfo,
        WireCommand::Wait { frames } => AgentCommand::Wait {
            frames: bounded_frames("frames", frames.unwrap_or(1), limits.max_wait_frames)?,
        },
        WireCommand::WaitSeconds {
            seconds,
            max_frames,
        } => AgentCommand::WaitSeconds {
            duration: positive_duration("seconds", seconds)?,
            max_frames: bounded_frames(
                "max_frames",
                max_frames.unwrap_or(limits.max_wait_frames),
                limits.max_wait_frames,
            )?,
        },
        WireCommand::AdvanceTime {
            seconds,
            step_seconds,
        } => {
            let duration = positive_duration("seconds", seconds)?;
            if duration > limits.max_time_advance {
                return Err(format!(
                    "seconds must not exceed {}",
                    limits.max_time_advance.as_secs_f64()
                ));
            }
            let step = step_seconds
                .map(|value| positive_duration("step_seconds", value))
                .transpose()?;
            if let Some(step) = step {
                validate_step_count(duration, step, limits.max_time_advance_steps)?;
            }
            AgentCommand::AdvanceTime { duration, step }
        }
        WireCommand::Capture { label } => AgentCommand::Capture {
            label: validate_capture_label(label)?,
        },
        WireCommand::CaptureAfterFrames { frames, label } => AgentCommand::CaptureAfterFrames {
            frames: bounded_frames("frames", frames, limits.max_wait_frames)?,
            label: validate_capture_label(label)?,
        },
        WireCommand::ReleaseAllInputs => AgentCommand::ReleaseAllInputs,
        WireCommand::Shutdown => AgentCommand::Shutdown,
        WireCommand::Click {
            x,
            y,
            button,
            frames,
        } => AgentCommand::Click {
            position: vec2("click position", x, y)?,
            button: parse_mouse_button(button.as_deref().unwrap_or("Left"))?,
            frames: bounded_frames("frames", frames.unwrap_or(1), limits.max_wait_frames)?,
        },
        WireCommand::Drag {
            from,
            to,
            button,
            steps,
            frames,
        } => {
            let steps =
                bounded_frames("steps", steps.unwrap_or(10), limits.max_action_steps.max(1))?;
            let frames = bounded_frames("frames", frames.unwrap_or(steps), limits.max_wait_frames)?;
            if frames < steps {
                return Err(format!("frames must be >= steps, got {frames} < {steps}"));
            }
            AgentCommand::Drag {
                from: vec2("drag start", from[0], from[1])?,
                to: vec2("drag end", to[0], to[1])?,
                button: parse_mouse_button(button.as_deref().unwrap_or("Left"))?,
                steps,
                frames,
            }
        }
        WireCommand::Scroll { lines, x, unit } => AgentCommand::MouseScroll {
            delta: vec2("scroll", x, lines)?,
            unit: parse_scroll_unit(unit.as_deref())?,
        },
        WireCommand::KeyTap { key, frames } | WireCommand::KeyHold { key, frames } => {
            AgentCommand::KeyHold {
                key: parse_key_code(&key)?,
                frames: bounded_frames("frames", frames.unwrap_or(1), limits.max_wait_frames)?,
            }
        }
        WireCommand::TargetInfo {
            target,
            kind,
            camera,
        } => AgentCommand::TargetInfo {
            target: parse_target(target)?,
            kind,
            camera: bounded_optional("camera", camera, MAX_SELECTOR_BYTES)?,
        },
        WireCommand::ClickTarget {
            target,
            kind,
            camera,
            button,
            frames,
        } => AgentCommand::ClickTarget {
            target: parse_target(target)?,
            kind,
            camera: bounded_optional("camera", camera, MAX_SELECTOR_BYTES)?,
            button: parse_mouse_button(button.as_deref().unwrap_or("Left"))?,
            frames: bounded_frames("frames", frames.unwrap_or(1), limits.max_wait_frames)?,
        },
        WireCommand::ResourceInfo { resource, field } => AgentCommand::ResourceInfo {
            resource: bounded_optional("resource", resource, MAX_SELECTOR_BYTES)?,
            field: bounded_optional("field", field, MAX_SELECTOR_BYTES)?,
        },
        WireCommand::EvaluatePredicate { predicate } => AgentCommand::EvaluatePredicate {
            predicate: parse_predicate(predicate)?,
        },
        WireCommand::WaitFor {
            predicate,
            max_frames,
        } => AgentCommand::WaitFor {
            predicate: parse_predicate(predicate)?,
            max_frames: bounded_frames(
                "max_frames",
                max_frames.unwrap_or(limits.max_wait_frames),
                limits.max_wait_frames,
            )?,
        },
        WireCommand::EcsSummary => AgentCommand::EcsSummary,
        WireCommand::ListEntities => AgentCommand::ListEntities,
        WireCommand::CameraInfo => AgentCommand::CameraInfo,
        WireCommand::StateInfo => AgentCommand::StateInfo,
        WireCommand::MarkerInfo => AgentCommand::MarkerInfo,
    };
    Ok(command)
}

fn parse_target(target: WireTargetSelector) -> Result<TargetSelector, String> {
    match (target.name, target.accessibility_label, target.marker) {
        (Some(value), None, None) => Ok(TargetSelector::Name(bounded_string(
            "target.name",
            value,
            MAX_SELECTOR_BYTES,
        )?)),
        (None, Some(value), None) => Ok(TargetSelector::AccessibilityLabel(bounded_string(
            "target.accessibility_label",
            value,
            MAX_SELECTOR_BYTES,
        )?)),
        (None, None, Some(value)) => Ok(TargetSelector::Marker(bounded_string(
            "target.marker",
            value,
            MAX_SELECTOR_BYTES,
        )?)),
        _ => Err(
            "target must contain exactly one of name, accessibility_label, or marker".to_string(),
        ),
    }
}

fn parse_predicate(predicate: WirePredicate) -> Result<Predicate, String> {
    match predicate {
        WirePredicate::StateEquals { state, value } => Ok(Predicate::StateEquals {
            state: bounded_string("state", state, MAX_SELECTOR_BYTES)?,
            value: scalar("value", value)?,
        }),
        WirePredicate::ResourceField {
            resource,
            field,
            operator,
            value,
        } => {
            let value = scalar("value", value)?;
            if operator.is_ordering() && !matches!(value, DiagnosticValue::Number(_)) {
                return Err("ordering comparisons require a numeric value".to_string());
            }
            Ok(Predicate::ResourceField {
                resource: bounded_string("resource", resource, MAX_SELECTOR_BYTES)?,
                field: bounded_string("field", field, MAX_SELECTOR_BYTES)?,
                operator,
                value,
            })
        }
        WirePredicate::MarkerCount { marker, min, max } => {
            if min.is_none() && max.is_none() {
                return Err("marker_count requires min, max, or both".to_string());
            }
            if let (Some(min), Some(max)) = (min, max)
                && min > max
            {
                return Err(format!(
                    "marker_count min must be <= max, got {min} > {max}"
                ));
            }
            Ok(Predicate::MarkerCount {
                marker: bounded_string("marker", marker, MAX_SELECTOR_BYTES)?,
                min,
                max,
            })
        }
        WirePredicate::TargetExists {
            target,
            kind,
            camera,
        } => Ok(Predicate::TargetExists {
            target: parse_target(target)?,
            kind,
            camera: bounded_optional("camera", camera, MAX_SELECTOR_BYTES)?,
        }),
        WirePredicate::TargetAbsent {
            target,
            kind,
            camera,
        } => Ok(Predicate::TargetAbsent {
            target: parse_target(target)?,
            kind,
            camera: bounded_optional("camera", camera, MAX_SELECTOR_BYTES)?,
        }),
    }
}
