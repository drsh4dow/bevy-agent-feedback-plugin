# bevy-agent-feedback-plugin

Local agent feedback for Bevy apps.

This crate lets Pi/Codex drive a running Bevy app through a small JSON-lines TCP protocol. An agent can press keys, press mouse buttons, wait for frames, and capture the primary window as PNGs.

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
{"id":1,"command":"key_down","key":"KeyW"}
{"id":2,"command":"wait","frames":30}
{"id":3,"command":"capture"}
{"id":4,"command":"key_up","key":"KeyW"}
```

Supported commands:

| Command | Fields |
| --- | --- |
| `key_down` | `key`, for example `"KeyW"` |
| `key_up` | `key`, for example `"KeyW"` |
| `mouse_down` | `button`, for example `"Left"` |
| `mouse_up` | `button`, for example `"Left"` |
| `wait` | optional `frames`; defaults to `1` |
| `capture` | none |

Responses echo `id`, set `ok`, and include either `result` or `error`.

## Examples

See [`examples/README.md`](examples/README.md).

- `minimal`: instrumented Bevy app for an external Pi/Codex driver.
- `agent_driven`: self-driving demo using the same TCP protocol.

## Notes

- Keep the socket bound to localhost unless you add your own access control.
- Captures require a graphics-capable environment.
- Agent input is injected before normal `Update` systems read `ButtonInput`.
