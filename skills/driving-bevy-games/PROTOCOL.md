# bevy-agent-feedback protocol v3 reference

Offline wire reference for `bevy-agent-feedback-plugin`. The running app's protocol file is authoritative: inspect its live `commands`, examples, timing mode, and caps rather than guessing.

## Transport, discovery, and envelope

- JSON-lines over TCP, one object and one response per line; one loopback client.
- Request line cap: 8192 bytes (`line_too_long`).
- Request: `{"id":<any>,"command":"<name>",...}`; omitted IDs are assigned by clients.
- Success: `{"id":..,"ok":true,"result":{...}}`.
- Error: `{"id":..,"ok":false,"error":{"code":"..","message":"..","context":{...}}}`.
- Malformed JSON uses `"id":null`; valid validation failures preserve the request ID.

Protocol-file fields:

| field | meaning |
|---|---|
| `protocol` | exactly `bevy-agent-feedback/3` |
| `socket_addr`, `session_id`, `pid`, `started_at_unix_ms` | endpoint and session identity |
| `heartbeat_file`, `heartbeat_interval_ms`, `stale_after_ms` | liveness; PID and fresh heartbeat are both required |
| `capture_dir` | persisted PNG directory |
| `coordinates` | logical primary-window input pixels, origin top-left |
| `deterministic_time` | whether Bevy virtual/fixed time is frozen between explicit advances |
| `max_wait_frames`, `max_action_steps` | per-request app-update/action caps |
| `max_abort_predicates` | abort predicates accepted by one semantic wait; fixed at 16 |
| `max_time_advance_steps`, `max_time_advance_seconds` | deterministic per-request caps; defaults 600 and 10 seconds |
| `command_timeout_ms` | default 10,000 ms |
| `window_modes` | `windowed`, `borderless_fullscreen`, `fullscreen` |
| `capture_completion` | `screenshot_captured` |
| `commands`, `examples` | exact live schemas and requests |

Other defaults: `max_wait_frames=300`, `max_action_steps=120`, retained captures `32`. Labels match `[A-Za-z0-9_-]{1,40}`. Timing durations must be finite, positive, and nonzero after conversion. Selector/state/resource/field/camera strings are 1..=128 bytes; diagnostic scalar strings are 1..=1024 bytes. All queues, scans, waits, actions, captures, and client chunks are bounded.

## Coordinates and frame semantics

Input (`cursor_move`, `click`, `drag`, semantic target centers) uses **logical window coordinates**. Capture image dimensions and image-helper `include`/`masks`/OCR regions use **physical PNG pixels**. Convert with capture `scale_factor`; recompute after a resize or scale-factor change.

`frames` counts plugin **app updates**, not compositor-presented frames and not elapsed gameplay time. `wait_frames` is the public-client name; it emits one compatibility wire command `"wait"`. Clients reject values above `max_wait_frames` before transmission; waits are never silently chunked. Bevy screenshot readback completion does not prove the OS/window compositor presented the frame.

## Exact lifecycle/timing/capture JSON

```jsonl
{"id":1,"command":"wait","frames":1}
{"id":2,"command":"wait_seconds","seconds":0.5,"max_frames":300}
{"id":3,"command":"advance_time","seconds":1.0,"step_seconds":0.016666667}
{"id":4,"command":"capture","label":"now"}
{"id":5,"command":"capture_after_frames","frames":1,"label":"ready"}
{"id":6,"command":"window_info"}
{"id":7,"command":"release_all_inputs"}
{"id":8,"command":"shutdown"}
```

| command | exact rules |
|---|---|
| `wait` | `frames?=1`, 1..=`max_wait_frames`; compatibility wire name only |
| `wait_seconds` | positive `seconds`; `max_frames?=max_wait_frames`; observes normal Bevy virtual time without changing it |
| `advance_time` | positive `seconds` <= advertised cap; `step_seconds?` positive; default is `Time<Fixed>::timestep()` (normally 1/60); requires deterministic mode |
| `capture` | optional valid `label`; waits for PNG readback/persistence |
| `capture_after_frames` | required `frames` 1..=`max_wait_frames`, optional valid `label`; one ordered wait/capture operation |

