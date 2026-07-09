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
    window::{CursorMoved, FileDragAndDrop, Ime, PrimaryWindow},
};
use serde_json::Value;
use std::{
    collections::VecDeque,
    sync::{
        atomic::Ordering,
        mpsc::{SyncSender, TryRecvError},
    },
};

#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AgentFeedbackSet {
    RequestAdmission,
    DiagnosticEvaluation,
    ResolvedInputInjection,
}

pub(crate) struct AgentFeedbackControlPlugin;

impl Plugin for AgentFeedbackControlPlugin {
    fn build(&self, app: &mut App) {
        let deterministic_time = app
            .world()
            .resource::<AgentFeedbackConfig>()
            .deterministic_time;
        let timing_control = timing::TimingControl::configure(app, deterministic_time);
        app.init_resource::<AgentFeedbackState>()
            .init_resource::<capture::CaptureControl>()
            .insert_resource(timing_control)
            .add_message::<CursorMoved>()
            .add_message::<FileDragAndDrop>()
            .add_message::<Ime>()
            .add_message::<KeyboardInput>()
            .add_message::<MouseButtonInput>()
            .add_message::<MouseMotion>()
            .add_message::<MouseWheel>()
            .add_message::<AppExit>()
            .configure_sets(
                PreUpdate,
                (
                    AgentFeedbackSet::RequestAdmission,
                    AgentFeedbackSet::DiagnosticEvaluation,
                    AgentFeedbackSet::ResolvedInputInjection,
                )
                    .chain()
                    .before(bevy::input::InputSystems),
            )
            .add_systems(
                First,
                timing::guard_time_advance.before(bevy::time::TimeSystems),
            )
            .add_systems(
                PreUpdate,
                (
                    begin_agent_frame,
                    release_disconnected_inputs,
                    tick_pending_actions,
                    capture::tick_pending,
                    timing::tick_waits,
                    drain_agent_requests,
                    timing::admit_timing_requests,
                    crate::runtime::idle_shutdown,
                )
                    .chain()
                    .in_set(AgentFeedbackSet::RequestAdmission),
            )
            .add_systems(Last, timing::account_time_advance);
        #[cfg(feature = "diagnostics")]
        app.add_systems(
            PreUpdate,
            inject_resolved_clicks.in_set(AgentFeedbackSet::ResolvedInputInjection),
        );
    }
}

#[derive(Resource, Default)]
pub(crate) struct AgentFeedbackState {
    pub(crate) frame: u64,
    pub(crate) game_time_secs: f64,
    pub(crate) latest_capture: Option<CaptureInfo>,
    pending_actions: VecDeque<PendingAction>,
    pub(crate) held_keys: Vec<KeyCode>,
    pub(crate) held_buttons: Vec<MouseButton>,
    pub(crate) cursor_position: Option<Vec2>,
    last_heartbeat_unix_ms: u128,
}

struct PendingAction {
    id: Value,
    frames_left: u16,
    responder: SyncSender<AgentResponse>,
    details: Option<Value>,
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
    mut controls: ParamSet<(
        ResMut<timing::TimingControl>,
        ResMut<capture::CaptureControl>,
    )>,
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
            canceled,
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
            command @ (AgentCommand::Wait { .. }
            | AgentCommand::WaitSeconds { .. }
            | AgentCommand::AdvanceTime { .. }) => controls.p0().enqueue(
                AgentRequest {
                    id,
                    command,
                    responder,
                    canceled,
                },
                command_limit,
            ),
            AgentCommand::Capture { label } => controls.p1().admit(
                &mut commands,
                &config,
                &state,
                &mut windows,
                capture::CaptureRequest {
                    id,
                    responder,
                    canceled,
                    frames: 0,
                    label,
                },
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
                                details: None,
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
                                details: None,
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
                                details: None,
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
            AgentCommand::CaptureAfterFrames { frames, label } => controls.p1().admit(
                &mut commands,
                &config,
                &state,
                &mut windows,
                capture::CaptureRequest {
                    id,
                    responder,
                    canceled,
                    frames,
                    label,
                },
            ),
            AgentCommand::TargetInfo { .. }
            | AgentCommand::ClickTarget { .. }
            | AgentCommand::ResourceInfo { .. }
            | AgentCommand::EvaluatePredicate { .. }
            | AgentCommand::WaitFor { .. }
            | AgentCommand::EcsSummary
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
                                canceled,
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

#[cfg(feature = "diagnostics")]
fn inject_resolved_clicks(
    config: Res<AgentFeedbackConfig>,
    diagnostics: Option<ResMut<crate::diagnostics::AgentDiagnosticsQueue>>,
    mut state: ResMut<AgentFeedbackState>,
    mut windows: Query<(Entity, &mut Window), With<PrimaryWindow>>,
    mut cursor_moved: MessageWriter<CursorMoved>,
    mut mouse_button_input: MessageWriter<MouseButtonInput>,
) {
    let Some(mut diagnostics) = diagnostics else {
        return;
    };
    let limit = diagnostics
        .capacity()
        .min(config.max_pending_commands.max(1));
    for _ in 0..limit {
        let Some(click) = diagnostics.pop_resolved_click() else {
            break;
        };
        if click.canceled.load(Ordering::Relaxed) {
            continue;
        }
        if state.pending_actions.len() >= config.max_pending_commands.max(1) {
            queue_full(click.responder, click.id, "too many pending actions");
            continue;
        }
        if let Err(error) =
            move_cursor_internal(&mut windows, &mut cursor_moved, &mut state, click.position)
        {
            let _ = click.responder.send(error_response(click.id, error));
            continue;
        }
        match write_mouse_button(
            &mut windows,
            &mut mouse_button_input,
            click.button,
            ButtonState::Pressed,
            &mut state,
        ) {
            Ok(_) => state.pending_actions.push_back(PendingAction {
                id: click.id,
                frames_left: click.frames,
                responder: click.responder,
                details: Some(click.details),
                kind: PendingActionKind::ReleaseButton(click.button),
            }),
            Err(()) => missing_window(click.responder, click.id),
        }
    }
}

mod capture;
mod commands;
mod timing;
use commands::{
    PendingActionResult, diagnostics_unavailable, error_response, missing_window, move_cursor,
    move_cursor_internal, ok, ok_with_window, primary_window_info, queue_full,
    release_all_inputs_internal, run_pending_action, validate_cursor_position,
    write_file_drag_drop, write_keyboard, write_mouse_button,
};
