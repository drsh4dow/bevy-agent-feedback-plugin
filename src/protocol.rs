use crate::{
    config::AgentFeedbackConfig,
    key_names::KEY_CODE_NAMES,
    session::{AgentFeedbackSession, PROTOCOL_VERSION},
};
use bevy::{input::mouse::MouseScrollUnit, prelude::*};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{fs, io, net::SocketAddr, path::PathBuf};

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
    Capture {
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
    EcsSummary,
    ListEntities,
    CameraInfo,
    StateInfo,
    MarkerInfo,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct CaptureInfo {
    pub(crate) sequence: u64,
    pub(crate) path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) label: Option<String>,
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
    Capture {
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
    EcsSummary,
    ListEntities,
    CameraInfo,
    StateInfo,
    MarkerInfo,
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

    pub(crate) fn error(id: Value, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(AgentError {
                code,
                message: message.into(),
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

pub(crate) fn parse_request(
    line: &str,
    max_wait_frames: u16,
    max_action_steps: u16,
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
    let command = parse_wire_command(request.command, max_wait_frames, max_action_steps).map_err(
        |message| ParseRequestError {
            id: request.id.clone(),
            code: "invalid_argument",
            message,
        },
    )?;

    Ok(AgentRequestBody {
        id: request.id,
        command,
    })
}

fn parse_wire_command(
    command: WireCommand,
    max_wait_frames: u16,
    max_action_steps: u16,
) -> Result<AgentCommand, String> {
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
        WireCommand::Capture { label } => AgentCommand::Capture {
            label: validate_capture_label(label)?,
        },
        WireCommand::ReleaseAllInputs => AgentCommand::ReleaseAllInputs,
        WireCommand::Shutdown => AgentCommand::Shutdown,
        WireCommand::Wait { frames } => AgentCommand::Wait {
            frames: bounded_frames("frames", frames.unwrap_or(1), max_wait_frames)?,
        },
        WireCommand::Click {
            x,
            y,
            button,
            frames,
        } => AgentCommand::Click {
            position: vec2("click position", x, y)?,
            button: parse_mouse_button(button.as_deref().unwrap_or("Left"))?,
            frames: bounded_frames("frames", frames.unwrap_or(1), max_wait_frames)?,
        },
        WireCommand::Drag {
            from,
            to,
            button,
            steps,
            frames,
        } => {
            let steps = bounded_frames("steps", steps.unwrap_or(10), max_action_steps.max(1))?;
            let frames = bounded_frames("frames", frames.unwrap_or(steps), max_wait_frames)?;
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
        WireCommand::KeyTap { key, frames } => AgentCommand::KeyHold {
            key: parse_key_code(&key)?,
            frames: bounded_frames("frames", frames.unwrap_or(1), max_wait_frames)?,
        },
        WireCommand::KeyHold { key, frames } => AgentCommand::KeyHold {
            key: parse_key_code(&key)?,
            frames: bounded_frames("frames", frames.unwrap_or(1), max_wait_frames)?,
        },
        WireCommand::EcsSummary => AgentCommand::EcsSummary,
        WireCommand::ListEntities => AgentCommand::ListEntities,
        WireCommand::CameraInfo => AgentCommand::CameraInfo,
        WireCommand::StateInfo => AgentCommand::StateInfo,
        WireCommand::MarkerInfo => AgentCommand::MarkerInfo,
    };

    Ok(command)
}

fn vec2(label: &str, x: f32, y: f32) -> Result<Vec2, String> {
    if x.is_finite() && y.is_finite() {
        Ok(Vec2::new(x, y))
    } else {
        Err(format!("{label} must contain finite coordinates"))
    }
}

fn validate_capture_label(label: Option<String>) -> Result<Option<String>, String> {
    let Some(label) = label else {
        return Ok(None);
    };
    let valid = (1..=40).contains(&label.len())
        && label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    if valid {
        Ok(Some(label))
    } else {
        Err("capture label must match [A-Za-z0-9_-]{1,40}".to_string())
    }
}

fn bounded_frames(label: &str, value: u16, max: u16) -> Result<u16, String> {
    if value == 0 || value > max {
        return Err(format!("{label} must be between 1 and {max}, got {value}"));
    }
    Ok(value)
}

fn parse_key_code(value: &str) -> Result<KeyCode, String> {
    parse_named("key", value, KEY_CODE_NAMES)
}

fn parse_mouse_button(value: &str) -> Result<MouseButton, String> {
    parse_named("button", value, MOUSE_BUTTON_NAMES)
}

fn parse_scroll_unit(value: Option<&str>) -> Result<MouseScrollUnit, String> {
    match value {
        Some(value) => parse_named("scroll unit", value, MOUSE_SCROLL_UNIT_NAMES),
        None => Ok(MouseScrollUnit::Line),
    }
}

fn parse_named<T: DeserializeOwned>(kind: &str, value: &str, names: &[&str]) -> Result<T, String> {
    let Some(name) = names.iter().find(|name| name.eq_ignore_ascii_case(value)) else {
        let value_lower = value.to_ascii_lowercase();
        let mut best = None;
        for name in names {
            let name_lower = name.to_ascii_lowercase();
            if let Some(distance) = edit_distance_with_cutoff(&value_lower, &name_lower, 2)
                && best.is_none_or(|(best_distance, _)| distance < best_distance)
            {
                best = Some((distance, *name));
            }
        }
        if let Some((_, suggestion)) = best {
            return Err(format!(
                "invalid {kind} '{value}'; did you mean '{suggestion}'?"
            ));
        }
        return Err(format!("invalid {kind} '{value}'"));
    };
    serde_json::from_value(Value::String((*name).to_string()))
        .map_err(|error| format!("invalid {kind} '{value}'; did you mean '{name}'? ({error})"))
}

fn edit_distance_with_cutoff(left: &str, right: &str, cutoff: usize) -> Option<usize> {
    if left.len().abs_diff(right.len()) > cutoff {
        return None;
    }

    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0; right.len() + 1];
    for (left_index, left_byte) in left.bytes().enumerate() {
        current[0] = left_index + 1;
        let mut row_minimum = current[0];
        for (right_index, right_byte) in right.bytes().enumerate() {
            let substitution = previous[right_index] + usize::from(left_byte != right_byte);
            let insertion = current[right_index] + 1;
            let deletion = previous[right_index + 1] + 1;
            let distance = substitution.min(insertion).min(deletion);
            current[right_index + 1] = distance;
            row_minimum = row_minimum.min(distance);
        }
        if row_minimum > cutoff {
            return None;
        }
        std::mem::swap(&mut previous, &mut current);
    }

    (previous[right.len()] <= cutoff).then_some(previous[right.len()])
}

pub(crate) fn write_protocol_file(
    config: &AgentFeedbackConfig,
    session: &AgentFeedbackSession,
    socket_addr: SocketAddr,
) -> io::Result<()> {
    if let Some(parent) = config.protocol_file.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir_all(&config.capture_dir)?;
    session.write_heartbeat()?;

    // Split large JSON blocks to keep serde_json::json! below its recursion limit.
    let commands = json!({
        "key_down": { "key": "case-insensitive Bevy KeyCode string, e.g. KeyW" },
        "key_up": { "key": "case-insensitive Bevy KeyCode string, e.g. KeyW" },
        "mouse_down": { "button": "case-insensitive MouseButton string, e.g. Left" },
        "mouse_up": { "button": "case-insensitive MouseButton string, e.g. Left" },
        "cursor_move": { "x": "logical pixels", "y": "logical pixels" },
        "mouse_motion": { "dx": "raw motion delta", "dy": "raw motion delta" },
        "mouse_scroll": { "x": "horizontal scroll", "y": "vertical scroll", "unit": "Line or Pixel; default Line" },
        "scroll": { "lines": "vertical line delta", "x": "optional horizontal line delta" },
        "click": { "x": "logical pixels", "y": "logical pixels", "button": "default Left", "frames": "press duration" },
        "drag": { "from": "[x,y]", "to": "[x,y]", "button": "default Left", "steps": format!("1..={}", config.max_action_steps.max(1)), "frames": format!("steps..={}", config.max_wait_frames) },
        "key_tap": { "key": "case-insensitive Bevy KeyCode string", "frames": "press duration" },
        "key_hold": { "key": "case-insensitive Bevy KeyCode string", "frames": "hold duration" },
        "release_all_inputs": {},
        "shutdown": {},
        "text": { "value": "UTF-8 text committed through Bevy Ime" },
        "file_hover": { "path": "path string" },
        "file_drop": { "path": "path string" },
        "file_cancel": {},
        "window_info": {},
        "wait": { "frames": format!("1..={}", config.max_wait_frames) },
        "capture": { "label": "optional [A-Za-z0-9_-]{1,40}" },
        "ecs_summary": { "requires": "diagnostics feature and AgentFeedbackDiagnosticsPlugin" },
        "list_entities": { "requires": "diagnostics feature and AgentFeedbackDiagnosticsPlugin" },
        "camera_info": { "requires": "diagnostics feature and AgentFeedbackDiagnosticsPlugin" },
        "state_info": { "requires": "diagnostics feature and AgentFeedbackDiagnosticsPlugin" },
        "marker_info": { "requires": "diagnostics feature and AgentFeedbackDiagnosticsPlugin::with_marker::<T>()" }
    });
    let examples = json!([
        { "id": 1, "command": "window_info" },
        { "id": 2, "command": "click", "x": 320.0, "y": 240.0, "button": "left" },
        { "id": 3, "command": "drag", "from": [320.0, 240.0], "to": [420.0, 240.0], "button": "Right", "steps": 5, "frames": 5 },
        { "id": 4, "command": "key_tap", "key": "keyw" },
        { "id": 5, "command": "capture", "label": "default" },
        { "id": 6, "command": "release_all_inputs" },
        { "id": 7, "command": "marker_info" },
        { "id": 8, "command": "shutdown" }
    ]);
    let protocol = json!({
        "protocol": PROTOCOL_VERSION,
        "session_id": session.session_id,
        "pid": session.pid,
        "started_at_unix_ms": session.started_at_unix_ms,
        "heartbeat_file": session.heartbeat_file.to_string_lossy(),
        "heartbeat_interval_ms": session.heartbeat_interval.as_millis(),
        "stale_after_ms": session.stale_after.as_millis(),
        "socket_addr": socket_addr.to_string(),
        "transport": "json-lines-over-tcp",
        "clients": "single local client at a time",
        "coordinates": "logical window pixels, origin at the top-left of the primary window",
        "capture_dir": config.capture_dir.to_string_lossy(),
        "command_timeout_ms": config.command_timeout.as_millis(),
        "max_action_steps": config.max_action_steps,
        "commands": commands,
        "examples": examples,
    });
    let bytes = serde_json::to_vec_pretty(&protocol).map_err(io::Error::other)?;
    fs::write(&config.protocol_file, bytes)
}

const MOUSE_BUTTON_NAMES: &[&str] = &["Left", "Right", "Middle", "Back", "Forward"];
const MOUSE_SCROLL_UNIT_NAMES: &[&str] = &["Line", "Pixel"];
#[cfg(test)]
mod tests;