Frozen deterministic mode rejects `wait_seconds` with `time_control_frozen`; normal mode rejects `advance_time` with `time_control_disabled`. Deterministic advancement requires unpaused `Time<Virtual>`, relative speed 1, a nominal step no larger than `max_delta`, and no conflicting `TimeUpdateStrategy`. `ceil(seconds/step_seconds)` must fit `max_time_advance_steps`. Clients derive chunks only from advertised caps: every non-final chunk is an integer number of nominal steps; only the final total chunk may contain a short remainder. This preserves the Update delta sequence across cap changes.

Determinism covers Bevy-managed virtual/fixed time only—not direct `Instant::now()`, OS/network clocks, unseeded RNG, external processes, or other external state.

A `result.capture` (and `result.latest_capture`) metadata object has this exact shape:

```json
{"sequence":5,"path":".../capture-000005-ready.png","label":"ready","requested_frame":40,"completed_frame":42,"image_width":1280,"image_height":720,"window_at_request":{"logical_width":1280.0,"logical_height":720.0,"physical_width":1280,"physical_height":720,"scale_factor":1.0,"cursor_position":[640.0,360.0],"focused":true,"visible":true,"mode":"windowed"},"window_at_completion":{"logical_width":1280.0,"logical_height":720.0,"physical_width":1280,"physical_height":720,"scale_factor":1.0,"cursor_position":[640.0,360.0],"focused":true,"visible":true,"mode":"windowed"},"completion":"screenshot_captured"}
```

`label` is omitted if absent; `window_at_completion` is omitted if the primary window disappeared. A capture success includes both `result.capture` and `result.latest_capture` plus flattened snapshot fields: `frame`, `game_time_secs`, `window`, `mouse_position`, `pressed_keys`, and `pressed_buttons`.

## Input commands

| command | params/defaults |
|---|---|
| `key_down` / `key_up` | physical `key` (`KeyCode`, case-insensitive) |
| `mouse_down` / `mouse_up` | `button` (`Left`, `Right`, `Middle`, `Back`, `Forward`) |
| `cursor_move` | logical `x`,`y` |
| `mouse_motion` | raw `dx`,`dy` |
| `mouse_scroll` | `y`, `x?=0`, `unit?=Line` (`Line` or `Pixel`) |
| `text` | UTF-8 `value` through Bevy IME |
| `file_hover` / `file_drop` | `path`; `file_cancel` has no params |
| `click` | logical `x`,`y`, `button?=Left`, `frames?=1` |
| `drag` | logical `from:[x,y]`,`to:[x,y]`, `button?=Left`, `steps?=10`, `frames?=steps`; frames >= steps |
| `scroll` | `lines`, `x?=0`, `unit?=Line` |
| `key_tap` / `key_hold` | `key`, `frames?=1` |

Held primitive input persists until released or `release_all_inputs`; compound actions auto-release. Frame parameters are app-update counts.

## Diagnostics and semantic targets

Require Cargo feature `diagnostics` (which enables `bevy/bevy_state` and `bevy/bevy_ui`) plus explicit registration:

```rust
app.add_plugins(
    AgentFeedbackDiagnosticsPlugin::default()
        .with_state::<AppState>()
        .with_marker::<Clickable>()
        .with_resource_field::<RoundStats, _, _>("score", |stats| stats.score),
);
```

Registration keys are exact short Rust type names; duplicate/colliding/oversized keys or more than 128 registrations are configuration errors.

Exact requests:

