use super::{
    AgentFeedbackState,
    commands::{primary_window_info, snapshot},
};
use crate::{
    config::AgentFeedbackConfig,
    protocol::{AgentErrorContext, AgentResponse, CaptureCompletion, CaptureInfo, WindowInfo},
};
use bevy::{
    prelude::*,
    render::view::window::screenshot::{Screenshot, ScreenshotCaptured},
    window::PrimaryWindow,
};
use serde_json::Value;
use std::{
    collections::VecDeque,
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
    },
};

#[derive(Resource, Default)]
pub(super) struct CaptureControl {
    pending: VecDeque<PendingCapture>,
    captures: VecDeque<PathBuf>,
    in_flight: usize,
    next_sequence: u64,
}

pub(super) struct CaptureRequest {
    pub(super) id: Value,
    pub(super) responder: SyncSender<AgentResponse>,
    pub(super) canceled: Arc<AtomicBool>,
    pub(super) frames: u16,
    pub(super) label: Option<String>,
}

struct PendingCapture {
    response: PendingResponse,
    frames_left: u16,
    label: Option<String>,
    requested_frame: u64,
    window_at_request: WindowInfo,
}

struct PendingResponse {
    id: Value,
    responder: SyncSender<AgentResponse>,
    canceled: Arc<AtomicBool>,
}

impl CaptureControl {
    pub(super) fn admit(
        &mut self,
        commands: &mut Commands,
        config: &AgentFeedbackConfig,
        state: &AgentFeedbackState,
        windows: &mut Query<(Entity, &mut Window), With<PrimaryWindow>>,
        request: CaptureRequest,
    ) -> bool {
        let CaptureRequest {
            id,
            responder,
            canceled,
            frames,
            label,
        } = request;
        let pending_count = self.pending.len().saturating_add(self.in_flight);
        if pending_count >= config.max_pending_commands.max(1) {
            send_error(
                id,
                responder,
                "queue_full",
                "too many pending captures",
                state,
                None,
            );
            return false;
        }
        if canceled.load(Ordering::Acquire) {
            send_error(
                id,
                responder,
                "command_canceled",
                "capture request was canceled before admission",
                state,
                None,
            );
            return false;
        }
        let Ok((_, window_at_request)) = primary_window_info(windows) else {
            send_error(
                id,
                responder,
                "missing_window",
                "primary Window resource is missing",
                state,
                None,
            );
            return false;
        };
        if let Err(error) = validate_capture_dir(&config.capture_dir) {
            send_error(
                id,
                responder,
                "capture_dir",
                format!("capture directory is unavailable: {error}"),
                state,
                Some(window_at_request),
            );
            return false;
        }

        let capture = PendingCapture {
            response: PendingResponse {
                id,
                responder,
                canceled,
            },
            frames_left: frames,
            label,
            requested_frame: state.frame,
            window_at_request,
        };
        if frames == 0 {
            self.spawn(commands, config, state, capture)
        } else {
            self.pending.push_back(capture);
            true
        }
    }

