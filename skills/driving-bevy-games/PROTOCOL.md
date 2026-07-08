# bevy-agent-feedback protocol v2 reference

Command catalog and wire format for driving a Bevy app instrumented with
`bevy-agent-feedback-plugin`. The running app's **protocol file is
authoritative** — it embeds the live `commands`, `examples`, `coordinates`,
socket, and limits. This file is the offline mirror plus response/error detail.

## Transport

- JSON-lines over TCP: one JSON object per line, one response line per request.
- Single local client at a time; loopback only.
- Request line limit: 8192 bytes (`line_too_long`).

## Discovery — the protocol file

Read the JSON file at `$BEVY_FEEDBACK_PROTOCOL` (default
`target/agent-feedback/agent-feedback.json`). Fields:

| field | meaning |
|---|---|
| `protocol` | must equal `bevy-agent-feedback/2` |
| `socket_addr` | `host:port` to connect |
| `session_id`, `pid`, `started_at_unix_ms` | session identity |
| `heartbeat_file`, `heartbeat_interval_ms`, `stale_after_ms` | liveness |
| `capture_dir` | where `capture` writes PNGs |
| `command_timeout_ms`, `max_action_steps` | limits (see below) |
| `coordinates` | `logical window pixels, origin top-left` |
| `commands`, `examples` | live command schema + sample requests |

Reject the session as stale unless `pid` is alive **and** `heartbeat_file`
holds a timestamp newer than `stale_after_ms`. The bundled clients enforce this.

## Request / response envelope

Request: `{"id": <any>, "command": "<name>", ...params}`. `id` echoes back;
clients auto-assign if omitted.

Success: `{"id":.., "ok":true, "result":{...}}`
Error: `{"id":.., "ok":false, "error":{"code":.., "message":..}}` — malformed JSON returns `"id": null`; syntactically valid request validation errors preserve the request `id`.

`result` may carry:

- `status` — short string (`ok`, `captured`, `waited`, `released`, …).
- `capture` / `latest_capture` — `{"sequence":u64, "path":"…png"}`.
- snapshot fields (flattened): `frame`, `game_time_secs`, `window`,
  `mouse_position` `[x,y]`, `pressed_keys[]`, `pressed_buttons[]`.
- `details` — diagnostics payload.

`mouse_position` is the agent logical cursor. `window`: `logical_width/height`,
`physical_width/height`, `scale_factor`, optional OS-reported
`cursor_position [x,y]`.

### Error codes

`invalid_request` (bad JSON / unknown or malformed command; includes key/button
suggestions), `line_too_long`, `queue_full`, `closed`, `timeout`,
`missing_window`, `position_out_of_bounds`, `capture_dir`, `capture_failed`,
`diagnostics_unavailable`, `socket_error`.

`position_out_of_bounds` keeps code `position_out_of_bounds`; its message is
`point [x,y] outside logical window WxH`.

## Commands

Coordinates are logical pixels, origin top-left. `frames` counts rendered
frames. Held input (`key_down`, `mouse_down`) persists until released or
`release_all_inputs`; compound actions auto-release.

### Input primitives

| command | params | notes |
|---|---|---|
| `key_down` / `key_up` | `key` | physical `KeyCode` name (below), case-insensitive |
| `mouse_down` / `mouse_up` | `button` | `MouseButton` name, case-insensitive |
| `cursor_move` | `x`, `y` | synthesize Bevy cursor movement to an agent logical position |
| `mouse_motion` | `dx`, `dy` | raw motion delta |
| `mouse_scroll` | `y`, `x?`=0, `unit?` | `unit`: `Line` (default) or `Pixel` |
| `text` | `value` | UTF-8 committed via Bevy IME |
| `file_hover` / `file_drop` | `path` | drag-and-drop file events |
| `file_cancel` | — | cancel a hover |

### Compound actions (safe; auto-release)

