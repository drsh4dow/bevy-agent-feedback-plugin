---
name: driving-bevy-games
description: Drive a running Bevy game — inject input, capture screenshots, verify behavior from pixels. Use when the user wants to playtest or visually verify a Bevy app, or wants bevy-agent-feedback-plugin wired into a game.
---

You control the game through a look → act → look loop: capture a screenshot, act, capture again. A behavior claim is only as good as the pixels you have seen.

## Wire the plugin (once per game)

Dev-only: optional dependency behind a cargo feature.

```toml
[dependencies]
bevy-agent-feedback-plugin = { version = "0.2", optional = true }

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

Done when `cargo check --features agent` passes.

## Launch

Preferred:

```sh
bevy-feedback run -- cargo run --features agent
```

The wrapper waits for protocol v2 readiness, streams logs, exports `BEVY_FEEDBACK_*`, writes artifacts, releases inputs, and sends `shutdown` on exit.

Manual fallback:

```sh
cargo run --features agent > /tmp/game.log 2>&1 &
```

Done when the protocol file exists and has `protocol: "bevy-agent-feedback/2"`, `socket_addr`, `session_id`, `pid`, and fresh heartbeat.

## Look → act → look

Use the Python client/wrapper:

```sh
python3 skills/driving-bevy-games/drive.py target/agent-feedback/agent-feedback.json <<'EOF'
{"command":"window_info"}
{"command":"capture"}
EOF
```

Each iteration:

1. **Look**: `capture`, then inspect `result.capture.path`.
2. **Act** in logical coordinates. Prefer high-level commands: `click`, `drag`, `scroll`, `key_tap`, `key_hold`.
3. **Wait**: `wait` enough frames for reaction.
4. **Look**: capture again and verify pixels changed.

Capture/window responses include frame, game time, window size/scale, mouse position, and agent-held inputs. If behavior fails, inspect artifacts/logs before more input.

## Cleanup

Use `release_all_inputs` then `shutdown`. Clients also release on close/disconnect; stale protocol files are rejected by pid/heartbeat checks.
