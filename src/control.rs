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
                    crate::runtime::idle_shutdown,
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
    cursor_position: Option<Vec2>,
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
    runtime: Option<ResMut<AgentFeedbackRuntime>>,
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
    let Some(mut runtime) = runtime else {
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
        let accepted = match command {
            AgentCommand::KeyDown(key) => {
                match write_keyboard(
                    &mut windows,
                    &mut keyboard_input,
                    key,
                    ButtonState::Pressed,
                    &mut state,
                ) {
                    Ok(info) => {
                        ok_with_window(responder, id, &state, info);
                        true
                    }
                    Err(()) => {
                        missing_window(responder, id);
                        false
                    }
                }
            }
            AgentCommand::KeyUp(key) => match write_keyboard(
                &mut windows,
                &mut keyboard_input,
                key,
                ButtonState::Released,
                &mut state,
            ) {
                Ok(info) => {
                    ok_with_window(responder, id, &state, info);
                    true
                }
                Err(()) => {
                    missing_window(responder, id);
                    false
                }
            },
            AgentCommand::MouseDown(button) => match write_mouse_button(
                &mut windows,
                &mut mouse_button_input,
                button,
                ButtonState::Pressed,
                &mut state,
            ) {
                Ok(info) => {
                    ok_with_window(responder, id, &state, info);
                    true
                }
                Err(()) => {
                    missing_window(responder, id);
                    false
                }
            },
            AgentCommand::MouseUp(button) => match write_mouse_button(
                &mut windows,
                &mut mouse_button_input,
                button,
                ButtonState::Released,
                &mut state,
            ) {
                Ok(info) => {
                    ok_with_window(responder, id, &state, info);
                    true
                }
                Err(()) => {
                    missing_window(responder, id);
                    false
                }
            },
            AgentCommand::CursorMove { position } => move_cursor(
                &mut windows,
                &mut cursor_moved,
                responder,
                id,
                &mut state,
                position,
            ),
            AgentCommand::MouseMotion { delta } => {
                mouse_motion.write(MouseMotion { delta });
                if let Some(accumulated) = accumulated_mouse_motion.as_deref_mut() {
                    accumulated.delta += delta;
                }
                ok(responder, id, &state);
                true
            }
            AgentCommand::MouseScroll { delta, unit } => match primary_window_info(&mut windows) {
                Ok((window, info)) => {
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
                    true
                }
                Err(()) => {
                    missing_window(responder, id);
                    false
                }
            },
            AgentCommand::Text { value } => match primary_window_info(&mut windows) {
                Ok((window, info)) => {
                    ime.write(Ime::Commit { window, value });
                    ok_with_window(responder, id, &state, info);
                    true
                }
                Err(()) => {
                    missing_window(responder, id);
                    false
                }
            },
            AgentCommand::FileHover { path } => write_file_drag_drop(
                &mut windows,
                &mut file_drag_drop,
                responder,
                id,
                &state,
                |window| FileDragAndDrop::HoveredFile {
                    window,
                    path_buf: path,
                },
            ),
            AgentCommand::FileDrop { path } => write_file_drag_drop(
                &mut windows,
                &mut file_drag_drop,
                responder,
                id,
                &state,
                |window| FileDragAndDrop::DroppedFile {
                    window,
                    path_buf: path,
                },
            ),
            AgentCommand::FileCancel => write_file_drag_drop(
                &mut windows,
                &mut file_drag_drop,
                responder,
                id,
                &state,
                |window| FileDragAndDrop::HoveredFileCanceled { window },
            ),
            AgentCommand::WindowInfo => match primary_window_info(&mut windows) {
                Ok((_, info)) => {
                    ok_with_window(responder, id, &state, info);
                    true
                }
                Err(()) => {
                    missing_window(responder, id);
                    false
                }
            },
            AgentCommand::Wait { frames } => {
                if state.pending_waits.len() >= command_limit {
                    let _ = responder.send(AgentResponse::error(
                        id,
                        "queue_full",
                        "too many pending wait commands",
                    ));
                    false
                } else {
                    state.pending_waits.push_back(PendingWait {
                        id,
                        frames_left: frames,
                        responder,
                    });
                    true
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
                Ok(info) => {
                    ok_with_window(responder, id, &state, info);
                    true
                }
                Err(()) => {
                    missing_window(responder, id);
                    false
                }
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
                true
            }
            AgentCommand::Click {
                position,
                button,
                frames,
            } => {
                if state.pending_actions.len() >= command_limit {
                    queue_full(responder, id, "too many pending actions");
                    false
                } else if let Err(error) =
                    move_cursor_internal(&mut windows, &mut cursor_moved, &mut state, position)
                {
                    let _ = responder.send(error_response(id, error));
                    false
                } else {
                    match write_mouse_button(
                        &mut windows,
                        &mut mouse_button_input,
                        button,
                        ButtonState::Pressed,
                        &mut state,
                    ) {
                        Ok(_) => {
                            state.pending_actions.push_back(PendingAction {
                                id,
                                frames_left: frames,
                                responder,
                                kind: PendingActionKind::ReleaseButton(button),
                            });
                            true
                        }
                        Err(()) => {
                            missing_window(responder, id);
                            false
                        }
                    }
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
                    false
                } else if let Err(error) = validate_cursor_position(&mut windows, to) {
                    let _ = responder.send(error_response(id, error));
                    false
                } else if let Err(error) =
                    move_cursor_internal(&mut windows, &mut cursor_moved, &mut state, from)
                {
                    let _ = responder.send(error_response(id, error));
                    false
                } else {
                    match write_mouse_button(
                        &mut windows,
                        &mut mouse_button_input,
                        button,
                        ButtonState::Pressed,
                        &mut state,
                    ) {
                        Ok(_) => {
                            state.pending_actions.push_back(PendingAction {
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
                            });
                            true
                        }
                        Err(()) => {
                            missing_window(responder, id);
                            false
                        }
                    }
                }
            }
            AgentCommand::KeyHold { key, frames } => {
                if state.pending_actions.len() >= command_limit {
                    queue_full(responder, id, "too many pending actions");
                    false
                } else {
                    match write_keyboard(
                        &mut windows,
                        &mut keyboard_input,
                        key,
                        ButtonState::Pressed,
                        &mut state,
                    ) {
                        Ok(_) => {
                            state.pending_actions.push_back(PendingAction {
                                id,
                                frames_left: frames,
                                responder,
                                kind: PendingActionKind::ReleaseKey(key),
                            });
                            true
                        }
                        Err(()) => {
                            missing_window(responder, id);
                            false
                        }
                    }
                }
            }
            AgentCommand::EcsSummary
            | AgentCommand::ListEntities
            | AgentCommand::CameraInfo
            | AgentCommand::StateInfo
            | AgentCommand::MarkerInfo => {
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
                        )
                    } else {
                        diagnostics_unavailable(responder, id);
                        false
                    }
                }
                #[cfg(not(feature = "diagnostics"))]
                {
                    diagnostics_unavailable(responder, id);
                    false
                }
            }
        };
        if accepted {
            runtime.record_accepted_command();
        }
    }
}

mod commands;
use commands::{
    PendingActionResult, capture_primary_window, diagnostics_unavailable, error_response,
    missing_window, move_cursor, move_cursor_internal, ok, ok_with_window, primary_window_info,
    queue_full, release_all_inputs_internal, run_pending_action, validate_cursor_position,
    write_file_drag_drop, write_keyboard, write_mouse_button,
};
