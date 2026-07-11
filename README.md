# bevy-agent-feedback-plugin

Local, bounded agent input, semantic diagnostics, deterministic Bevy time, and completion-confirmed PNG feedback for Bevy 0.19 apps over the 0.5 JSON-lines TCP protocol.

## Install and wire

```toml
[dependencies]
bevy-agent-feedback-plugin = { version = "0.5", optional = true, features = ["diagnostics"] }

[features]
agent = ["dep:bevy-agent-feedback-plugin"]
```

Add feedback after `DefaultPlugins` (or after the plugins providing Bevy time, window, render, and input resources):

```rust
use bevy::prelude::*;
use bevy_agent_feedback_plugin::{
    AgentFeedbackConfig, AgentFeedbackDiagnosticsPlugin, AgentFeedbackPlugin,
};
use std::{net::SocketAddr, path::PathBuf};

App::new()
    .add_plugins(DefaultPlugins)
    .add_plugins(AgentFeedbackPlugin::new(AgentFeedbackConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        protocol_file: PathBuf::from("target/agent-feedback/agent-feedback.json"),
        capture_dir: PathBuf::from("target/agent-feedback/captures"),
        deterministic_time: true,
        ..Default::default()
    }))
    .add_plugins(
        AgentFeedbackDiagnosticsPlugin::default()
            .with_state::<AppState>()
            .with_marker::<Clickable>()
            .with_resource_field::<RoundStats, _, _>("score", |stats| stats.score),
    )
    .run();
```

The optional `diagnostics` feature enables Bevy state/UI support and registered state, resource-field, marker, predicate, semantic-target, and atomic named-click commands. Port `0` lets the OS choose a free local port. The generated protocol file advertises the chosen socket, heartbeat, capture directory, supported commands, deterministic mode, and live caps.

Deterministic mode freezes Bevy-managed virtual/fixed time between `advance_time` requests. It cannot control direct `Instant::now()`, OS/network clocks, unseeded RNG, or other external state. Pinning `WindowResolution` and scale factor helps reproduce PNG dimensions, but remains subject to the display/window manager.

## Run and drive

```sh
cargo install bevy-agent-feedback-plugin
bevy-feedback doctor

bevy-feedback run \
  --require-window-size 1280x720 \
  --prepare-timeout 600000 \
  --protocol-timeout 30000 \
  --game-cwd "$PWD" \
  --prepare cargo build --features agent \
  --game cargo run --features agent \
  --driver python3 my_driver.py
```

Protocol readiness only means the socket exists; it does not prove the game is ready. For animated games, wait on a registered semantic state/resource/marker/target predicate, then use `capture_after_frames(1)`. For genuinely static content, strict region-scoped `wait_until_stable` is available. See the canonical [`driving-bevy-games` skill](skills/driving-bevy-games/SKILL.md) for the readiness/time decision, exact setup, input defaults, and physical-PNG-pixel mask fallback.

Public clients use:

- `wait_frames`: one bounded app-update request, not gameplay elapsed time; oversized waits fail before transmission.
- `wait_seconds`: observational normal-time wait; frozen deterministic mode rejects it.
- `advance_time`: deterministic gameplay progression, chunked only from advertised caps.
- `wait_until_first_capture` and `capture_after_frames`: screenshot-readback-completed PNGs.
- registered predicate waits with generic abort predicates/state abort values, and atomic `click_named`/accessibility-label/marker input dispatch.

Capture metadata includes sequence/path/label, request and completion app-update frames, PNG dimensions, request/completion window metadata, and `completion: "screenshot_captured"`. This proves Bevy screenshot readback and PNG persistence, not OS/window-compositor presentation.

Raw 0.5 JSON-lines retains the compatibility wire command `"wait"`:

```jsonl
{"id":1,"command":"wait","frames":1}
{"id":2,"command":"wait_seconds","seconds":0.5,"max_frames":300}
{"id":3,"command":"advance_time","seconds":1.0,"step_seconds":0.016666667}
{"id":4,"command":"wait_for","predicate":{"type":"state_equals","state":"AppState","value":"Playing"},"abort_predicates":[{"type":"state_equals","state":"AppState","value":"Failed"}],"max_frames":300}
{"id":5,"command":"click_target","target":{"name":"Play"}}
{"id":6,"command":"capture_after_frames","frames":1,"label":"playing"}
```

Input coordinates are logical primary-window pixels. PNG crop/include/mask rectangles are physical image pixels; use capture dimensions and scale factor, and recompute after resize. Normalized-coordinate helpers query the actual logical size, including when a window manager overrides the requested size.

## Clients

- Rust: `bevy_agent_feedback_plugin::client::AgentClient`.
- Python canonical source: `clients/python/bevy_feedback.py`; `bevy-feedback run` injects its byte-identical skill bundle. Use `import bevy_feedback`, `bevy_feedback.run(main)`, and `fail(...)`.
- TypeScript: `clients/typescript/bevy_feedback.ts`, dependency-free with Node type stripping or `tsx`.

All clients expose an immutable advertised capabilities object while retaining flat capability fields, replay request-only or transcript-envelope JSONL, retain structured error context/latest capture metadata, and release held inputs on close. Semantic timeout/abort helpers best-effort attach a labeled failure capture. `click_target` returns `input_dispatched` resolution/dispatch evidence; it does not prove the game accepted the action.

```ts
import { BevyFeedbackClient } from "./clients/typescript/bevy_feedback.ts";

const game = new BevyFeedbackClient();
console.log(await game.windowInfo());
await game.close();
```

## Artifacts and CI

`bevy-feedback run` optionally runs `--prepare` with its own `--prepare-timeout`, then starts the game and begins `--protocol-timeout` at spawn. `--ready-timeout` remains a deprecated alias. Commands inherit the caller cwd; `--game-cwd` affects only the game.

Every completed run writes versioned `run-summary.json` with stable result code, phase/timings, effective commands/cwd, artifacts, required/actual window dimensions, coarse process exit, warnings, and teardown observations. `--require-window-size WIDTHxHEIGHT` (or `BEVY_FEEDBACK_REQUIRED_WINDOW_SIZE`) enforces the actual logical primary-window size after protocol startup. Terminal diagnostics also report actual logical/physical dimensions and scale factor.

Failures start with one stable code and summary, then bounded structured semantic evidence, the post-failure capture when available, one normalized/deduplicated log tail, artifact references, and teardown outcome. Full game/driver logs remain unchanged as artifacts; no cause is guessed from Cargo or Bevy text. Forced termination fails the run; clean exit before acknowledgment is a warning.

See [`docs/ci-linux.md`](docs/ci-linux.md) for maintained external `xvfb-run` and artifact-upload recipes. The runner can enforce window dimensions but intentionally does not own display lifecycle. Windowed screenshot readback requires a usable display and remains subject to window/compositor constraints.
