# Examples

These examples show how Pi/Codex can discover and drive an instrumented Bevy app.

## `minimal`

Run the app:

```sh
cargo run --example minimal
```

Then ask Pi/Codex to:

1. read `target/agent-feedback/examples/minimal/agent-feedback.json`,
2. connect to `socket_addr`,
3. send `window_info`, `cursor_move`, `mouse_down`, `wait`, `mouse_up`, `key_down KeyW`, `capture`, and `key_up` as needed,
4. report the PNG path from the capture response.

The blue sphere moves when `KeyW` is held.

## `agent_driven`

Run the self-driving demo:

```sh
cargo run --example agent_driven
```

The example starts Bevy, reads its own protocol file, connects as an agent, sends input, captures before/after PNGs, prints their paths, then exits.

## Headless Environments

Windowed examples need `DISPLAY` or `WAYLAND_DISPLAY` on Linux. Use this for compile-only validation:

```sh
cargo check --examples
```
