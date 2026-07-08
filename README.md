# bevy-agent-feedback-plugin

Local agent feedback for Bevy apps.

This crate lets Pi/Codex drive a running Bevy app through a v2 JSON-lines TCP protocol. Agents can press keys, move/click/drag/scroll the mouse, submit text/file-drop events, query window metadata, wait for frames, capture labeled PNG screenshots, replay transcripts, and shut the app down cleanly.

## Install

```sh
cargo add bevy-agent-feedback-plugin --optional
```

Manual `Cargo.toml` form uses the `0.3` line:

```toml
bevy-agent-feedback-plugin = { version = "0.3", optional = true }
```

```toml
[features]
agent = ["dep:bevy-agent-feedback-plugin"]
```

```rust
#[cfg(feature = "agent")]
app.add_plugins(bevy_agent_feedback_plugin::AgentFeedbackPlugin::new(
    bevy_agent_feedback_plugin::AgentFeedbackConfig {
        bind_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        protocol_file: std::env::var_os("BEVY_FEEDBACK_PROTOCOL")
            .map(Into::into)
            .unwrap_or_else(|| "target/agent-feedback/agent-feedback.json".into()),
        capture_dir: std::env::var_os("BEVY_FEEDBACK_CAPTURE_DIR")
            .map(Into::into)
            .unwrap_or_else(|| "target/agent-feedback/captures".into()),
        ..Default::default()
    },
));
```

Install the wrapper binary (`bevy-feedback`):

```sh
cargo install bevy-agent-feedback-plugin
bevy-feedback --version
bevy-feedback doctor
```

Dev form from this repo:

```sh
cargo run --bin bevy-feedback -- run -- cargo run --example minimal
```

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

For deterministic captures, pin the window size and scale factor:

```rust
Window {
    resolution: bevy::window::WindowResolution::new(1280, 720)
        .with_scale_factor_override(1.0),
    ..default()
}
```

## Drive it

Manual wrapper:

```sh
cargo run --bin bevy-feedback -- run -- cargo run --example minimal
```

Recommended automated mode:

```sh
cargo run --bin bevy-feedback -- run --ready-timeout 180000 \
  --game cargo run --example minimal \
  --driver python3 my_driver.py
```

The wrapper exports the protocol/capture/artifact paths, waits for protocol readiness, streams logs, releases inputs, sends `shutdown`, copies PNGs from the protocol file's `capture_dir` into `artifacts/screenshots/`, and prints those screenshot paths on success. Protocol ready means the automation socket exists; wait for a stable game frame in the driver before capturing. Timeout flags use milliseconds and can also come from environment variables:

| flag | env | default |
|---|---|---|
| `--ready-timeout MS` | `BEVY_FEEDBACK_READY_TIMEOUT_MS` | `60000` |
| `--driver-timeout MS` | `BEVY_FEEDBACK_DRIVER_TIMEOUT_MS` | `300000` |
| `--shutdown-timeout MS` | `BEVY_FEEDBACK_SHUTDOWN_TIMEOUT_MS` | `5000` |

Artifacts from `bevy-feedback run`:

| path | purpose |
|---|---|
| `game.log` | game stdout/stderr |
| `driver.log` | driver stdout/stderr |
| `protocol.json` | copied protocol/session metadata |
| `transcript.jsonl` | replayable request/response/timing envelopes |
| `captures/` | wrapper-exported fallback live capture dir |
| `screenshots/` | final copied PNGs from the protocol `capture_dir` |
| `failure-summary.txt` | failure reason, log tail, newest capture |

`--game ... --driver ...` is the best mode for automation because the driver receives `BEVY_FEEDBACK_TRANSCRIPT`, so client commands are recorded in `transcript.jsonl`.

Raw JSON-lines also works:

```jsonl
{"id":1,"command":"window_info"}
{"id":2,"command":"click","x":320,"y":240,"button":"left"}
{"id":3,"command":"key_hold","key":"KeyW","frames":30}
{"id":4,"command":"capture","label":"default"}
```

## Clients

Rust: `bevy_agent_feedback_plugin::client::AgentClient`, including `capture_labeled("name")`.
Python: `clients/python/bevy_feedback.py`; wrap drivers in `bevy_feedback.run(main)` and use `fail("message")` for expected game/client failures without tracebacks. `capture(label="name")` names PNGs.
TypeScript: `clients/typescript/bevy_feedback.ts`, dependency-free (`node` with type stripping or `tsx`), with `capture(label?)`.

Rust/Python clients include pixel/OCR assertions. TypeScript covers core driving only. All clients can replay transcript envelopes (`request` + `response` + timing) and older request-only JSONL, and release held inputs on close. See [`skills/driving-bevy-games/SKILL.md`](skills/driving-bevy-games/SKILL.md) for the first-use workflow.

```ts
import { BevyFeedbackClient } from "./clients/typescript/bevy_feedback.ts";

const game = new BevyFeedbackClient();
console.log(await game.windowInfo());
await game.close();
```

## Optional diagnostics

Enable the `diagnostics` feature and add `AgentFeedbackDiagnosticsPlugin` for `ecs_summary`, `list_entities`, `camera_info`, registered `state_info`, and registered marker-component `marker_info` commands.

```rust
#[derive(Component)]
struct Selectable;

app.add_plugins(
    bevy_agent_feedback_plugin::AgentFeedbackDiagnosticsPlugin::default()
        .with_marker::<Selectable>(),
);
```

## CI

See [`docs/ci-linux.md`](docs/ci-linux.md) for the headless `xvfb-run` recipe and artifact upload path. Windowed captures need a display (`DISPLAY`, `WAYLAND_DISPLAY`, or `xvfb-run`).
