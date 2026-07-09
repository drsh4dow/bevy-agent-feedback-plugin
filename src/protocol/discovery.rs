use crate::{
    config::AgentFeedbackConfig,
    session::{AgentFeedbackSession, PROTOCOL_VERSION},
};
use serde_json::json;
use std::{fs, io, net::SocketAddr};

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

    let diagnostics_requirement = "diagnostics feature and AgentFeedbackDiagnosticsPlugin";
    let selector = json!({
        "exactly_one": ["name", "accessibility_label", "marker"],
        "string_bytes": "1..=128",
    });
    let predicates = json!({
        "discriminator": "type: state_equals|resource_field|marker_count|target_exists|target_absent",
        "state_equals": { "state": "exact 1..=128 byte key", "value": "bounded scalar" },
        "resource_field": { "resource": "exact 1..=128 byte key", "field": "exact 1..=128 byte key", "operator": "eq|ne|lt|lte|gt|gte", "value": "bounded scalar; ordering requires a number" },
        "marker_count": { "marker": "exact 1..=128 byte key", "min": "optional u32", "max": "optional u32; at least one bound is required" },
        "target_exists": { "target": selector.clone(), "kind": "any|ui|world; default any", "camera": "optional exact 1..=128 byte name" },
        "target_absent": { "target": selector.clone(), "kind": "any|ui|world; default any", "camera": "optional exact 1..=128 byte name" },
        "scalar": "null, boolean, finite number, or UTF-8 string of 1..=1024 bytes",
    });
    let commands = json!({
        "key_down": { "key": "case-insensitive Bevy KeyCode string, e.g. KeyW" },
        "key_up": { "key": "case-insensitive Bevy KeyCode string, e.g. KeyW" },
        "mouse_down": { "button": "case-insensitive MouseButton string, e.g. Left" },
        "mouse_up": { "button": "case-insensitive MouseButton string, e.g. Left" },
        "cursor_move": { "x": "logical pixels", "y": "logical pixels" },
        "mouse_motion": { "dx": "raw motion delta", "dy": "raw motion delta" },
        "mouse_scroll": { "x": "horizontal scroll", "y": "vertical scroll", "unit": "Line or Pixel; default Line" },
        "scroll": { "lines": "vertical line delta", "x": "optional horizontal line delta" },
        "click": { "x": "logical pixels", "y": "logical pixels", "button": "default Left", "frames": format!("1..={}; default 1", config.max_wait_frames) },
        "drag": { "from": "[x,y]", "to": "[x,y]", "button": "default Left", "steps": format!("1..={}; default 10", config.max_action_steps.max(1)), "frames": format!("steps..={}; default steps", config.max_wait_frames) },
        "key_tap": { "key": "case-insensitive Bevy KeyCode string", "frames": format!("1..={}; default 1", config.max_wait_frames) },
        "key_hold": { "key": "case-insensitive Bevy KeyCode string", "frames": format!("1..={}; default 1", config.max_wait_frames) },
        "release_all_inputs": {},
        "shutdown": {},
        "text": { "value": "UTF-8 text committed through Bevy Ime" },
        "file_hover": { "path": "path string" },
        "file_drop": { "path": "path string" },
        "file_cancel": {},
        "window_info": {},
        "wait": { "frames": format!("1..={}; default 1", config.max_wait_frames) },
        "wait_seconds": { "seconds": "positive finite f64 converted to nonzero duration", "max_frames": format!("1..={}; default {}", config.max_wait_frames, config.max_wait_frames) },
        "advance_time": { "seconds": format!("positive finite duration <= {} seconds", config.max_time_advance.as_secs_f64()), "step_seconds": format!("optional positive finite duration; ceil(seconds/step_seconds) <= {}; default Time<Fixed>::timestep or 1/60", config.max_time_advance_steps.max(1)) },
        "capture": { "label": "optional [A-Za-z0-9_-]{1,40}" },
        "capture_after_frames": { "frames": format!("required 1..={}", config.max_wait_frames), "label": "optional [A-Za-z0-9_-]{1,40}" },
        "target_info": { "target": selector.clone(), "kind": "any|ui|world; default any", "camera": "optional exact 1..=128 byte name", "requires": diagnostics_requirement },
        "click_target": { "target": selector, "kind": "any|ui|world; default any", "camera": "optional exact 1..=128 byte name", "button": "default Left", "frames": format!("1..={}; default 1", config.max_wait_frames), "requires": diagnostics_requirement },
        "resource_info": { "resource": "optional exact 1..=128 byte key", "field": "optional exact 1..=128 byte key", "requires": diagnostics_requirement },
        "evaluate_predicate": { "predicate": predicates.clone(), "requires": diagnostics_requirement },
        "wait_for": { "predicate": predicates, "max_frames": format!("1..={}; default {}", config.max_wait_frames, config.max_wait_frames), "requires": diagnostics_requirement },
        "ecs_summary": { "requires": diagnostics_requirement },
        "list_entities": { "requires": diagnostics_requirement },
        "camera_info": { "requires": diagnostics_requirement },
        "state_info": { "requires": diagnostics_requirement },
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
        "deterministic_time": config.deterministic_time,
        "max_action_steps": config.max_action_steps,
        "max_wait_frames": config.max_wait_frames,
        "max_time_advance_steps": config.max_time_advance_steps.max(1),
        "max_time_advance_seconds": config.max_time_advance.as_secs_f64(),
        "window_modes": ["windowed", "borderless_fullscreen", "fullscreen"],
        "capture_completion": "screenshot_captured",
        "commands": commands,
        "examples": examples,
    });
    let bytes = serde_json::to_vec_pretty(&protocol).map_err(io::Error::other)?;
    fs::write(&config.protocol_file, bytes)
}
