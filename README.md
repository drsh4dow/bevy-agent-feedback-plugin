# bevy-agent-feedback-plugin

Local agent feedback for Bevy apps.

This crate lets Pi/Codex drive a running Bevy app through a small JSON-lines TCP protocol. An agent can press keys, move/click/drag/scroll the mouse, submit text and file-drop events, query primary-window coordinates, wait for frames, and capture the primary window as PNGs.

## Quick Start

```rust
use bevy::prelude::*;
use bevy_agent_feedback_plugin::{AgentFeedbackConfig, AgentFeedbackPlugin};
use std::{net::SocketAddr, path::PathBuf};

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(AgentFeedbackPlugin::new(AgentFeedbackConfig {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            protocol_file: PathBuf::from("target/agent-feedback/agent-feedback.json"),
            capture_dir: PathBuf::from("target/agent-feedback/captures"),
            ..Default::default()
        }))
        .run();
}
```

Use port `0` for examples and tests. The plugin writes the chosen socket address to the protocol file.

## Pi/Codex Workflow

1. Run the Bevy app.
2. Read the configured protocol file.
3. Connect to `socket_addr`.
4. Send one newline-terminated JSON request per command.
5. Read one newline-terminated JSON response per request.
6. Inspect `result.capture.path` after `capture`.

Example request sequence:

```jsonl
{"id":1,"command":"window_info"}
{"id":2,"command":"cursor_move","x":320,"y":240}
{"id":3,"command":"mouse_down","button":"Left"}
{"id":4,"command":"wait","frames":3}
{"id":5,"command":"mouse_up","button":"Left"}
{"id":6,"command":"capture"}
```

Supported commands:

| Command | Fields |
| --- | --- |
| `key_down` | `key`, for example `"KeyW"` |
| `key_up` | `key`, for example `"KeyW"` |
| `mouse_down` | `button`, for example `"Left"` |
| `mouse_up` | `button`, for example `"Left"` |
| `cursor_move` | `x`, `y` in logical primary-window pixels |
| `mouse_motion` | `dx`, `dy` raw motion delta |
| `mouse_scroll` | `y`, optional `x`, optional `unit` (`"Line"` or `"Pixel"`) |
| `text` | `value`, committed through Bevy `Ime` |
| `file_hover` | `path` |
| `file_drop` | `path` |
| `file_cancel` | none |
| `window_info` | none |
| `wait` | optional `frames`; defaults to `1` |
| `capture` | none |

Valid responses echo `id`, set `ok`, and include either `result` or `error`; malformed requests may return `id: null`. Window-aware responses include logical size, physical size, scale factor, and cursor position so agents can convert between screenshots and Bevy logical coordinates. Keyboard commands target physical `KeyCode` input; apps should read `ButtonInput<KeyCode>` or `KeyboardInput.key_code`. Compose click as `cursor_move`, `mouse_down`, `wait` 1 frame, `mouse_up`; compose drag by inserting more `cursor_move` steps before `mouse_up`.

## Examples

See [`examples/README.md`](examples/README.md).

- `minimal`: instrumented Bevy app for an external Pi/Codex driver.
- `agent_driven`: self-driving demo using the same TCP protocol.

## Notes

- Keep the socket bound to localhost unless you add your own access control.
- Captures require a graphics-capable environment.
- Agent input is injected before normal `Update` systems read `ButtonInput`.
