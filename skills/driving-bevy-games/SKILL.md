---
name: driving-bevy-games
description: Drive and verify a running Bevy game through bevy-agent-feedback-plugin using semantic readiness, deterministic time, and completion-confirmed PNG captures.
---

## Choose readiness by game type

**Animated or interactive game:** wait for registered game state, resource, marker, or semantic target predicates, then call `capture_after_frames(1)`. The predicate proves game readiness; the one-frame delayed capture gives the ready state a render boundary and waits for screenshot readback.

**Genuinely static scene or bounded static region:** `wait_until_stable` is a strict pixel-identical check. Scope it with `include`/`masks` when appropriate; do not use whole-screen stability for animation, particles, cursors, clocks, or video.

`protocol ready != game ready`: the protocol file and socket prove only that automation can connect. Assets, menus, save data, cameras, and gameplay state may still be loading.

## First run

Install and verify:

```sh
cargo install bevy-agent-feedback-plugin
bevy-feedback --version
bevy-feedback doctor
```

Run game and driver:

```sh
bevy-feedback run \
  --prepare-timeout 600000 \
  --protocol-timeout 30000 \
  --game-cwd "$PWD" \
  --prepare cargo build --features agent \
  --game cargo run --features agent \
  --driver python3 my_driver.py
```

`bevy-feedback run` injects the bundled `bevy_feedback` module. Manual drivers outside the wrapper need `PYTHONPATH=skills/driving-bevy-games`. The canonical source is `clients/python/bevy_feedback.py`; the skill copy is byte-identical.

## Canonical animated workflow

The registered keys below are exact short Rust type names (`AppState`, `RoundStats`, `Clickable`). Exact `Name`, accessibility-label, and marker selectors must resolve once: duplicates return `ambiguous_target`; clients never choose the first match.

```python
import json
from pathlib import Path
import bevy_feedback
from bevy_feedback import BevyFeedbackClient, fail

def main(game: BevyFeedbackClient) -> None:
    # Completion-confirmed first readable render.
    first = game.wait_until_first_capture()

    # Registered semantic readiness and bounded absence checks.
    game.wait_for_state("AppState", "MainMenu", max_frames=300)
    game.wait_for_resource("RoundStats", "loaded", "eq", True, max_frames=300)
    game.wait_for_resource("RoundStats", "loading", "eq", False, max_frames=300)
    game.wait_for_marker_present("Clickable", max_frames=300)
    game.wait_for_marker_absent("LoadingSpinner", max_frames=300)
    game.wait_for_target({"name": "Play"}, max_frames=300)
    game.wait_for_target_absent({"name": "BlockingModal"}, max_frames=300)

    game.click_named("Play")  # atomic resolve + click; no cached coordinates
    game.wait_for_state("AppState", "Playing", max_frames=300)
    ready = game.capture_after_frames(1, label="playing")

    info = game.last_capture_info
    if info is None or info["completion"] != "screenshot_captured":
        fail(f"capture did not complete readback: {info}")
    if not Path(ready).exists():
        fail(f"missing capture: {ready}")
    print(json.dumps({
        "first": str(first),
        "ready": str(ready),
        "capture": info,  # sequence/path/label, frames, dimensions, windows, completion
    }))

bevy_feedback.run(main)
```

State waits support exact equality and `abort_values=[...]`; generic waits accept `abort_predicates=[...]`. Success is checked first, then abort predicates in request order, then timeout. Resource comparisons support `eq|ne|lt|lte|gt|gte`; model absence as a registered boolean/nullable field. Markers and targets provide explicit present/absent waits. Do not invent a state-absence helper.

Inspect immutable advertised limits through `game.capabilities`; the existing flat fields remain available. Wait helpers reject values above `max_wait_frames` before I/O with remediation. They never silently split a wait—raise the server cap or issue explicit bounded requests whose postconditions you control.

Use `wait_frames(n)` only to count app updates—for input propagation or a render boundary. It is **not** elapsed gameplay time. Under normal time, `wait_seconds(seconds, max_frames=...)` observes Bevy virtual time without changing it. Frozen deterministic mode rejects `wait_seconds`; use `advance_time(seconds, step_seconds=...)`. Clients chunk deterministic advancement from advertised caps while preserving full nominal steps in non-final chunks and allowing only the final chunk to have a short remainder.

After semantic readiness, prefer `capture_after_frames(1)` rather than a separate frame wait plus capture. `completion == "screenshot_captured"` proves Bevy emitted `ScreenshotCaptured` after render readback and that the PNG was persisted. It does **not** prove the OS/window compositor presented the image.

## Wire the game

```toml
[dependencies]
bevy-agent-feedback-plugin = { version = "0.5", optional = true, features = ["diagnostics"] }

[features]
agent = ["dep:bevy-agent-feedback-plugin"]
```

