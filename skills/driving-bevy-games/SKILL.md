---
name: driving-bevy-games
description: Drive a running Bevy game through the bevy-agent-feedback-plugin socket — send input, capture screenshots, verify behavior from pixels. Use when the user wants to playtest or visually verify a Bevy app, or wants bevy-agent-feedback-plugin wired into a game.
---

## First run

1. Install and verify:

```sh
cargo install bevy-agent-feedback-plugin
bevy-feedback --version
bevy-feedback doctor
```

2. Run game + driver:

```sh
bevy-feedback run --ready-timeout 180000 \
  --game cargo run --features agent \
  --driver python3 tests/drive_smoke.py
```

`bevy-feedback run` and `doctor` inject the bundled Python client automatically; `PYTHONPATH` is not required. Set `PYTHONPATH` to this skill directory only when running a driver script by hand outside `bevy-feedback run`.

`protocol ready != game ready`: the socket exists; assets, menus, save data, and cameras may still be loading. Wait for a stable frame before acting.

3. Copy-paste smoke driver:

```python
import json
from pathlib import Path
import bevy_feedback
from bevy_feedback import BevyFeedbackClient, fail

def main(game: BevyFeedbackClient) -> None:
    ready = game.wait_until_stable(frames=15, attempts=40, label="boot")
    game.click(*game.point(0.50, 0.50))  # click(x, y, button="Left"); point() maps fractions to logical pixels
    game.wait(10)
    before = game.capture(label="before_drag")
    game.drag("Left", game.window_center(), game.point(0.90, 0.50), steps=30, frames=45)
    game.wait(10)
    after = game.capture(label="after_drag")
    game.assert_changed(before, after, min_pixels=1)
    if not Path(after).exists():
        fail(f"missing final capture: {after}")
    print(json.dumps({"boot": str(ready), "before_drag": str(before), "after_drag": str(after)}))

bevy_feedback.run(main)
```

4. Results: live PNGs are in the protocol file's `capture_dir`; after `bevy-feedback run`, use the printed `<artifacts>/screenshots/`. On failure, read `failure-summary.txt`, `game.log`, `driver.log`, `transcript.jsonl`.

## Wire the game

```toml
[dependencies]
bevy-agent-feedback-plugin = { version = "0.3", optional = true, features = ["diagnostics"] }

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
    app.add_plugins(
        AgentFeedbackDiagnosticsPlugin::default()
            // .with_state::<AppState>()
            // .with_marker::<Selectable>()
    );
}
```

Use diagnostics only for ECS/state/marker queries. Done when `cargo check --features agent` passes.

## Readiness

No diagnostics: pixels are truth. `wait_until_stable` settles boot; `wait_until_changed` verifies act→react (fails on an already-settled screen).

```python
ready = game.wait_until_stable(frames=15, attempts=40, label="ready")
game.click(*game.point(0.50, 0.50))
changed = game.wait_until_changed(ready, frames=10, attempts=30, label="after_click")
game.wait_until_color((255, 255, 255), (20, 20, 120, 40), tolerance=10, attempts=30, label="hud")
```

Diagnostics: recommended when waiting on Bevy states. Add `AgentFeedbackDiagnosticsPlugin::default().with_state::<AppState>()`, then poll `state_info`. Plain `AgentFeedbackPlugin` has no state query.

## Assertions & inputs

Use look → act → look. Trust a command only after a pixel/color/text/diagnostic check.
Clicks work on unmodified idiomatic games as of this plugin version: synthetic input syncs `Window::cursor_position`.

- Prefer `click`, `drag`, `scroll`, `key_tap`, `key_hold`; they auto-release.
- `click(x, y, button="Left")` takes logical pixel coords; fractional: `game.click(*game.point(fx, fy))`. Button first fails: `invalid type: string "Left", expected f32`.
- Coordinates are logical pixels, origin top-left. Use `window_center()` and `point(frac_x, frac_y)` for portable smoke tests.
- Labeled captures use `[A-Za-z0-9_-]{1,40}` and produce `capture-000123-label.png`.
- Use `fail("message")`; `bevy_feedback.run(main)` prints one-line JSON and hides expected game/client tracebacks.

Exact window dimensions are only safe when the test owns the display environment (Xvfb/headless CI); local window managers override `WindowResolution`.

```python
info = game.window_info()["result"]["window"]
if (info["logical_width"], info["logical_height"], info["scale_factor"]) != (1280.0, 720.0, 1.0):
    fail(f"unexpected window metrics: {info}")
```

## Manual mode / artifacts / cleanup / troubleshooting

```sh
bevy-feedback run -- cargo run --features agent
python3 .agents/skills/driving-bevy-games/drive.py "$BEVY_FEEDBACK_PROTOCOL" < commands.jsonl
```

| path | purpose |
|---|---|
| `game.log` / `driver.log` | stdout/stderr streams |
| `protocol.json` | copied live protocol/session metadata |
| `transcript.jsonl` | replayable request/response/timing envelopes |
| `captures/` | wrapper-exported fallback live capture dir |
| `screenshots/` | final copied PNGs from protocol `capture_dir` |
| `failure-summary.txt` | failure reason, log tails, newest capture |

The wrapper releases inputs and sends `shutdown`. Manual clients should send `release_all_inputs` and `shutdown`, or call `BevyFeedbackClient.close()`.

| symptom | fix |
|---|---|
| `protocol file not found` | start the game through `bevy-feedback run` |
| readiness timeout | increase `--ready-timeout MS`; clean Bevy builds can exceed 60s |
| stale protocol | stop old games; clients reject stale pid/heartbeat data |
| `import bevy_feedback` fails | `bevy-feedback run` injects the bundled client; for manual scripts, set `PYTHONPATH` to the real skill directory |
| no screenshots in artifacts | inspect `protocol.json.capture_dir`; wrapper copies that directory |
| OCR errors | install Pillow for crops; install `tesseract` plus language data |
| no display/CI | use a real display, Wayland, or `xvfb-run -s '-screen 0 1280x720x24'` |
