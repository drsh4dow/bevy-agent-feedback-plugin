use super::{AgentFeedbackState, commands::snapshot};
use crate::{
    config::AgentFeedbackConfig,
    protocol::{
        AgentCommand, AgentErrorContext, AgentResponse, AgentTimingContext, AgentTimingResult,
    },
    runtime::AgentRequest,
};
use bevy::{
    prelude::*,
    time::{Fixed, Real, TimePlugin, TimeUpdateStrategy, Virtual},
};
use serde_json::Value;
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
    },
    time::Duration,
};

#[derive(Clone, Copy)]
enum Availability {
    Disabled,
    Available,
    Unavailable(&'static str),
}

#[derive(Resource)]
pub(super) struct TimingControl {
    availability: Availability,
    incoming: VecDeque<AgentRequest>,
    frame_waits: VecDeque<FrameWait>,
    seconds_waits: VecDeque<SecondsWait>,
    advance: Option<PendingAdvance>,
}

struct PendingResponse {
    id: Value,
    responder: SyncSender<AgentResponse>,
    canceled: Arc<AtomicBool>,
}

struct FrameWait {
    response: PendingResponse,
    frames_left: u16,
}

struct SecondsWait {
    response: PendingResponse,
    requested: Duration,
    start: Duration,
    max_frames: u16,
    frames: u16,
}

struct PendingAdvance {
    response: PendingResponse,
    requested: Duration,
    remaining: Duration,
    nominal_step: Duration,
    current_step: Duration,
    start: Duration,
    previous: Duration,
    step_count: u16,
    admitted_frame: u64,
}

impl TimingControl {
    pub(super) fn configure(app: &mut App, deterministic: bool) -> Self {
        let availability = if !deterministic {
            Availability::Disabled
        } else if !app.is_plugin_added::<TimePlugin>() {
            Availability::Unavailable("time_plugin_missing")
        } else if !has_required_time_resources(app) {
            Availability::Unavailable("time_resources_missing")
        } else {
            configure_manual_zero(app)
        };
        Self {
            availability,
            incoming: VecDeque::new(),
            frame_waits: VecDeque::new(),
            seconds_waits: VecDeque::new(),
            advance: None,
        }
    }