```jsonl
{"id":20,"command":"target_info","target":{"name":"Play"},"kind":"any"}
{"id":21,"command":"click_target","target":{"accessibility_label":"Play"},"kind":"ui","button":"Left","frames":1}
{"id":22,"command":"resource_info","resource":"RoundStats","field":"score"}
{"id":23,"command":"evaluate_predicate","predicate":{"type":"state_equals","state":"AppState","value":"Playing"}}
{"id":24,"command":"wait_for","predicate":{"type":"resource_field","resource":"RoundStats","field":"score","operator":"gte","value":10},"abort_predicates":[{"type":"state_equals","state":"AppState","value":"Failed"}],"max_frames":300}
{"id":25,"command":"wait_for","predicate":{"type":"marker_count","marker":"Enemy","min":1},"max_frames":300}
{"id":26,"command":"wait_for","predicate":{"type":"target_exists","target":{"marker":"Clickable"},"kind":"any"},"max_frames":300}
{"id":27,"command":"wait_for","predicate":{"type":"target_absent","target":{"name":"BlockingModal"},"kind":"any"},"max_frames":300}
```

Target selector has **exactly one** of `name`, `accessibility_label`, or registered `marker`. `kind?` is `any` (default), `ui`, or `world`; `camera?` is an exact camera Name. `click_target` resolves and dispatches pointer input atomically. Its completion status is `input_dispatched`; details include `target_resolved`, `entity`, `logical_position`, `input_dispatched`, `button`, and resolved target metadata. This proves resolution and Bevy input dispatch, not that gameplay accepted the action—follow interactions with a semantic postcondition. Exact duplicate matches return `ambiguous_target` with at most 16 candidate details and `candidate_details_truncated` when needed; they never select the first. A definitive miss after scanning at most 256 entities returns `target_not_found`; if the cap prevents a definitive miss, `target_search_truncated` reports a candidate-count lower bound. UI targets require visible, unclipped layout bounds. World targets require a unique active compatible camera and successful viewport projection.

Predicate forms:

- `state_equals`: exact registered `state`, bounded scalar `value`.
- `resource_field`: exact registered `resource` and `field`; operator `eq|ne|lt|lte|gt|gte`; ordering requires numbers.
- `marker_count`: exact registered `marker`; at least one of u32 `min`/`max`.
- `target_exists` / `target_absent`: target selector plus optional kind/camera.

Only outcome `matched` satisfies a wait. `abort_predicates` is optional and accepts at most the advertised `max_abort_predicates`; every entry uses the same generic predicate schema. Each evaluation pass checks the success predicate first, then abort predicates in request order, then timeout. A success therefore wins when success, abort, and timeout coincide. A matching abort returns `predicate_aborted` with its observation and the exact snapshot frame. State helpers convert `abort_values` into generic `state_equals` abort predicates; there is no state-only wire path.

Marker scans cap at 256. If `count_is_lower_bound=true`, a minimum can still match when proven, but a maximum/absence that cannot be proven is `indeterminate`, never a false match. Target absence is likewise indeterminate after a truncated search.

Legacy diagnostic commands remain: `ecs_summary`; `list_entities` (256 cap); `camera_info` (32 cap); registered `state_info`; registered `marker_info`. Their `truncated` and `*_is_lower_bound` flags mean totals are not exact.

## Errors and completion context

Common codes include `invalid_request`, `invalid_argument`, `line_too_long`, `queue_full`, `closed`, `timeout`, `predicate_timeout`, `predicate_aborted`, `missing_window`, `position_out_of_bounds`, `capture_dir`, `capture_failed`, `diagnostics_unavailable`, `ambiguous_target`, `target_search_truncated`, `target_not_found`, timing-state errors, and `socket_error`.

Errors may carry bounded `context.latest_capture`, `snapshot`, `timing`, `observed_predicate`, `ecs_summary`, and `diagnostic` details. After `predicate_timeout` or `predicate_aborted`, client wait helpers best-effort request a `semantic-wait-failure` capture and attach its metadata as `failure_capture` on the original error. Capture failure never replaces the semantic error.

An invalid key/button name returns `invalid_request` with a suggestion. `position_out_of_bounds` reports the point and logical window size. On Wayland, cursor commands write synthetic state directly into Bevy's `Window`; OS cursor warping is not required.
