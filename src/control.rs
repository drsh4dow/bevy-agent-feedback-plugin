use crate::{
    config::AgentFeedbackConfig,
    protocol::{AgentCommand, AgentResponse, CaptureInfo, WindowInfo},
    runtime::{AgentFeedbackRuntime, AgentRequest},
};
use bevy::{
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
    sync::mpsc::{SyncSender, TryRecvError},
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
            .add_systems(
                PreUpdate,
                (tick_pending_waits, drain_agent_requests)
                    .chain()
                    .before(bevy::input::InputSystems),
            );
    }
}

#[derive(Resource, Default)]
struct AgentFeedbackState {
    next_capture: u64,
    latest_capture: Option<CaptureInfo>,
    pending_waits: VecDeque<PendingWait>,
    captures: VecDeque<PathBuf>,
}

struct PendingWait {
    id: Value,
    frames_left: u16,
    responder: SyncSender<AgentResponse>,
}

fn tick_pending_waits(mut state: ResMut<AgentFeedbackState>) {
    let latest_capture = state.latest_capture.clone();
    state.pending_waits.retain_mut(|wait| {
        wait.frames_left -= 1;
        if wait.frames_left > 0 {
            return true;
        }

        let _ = wait.responder.send(AgentResponse::ok(
            wait.id.clone(),
            "waited",
            latest_capture.clone(),
        ));
        false
    });
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
) {
    let Some(runtime) = runtime else {
        return;
    };
    let mut requests = Vec::new();
    let receiver = match runtime.requests.lock() {
        Ok(receiver) => receiver,
        Err(_) => {
            log::error!("agent feedback command queue lock was poisoned");
            return;
        }
    };
    for _ in 0..config.max_pending_commands.max(1) {
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
                match write_keyboard(&mut windows, &mut keyboard_input, key, ButtonState::Pressed) {
                    Ok(info) => ok_with_window(responder, id, &state, info),
                    Err(()) => missing_window(responder, id),
                }
            }
            AgentCommand::KeyUp(key) => match write_keyboard(
                &mut windows,
                &mut keyboard_input,
                key,
                ButtonState::Released,
            ) {
                Ok(info) => ok_with_window(responder, id, &state, info),
                Err(()) => missing_window(responder, id),
            },
            AgentCommand::MouseDown(button) => match write_mouse_button(
                &mut windows,
                &mut mouse_button_input,
                button,
                ButtonState::Pressed,
            ) {
                Ok(info) => ok_with_window(responder, id, &state, info),
                Err(()) => missing_window(responder, id),
            },
            AgentCommand::MouseUp(button) => match write_mouse_button(
                &mut windows,
                &mut mouse_button_input,
                button,
                ButtonState::Released,
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
                if state.pending_waits.len() >= config.max_pending_commands.max(1) {
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
        }
    }
}

fn ok(responder: SyncSender<AgentResponse>, id: Value, state: &AgentFeedbackState) {
    let _ = responder.send(AgentResponse::ok(id, "ok", state.latest_capture.clone()));
}

fn ok_with_window(
    responder: SyncSender<AgentResponse>,
    id: Value,
    state: &AgentFeedbackState,
    window: WindowInfo,
) {
    let _ = responder.send(AgentResponse::ok_with_window(
        id,
        "ok",
        state.latest_capture.clone(),
        window,
    ));
}

fn missing_window(responder: SyncSender<AgentResponse>, id: Value) {
    let _ = responder.send(AgentResponse::error(
        id,
        "missing_window",
        "primary Window resource is missing",
    ));
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
    state: ButtonState,
) -> Result<WindowInfo, ()> {
    let (window, info) = primary_window_info(windows)?;
    writer.write(KeyboardInput {
        key_code: key,
        logical_key: Key::Unidentified(NativeKey::Unidentified),
        state,
        text: None,
        repeat: false,
        window,
    });
    Ok(info)
}

fn write_mouse_button(
    windows: &mut Query<(Entity, &mut Window), With<PrimaryWindow>>,
    writer: &mut MessageWriter<MouseButtonInput>,
    button: MouseButton,
    state: ButtonState,
) -> Result<WindowInfo, ()> {
    let (window, info) = primary_window_info(windows)?;
    writer.write(MouseButtonInput {
        button,
        state,
        window,
    });
    Ok(info)
}

fn move_cursor(
    windows: &mut Query<(Entity, &mut Window), With<PrimaryWindow>>,
    writer: &mut MessageWriter<CursorMoved>,
    responder: SyncSender<AgentResponse>,
    id: Value,
    state: &AgentFeedbackState,
    position: Vec2,
) {
    let Ok((window_entity, mut window)) = windows.single_mut() else {
        missing_window(responder, id);
        return;
    };
    if !contains_position(&window, position) {
        let _ = responder.send(AgentResponse::error(
            id,
            "position_out_of_bounds",
            "cursor position is outside the primary window",
        ));
        return;
    }

    let previous = window.cursor_position();
    window.set_cursor_position(Some(position));
    writer.write(CursorMoved {
        window: window_entity,
        position,
        delta: previous.map(|previous| position - previous),
    });
    ok_with_window(responder, id, state, WindowInfo::from_window(&window));
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
                    AgentResponse::captured(id.clone(), capture.clone(), window.clone())
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