| command | params | notes |
|---|---|---|
| `click` | `x`, `y`, `button?`=Left, `frames?`=1 | press+release over `frames` |
| `drag` | `from`=[x,y], `to`=[x,y], `button?`=Left, `steps?`=10, `frames?`=steps | `frames` ≥ `steps`; `steps` ≤ `max_action_steps` |
| `scroll` | `lines`, `x?`=0, `unit?` | vertical line delta (wraps `mouse_scroll`) |
| `key_tap` | `key`, `frames?`=1 | hold for `frames` then release |
| `key_hold` | `key`, `frames?`=1 | hold for `frames` then release |
| `release_all_inputs` | — | release every agent-held key/button |

### Lifecycle / query

| command | params | result |
|---|---|---|
| `window_info` | — | window + snapshot |
| `wait` | `frames?`=1 | advance `frames` (1..=`max_wait_frames`) |
| `capture` | — | write PNG; `capture.path` |
| `shutdown` | — | exit the app cleanly |

### Diagnostics

Require the plugin's `diagnostics` cargo feature **and**
`AgentFeedbackDiagnosticsPlugin`; otherwise `diagnostics_unavailable`.

| command | result (`details`) |
|---|---|
| `ecs_summary` | `entity_count`, optional `entity_count_is_lower_bound`, `component_count`, `archetype_count` |
| `list_entities` | `entities:[{entity, components[]}]`, `total`, `truncated`, optional `total_is_lower_bound` (cap 256) |
| `camera_info` | `cameras:[{entity, is_active, order, viewport, translation, projection}]`, `total`, `truncated`, optional `total_is_lower_bound` (cap 32) |
| `state_info` | `states[]` — only types registered via `.with_state::<S>()` |
| `marker_info` | `markers:[{name, count, entities[], truncated, count_is_lower_bound?}]` — only types registered via `.with_marker::<T>()` |

When `truncated` is true, `total`/`count`/`entity_count` may be a lower bound instead of an exact full-world scan. The corresponding `*_is_lower_bound` field is present only in that case.

## Limits

Config-driven; live values are in the protocol file. Defaults:
`max_wait_frames` 300, `max_action_steps` 120, `command_timeout` 10s,
retained captures 32 (older PNGs pruned).

## Names

**MouseButton:** `Left`, `Right`, `Middle`, `Back`, `Forward`.
**Scroll unit:** `Line`, `Pixel`.

**KeyCode** (Bevy physical keys, case-insensitive):

- Letters: `KeyA`..`KeyZ`
- Digits: `Digit0`..`Digit9`
- Function: `F1`..`F35`
- Arrows: `ArrowUp`, `ArrowDown`, `ArrowLeft`, `ArrowRight`
- Whitespace/edit: `Space`, `Enter`, `Tab`, `Backspace`, `Delete`, `Escape`,
  `Insert`, `Home`, `End`, `PageUp`, `PageDown`, `CapsLock`
- Modifiers: `ShiftLeft`/`ShiftRight`, `ControlLeft`/`ControlRight`,
  `AltLeft`/`AltRight`, `SuperLeft`/`SuperRight`
- Punctuation: `Backquote`, `Minus`, `Equal`, `BracketLeft`, `BracketRight`,
  `Backslash`, `Semicolon`, `Quote`, `Comma`, `Period`, `Slash`
- Numpad: `Numpad0`..`Numpad9`, `NumpadAdd`, `NumpadSubtract`,
  `NumpadMultiply`, `NumpadDivide`, `NumpadDecimal`, `NumpadEnter`, `NumLock`
- Media/system: `PrintScreen`, `ScrollLock`, `Pause`, `ContextMenu`,
  `MediaPlayPause`, `AudioVolumeUp`/`Down`/`Mute`, and more.

An invalid name returns `invalid_request` with a "did you mean" suggestion.

On Wayland, cursor commands do not require OS cursor warping; the plugin writes Bevy cursor events and reports the agent logical cursor in `mouse_position`.
