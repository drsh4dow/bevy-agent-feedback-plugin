---
name: driving-bevy-games
description: Drive a running Bevy game — inject input, capture screenshots, verify behavior from pixels. Use when the user wants to playtest or visually verify a Bevy app, or wants bevy-agent-feedback-plugin wired into a game.
---

You control the game through a look → act → look loop: capture a screenshot, act, capture again. A behavior claim is only as good as the pixels you have seen.

## Wire the plugin (once per game)

Dev-only: an optional dependency behind a cargo feature. Release builds never compile it.

```toml
[dependencies]
bevy-agent-feedback-plugin = { git = "https://github.com/drsh4dow/bevy-agent-feedback-plugin", optional = true }

[features]
agent = ["dep:bevy-agent-feedback-plugin"]
```

```rust
#[cfg(feature = "agent")]
app.add_plugins(bevy_agent_feedback_plugin::AgentFeedbackPlugin::new(
    bevy_agent_feedback_plugin::AgentFeedbackConfig {
        bind_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        protocol_file: "target/agent-feedback/agent-feedback.json".into(),
        capture_dir: "target/agent-feedback/captures".into(),
        ..Default::default()
    },
));
```

Done when `cargo check --features agent` passes.

## Launch

```sh
cargo run --features agent > /tmp/game.log 2>&1 &
```

Needs a display (`DISPLAY`/`WAYLAND_DISPLAY`). First build is slow — poll patiently. Done when the protocol file exists and contains `socket_addr`.

## Look → act → look

Send commands with the co-located helper (one JSON command per line, one response per line):

```sh
python3 drive.py target/agent-feedback/agent-feedback.json <<'EOF'
{"command":"window_info"}
{"command":"capture"}
EOF
```

Each iteration:

1. **Look**: `capture`, then view the PNG at `result.capture.path` with the read tool.
2. **Act** in logical coordinates. Captures are physical pixels; commands take logical pixels: `logical = physical / scale_factor` (from `window_info`). Click = `cursor_move`, `mouse_down`, `wait` 1, `mouse_up`. Drag = extra `cursor_move` steps before `mouse_up`. Held key = `key_down`, `wait` N, `key_up`.
3. **Wait**: `wait` enough frames for the game to react before looking again.
4. **Look**: `capture` and confirm the expected change is visible. If it is not, diagnose (check `/tmp/game.log`, re-read `window_info`) before the next action.

An action is done when a capture shows its effect; the session is done when every behavior you report is backed by a capture you viewed.

The protocol file is self-describing — it lists every command with field docs. Read it instead of guessing.

## Cleanup

Kill the game process and release any held keys/buttons first (`key_up`/`mouse_up`) if the game keeps running.
