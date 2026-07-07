# bevy-agent-feedback-plugin

Local agent feedback for Bevy apps.

This crate lets Pi/Codex drive a running Bevy app through a v2 JSON-lines TCP protocol. Agents can press keys, move/click/drag/scroll the mouse, submit text/file-drop events, query window metadata, wait for frames, capture PNG screenshots, replay transcripts, and shut the app down cleanly.

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

Use port `0` for examples and tests. The plugin writes a self-describing protocol file with the chosen socket, session heartbeat, capture directory, commands, and examples.

## Drive it

```sh
cargo run --bin bevy-feedback -- run -- cargo run --example minimal
```

With a separate driver:

```sh
cargo run --bin bevy-feedback -- run \
  --game cargo run --example minimal \
  --driver python3 my_driver.py
```

The wrapper exports the protocol/capture/artifact paths, waits for readiness, streams logs, releases inputs, sends `shutdown`, and writes artifacts.

Raw JSON-lines also works:

```jsonl
{"id":1,"command":"window_info"}
{"id":2,"command":"click","x":320,"y":240,"button":"left"}
{"id":3,"command":"key_hold","key":"KeyW","frames":30}
{"id":4,"command":"capture"}
```

## Clients

Rust: `bevy_agent_feedback_plugin::client::AgentClient`.
Python: `clients/python/bevy_feedback.py`; `skills/driving-bevy-games/drive.py` remains a compatibility wrapper.

Both clients can replay transcripts, release held inputs on close, and run optional OCR assertions through the system `tesseract` CLI.

## Optional diagnostics

Enable the `diagnostics` feature and add `AgentFeedbackDiagnosticsPlugin` for `ecs_summary`, `list_entities`, `camera_info`, and registered `state_info` commands.

## CI

See [`docs/ci-linux.md`](docs/ci-linux.md). Windowed captures need a display (`DISPLAY`, `WAYLAND_DISPLAY`, or `xvfb-run`).
