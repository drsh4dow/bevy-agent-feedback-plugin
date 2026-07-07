---
name: driving-bevy-games
description: Drive a running Bevy game through the bevy-agent-feedback-plugin socket — send input, capture screenshots, verify behavior from pixels. Use when the user wants to playtest or visually verify a Bevy app, or wants bevy-agent-feedback-plugin wired into a game.
---

You control the game through a **look → act → look** loop: capture a screenshot, act, capture again. A behavior claim is only as good as the pixels you have seen.

Bundled here, so the skill works with no repo access:

- `bevy_feedback.py` — Python client: driving plus pixel/OCR assertions (`assert_changed`, `assert_color_present`, `wait_until_text`).
- `drive.py` — stdin→stdout JSON-lines driver over that client.
- `PROTOCOL.md` — offline command/key catalog and response/error shapes.

## Prerequisites

- `bevy-feedback` launcher: `cargo install bevy-agent-feedback-plugin`.
- Python 3.10+ for the client. Pixel assertions need `pip install pillow`; OCR needs the `tesseract` binary.

## Wire the plugin (once per game)

Dev-only optional dependency behind a cargo feature. Enabling `diagnostics` on the dep unlocks the ECS/marker introspection commands.

```toml
[dependencies]
bevy-agent-feedback-plugin = { version = "0.2", optional = true, features = ["diagnostics"] }

[features]
agent = ["dep:bevy-agent-feedback-plugin"]
```

```rust
#[cfg(feature = "agent")]
{
    use bevy_agent_feedback_plugin::{
        AgentFeedbackConfig, AgentFeedbackDiagnosticsPlugin, AgentFeedbackPlugin,
    };
    app.add_plugins(AgentFeedbackPlugin::new(AgentFeedbackConfig {
        bind_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        protocol_file: std::env::var_os("BEVY_FEEDBACK_PROTOCOL")
            .map(Into::into)
            .unwrap_or_else(|| "target/agent-feedback/agent-feedback.json".into()),
        capture_dir: std::env::var_os("BEVY_FEEDBACK_CAPTURE_DIR")
            .map(Into::into)
            .unwrap_or_else(|| "target/agent-feedback/captures".into()),
        ..Default::default()
    }));
    // Optional diagnostics; register the state/marker types you want to query:
    // .with_state::<AppState>().with_marker::<Selectable>()
    app.add_plugins(AgentFeedbackDiagnosticsPlugin::default());
}
```

Pin the window for deterministic captures: `WindowResolution::new(1280, 720).with_scale_factor_override(1.0)`.

Done when `cargo check --features agent` passes.

## Launch

Preferred:

```sh
bevy-feedback run -- cargo run --features agent
```

The wrapper waits for protocol v2 readiness, streams logs, exports `BEVY_FEEDBACK_*`, writes artifacts (`game.log`, `transcript.jsonl`, `screenshots/`), releases inputs, and sends `shutdown` on exit.

Manual fallback:

```sh
cargo run --features agent > /tmp/game.log 2>&1 &
```

Done when the protocol file exists with `protocol: "bevy-agent-feedback/2"`, `socket_addr`, `session_id`, `pid`, and a fresh heartbeat.

## Look → act → look

Read the running app's protocol file first: its `commands`/`examples` are the authoritative catalog; `PROTOCOL.md` is the offline mirror.

Raw JSON-lines through the bundled driver:

```sh
python3 drive.py "$BEVY_FEEDBACK_PROTOCOL" <<'EOF'
{"command":"window_info"}
{"command":"capture"}
EOF
```

Installed path: `skill://driving-bevy-games/drive.py`. Both resolve `$BEVY_FEEDBACK_PROTOCOL` or default `target/agent-feedback/agent-feedback.json`.

Or script the client for pixel-truth verification:

```python
from bevy_feedback import BevyFeedbackClient

with BevyFeedbackClient() as game:
    before = game.capture()
    game.key_hold("KeyW", frames=30)
    game.wait(30)
    game.assert_changed(before, game.capture())  # pixels moved
```

Each iteration:

1. **Look**: `capture`, then open `result.capture.path` (a PNG) to see the frame.
2. **Act** in logical coordinates. Prefer compound commands: `click`, `drag`, `scroll`, `key_tap`, `key_hold`.
3. **Wait**: `wait` enough frames for the reaction.
4. **Look**: capture again and confirm pixels changed (`assert_changed`, `assert_color_present`, `wait_until_text`).

Capture/window responses include frame, game time, window size/scale, mouse position, and agent-held inputs. If behavior fails, inspect artifacts/logs before more input.

Diagnostics (when wired with the `diagnostics` feature): `ecs_summary`, `list_entities`, `camera_info`, `state_info`, `marker_info`. Shapes in `PROTOCOL.md`.

## Cleanup

`release_all_inputs` then `shutdown`. Clients also release on close/disconnect; stale protocol files are rejected by pid/heartbeat checks.
