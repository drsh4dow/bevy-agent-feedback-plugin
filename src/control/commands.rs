use super::*;

pub(super) fn run_pending_action(
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
                    if let Some(details) = action.details.take() {
                        let _ = action.responder.send(AgentResponse::details_with_context(
                            action.id.clone(),
                            "clicked_target",
                            state.latest_capture.clone(),
                            snapshot(state, Some(info)),
                            details,
                        ));
                    } else {
                        ok_with_window(action.responder.clone(), action.id.clone(), state, info);
                    }
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
                let move_result = move_cursor_internal(windows, cursor_moved, state, position);
                if let Err(error) = move_result {
                    if write_mouse_button(
                        windows,
                        mouse_button_input,
                        *button,
                        ButtonState::Released,
                        state,
                    )
                    .is_err()
                    {
                        track_button(state, *button, ButtonState::Released);
                    }
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

pub(super) enum PendingActionResult {
    Keep,
    Done,
}

pub(super) fn ok(responder: SyncSender<AgentResponse>, id: Value, state: &AgentFeedbackState) {
    let _ = responder.send(AgentResponse::ok(
        id,
        "ok",
        state.latest_capture.clone(),
        None,
    ));
}

pub(super) fn ok_with_window(
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

pub(super) fn missing_window(responder: SyncSender<AgentResponse>, id: Value) {
    let _ = responder.send(AgentResponse::error(
        id,
        "missing_window",
        "primary Window resource is missing",
    ));
}

pub(super) fn queue_full(responder: SyncSender<AgentResponse>, id: Value, message: &'static str) {
    let _ = responder.send(AgentResponse::error(id, "queue_full", message));
}

pub(super) fn diagnostics_unavailable(responder: SyncSender<AgentResponse>, id: Value) {
    let _ = responder.send(AgentResponse::error(
        id,
        "diagnostics_unavailable",
        "diagnostics require the diagnostics feature and AgentFeedbackDiagnosticsPlugin",
    ));
}

pub(super) fn error_response(id: Value, error: CommandError) -> AgentResponse {
    match error {
        CommandError::MissingWindow => {
            AgentResponse::error(id, "missing_window", "primary Window resource is missing")
        }
        CommandError::PositionOutOfBounds {
            position,
            logical_width,
            logical_height,
        } => AgentResponse::error(
            id,
            "position_out_of_bounds",
            format!(
                "point [{},{}] outside logical window {}x{}",
                position.x, position.y, logical_width, logical_height
            ),
        ),
    }
}

pub(super) fn snapshot(state: &AgentFeedbackState, window: Option<WindowInfo>) -> AgentSnapshot {
    let mouse_position = state
        .cursor_position
        .map(|position| [position.x, position.y]);
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

pub(super) fn primary_window_info(
    windows: &mut Query<(Entity, &mut Window), With<PrimaryWindow>>,
) -> Result<(Entity, WindowInfo), ()> {
    let Ok((entity, window)) = windows.single_mut() else {
        return Err(());
    };
    Ok((entity, WindowInfo::from_window(&window)))
}

pub(super) fn write_keyboard(
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

pub(super) fn write_mouse_button(
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

pub(super) fn track_button(
    state: &mut AgentFeedbackState,
    button: MouseButton,
    button_state: ButtonState,
) {
    match button_state {
        ButtonState::Pressed if !state.held_buttons.contains(&button) => {
            state.held_buttons.push(button);
        }
        ButtonState::Released => state.held_buttons.retain(|held| *held != button),
        ButtonState::Pressed => {}
    }
}

pub(super) fn move_cursor(
    windows: &mut Query<(Entity, &mut Window), With<PrimaryWindow>>,
    writer: &mut MessageWriter<CursorMoved>,
    responder: SyncSender<AgentResponse>,
    id: Value,
    state: &mut AgentFeedbackState,
    position: Vec2,
) -> bool {
    match move_cursor_internal(windows, writer, state, position) {
        Ok(info) => {
            ok_with_window(responder, id, state, info);
            true
        }
        Err(error) => {
            let _ = responder.send(error_response(id, error));
            false
        }
    }
}

pub(super) fn move_cursor_internal(
    windows: &mut Query<(Entity, &mut Window), With<PrimaryWindow>>,
    writer: &mut MessageWriter<CursorMoved>,
    state: &mut AgentFeedbackState,
    position: Vec2,
) -> Result<WindowInfo, CommandError> {
    let Ok((window_entity, mut window)) = windows.single_mut() else {
        return Err(CommandError::MissingWindow);
    };
    validate_position(&window, position)?;
    window
        .bypass_change_detection()
        .set_cursor_position(Some(position));

    let previous = state.cursor_position.or_else(|| window.cursor_position());
    state.cursor_position = Some(position);
    writer.write(CursorMoved {
        window: window_entity,
        position,
        delta: previous.map(|previous| position - previous),
    });
    Ok(WindowInfo::from_window(&window))
}

pub(super) fn validate_cursor_position(
    windows: &mut Query<(Entity, &mut Window), With<PrimaryWindow>>,
    position: Vec2,
) -> Result<(), CommandError> {
    let Ok((_, window)) = windows.single_mut() else {
        return Err(CommandError::MissingWindow);
    };
    validate_position(&window, position)
}

fn validate_position(window: &Window, position: Vec2) -> Result<(), CommandError> {
    if contains_position(window, position) {
        Ok(())
    } else {
        Err(CommandError::PositionOutOfBounds {
            position,
            logical_width: window.width(),
            logical_height: window.height(),
        })
    }
}

pub(super) enum CommandError {
    MissingWindow,
    PositionOutOfBounds {
        position: Vec2,
        logical_width: f32,
        logical_height: f32,
    },
}

pub(super) fn release_all_inputs_internal(
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

pub(super) fn write_file_drag_drop(
    windows: &mut Query<(Entity, &mut Window), With<PrimaryWindow>>,
    writer: &mut MessageWriter<FileDragAndDrop>,
    responder: SyncSender<AgentResponse>,
    id: Value,
    state: &AgentFeedbackState,
    message: impl FnOnce(Entity) -> FileDragAndDrop,
) -> bool {
    let Ok((window, info)) = primary_window_info(windows) else {
        missing_window(responder, id);
        return false;
    };
    writer.write(message(window));
    ok_with_window(responder, id, state, info);
    true
}

fn contains_position(window: &Window, position: Vec2) -> bool {
    position.x >= 0.0
        && position.y >= 0.0
        && position.x < window.width()
        && position.y < window.height()
}
