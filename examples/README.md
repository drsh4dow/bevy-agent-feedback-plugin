# Examples

These examples show how Pi/Codex can discover and drive an instrumented Bevy app with protocol v3.

## `minimal`

Run directly:

```sh
cargo run --example minimal
```

Or let the wrapper own lifecycle and artifacts:

```sh
cargo run --bin bevy-feedback -- run -- cargo run --example minimal
```

Then drive `target/agent-feedback/examples/minimal/agent-feedback.json` (or `$BEVY_FEEDBACK_PROTOCOL` under the wrapper). Useful commands: `window_info`, `capture`, `click`, `drag`, `scroll`, `key_tap`, `key_hold`, `release_all_inputs`, `shutdown`.

The blue sphere moves when `KeyW` is held.

## `agent_driven`

```sh
cargo run --example agent_driven
```

The example starts Bevy, uses the Rust `AgentClient`, waits, captures before/after PNGs, holds `KeyW`, prints capture paths, then exits.

## Headless/CI

Windowed examples need `DISPLAY` or `WAYLAND_DISPLAY` on Linux. Use compile-only validation when no display is available:

```sh
cargo check --examples
```

For rendered CI, see `docs/ci-linux.md`.
