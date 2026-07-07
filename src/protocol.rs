use crate::{
    config::AgentFeedbackConfig,
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
    Capture,
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
    Capture,
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
) -> Result<AgentRequestBody, String> {
    let request: WireRequest = serde_json::from_str(line).map_err(|error| error.to_string())?;
    let command = match request.command {
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
        WireCommand::Capture => AgentCommand::Capture,
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

    Ok(AgentRequestBody {
        id: request.id,
        command,
    })
}

fn vec2(label: &str, x: f32, y: f32) -> Result<Vec2, String> {
    if x.is_finite() && y.is_finite() {
        Ok(Vec2::new(x, y))
    } else {
        Err(format!("{label} must contain finite coordinates"))
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
        if !value_lower.is_empty()
            && let Some(suggestion) = names.iter().find(|name| {
                let name_lower = name.to_ascii_lowercase();
                name_lower.starts_with(&value_lower) || value_lower.starts_with(&name_lower)
            })
        {
            return Err(format!(
                "invalid {kind} '{value}'; did you mean '{suggestion}'?"
            ));
        }
        return Err(format!("invalid {kind} '{value}'"));
    };
    serde_json::from_value(Value::String((*name).to_string()))
        .map_err(|error| format!("invalid {kind} '{value}'; did you mean '{name}'? ({error})"))
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
        "capture": {},
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
        { "id": 5, "command": "capture" },
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
const KEY_CODE_NAMES: &[&str] = &[
    "Backquote",
    "Backslash",
    "BracketLeft",
    "BracketRight",
    "Comma",
    "Digit0",
    "Digit1",
    "Digit2",
    "Digit3",
    "Digit4",
    "Digit5",
    "Digit6",
    "Digit7",
    "Digit8",
    "Digit9",
    "Equal",
    "IntlBackslash",
    "IntlRo",
    "IntlYen",
    "KeyA",
    "KeyB",
    "KeyC",
    "KeyD",
    "KeyE",
    "KeyF",
    "KeyG",
    "KeyH",
    "KeyI",
    "KeyJ",
    "KeyK",
    "KeyL",
    "KeyM",
    "KeyN",
    "KeyO",
    "KeyP",
    "KeyQ",
    "KeyR",
    "KeyS",
    "KeyT",
    "KeyU",
    "KeyV",
    "KeyW",
    "KeyX",
    "KeyY",
    "KeyZ",
    "Minus",
    "Period",
    "Quote",
    "Semicolon",
    "Slash",
    "AltLeft",
    "AltRight",
    "Backspace",
    "CapsLock",
    "ContextMenu",
    "ControlLeft",
    "ControlRight",
    "Enter",
    "SuperLeft",
    "SuperRight",
    "ShiftLeft",
    "ShiftRight",
    "Space",
    "Tab",
    "Convert",
    "KanaMode",
    "Lang1",
    "Lang2",
    "Lang3",
    "Lang4",
    "Lang5",
    "NonConvert",
    "Delete",
    "End",
    "Help",
    "Home",
    "Insert",
    "PageDown",
    "PageUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
    "ArrowUp",
    "NumLock",
    "Numpad0",
    "Numpad1",
    "Numpad2",
    "Numpad3",
    "Numpad4",
    "Numpad5",
    "Numpad6",
    "Numpad7",
    "Numpad8",
    "Numpad9",
    "NumpadAdd",
    "NumpadBackspace",
    "NumpadClear",
    "NumpadClearEntry",
    "NumpadComma",
    "NumpadDecimal",
    "NumpadDivide",
    "NumpadEnter",
    "NumpadEqual",
    "NumpadHash",
    "NumpadMemoryAdd",
    "NumpadMemoryClear",
    "NumpadMemoryRecall",
    "NumpadMemoryStore",
    "NumpadMemorySubtract",
    "NumpadMultiply",
    "NumpadParenLeft",
    "NumpadParenRight",
    "NumpadStar",
    "NumpadSubtract",
    "Escape",
    "Fn",
    "FnLock",
    "PrintScreen",
    "ScrollLock",
    "Pause",
    "BrowserBack",
    "BrowserFavorites",
    "BrowserForward",
    "BrowserHome",
    "BrowserRefresh",
    "BrowserSearch",
    "BrowserStop",
    "Eject",
    "LaunchApp1",
    "LaunchApp2",
    "LaunchMail",
    "MediaPlayPause",
    "MediaSelect",
    "MediaStop",
    "MediaTrackNext",
    "MediaTrackPrevious",
    "Power",
    "Sleep",
    "AudioVolumeDown",
    "AudioVolumeMute",
    "AudioVolumeUp",
    "WakeUp",
    "Meta",
    "Hyper",
    "Turbo",
    "Abort",
    "Resume",
    "Suspend",
    "Again",
    "Copy",
    "Cut",
    "Find",
    "Open",
    "Paste",
    "Props",
    "Select",
    "Undo",
    "Hiragana",
    "Katakana",
    "F1",
    "F2",
    "F3",
    "F4",
    "F5",
    "F6",
    "F7",
    "F8",
    "F9",
    "F10",
    "F11",
    "F12",
    "F13",
    "F14",
    "F15",
    "F16",
    "F17",
    "F18",
    "F19",
    "F20",
    "F21",
    "F22",
    "F23",
    "F24",
    "F25",
    "F26",
    "F27",
    "F28",
    "F29",
    "F30",
    "F31",
    "F32",
    "F33",
    "F34",
    "F35",
];

#[cfg(test)]
mod tests {
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
        let error = parse_request(r#"{"id":1,"command":"mouse_down","button":"righ"}"#, 10, 10)
            .expect_err("invalid button");

        assert!(error.contains("Right"));
    }

    #[test]
    fn rejects_wait_commands_outside_the_frame_bound() {
        let error = parse_request(r#"{"id":"slow","command":"wait","frames":11}"#, 10, 10)
            .expect_err("frame bound should be enforced");

        assert!(error.contains("frames"));
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

        let protocol: Value = serde_json::from_slice(&fs::read(&config.protocol_file).unwrap())
            .expect("protocol json");
        assert_eq!(protocol["protocol"], PROTOCOL_VERSION);
        assert_eq!(protocol["session_id"], session.session_id);
        assert!(session.heartbeat_file.exists());
        let _ = fs::remove_dir_all(root);
    }
}
