use crate::{
    config::AgentFeedbackConfig,
    protocol::{AgentCommand, AgentResponse, AgentSnapshot, CaptureInfo, WindowInfo},
    runtime::{AgentFeedbackRuntime, AgentRequest},
    session::{AgentFeedbackSession, unix_ms},
};
use bevy::{
    app::AppExit,
    input::{
        ButtonState,
        keyboard::{Key, KeyboardInput, NativeKey},
        mouse::{
            AccumulatedMouseMotion, AccumulatedMouseScroll, MouseButtonInput, MouseMotion,
            MouseWheel,
        },
        touch::TouchPhase,
    },
    prelude::*,
    render::view::window::screenshot::{Screenshot, ScreenshotCaptured},
    window::{CursorMoved, FileDragAndDrop, Ime, PrimaryWindow},
};
use serde_json::Value;
use std::{
    collections::VecDeque,
    fs, io,
    path::{Path, PathBuf},
    sync::{
        atomic::Ordering,
        mpsc::{SyncSender, TryRecvError},
    },
};

pub(crate) struct AgentFeedbackControlPlugin;

impl Plugin for AgentFeedbackControlPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AgentFeedbackState>()
            .add_message::<CursorMoved>()
            .add_message::<FileDragAndDrop>()
            .add_message::<Ime>()
            .add_message::<KeyboardInput>()
            .add_message::<MouseButtonInput>()
            .add_message::<MouseMotion>()
            .add_message::<MouseWheel>()
            .add_message::<AppExit>()
            .add_systems(
                PreUpdate,
                (
                    begin_agent_frame,
                    release_disconnected_inputs,
                    tick_pending_actions,
                    tick_pending_waits,
                    drain_agent_requests,
                )
                    .chain()
                    .before(bevy::input::InputSystems),
            );
    }
}

#[derive(Resource, Default)]
struct AgentFeedbackState {
    frame: u64,
    game_time_secs: f64,
    next_capture: u64,
    latest_capture: Option<CaptureInfo>,
    pending_waits: VecDeque<PendingWait>,
    pending_actions: VecDeque<PendingAction>,
    captures: VecDeque<PathBuf>,
    held_keys: Vec<KeyCode>,
    held_buttons: Vec<MouseButton>,
    last_heartbeat_unix_ms: u128,
}

struct PendingWait {
    id: Value,
    frames_left: u16,
    responder: SyncSender<AgentResponse>,
}

struct PendingAction {
    id: Value,
    frames_left: u16,
    responder: SyncSender<AgentResponse>,
    kind: PendingActionKind,
}

enum PendingActionKind {
    ReleaseKey(KeyCode),
    ReleaseButton(MouseButton),
    Drag {
        from: Vec2,
        to: Vec2,
        button: MouseButton,
        total_frames: u16,
        steps: u16,
        last_step: u16,
    },
}

fn begin_agent_frame(
    time: Option<Res<Time>>,
    session: Option<Res<AgentFeedbackSession>>,
    mut state: ResMut<AgentFeedbackState>,
) {
    state.frame = state.frame.saturating_add(1);
    if let Some(time) = time {
        state.game_time_secs = time.elapsed_secs_f64();
    }

    let Some(session) = session else {
        return;
    };
    let now = unix_ms();
    if state.last_heartbeat_unix_ms == 0
        || now.saturating_sub(state.last_heartbeat_unix_ms)
            >= session.heartbeat_interval.as_millis()
    {
        if let Err(error) = session.write_heartbeat() {
            log::warn!("failed to write agent feedback heartbeat: {error}");
        }
        state.last_heartbeat_unix_ms = now;
    }
}

fn release_disconnected_inputs(
    runtime: Option<Res<AgentFeedbackRuntime>>,
    mut state: ResMut<AgentFeedbackState>,
    mut windows: Query<(Entity, &mut Window), With<PrimaryWindow>>,
    mut keyboard_input: MessageWriter<KeyboardInput>,
    mut mouse_button_input: MessageWriter<MouseButtonInput>,
) {
    let Some(runtime) = runtime else {
        return;
    };
    if runtime.release_on_disconnect.swap(false, Ordering::Relaxed) {
        let _ = release_all_inputs_internal(
            &mut windows,
            &mut keyboard_input,
            &mut mouse_button_input,
            &mut state,
        );
    }
}

