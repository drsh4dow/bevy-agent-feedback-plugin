use crate::config::AgentFeedbackConfig;
use bevy::{input::mouse::MouseScrollUnit, prelude::*};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fs, io, net::SocketAddr, path::PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AgentCommand {
    KeyDown(KeyCode),
    KeyUp(KeyCode),
    MouseDown(MouseButton),
    MouseUp(MouseButton),
    CursorMove { position: Vec2 },
    MouseMotion { delta: Vec2 },
    MouseScroll { delta: Vec2, unit: MouseScrollUnit },
    Text { value: String },
    FileHover { path: PathBuf },
    FileDrop { path: PathBuf },
    FileCancel,
    WindowInfo,
    Wait { frames: u16 },
    Capture,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct CaptureInfo {
    pub(crate) sequence: u64,
    pub(crate) path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
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
        key: KeyCode,
    },
    KeyUp {
        key: KeyCode,
    },
    MouseDown {
        button: MouseButton,
    },
    MouseUp {
        button: MouseButton,
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
        unit: Option<MouseScrollUnit>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    window: Option<WindowInfo>,
}

#[derive(Debug, Serialize)]
struct AgentError {
    code: &'static str,
    message: String,
}

impl AgentResponse {
    pub(crate) fn ok(id: Value, status: &'static str, latest_capture: Option<CaptureInfo>) -> Self {
        Self::result(id, status, latest_capture, None, None)
    }

    pub(crate) fn ok_with_window(
        id: Value,
        status: &'static str,
        latest_capture: Option<CaptureInfo>,
        window: WindowInfo,
    ) -> Self {
        Self::result(id, status, latest_capture, None, Some(window))
    }

    pub(crate) fn captured(id: Value, capture: CaptureInfo, window: Option<WindowInfo>) -> Self {
        Self::result(id, "captured", Some(capture.clone()), Some(capture), window)
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
        window: Option<WindowInfo>,
    ) -> Self {
        Self {
            id,
            ok: true,
            result: Some(AgentResult {
                status,
                latest_capture,
                capture,
                window,
            }),
            error: None,
        }
    }
}

pub(crate) fn parse_request(line: &str, max_wait_frames: u16) -> Result<AgentRequestBody, String> {
    let request: WireRequest = serde_json::from_str(line).map_err(|error| error.to_string())?;
    let command = match request.command {
        WireCommand::KeyDown { key } => AgentCommand::KeyDown(key),
        WireCommand::KeyUp { key } => AgentCommand::KeyUp(key),
        WireCommand::MouseDown { button } => AgentCommand::MouseDown(button),
        WireCommand::MouseUp { button } => AgentCommand::MouseUp(button),
        WireCommand::CursorMove { x, y } => AgentCommand::CursorMove {
            position: vec2("cursor position", x, y)?,
        },
        WireCommand::MouseMotion { dx, dy } => AgentCommand::MouseMotion {
            delta: vec2("mouse motion", dx, dy)?,
        },
        WireCommand::MouseScroll { x, y, unit } => AgentCommand::MouseScroll {
            delta: vec2("mouse scroll", x, y)?,
            unit: unit.unwrap_or(MouseScrollUnit::Line),
        },
        WireCommand::Text { value } => AgentCommand::Text { value },
        WireCommand::FileHover { path } => AgentCommand::FileHover { path },
        WireCommand::FileDrop { path } => AgentCommand::FileDrop { path },
        WireCommand::FileCancel => AgentCommand::FileCancel,
        WireCommand::WindowInfo => AgentCommand::WindowInfo,
        WireCommand::Capture => AgentCommand::Capture,
        WireCommand::Wait { frames } => {
            let frames = frames.unwrap_or(1);
            if frames == 0 || frames > max_wait_frames {
                return Err(format!(
                    "frames must be between 1 and {max_wait_frames}, got {frames}"
                ));
            }
            AgentCommand::Wait { frames }
        }
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

pub(crate) fn write_protocol_file(
    config: &AgentFeedbackConfig,
    socket_addr: SocketAddr,
) -> io::Result<()> {
    if let Some(parent) = config.protocol_file.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir_all(&config.capture_dir)?;

    let protocol = json!({
        "protocol": "bevy-agent-feedback/1",
        "socket_addr": socket_addr.to_string(),
        "transport": "json-lines-over-tcp",
        "clients": "single local client at a time",
        "coordinates": "logical window pixels, origin at the top-left of the primary window",
        "capture_dir": config.capture_dir.to_string_lossy(),
        "command_timeout_ms": config.command_timeout.as_millis(),
        "commands": {
            "key_down": { "key": "Bevy KeyCode string, e.g. KeyW; read via ButtonInput<KeyCode>" },
            "key_up": { "key": "Bevy KeyCode string, e.g. KeyW; read via ButtonInput<KeyCode>" },
            "mouse_down": { "button": "MouseButton string, e.g. Left; pair with wait and mouse_up for click/drag" },
            "mouse_up": { "button": "MouseButton string, e.g. Left; releases a prior mouse_down" },
            "cursor_move": { "x": "logical pixels", "y": "logical pixels" },
            "mouse_motion": { "dx": "raw motion delta", "dy": "raw motion delta" },
            "mouse_scroll": { "x": "horizontal scroll", "y": "vertical scroll", "unit": "Line or Pixel; default Line" },
            "text": { "value": "UTF-8 text committed through Bevy Ime" },
            "file_hover": { "path": "path string" },
            "file_drop": { "path": "path string" },
            "file_cancel": {},
            "window_info": {},
            "wait": { "frames": format!("1..={}", config.max_wait_frames) },
            "capture": {}
        },
        "examples": [
            { "id": 1, "command": "window_info" },
            { "id": 2, "command": "cursor_move", "x": 320.0, "y": 240.0 },
            { "id": 3, "command": "mouse_down", "button": "Left" },
            { "id": 4, "command": "wait", "frames": 3 },
            { "id": 5, "command": "mouse_up", "button": "Left" },
            { "id": 6, "command": "text", "value": "hello" },
            { "id": 7, "command": "capture" }
        ]
    });
    let bytes = serde_json::to_vec_pretty(&protocol).map_err(io::Error::other)?;
    fs::write(&config.protocol_file, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cursor_move_command() {
        let request = parse_request(r#"{"id":1,"command":"cursor_move","x":12,"y":34}"#, 10)
            .expect("valid request");

        assert_eq!(request.id, Value::from(1));
        assert_eq!(
            request.command,
            AgentCommand::CursorMove {
                position: Vec2::new(12.0, 34.0)
            }
        );
    }

    #[test]
    fn rejects_wait_commands_outside_the_frame_bound() {
        let error = parse_request(r#"{"id":"slow","command":"wait","frames":11}"#, 10)
            .expect_err("frame bound should be enforced");

        assert!(error.contains("frames"));
    }
}