    pub(super) fn enqueue(&mut self, request: AgentRequest, limit: usize) -> bool {
        let pending = self
            .incoming
            .len()
            .saturating_add(self.frame_waits.len())
            .saturating_add(self.seconds_waits.len())
            .saturating_add(usize::from(self.advance.is_some()));
        if pending >= limit.max(1) {
            let _ = request.responder.send(AgentResponse::error(
                request.id,
                "queue_full",
                "too many pending timing commands",
            ));
            false
        } else {
            self.incoming.push_back(request);
            true
        }
    }
}

fn has_required_time_resources(app: &App) -> bool {
    app.world().contains_resource::<Time>()
        && app.world().contains_resource::<Time<Real>>()
        && app.world().contains_resource::<Time<Virtual>>()
        && app.world().contains_resource::<Time<Fixed>>()
        && app.world().contains_resource::<TimeUpdateStrategy>()
}

fn configure_manual_zero(app: &mut App) -> Availability {
    let mut strategy = app.world_mut().resource_mut::<TimeUpdateStrategy>();
    match *strategy {
        TimeUpdateStrategy::Automatic | TimeUpdateStrategy::ManualDuration(Duration::ZERO) => {
            *strategy = TimeUpdateStrategy::ManualDuration(Duration::ZERO);
            Availability::Available
        }
        TimeUpdateStrategy::ManualDuration(_) => {
            Availability::Unavailable("manual_duration_already_configured")
        }
        TimeUpdateStrategy::ManualInstant(_) => {
            Availability::Unavailable("manual_instant_already_configured")
        }
        TimeUpdateStrategy::FixedTimesteps(_) => {
            Availability::Unavailable("fixed_timesteps_already_configured")
        }
    }
}

pub(super) fn tick_waits(
    config: Res<AgentFeedbackConfig>,
    time: Option<Res<Time<Virtual>>>,
    mut control: ResMut<TimingControl>,
    state: Res<AgentFeedbackState>,
) {
    let limit = config.max_pending_commands.max(1);
    let capture = state.latest_capture.clone();
    let frame_count = control.frame_waits.len().min(limit);
    for _ in 0..frame_count {
        let Some(mut wait) = control.frame_waits.pop_front() else {
            break;
        };
        if wait.response.canceled.load(Ordering::Acquire) {
            continue;
        }
        wait.frames_left = wait.frames_left.saturating_sub(1);
        if wait.frames_left == 0 {
            let _ = wait.response.responder.send(AgentResponse::ok(
                wait.response.id,
                "waited",
                capture.clone(),
                Some(snapshot(&state, None)),
            ));
        } else {
            control.frame_waits.push_back(wait);
        }
    }

    let seconds_count = control.seconds_waits.len().min(limit);
    for _ in 0..seconds_count {
        let Some(mut wait) = control.seconds_waits.pop_front() else {
            break;
        };
        if wait.response.canceled.load(Ordering::Acquire) {
            continue;
        }
        wait.frames = wait.frames.saturating_add(1);
        let Some(time) = time.as_deref() else {
            send_timing_error(
                wait.response,
                "time_control_unavailable",
                "Time<Virtual> disappeared while wait_seconds was pending",
                &state,
                AgentTimingContext {
                    state: Some("unavailable"),
                    reason: Some("virtual_time_resource_missing"),
                    requested_seconds: Some(wait.requested.as_secs_f64()),
                    frames: Some(wait.frames),
                    ..default_context()
                },
            );
            continue;
        };
        let observed = time.elapsed().saturating_sub(wait.start);
        if observed >= wait.requested {
            let _ = wait
                .response
                .responder
                .send(AgentResponse::details_with_context(
                    wait.response.id,
                    "waited",
                    capture.clone(),
                    snapshot(&state, None),
                    serde_json::json!({
                        "requested_seconds": wait.requested.as_secs_f64(),
                        "observed_seconds": observed.as_secs_f64(),
                        "frames": wait.frames,
                    }),
                ));
        } else if wait.frames == wait.max_frames {
            send_timing_error(
                wait.response,
                "wait_seconds_timeout",
                "virtual time did not advance by the requested duration within max_frames",
                &state,
                AgentTimingContext {
                    requested_seconds: Some(wait.requested.as_secs_f64()),
                    observed_seconds: Some(observed.as_secs_f64()),
                    frames: Some(wait.frames),
                    ..default_context()
                },
            );
        } else {
            control.seconds_waits.push_back(wait);
        }
    }
}

pub(super) fn admit_timing_requests(
    config: Res<AgentFeedbackConfig>,
    virtual_time: Option<Res<Time<Virtual>>>,
    fixed_time: Option<Res<Time<Fixed>>>,
    strategy: Option<ResMut<TimeUpdateStrategy>>,
    mut control: ResMut<TimingControl>,
    state: Res<AgentFeedbackState>,
) {
    let limit = config.max_pending_commands.max(1);
    let count = control.incoming.len().min(limit);
    let mut strategy = strategy;
    for _ in 0..count {
        let Some(request) = control.incoming.pop_front() else {
            break;
        };
        if request.canceled.load(Ordering::Acquire) {
            continue;
        }
        let AgentRequest {
            id,
            command,
            responder,
            canceled,
        } = request;
        let response = pending_response(id, responder, canceled);
        match command {
            AgentCommand::Wait { frames } => control.frame_waits.push_back(FrameWait {
                response,
                frames_left: frames,
            }),
            AgentCommand::WaitSeconds {
                duration,
                max_frames,
            } => admit_seconds_wait(
                response,
                duration,
                max_frames,
                virtual_time.as_deref(),
                &mut control,
                &state,
            ),
            AgentCommand::AdvanceTime { duration, step } => admit_advance(
                response,
                duration,
                step,
                &config,
                virtual_time.as_deref(),
                fixed_time.as_deref(),
                strategy.as_deref_mut(),
                &mut control,
                &state,
            ),
            _ => unreachable!("only timing commands enter TimingControl"),
        }
    }
}

fn admit_seconds_wait(
    response: PendingResponse,
    requested: Duration,
    max_frames: u16,
    time: Option<&Time<Virtual>>,
    control: &mut TimingControl,
    state: &AgentFeedbackState,
) {
    let Some(time) = time else {
        send_unavailable(response, "virtual_time_resource_missing", state);
        return;
    };
    match control.availability {
        Availability::Available => send_timing_error(
            response,
            "time_control_frozen",
            "deterministic time is frozen; use advance_time instead",
            state,
            AgentTimingContext {
                state: Some("frozen"),
                requested_seconds: Some(requested.as_secs_f64()),
                ..default_context()
            },
        ),
        Availability::Unavailable(reason) => send_unavailable(response, reason, state),
        Availability::Disabled => control.seconds_waits.push_back(SecondsWait {
            response,
            requested,
            start: time.elapsed(),
            max_frames,
            frames: 0,
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn admit_advance(
    response: PendingResponse,
    requested: Duration,
    supplied_step: Option<Duration>,
    config: &AgentFeedbackConfig,
    virtual_time: Option<&Time<Virtual>>,
    fixed_time: Option<&Time<Fixed>>,
    strategy: Option<&mut TimeUpdateStrategy>,
    control: &mut TimingControl,
    state: &AgentFeedbackState,
) {
    match control.availability {
        Availability::Disabled => {
            send_timing_error(
                response,
                "time_control_disabled",
                "advance_time requires AgentFeedbackConfig::deterministic_time = true",
                state,
                AgentTimingContext {
                    state: Some("disabled"),
                    requested_seconds: Some(requested.as_secs_f64()),
                    ..default_context()
                },
            );
            return;
        }
        Availability::Unavailable(reason) => {
            send_unavailable(response, reason, state);
            return;
        }
        Availability::Available => {}
    }
    if control.advance.is_some() {
        let _ = response.responder.send(AgentResponse::error(
            response.id,
            "time_advance_busy",
            "another advance_time command is pending",
        ));
        return;
    }
    let (Some(time), Some(fixed), Some(strategy)) = (virtual_time, fixed_time, strategy) else {
        send_unavailable(response, "time_resource_disappeared", state);
        return;
    };
    if time.is_paused() {
        send_invalid_advance(response, "virtual_time_paused", requested, state);
        return;
    }
    if time.relative_speed_f64() != 1.0 {
        send_invalid_advance(response, "virtual_time_speed_not_one", requested, state);
        return;
    }
    let nominal_step = supplied_step.unwrap_or_else(|| fixed.timestep());
    if nominal_step.is_zero() {
        send_invalid_advance(response, "zero_nominal_step", requested, state);
        return;
    }
    if nominal_step > time.max_delta() {
        send_invalid_advance(response, "nominal_step_exceeds_max_delta", requested, state);
        return;
    }
    let Some(step_count) = checked_step_count(requested, nominal_step) else {
        send_invalid_advance(response, "step_count_overflow", requested, state);
        return;
    };
    if step_count > config.max_time_advance_steps.max(1) {
        send_invalid_advance(response, "step_count_exceeds_limit", requested, state);
        return;
    }
    if requested > config.max_time_advance {
        send_invalid_advance(response, "duration_exceeds_limit", requested, state);
        return;
    }
    if !matches!(*strategy, TimeUpdateStrategy::ManualDuration(delta) if delta.is_zero()) {
        control.availability = Availability::Unavailable("runtime_strategy_conflict");
        send_unavailable(response, "runtime_strategy_conflict", state);
        return;
    }
    let current_step = nominal_step.min(requested);
    *strategy = TimeUpdateStrategy::ManualDuration(current_step);
    control.advance = Some(PendingAdvance {
        response,
        requested,
        remaining: requested,
        nominal_step,
        current_step,
        start: time.elapsed(),
        previous: time.elapsed(),
        step_count: 0,
        admitted_frame: state.frame,
    });
}

fn checked_step_count(duration: Duration, step: Duration) -> Option<u16> {
    let duration_nanos = duration.as_nanos();
    let step_nanos = step.as_nanos();
    if step_nanos == 0 {
        return None;
    }
    u16::try_from(duration_nanos.div_ceil(step_nanos)).ok()
}

pub(super) fn guard_time_advance(
    mut strategy: Option<ResMut<TimeUpdateStrategy>>,
    mut control: ResMut<TimingControl>,
    state: Res<AgentFeedbackState>,
) {
    let Some(advance) = control.advance.as_ref() else {
        if matches!(control.availability, Availability::Available) {
            match strategy.as_deref_mut() {
                Some(TimeUpdateStrategy::ManualDuration(delta)) if delta.is_zero() => {}
                Some(strategy) => {
                    *strategy = TimeUpdateStrategy::ManualDuration(Duration::ZERO);
                    control.availability = Availability::Unavailable("runtime_strategy_conflict");
                }
                None => {
                    control.availability =
                        Availability::Unavailable("time_strategy_resource_missing");
                }
            }
        }
        return;
    };
    let canceled = advance.response.canceled.load(Ordering::Acquire);
    let expected = advance.current_step;
    let strategy_matches = strategy.as_deref().is_some_and(
        |value| matches!(*value, TimeUpdateStrategy::ManualDuration(delta) if delta == expected),
    );
    if !canceled && strategy_matches {
        return;
    }
    if let Some(strategy) = strategy.as_deref_mut() {
        *strategy = TimeUpdateStrategy::ManualDuration(Duration::ZERO);
    }
    let Some(advance) = control.advance.take() else {
        return;
    };
    if !canceled {
        control.availability = Availability::Unavailable("runtime_strategy_conflict");
        send_timing_error(
            advance.response,
            "time_control_unavailable",
            "time update strategy changed before the armed delta could be consumed",
            &state,
            AgentTimingContext {
                state: Some("unavailable"),
                reason: Some("runtime_strategy_conflict"),
                expected_seconds: Some(expected.as_secs_f64()),
                ..default_context()
            },
        );
    }
}

pub(super) fn account_time_advance(
    virtual_time: Option<Res<Time<Virtual>>>,
    strategy: Option<ResMut<TimeUpdateStrategy>>,
    mut control: ResMut<TimingControl>,
    mut state: ResMut<AgentFeedbackState>,
) {
    if let Some(time) = virtual_time.as_deref() {
        state.game_time_secs = time.elapsed_secs_f64();
    }
    let Some(mut advance) = control.advance.take() else {
        return;
    };
    if advance.admitted_frame == state.frame {
        control.advance = Some(advance);
        return;
    }
    let Some(mut strategy) = strategy else {
        if !advance.response.canceled.load(Ordering::Acquire) {
            send_unavailable(advance.response, "time_strategy_resource_missing", &state);
        }
        control.availability = Availability::Unavailable("time_strategy_resource_missing");
        return;
    };
    if advance.response.canceled.load(Ordering::Acquire) {
        *strategy = TimeUpdateStrategy::ManualDuration(Duration::ZERO);
        return;
    }
    let Some(time) = virtual_time.as_deref() else {
        *strategy = TimeUpdateStrategy::ManualDuration(Duration::ZERO);
        control.availability = Availability::Unavailable("virtual_time_resource_missing");
        send_unavailable(advance.response, "virtual_time_resource_missing", &state);
        return;
    };
    let actual_step = time.elapsed().saturating_sub(advance.previous);
    let strategy_matches = matches!(*strategy, TimeUpdateStrategy::ManualDuration(delta) if delta == advance.current_step);
    if actual_step != advance.current_step || !strategy_matches {
        *strategy = TimeUpdateStrategy::ManualDuration(Duration::ZERO);
        control.availability = Availability::Unavailable("advance_delta_mismatch");
        send_timing_error(
            advance.response,
            "time_advance_mismatch",
            "virtual time did not consume the exact armed delta",
            &state,
            AgentTimingContext {
                state: Some("unavailable"),
                reason: Some(if strategy_matches {
                    "virtual_delta_mismatch"
                } else {
                    "runtime_strategy_conflict"
                }),
                requested_seconds: Some(advance.requested.as_secs_f64()),
                expected_seconds: Some(advance.current_step.as_secs_f64()),
                actual_seconds: Some(actual_step.as_secs_f64()),
                step_count: Some(advance.step_count),
                ..default_context()
            },
        );
        return;
    }
    advance.remaining = advance.remaining.saturating_sub(actual_step);
    advance.previous = time.elapsed();
    advance.step_count = advance.step_count.saturating_add(1);
    if !advance.remaining.is_zero() {
        advance.current_step = advance.nominal_step.min(advance.remaining);
        *strategy = TimeUpdateStrategy::ManualDuration(advance.current_step);
        control.advance = Some(advance);
        return;
    }
    *strategy = TimeUpdateStrategy::ManualDuration(Duration::ZERO);
    let actual = time.elapsed().saturating_sub(advance.start);
    if actual != advance.requested {
        send_timing_error(
            advance.response,
            "time_advance_mismatch",
            "total virtual-time advancement did not equal the requested duration",
            &state,
            AgentTimingContext {
                requested_seconds: Some(advance.requested.as_secs_f64()),
                expected_seconds: Some(advance.requested.as_secs_f64()),
                actual_seconds: Some(actual.as_secs_f64()),
                step_count: Some(advance.step_count),
                ..default_context()
            },
        );
        return;
    }
    assert_eq!(
        actual, advance.requested,
        "successful deterministic advancement must be exact"
    );
    let result = AgentTimingResult {
        start_seconds: advance.start.as_secs_f64(),
        end_seconds: time.elapsed_secs_f64(),
        actual_seconds: actual.as_secs_f64(),
        step_count: advance.step_count,
    };
    let _ = advance.response.responder.send(AgentResponse::timing(
        advance.response.id,
        state.latest_capture.clone(),
        snapshot(&state, None),
        result,
    ));
}

fn pending_response(
    id: Value,
    responder: SyncSender<AgentResponse>,
    canceled: Arc<AtomicBool>,
) -> PendingResponse {
    PendingResponse {
        id,
        responder,
        canceled,
    }
}

fn send_invalid_advance(
    response: PendingResponse,
    reason: &'static str,
    requested: Duration,
    state: &AgentFeedbackState,
) {
    send_timing_error(
        response,
        "invalid_time_advance",
        "advance_time cannot run with the current virtual-time configuration",
        state,
        AgentTimingContext {
            reason: Some(reason),
            requested_seconds: Some(requested.as_secs_f64()),
            ..default_context()
        },
    );
}

fn send_unavailable(response: PendingResponse, reason: &'static str, state: &AgentFeedbackState) {
    send_timing_error(
        response,
        "time_control_unavailable",
        "add AgentFeedbackPlugin after TimePlugin and preserve Automatic time configuration",
        state,
        AgentTimingContext {
            state: Some("unavailable"),
            reason: Some(reason),
            ..default_context()
        },
    );
}

fn send_timing_error(
    response: PendingResponse,
    code: &'static str,
    message: &'static str,
    state: &AgentFeedbackState,
    timing: AgentTimingContext,
) {
    let _ = response.responder.send(AgentResponse::contextual_error(
        response.id,
        code,
        message,
        AgentErrorContext {
            latest_capture: state.latest_capture.clone(),
            snapshot: Some(snapshot(state, None)),
            timing: Some(timing),
            ..AgentErrorContext::default()
        },
    ));
}

fn default_context() -> AgentTimingContext {
    AgentTimingContext::default()
}