Add feedback after `DefaultPlugins` (or at least after Bevy `TimePlugin` plus window/render/input providers). Deterministic mode needs those time resources before plugin construction.

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
        deterministic_time: true,
        ..Default::default()
    }));
    app.add_plugins(
        AgentFeedbackDiagnosticsPlugin::default()
            .with_state::<AppState>()
            .with_marker::<Clickable>()
            .with_resource_field::<RoundStats, _, _>("loaded", |stats| stats.loaded),
    );
}
```

The `diagnostics` crate feature enables Bevy state/UI support (`bevy/bevy_state`, `bevy/bevy_ui`). Registration is explicit and bounded. Plain `AgentFeedbackPlugin` has no state, resource, marker, predicate, or target queries.

Deterministic mode freezes Bevy-managed virtual/fixed time between `advance_time` requests. It cannot control direct `Instant::now()`, OS or network clocks, unseeded RNG, external processes, or other external state. Do not claim repeatability unless those sources are controlled too.

## Inputs and visual fallback

Prefer semantic target waits and `click_named`/`click_accessibility_label`/`click_marker`. An `input_dispatched` result proves target resolution and Bevy pointer dispatch only; always follow it with a semantic gameplay postcondition. Use coordinates only when no semantic registration exists.

- Input coordinates are **logical window pixels**, origin top-left.
- PNG assertions, OCR crops, `include`, and `masks` use **physical PNG pixels**.
- Convert with the capture's `window_at_request.scale_factor`; use `image_width`/`image_height`, and recompute after resize or scale-factor changes.
- Masked-region checks are a focused fallback, not the primary readiness signal. Keep masks small: they can hide regressions.
- `drag` defaults to `steps=10`, `frames=steps`; the established explicit long drag is `steps=30, frames=45`.
- Wire `key_tap` and `key_hold` default to one app-update frame. Python `key_hold(key, frames)` requires the frame count. Frames still do not mean gameplay seconds.
- Compound input auto-releases. Labeled captures use `[A-Za-z0-9_-]{1,40}`.

Static-region fallback:

```python
stable = game.wait_until_stable(
    frames=10,
    attempts=30,
    stable=2,
    include=(0, 0, 1280, 180),       # physical PNG pixels
    masks=((1160, 0, 120, 80),),     # physical PNG pixels
    label="static_hud",
)
game.drag("Left", game.window_center(), game.point(0.90, 0.50), steps=30, frames=45)
game.key_tap("Enter")
game.key_hold("KeyW", 45)
```

## Results, cleanup, troubleshooting

Live PNGs are in the protocol file's `capture_dir`; wrapper artifacts use `<artifacts>/screenshots/`. Semantic timeout/abort helpers best-effort request `semantic-wait-failure` and attach its metadata without replacing the original error if capture fails. Failures preserve full logs and transcripts; `failure-summary.txt` prioritizes bounded structured context and that post-failure capture, then one deduplicated log tail.

| path | purpose |
|---|---|
| `game.log` / `driver.log` | stdout/stderr streams |
| `protocol.json` | copied live protocol, advertised commands, timing mode, and caps |
| `transcript.jsonl` | replayable request/response/timing envelopes and error context |
| `captures/` | wrapper-exported fallback live capture directory |
| `screenshots/` | final copied PNGs |
| `run-summary.json` | versioned result code, phase/timings, launch context, artifacts, process exit, teardown |
| `failure-summary.txt` | stable code/summary, semantic evidence, post-failure capture, one deduplicated tail, artifact references, teardown |

The wrapper releases inputs and sends `shutdown`. Manual clients should call `BevyFeedbackClient.close()`.

### Runner result codes

| code | remedy |
|---|---|
| `prepare_spawn_failed`, `prepare_nonzero_exit`, `prepare_timeout`, `prepare_wait_failed` | verify the preparation argv/toolchain; inspect `prepare.log`; raise `--prepare-timeout` for cold compilation |
| `game_spawn_failed`, `game_wait_failed`, `game_nonzero_exit` | verify game argv and `--game-cwd`; inspect `game.log`; check asset roots and enable Bevy image-format features used by assets |
| `protocol_early_exit`, `protocol_timeout` | confirm the plugin is enabled and writes `BEVY_FEEDBACK_PROTOCOL`; increase `--protocol-timeout` for startup only, not preparation |
| `window_size_unavailable`, `window_size_mismatch` | create one primary window; own the display and pin resolution/scale; match `--require-window-size` |
| `driver_spawn_failed`, `driver_nonzero_exit`, `driver_timeout`, `driver_wait_failed` | inspect `driver.log`, transcript, and semantic failure capture; bound waits below advertised frame caps |
| `teardown_forced_termination` | ensure the driver closes, the game handles protocol `shutdown`, and both exit before `--shutdown-timeout` |
| `artifact_setup_failed`, `artifact_copy_failed`, `driver_log_failed` | make the artifact root writable and ensure sufficient disk space |

Protocol codes and remedies are maintained separately in [`PROTOCOL.md`](PROTOCOL.md#errors-and-completion-context).

Exact window dimensions are reliable only when the test owns the display environment. Window managers may override `WindowResolution`, and screenshot readback has window/display/compositor constraints.