fn tick_pending_actions(
    config: Res<AgentFeedbackConfig>,
    mut state: ResMut<AgentFeedbackState>,
    mut windows: Query<(Entity, &mut Window), With<PrimaryWindow>>,
    mut cursor_moved: MessageWriter<CursorMoved>,
    mut keyboard_input: MessageWriter<KeyboardInput>,
    mut mouse_button_input: MessageWriter<MouseButtonInput>,
) {
    let count = state
        .pending_actions
        .len()
        .min(config.max_pending_commands.max(1));
    for _ in 0..count {
        let Some(mut action) = state.pending_actions.pop_front() else {
            return;
        };
        action.frames_left = action.frames_left.saturating_sub(1);
        match run_pending_action(
            &mut action,
            &mut windows,
            &mut cursor_moved,
            &mut keyboard_input,
            &mut mouse_button_input,
            &mut state,
        ) {
            PendingActionResult::Keep => state.pending_actions.push_back(action),
            PendingActionResult::Done => {}
        }
    }
}

fn tick_pending_waits(mut state: ResMut<AgentFeedbackState>) {
    let latest_capture = state.latest_capture.clone();
    let count = state.pending_waits.len();
    for _ in 0..count {
        let Some(mut wait) = state.pending_waits.pop_front() else {
            return;
        };
        wait.frames_left = wait.frames_left.saturating_sub(1);
        if wait.frames_left > 0 {
            state.pending_waits.push_back(wait);
            continue;
        }

        let _ = wait.responder.send(AgentResponse::ok(
            wait.id,
            "waited",
            latest_capture.clone(),
            None,
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn drain_agent_requests(
    mut commands: Commands,
    runtime: Option<Res<AgentFeedbackRuntime>>,
    config: Res<AgentFeedbackConfig>,
    mut state: ResMut<AgentFeedbackState>,
    mut windows: Query<(Entity, &mut Window), With<PrimaryWindow>>,
    mut cursor_moved: MessageWriter<CursorMoved>,
    mut file_drag_drop: MessageWriter<FileDragAndDrop>,
    mut ime: MessageWriter<Ime>,
    mut keyboard_input: MessageWriter<KeyboardInput>,
    mut mouse_button_input: MessageWriter<MouseButtonInput>,
    mut mouse_motion: MessageWriter<MouseMotion>,
    mut mouse_wheel: MessageWriter<MouseWheel>,
    mut accumulated_mouse_motion: Option<ResMut<AccumulatedMouseMotion>>,
    mut accumulated_mouse_scroll: Option<ResMut<AccumulatedMouseScroll>>,
    #[cfg(feature = "diagnostics")] mut diagnostics: Option<
        ResMut<crate::diagnostics::AgentDiagnosticsQueue>,
    >,
) {
    let Some(runtime) = runtime else {
        return;
    };
    let command_limit = config.max_pending_commands.max(1);
    let mut requests = Vec::new();
    let receiver = match runtime.requests.lock() {
        Ok(receiver) => receiver,
        Err(_) => {
            log::error!("agent feedback command queue lock was poisoned");
            return;
        }
    };
    for _ in 0..command_limit {
        match receiver.try_recv() {
            Ok(request) => requests.push(request),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => return,
        }
    }
    drop(receiver);

    for request in requests {
        let AgentRequest {
            id,
            command,
            responder,
        } = request;
        match command {
            AgentCommand::KeyDown(key) => {
                match write_keyboard(
                    &mut windows,
                    &mut keyboard_input,
                    key,
                    ButtonState::Pressed,
                    &mut state,
                ) {
                    Ok(info) => ok_with_window(responder, id, &state, info),
                    Err(()) => missing_window(responder, id),
                }
            }
            AgentCommand::KeyUp(key) => match write_keyboard(
                &mut windows,
                &mut keyboard_input,
                key,
                ButtonState::Released,
                &mut state,
            ) {
                Ok(info) => ok_with_window(responder, id, &state, info),
                Err(()) => missing_window(responder, id),
            },
            AgentCommand::MouseDown(button) => match write_mouse_button(
                &mut windows,
                &mut mouse_button_input,
                button,
                ButtonState::Pressed,
                &mut state,
            ) {
                Ok(info) => ok_with_window(responder, id, &state, info),
                Err(()) => missing_window(responder, id),
            },
            AgentCommand::MouseUp(button) => match write_mouse_button(
                &mut windows,
                &mut mouse_button_input,
                button,
                ButtonState::Released,
                &mut state,
            ) {
                Ok(info) => ok_with_window(responder, id, &state, info),
                Err(()) => missing_window(responder, id),
            },
            AgentCommand::CursorMove { position } => {
                move_cursor(
                    &mut windows,
                    &mut cursor_moved,
                    responder,
                    id,
                    &state,
                    position,
                );
            }
            AgentCommand::MouseMotion { delta } => {
                mouse_motion.write(MouseMotion { delta });
                if let Some(accumulated) = accumulated_mouse_motion.as_deref_mut() {
                    accumulated.delta += delta;
                }
                ok(responder, id, &state);
            }
            AgentCommand::MouseScroll { delta, unit } => {
                let Ok((window, info)) = primary_window_info(&mut windows) else {
                    missing_window(responder, id);
                    continue;
                };
                mouse_wheel.write(MouseWheel {
                    unit,
                    x: delta.x,
                    y: delta.y,
                    window,
                    phase: TouchPhase::Moved,
                });
                if let Some(accumulated) = accumulated_mouse_scroll.as_deref_mut() {
                    accumulated.unit = unit;
                    accumulated.delta += delta;
                }
                ok_with_window(responder, id, &state, info);
            }
            AgentCommand::Text { value } => {
                let Ok((window, info)) = primary_window_info(&mut windows) else {
                    missing_window(responder, id);
                    continue;
                };
                ime.write(Ime::Commit { window, value });
                ok_with_window(responder, id, &state, info);
            }
            AgentCommand::FileHover { path } => {
                write_file_drag_drop(
                    &mut windows,
                    &mut file_drag_drop,
                    responder,
                    id,
                    &state,
                    |window| FileDragAndDrop::HoveredFile {
                        window,
                        path_buf: path,
                    },
                );
            }
            AgentCommand::FileDrop { path } => {
                write_file_drag_drop(
                    &mut windows,
                    &mut file_drag_drop,
                    responder,
                    id,
                    &state,
                    |window| FileDragAndDrop::DroppedFile {
                        window,
                        path_buf: path,
                    },
                );
            }
            AgentCommand::FileCancel => {
                write_file_drag_drop(
                    &mut windows,
                    &mut file_drag_drop,
                    responder,
                    id,
                    &state,
                    |window| FileDragAndDrop::HoveredFileCanceled { window },
                );
            }
            AgentCommand::WindowInfo => match primary_window_info(&mut windows) {
                Ok((_, info)) => ok_with_window(responder, id, &state, info),
                Err(()) => missing_window(responder, id),
            },
            AgentCommand::Wait { frames } => {
                if state.pending_waits.len() >= command_limit {
                    let _ = responder.send(AgentResponse::error(
                        id,
                        "queue_full",
                        "too many pending wait commands",
                    ));
                } else {
                    state.pending_waits.push_back(PendingWait {
                        id,
                        frames_left: frames,
                        responder,
                    });
                }
            }
            AgentCommand::Capture => capture_primary_window(
                &mut commands,
                &config,
                &mut state,
                &mut windows,
                id,
                responder,
            ),
            AgentCommand::ReleaseAllInputs => match release_all_inputs_internal(
                &mut windows,
                &mut keyboard_input,
                &mut mouse_button_input,
                &mut state,
            ) {
                Ok(info) => ok_with_window(responder, id, &state, info),
                Err(()) => missing_window(responder, id),
            },
            AgentCommand::Shutdown => {
                let window = release_all_inputs_internal(
                    &mut windows,
                    &mut keyboard_input,
                    &mut mouse_button_input,
                    &mut state,
                )
                .ok();
                match window {
                    Some(info) => ok_with_window(responder, id, &state, info),
                    None => ok(responder, id, &state),
                }
                commands.write_message(AppExit::Success);
            }
            AgentCommand::Click {
                position,
                button,
                frames,
            } => {
                if state.pending_actions.len() >= command_limit {
                    queue_full(responder, id, "too many pending actions");
                    continue;
                }
                if let Err(error) = move_cursor_internal(&mut windows, &mut cursor_moved, position)
                {
                    let _ = responder.send(error_response(id, error));
                    continue;
                }
                match write_mouse_button(
                    &mut windows,
                    &mut mouse_button_input,
                    button,
                    ButtonState::Pressed,
                    &mut state,
                ) {
                    Ok(_) => state.pending_actions.push_back(PendingAction {
                        id,
                        frames_left: frames,
                        responder,
                        kind: PendingActionKind::ReleaseButton(button),
                    }),
                    Err(()) => missing_window(responder, id),
                }
            }
            AgentCommand::Drag {
                from,
                to,
                button,
                steps,
                frames,
            } => {
                if state.pending_actions.len() >= command_limit {
                    queue_full(responder, id, "too many pending actions");
                    continue;
                }
                if let Err(error) = move_cursor_internal(&mut windows, &mut cursor_moved, from) {
                    let _ = responder.send(error_response(id, error));
                    continue;
                }
                match write_mouse_button(
                    &mut windows,
                    &mut mouse_button_input,
                    button,
                    ButtonState::Pressed,
                    &mut state,
                ) {
                    Ok(_) => state.pending_actions.push_back(PendingAction {
                        id,
                        frames_left: frames,
                        responder,
                        kind: PendingActionKind::Drag {
                            from,
                            to,
                            button,
                            total_frames: frames,
                            steps,
                            last_step: 0,
                        },
                    }),
                    Err(()) => missing_window(responder, id),
                }
            }
            AgentCommand::KeyHold { key, frames } => {
                if state.pending_actions.len() >= command_limit {
                    queue_full(responder, id, "too many pending actions");
                    continue;
                }
                match write_keyboard(
                    &mut windows,
                    &mut keyboard_input,
                    key,
                    ButtonState::Pressed,
                    &mut state,
                ) {
                    Ok(_) => state.pending_actions.push_back(PendingAction {
                        id,
                        frames_left: frames,
                        responder,
                        kind: PendingActionKind::ReleaseKey(key),
                    }),
                    Err(()) => missing_window(responder, id),
                }
            }
            AgentCommand::EcsSummary
            | AgentCommand::ListEntities
            | AgentCommand::CameraInfo
            | AgentCommand::StateInfo => {
                #[cfg(feature = "diagnostics")]
                {
                    if let Some(queue) = diagnostics.as_deref_mut() {
                        queue.enqueue(
                            AgentRequest {
                                id,
                                command,
                                responder,
                            },
                            command_limit,
                        );
                    } else {
                        diagnostics_unavailable(responder, id);
                    }
                }
                #[cfg(not(feature = "diagnostics"))]
                diagnostics_unavailable(responder, id);
            }
        }
    }
}

fn run_pending_action(
    action: &mut PendingAction,
    windows: &mut Query<(Entity, &mut Window), With<PrimaryWindow>>,
    cursor_moved: &mut MessageWriter<CursorMoved>,
    keyboard_input: &mut MessageWriter<KeyboardInput>,
    mouse_button_input: &mut MessageWriter<MouseButtonInput>,
    state: &mut AgentFeedbackState,
) -> PendingActionResult {
    match &mut action.kind {
        PendingActionKind::ReleaseKey(key) => {
            if action.frames_left > 0 {
                return PendingActionResult::Keep;
            }
            match write_keyboard(windows, keyboard_input, *key, ButtonState::Released, state) {
                Ok(info) => {
                    ok_with_window(action.responder.clone(), action.id.clone(), state, info)
                }
                Err(()) => missing_window(action.responder.clone(), action.id.clone()),
            }
            PendingActionResult::Done
        }
        PendingActionKind::ReleaseButton(button) => {
            if action.frames_left > 0 {
                return PendingActionResult::Keep;
            }
            match write_mouse_button(
                windows,
                mouse_button_input,
                *button,
                ButtonState::Released,
                state,
            ) {
                Ok(info) => {
                    ok_with_window(action.responder.clone(), action.id.clone(), state, info)
                }
                Err(()) => missing_window(action.responder.clone(), action.id.clone()),
            }
            PendingActionResult::Done
        }
        PendingActionKind::Drag {
            from,
            to,
            button,
            total_frames,
            steps,
            last_step,
        } => {
            let elapsed = total_frames.saturating_sub(action.frames_left).max(1);
            let step = (u32::from(elapsed) * u32::from(*steps))
                .div_ceil(u32::from(*total_frames))
                .min(u32::from(*steps)) as u16;
            if step > *last_step {
                *last_step = step;
                let t = f32::from(step) / f32::from(*steps);
                let position = from.lerp(*to, t);
                let move_result = move_cursor_internal(windows, cursor_moved, position);
                if let Err(error) = move_result {
                    let _ = action
                        .responder
                        .send(error_response(action.id.clone(), error));
                    return PendingActionResult::Done;
                }
            }
            if action.frames_left > 0 {
                return PendingActionResult::Keep;
            }
            match write_mouse_button(
                windows,
                mouse_button_input,
                *button,
                ButtonState::Released,
                state,
            ) {
                Ok(info) => {
                    ok_with_window(action.responder.clone(), action.id.clone(), state, info)
                }
                Err(()) => missing_window(action.responder.clone(), action.id.clone()),
            }
            PendingActionResult::Done
        }
    }
}

enum PendingActionResult {
    Keep,
    Done,
}

fn ok(responder: SyncSender<AgentResponse>, id: Value, state: &AgentFeedbackState) {
    let _ = responder.send(AgentResponse::ok(
        id,
        "ok",
        state.latest_capture.clone(),
        None,
    ));
}

fn ok_with_window(
    responder: SyncSender<AgentResponse>,
    id: Value,
    state: &AgentFeedbackState,
    window: WindowInfo,
) {
    let _ = responder.send(AgentResponse::ok(
        id,
        "ok",
        state.latest_capture.clone(),
        Some(snapshot(state, Some(window))),
    ));
}

fn missing_window(responder: SyncSender<AgentResponse>, id: Value) {
    let _ = responder.send(AgentResponse::error(
        id,
        "missing_window",
        "primary Window resource is missing",
    ));
}

fn queue_full(responder: SyncSender<AgentResponse>, id: Value, message: &'static str) {
    let _ = responder.send(AgentResponse::error(id, "queue_full", message));
}

fn diagnostics_unavailable(responder: SyncSender<AgentResponse>, id: Value) {
    let _ = responder.send(AgentResponse::error(
        id,
        "diagnostics_unavailable",
        "diagnostics require the diagnostics feature and AgentFeedbackDiagnosticsPlugin",
    ));
}

fn error_response(id: Value, error: CommandError) -> AgentResponse {
    match error {
        CommandError::MissingWindow => {
            AgentResponse::error(id, "missing_window", "primary Window resource is missing")
        }
        CommandError::PositionOutOfBounds => AgentResponse::error(
            id,
            "position_out_of_bounds",
            "cursor position is outside the primary window",
        ),
    }
}

fn snapshot(state: &AgentFeedbackState, window: Option<WindowInfo>) -> AgentSnapshot {
    let mouse_position = window.as_ref().and_then(|window| window.cursor_position);
    AgentSnapshot {
        frame: state.frame,
        game_time_secs: state.game_time_secs,
        window,
        mouse_position,
        pressed_keys: state
            .held_keys
            .iter()
            .map(|key| format!("{key:?}"))
            .collect(),
        pressed_buttons: state
            .held_buttons
            .iter()
            .map(|button| format!("{button:?}"))
            .collect(),
    }
}

fn primary_window_info(
    windows: &mut Query<(Entity, &mut Window), With<PrimaryWindow>>,
) -> Result<(Entity, WindowInfo), ()> {
    let Ok((entity, window)) = windows.single_mut() else {
        return Err(());
    };
    Ok((entity, WindowInfo::from_window(&window)))
}

fn write_keyboard(
    windows: &mut Query<(Entity, &mut Window), With<PrimaryWindow>>,
    writer: &mut MessageWriter<KeyboardInput>,
    key: KeyCode,
    button_state: ButtonState,
    state: &mut AgentFeedbackState,
) -> Result<WindowInfo, ()> {
    let (window, info) = primary_window_info(windows)?;
    writer.write(KeyboardInput {
        key_code: key,
        logical_key: Key::Unidentified(NativeKey::Unidentified),
        state: button_state,
        text: None,
        repeat: false,
        window,
    });
    track_key(state, key, button_state);
    Ok(info)
}

fn write_mouse_button(
    windows: &mut Query<(Entity, &mut Window), With<PrimaryWindow>>,
    writer: &mut MessageWriter<MouseButtonInput>,
    button: MouseButton,
    button_state: ButtonState,
    state: &mut AgentFeedbackState,
) -> Result<WindowInfo, ()> {
    let (window, info) = primary_window_info(windows)?;
    writer.write(MouseButtonInput {
        button,
        state: button_state,
        window,
    });
    track_button(state, button, button_state);
    Ok(info)
}

fn track_key(state: &mut AgentFeedbackState, key: KeyCode, button_state: ButtonState) {
    match button_state {
        ButtonState::Pressed if !state.held_keys.contains(&key) => state.held_keys.push(key),
        ButtonState::Released => state.held_keys.retain(|held| *held != key),
        ButtonState::Pressed => {}
    }
}

fn track_button(state: &mut AgentFeedbackState, button: MouseButton, button_state: ButtonState) {
    match button_state {
        ButtonState::Pressed if !state.held_buttons.contains(&button) => {
            state.held_buttons.push(button);
        }
        ButtonState::Released => state.held_buttons.retain(|held| *held != button),
        ButtonState::Pressed => {}
    }
}

fn move_cursor(
    windows: &mut Query<(Entity, &mut Window), With<PrimaryWindow>>,
    writer: &mut MessageWriter<CursorMoved>,
    responder: SyncSender<AgentResponse>,
    id: Value,
    state: &AgentFeedbackState,
    position: Vec2,
) {
    match move_cursor_internal(windows, writer, position) {
        Ok(info) => ok_with_window(responder, id, state, info),
        Err(error) => {
            let _ = responder.send(error_response(id, error));
        }
    }
}

fn move_cursor_internal(
    windows: &mut Query<(Entity, &mut Window), With<PrimaryWindow>>,
    writer: &mut MessageWriter<CursorMoved>,
    position: Vec2,
) -> Result<WindowInfo, CommandError> {
    let Ok((window_entity, mut window)) = windows.single_mut() else {
        return Err(CommandError::MissingWindow);
    };
    if !contains_position(&window, position) {
        return Err(CommandError::PositionOutOfBounds);
    }

    let previous = window.cursor_position();
    window.set_cursor_position(Some(position));
    writer.write(CursorMoved {
        window: window_entity,
        position,
        delta: previous.map(|previous| position - previous),
    });
    Ok(WindowInfo::from_window(&window))
}

enum CommandError {
    MissingWindow,
    PositionOutOfBounds,
}

fn release_all_inputs_internal(
    windows: &mut Query<(Entity, &mut Window), With<PrimaryWindow>>,
    keyboard_input: &mut MessageWriter<KeyboardInput>,
    mouse_button_input: &mut MessageWriter<MouseButtonInput>,
    state: &mut AgentFeedbackState,
) -> Result<WindowInfo, ()> {
    let (window, info) = primary_window_info(windows)?;
    for key in state.held_keys.drain(..) {
        keyboard_input.write(KeyboardInput {
            key_code: key,
            logical_key: Key::Unidentified(NativeKey::Unidentified),
            state: ButtonState::Released,
            text: None,
            repeat: false,
            window,
        });
    }
    for button in state.held_buttons.drain(..) {
        mouse_button_input.write(MouseButtonInput {
            button,
            state: ButtonState::Released,
            window,
        });
    }
    let latest_capture = state.latest_capture.clone();
    let response_snapshot = snapshot(state, Some(info.clone()));
    for action in state.pending_actions.drain(..) {
        let _ = action.responder.send(AgentResponse::ok(
            action.id,
            "released",
            latest_capture.clone(),
            Some(response_snapshot.clone()),
        ));
    }
    Ok(info)
}

fn write_file_drag_drop(
    windows: &mut Query<(Entity, &mut Window), With<PrimaryWindow>>,
    writer: &mut MessageWriter<FileDragAndDrop>,
    responder: SyncSender<AgentResponse>,
    id: Value,
    state: &AgentFeedbackState,
    message: impl FnOnce(Entity) -> FileDragAndDrop,
) {
    let Ok((window, info)) = primary_window_info(windows) else {
        missing_window(responder, id);
        return;
    };
    writer.write(message(window));
    ok_with_window(responder, id, state, info);
}

fn contains_position(window: &Window, position: Vec2) -> bool {
    position.x >= 0.0
        && position.y >= 0.0
        && position.x < window.width()
        && position.y < window.height()
}

fn capture_primary_window(
    commands: &mut Commands,
    config: &AgentFeedbackConfig,
    state: &mut AgentFeedbackState,
    windows: &mut Query<(Entity, &mut Window), With<PrimaryWindow>>,
    id: Value,
    responder: SyncSender<AgentResponse>,
) {
    if let Err(error) = fs::create_dir_all(&config.capture_dir) {
        let _ = responder.send(AgentResponse::error(
            id,
            "capture_dir",
            format!("failed to create capture directory: {error}"),
        ));
        return;
    }

    let window = primary_window_info(windows).ok().map(|(_, info)| info);
    let snapshot = Some(snapshot(state, window));
    let sequence = state.next_capture;
    state.next_capture += 1;
    let path = config
        .capture_dir
        .join(format!("capture-{sequence:06}.png"));
    let capture = CaptureInfo {
        sequence,
        path: path.to_string_lossy().into_owned(),
    };
    let max_captures = config.max_captures.max(1);

    commands.spawn(Screenshot::primary_window()).observe(
        move |screenshot: On<ScreenshotCaptured>, mut state: ResMut<AgentFeedbackState>| {
            let response = match save_capture(&screenshot.image, &path) {
                Ok(()) => {
                    state.latest_capture = Some(capture.clone());
                    state.captures.push_back(path.clone());
                    while state.captures.len() > max_captures {
                        if let Some(old_capture) = state.captures.pop_front() {
                            let _ = fs::remove_file(old_capture);
                        }
                    }
                    AgentResponse::captured(id.clone(), capture.clone(), snapshot.clone())
                }
                Err(error) => AgentResponse::error(
                    id.clone(),
                    "capture_failed",
                    format!("failed to save capture: {error}"),
                ),
            };
            let _ = responder.send(response);
        },
    );
}

fn save_capture(image: &bevy::image::Image, path: &Path) -> io::Result<()> {
    let rgb = image
        .clone()
        .try_into_dynamic()
        .map_err(io::Error::other)?
        .to_rgb8();
    rgb.save_with_format(path, image::ImageFormat::Png)
        .map_err(io::Error::other)
}