    fn spawn(
        &mut self,
        commands: &mut Commands,
        config: &AgentFeedbackConfig,
        state: &AgentFeedbackState,
        capture: PendingCapture,
    ) -> bool {
        if capture.response.canceled.load(Ordering::Acquire) {
            send_error(
                capture.response.id,
                capture.response.responder,
                "command_canceled",
                "capture request was canceled before screenshot spawn",
                state,
                Some(capture.window_at_request),
            );
            return false;
        }
        let sequence = self.next_sequence;
        let Some(next_sequence) = sequence.checked_add(1) else {
            send_error(
                capture.response.id,
                capture.response.responder,
                "capture_failed",
                "capture sequence is exhausted",
                state,
                Some(capture.window_at_request),
            );
            return false;
        };
        self.next_sequence = next_sequence;
        self.in_flight += 1;

        let filename = match &capture.label {
            Some(label) => format!("capture-{sequence:06}-{label}.png"),
            None => format!("capture-{sequence:06}.png"),
        };
        let path = config.capture_dir.join(filename);
        let capture_path = path.to_string_lossy().into_owned();
        let max_captures = config.max_captures.max(1);
        commands.spawn(Screenshot::primary_window()).observe(
            move |screenshot: On<ScreenshotCaptured>,
                  mut state: ResMut<AgentFeedbackState>,
                  mut control: ResMut<CaptureControl>,
                  completion_window: Query<&Window, With<PrimaryWindow>>| {
                debug_assert!(control.in_flight > 0);
                control.in_flight = control.in_flight.saturating_sub(1);
                let completion_window =
                    completion_window.single().ok().map(WindowInfo::from_window);
                let completion_snapshot = snapshot(&state, completion_window.clone());
                if capture.response.canceled.load(Ordering::Acquire) {
                    let response = AgentResponse::contextual_error(
                        capture.response.id.clone(),
                        "command_canceled",
                        "capture request was canceled before screenshot completion",
                        AgentErrorContext {
                            latest_capture: state.latest_capture.clone(),
                            snapshot: Some(completion_snapshot),
                            ..default()
                        },
                    );
                    send_response(&capture.response.responder, response);
                    return;
                }
                let info = CaptureInfo {
                    sequence,
                    path: capture_path.clone(),
                    label: capture.label.clone(),
                    requested_frame: capture.requested_frame,
                    completed_frame: state.frame,
                    image_width: screenshot.image.texture_descriptor.size.width,
                    image_height: screenshot.image.texture_descriptor.size.height,
                    window_at_request: capture.window_at_request.clone(),
                    window_at_completion: completion_window,
                    completion: CaptureCompletion::ScreenshotCaptured,
                };
                let response = match save_capture(&screenshot.image, &path) {
                    Ok(()) => {
                        state.latest_capture = Some(info.clone());
                        control.captures.push_back(path.clone());
                        retain_recent_captures(&mut control.captures, max_captures);
                        AgentResponse::captured(
                            capture.response.id.clone(),
                            info,
                            Some(completion_snapshot),
                        )
                    }
                    Err(error) => AgentResponse::contextual_error(
                        capture.response.id.clone(),
                        "capture_failed",
                        format!("failed to encode or save capture: {error}"),
                        AgentErrorContext {
                            latest_capture: state.latest_capture.clone(),
                            snapshot: Some(completion_snapshot),
                            ..default()
                        },
                    ),
                };
                send_response(&capture.response.responder, response);
            },
        );
        true
    }
}

pub(super) fn tick_pending(
    mut commands: Commands,
    config: Res<AgentFeedbackConfig>,
    state: Res<AgentFeedbackState>,
    mut control: ResMut<CaptureControl>,
) {
    let count = control
        .pending
        .len()
        .min(config.max_pending_commands.max(1));
    for _ in 0..count {
        let Some(mut capture) = control.pending.pop_front() else {
            break;
        };
        if capture.response.canceled.load(Ordering::Acquire) {
            send_error(
                capture.response.id,
                capture.response.responder,
                "command_canceled",
                "pending capture request was canceled",
                &state,
                Some(capture.window_at_request),
            );
            continue;
        }
        capture.frames_left = capture.frames_left.saturating_sub(1);
        if capture.frames_left == 0 {
            control.spawn(&mut commands, &config, &state, capture);
        } else {
            control.pending.push_back(capture);
        }
    }
}

fn validate_capture_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    if fs::metadata(path)?.is_dir() {
        Ok(())
    } else {
        Err(io::Error::other(
            "configured capture path is not a directory",
        ))
    }
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

fn retain_recent_captures(captures: &mut VecDeque<PathBuf>, max_captures: usize) {
    while captures.len() > max_captures {
        if let Some(old_capture) = captures.pop_front()
            && let Err(error) = fs::remove_file(&old_capture)
        {
            log::warn!(
                "failed to remove retained capture {}: {error}",
                old_capture.display()
            );
        }
    }
}

fn send_error(
    id: Value,
    responder: SyncSender<AgentResponse>,
    code: &'static str,
    message: impl Into<String>,
    state: &AgentFeedbackState,
    window: Option<WindowInfo>,
) {
    let response = AgentResponse::contextual_error(
        id,
        code,
        message,
        AgentErrorContext {
            latest_capture: state.latest_capture.clone(),
            snapshot: Some(snapshot(state, window)),
            ..default()
        },
    );
    send_response(&responder, response);
}

fn send_response(responder: &SyncSender<AgentResponse>, response: AgentResponse) {
    if responder.send(response).is_err() {
        log::debug!("capture response receiver disconnected");
    }
}
