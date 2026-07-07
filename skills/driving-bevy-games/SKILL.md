---
name: driving-bevy-games
description: Drive a running Bevy game through the bevy-agent-feedback-plugin socket — send input, capture screenshots, verify behavior from pixels. Use when the user wants to playtest or visually verify a Bevy app, or wants bevy-agent-feedback-plugin wired into a game.
---

## Mental model

Use a strict **look → act → look** loop. Pixels are truth: capture before acting, send bounded input, wait for frames, capture again, then assert the frame changed or the expected color/text appeared.

Bundled files:

- `bevy_feedback.py` — Python client plus pixel/OCR assertions.
- `drive.py` — stdin JSON-lines to stdout JSON-lines driver.
- `PROTOCOL.md` — offline command catalog and response/error shapes. The live protocol file is still authoritative.

## Install/prereqs

```sh
cargo install bevy-agent-feedback-plugin
```

- Python 3.10+ for `bevy_feedback.py` and `drive.py`.
- Optional pixel helpers: `pip install pillow`.
- Optional OCR helpers: install `tesseract` and language data.

## Wire the game

Add the plugin as a dev-only optional dependency. Enable `diagnostics` only if you need ECS/state/marker introspection.

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

    // Optional diagnostics; register only types you need to inspect.
    app.add_plugins(
        AgentFeedbackDiagnosticsPlugin::default()
            // .with_state::<AppState>()
            // .with_marker::<Selectable>()
    );
}
```

Pin deterministic captures and assert this from the driver:

```rust
Window {
    resolution: bevy::window::WindowResolution::new(1280, 720)
        .with_scale_factor_override(1.0),
    ..default()
}
```

Done when `cargo check --features agent` passes.

## Best path: wrapper + driver

```sh
PYTHONPATH=.agents/skills/driving-bevy-games \
bevy-feedback run --ready-timeout 180000 \
  --game cargo run --features agent \
  --driver python3 tests/drive_camera.py
```

Why this path:

- `bevy-feedback run` exports `BEVY_FEEDBACK_PROTOCOL`, `BEVY_FEEDBACK_CAPTURE_DIR`, `BEVY_FEEDBACK_ARTIFACTS`, and `BEVY_FEEDBACK_TRANSCRIPT`.
- `--ready-timeout 180000` prevents clean Bevy builds from failing at the default 60s readiness wait.
- `--game ... --driver ...` runs the game and driver under one lifecycle: wait for protocol readiness, run the driver, release inputs, send `shutdown`, copy artifacts.

## Driver skeleton

```python
from bevy_feedback import BevyFeedbackClient

EXPECTED_SIZE = (1280.0, 720.0)
EXPECTED_SCALE = 1.0

with BevyFeedbackClient() as game:
    info = game.window_info()["result"]["window"]
    assert (info["logical_width"], info["logical_height"]) == EXPECTED_SIZE, info
    assert info["scale_factor"] == EXPECTED_SCALE, info

    before = game.capture()
    game.key_hold("KeyW", frames=10)
    game.wait(10)
    after = game.capture()
    game.assert_changed(before, after)
```

Add domain assertions after the second capture: colors, text, region changes, or protocol diagnostics. Do not trust input success alone.

## Manual mode

Run the game through the wrapper without a driver:

```sh
bevy-feedback run -- cargo run --features agent
```

In another shell, send raw JSON-lines:

```sh
python3 .agents/skills/driving-bevy-games/drive.py "$BEVY_FEEDBACK_PROTOCOL" < commands.jsonl
```

`drive.py` also defaults to `$BEVY_FEEDBACK_PROTOCOL` and then `target/agent-feedback/agent-feedback.json`.

## Artifacts

| path | produced by | purpose |
|---|---|---|
| `game.log` | wrapper | game stdout/stderr stream |
| `protocol.json` | wrapper cleanup/failure | copied live protocol/session metadata |
| `transcript.jsonl` | clients via `BEVY_FEEDBACK_TRANSCRIPT` | replayable request/response/timing envelopes |
| `captures/` | live plugin | capture command PNGs during the run |
| `screenshots/` | wrapper cleanup/failure | final copy of PNG captures for upload |
| `failure-summary.txt` | wrapper failure path | failure reason, log tail, newest capture |

`run --driver` is the mode that reliably produces transcript entries for driver commands, because the wrapper exports `BEVY_FEEDBACK_TRANSCRIPT` before starting the driver.

## Inputs

Prefer compound actions: `click`, `drag`, `scroll`, `key_tap`, `key_hold`. They auto-release and are easier to reason about than primitive down/up pairs.

Coordinates are logical window pixels, origin top-left. `mouse_position` in responses is the agent logical cursor. `window.cursor_position` appears only when the OS/window reports a cursor. On Wayland, cursor commands synthesize Bevy `CursorMoved` events and do not require OS cursor warping.

## Diagnostics

Diagnostics are debug-only: enable the crate `diagnostics` feature and add `AgentFeedbackDiagnosticsPlugin`.

Use diagnostics after capture/input checks, not instead of them:

- `ecs_summary`
- `list_entities`
- `camera_info`
- `state_info`
- `marker_info`

Large diagnostics are capped. When capped, counts are lower bounds and include `*_is_lower_bound: true`; entity/camera arrays are truncated.

## Cleanup

The wrapper releases inputs and sends `shutdown` during cleanup. Manual clients should call:

```jsonl
{"command":"release_all_inputs"}
{"command":"shutdown"}
```

`BevyFeedbackClient.close()` releases held inputs before closing the socket. Stale protocol files are rejected by pid and heartbeat checks.

## Troubleshooting

- Readiness timeout: increase `--ready-timeout MS` or set `BEVY_FEEDBACK_READY_TIMEOUT_MS` when a clean Bevy build exceeds 60s.
- Stale protocol: delete old `target/agent-feedback/agent-feedback.json` only after confirming no game is alive; clients reject stale pid/heartbeat data.
- Driver failures: `drive.py` prints JSON error envelopes and exits `1` for bad lines or command failures; no Python traceback for expected protocol errors.
- Wrong window size: pin `WindowResolution` and assert `window_info().result.window.logical_*` plus `scale_factor` before input.
- No display/CI: run under a real display, Wayland, or `xvfb-run -s '-screen 0 1280x720x24'`.
- OCR failures: install Pillow for region crops and `tesseract` plus language packs for text assertions.
