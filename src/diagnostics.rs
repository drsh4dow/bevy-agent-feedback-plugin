mod predicates;
mod targets;

use crate::{
    control::{AgentFeedbackSet, AgentFeedbackState},
    protocol::{
        AgentCommand, AgentErrorContext, AgentResponse, AgentSnapshot, DiagnosticErrorContext,
        EcsSummaryContext, ObservedPredicate, Predicate, PredicateOutcome, WindowInfo,
    },
    runtime::AgentRequest,
};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use predicates::{EvaluationError, PredicateRegistry, RegistryError};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
    },
};
use targets::RegisteredMarker as TargetMarker;

const MAX_DIAGNOSTIC_ENTITIES: usize = 256;
const MAX_DIAGNOSTIC_CAMERAS: usize = 32;

/// Optional diagnostics commands and explicitly registered semantic readers.
///
/// Add this next to [`crate::AgentFeedbackPlugin`] with the `diagnostics` Cargo
/// feature. Registration keys are exact short Rust type names. Duplicate keys,
/// short-name collisions, oversized keys, and more than 128 registrations panic
/// during plugin construction as configuration errors.
#[derive(Clone, Default)]
pub struct AgentFeedbackDiagnosticsPlugin {
    registry: PredicateRegistry,
}

impl AgentFeedbackDiagnosticsPlugin {
    /// Registers a Bevy state type for state diagnostics and predicates.
    #[must_use]
    pub fn with_state<S: States>(mut self) -> Self {
        registration(self.registry.register_state::<S>());
        self
    }

    /// Registers a marker component for diagnostics, predicates, and targets.
    #[must_use]
    pub fn with_marker<T: Component>(mut self) -> Self {
        registration(self.registry.register_marker::<T>());
        self
    }

    /// Registers one bounded scalar field reader for a Bevy resource.
    ///
    /// The reader is invoked only by diagnostics commands. Its result must be a
    /// valid [`crate::DiagnosticValue`]: null, boolean, finite number, or a UTF-8
    /// string no longer than 1024 bytes.
    #[must_use]
    pub fn with_resource_field<R, V, F>(mut self, field: impl Into<String>, reader: F) -> Self
    where
        R: Resource,
        V: Into<crate::DiagnosticValue>,
        F: Fn(&R) -> V + Send + Sync + 'static,
    {
        registration(
            self.registry
                .register_resource_field::<R, V, F>(field, reader),
        );
        self
    }
}

fn registration(result: Result<(), RegistryError>) {
    if let Err(error) = result {
        panic!(
            "diagnostics registration failed [{}]: {} ({})",
            error.code, error.message, error.details
        );
    }
}

impl Plugin for AgentFeedbackDiagnosticsPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(AgentDiagnosticsQueue {
            requests: VecDeque::new(),
            waits: VecDeque::new(),
            resolved_clicks: VecDeque::new(),
            registry: self.registry.clone(),
            capacity: 1,
        })
        .add_systems(
            PreUpdate,
            answer_diagnostics.in_set(AgentFeedbackSet::DiagnosticEvaluation),
        );
    }
}

#[derive(Resource)]
pub(crate) struct AgentDiagnosticsQueue {
    requests: VecDeque<AgentRequest>,
    waits: VecDeque<PendingWait>,
    resolved_clicks: VecDeque<ResolvedClick>,
    registry: PredicateRegistry,
    capacity: usize,
}

impl AgentDiagnosticsQueue {
    pub(crate) fn enqueue(&mut self, request: AgentRequest, max_requests: usize) -> bool {
        self.capacity = max_requests.max(1);
        if self.total_pending() >= self.capacity {
            let _ = request.responder.send(AgentResponse::error(
                request.id,
                "queue_full",
                "diagnostics requests, waits, and resolved clicks reached max_pending_commands",
            ));
            return false;
        }
        self.requests.push_back(request);
        true
    }

    fn total_pending(&self) -> usize {
        self.requests.len() + self.waits.len() + self.resolved_clicks.len()
    }

    pub(crate) fn pop_resolved_click(&mut self) -> Option<ResolvedClick> {
        self.resolved_clicks.pop_front()
    }

    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }
}

pub(crate) struct ResolvedClick {
    pub(crate) id: Value,
    pub(crate) responder: SyncSender<AgentResponse>,
    pub(crate) canceled: Arc<AtomicBool>,
    pub(crate) position: Vec2,
    pub(crate) button: MouseButton,
    pub(crate) frames: u16,
    pub(crate) details: Value,
}

struct PendingWait {
    id: Value,
    responder: SyncSender<AgentResponse>,
    canceled: Arc<AtomicBool>,
    predicate: Predicate,
    abort_predicates: Vec<Predicate>,
    max_frames: u16,
    future_evaluations: u16,
}

fn answer_diagnostics(world: &mut World) {
    let Some(mut queue) = world.remove_resource::<AgentDiagnosticsQueue>() else {
        return;
    };
    evaluate_pending_waits(world, &mut queue);
    let request_count = queue.requests.len().min(queue.capacity);
    for _ in 0..request_count {
        let Some(request) = queue.requests.pop_front() else {
            break;
        };
        if request.canceled.load(Ordering::Relaxed) {
            continue;
        }
        answer_request(world, &mut queue, request);
    }
    world.insert_resource(queue);
}

fn evaluate_pending_waits(world: &mut World, queue: &mut AgentDiagnosticsQueue) {
    let count = queue.waits.len().min(queue.capacity);
    for _ in 0..count {
        let Some(mut wait) = queue.waits.pop_front() else {
            break;
        };
        if wait.canceled.load(Ordering::Relaxed) {
            continue;
        }
        wait.future_evaluations = wait.future_evaluations.saturating_add(1);
        match queue.registry.evaluate(world, &wait.predicate) {
            Ok(observed) if observed.outcome == PredicateOutcome::Matched => {
                send_observation(
                    world,
                    wait.responder,
                    wait.id,
                    "predicate_matched",
                    observed,
                );
            }
            Ok(observed) => match matching_abort(&queue.registry, world, &wait.abort_predicates) {
                Ok(Some(abort)) => send_abort(world, wait.responder, wait.id, abort),
                Ok(None) if wait.future_evaluations == wait.max_frames => {
                    let context = error_context(world, Some(observed));
                    let _ = wait.responder.send(AgentResponse::contextual_error(
                        wait.id,
                        "predicate_timeout",
                        format!(
                            "predicate did not match after exactly {} future evaluations",
                            wait.max_frames
                        ),
                        context,
                    ));
                }
                Ok(None) => queue.waits.push_back(wait),
                Err(error) => send_evaluation_error(wait.responder, wait.id, error),
            },
            Err(error) => send_evaluation_error(wait.responder, wait.id, error),
        }
    }
}

fn answer_request(world: &mut World, queue: &mut AgentDiagnosticsQueue, request: AgentRequest) {
    let AgentRequest {
        id,
        command,
        responder,
        canceled,
    } = request;
    match command {
        AgentCommand::EcsSummary => send_details(responder, id, "ok", ecs_summary(world)),
        AgentCommand::ListEntities => send_details(responder, id, "ok", list_entities(world)),
        AgentCommand::CameraInfo => send_details(responder, id, "ok", camera_info(world)),
        AgentCommand::StateInfo => {
            send_details(responder, id, "ok", state_info(&queue.registry, world))
        }
        AgentCommand::MarkerInfo => {
            send_details(responder, id, "ok", marker_info(&queue.registry, world))
        }
        AgentCommand::ResourceInfo { resource, field } => {
            match queue
                .registry
                .resource_info(world, resource.as_deref(), field.as_deref())
            {
                Ok(details) => send_serialized(responder, id, "resource_info", &details),
                Err(error) => send_evaluation_error(responder, id, error),
            }
        }
        AgentCommand::EvaluatePredicate { predicate } => {
            match queue.registry.evaluate(world, &predicate) {
                Ok(observed) => {
                    send_observation(world, responder, id, "predicate_evaluated", observed)
                }
                Err(error) => send_evaluation_error(responder, id, error),
            }
        }
        AgentCommand::WaitFor {
            predicate,
            abort_predicates,
            max_frames,
        } => match queue.registry.evaluate(world, &predicate) {
            Ok(observed) if observed.outcome == PredicateOutcome::Matched => {
                send_observation(world, responder, id, "predicate_matched", observed);
            }
            Ok(_) => match matching_abort(&queue.registry, world, &abort_predicates) {
                Ok(Some(abort)) => send_abort(world, responder, id, abort),
                Ok(None) => queue.waits.push_back(PendingWait {
                    id,
                    responder,
                    canceled,
                    predicate,
                    abort_predicates,
                    max_frames,
                    future_evaluations: 0,
                }),
                Err(error) => send_evaluation_error(responder, id, error),
            },
            Err(error) => send_evaluation_error(responder, id, error),
        },
        AgentCommand::TargetInfo {
            target,
            kind,
            camera,
        } => match resolve_target(&queue.registry, world, &target, kind, camera.as_deref()) {
            Ok(target) => send_serialized(responder, id, "target_info", &target),
            Err(error) => {
                send_diagnostic_error(responder, id, error.code, error.message, &error.details)
            }
        },
        AgentCommand::ClickTarget {
            target,
            kind,
            camera,
            button,
            frames,
        } => match resolve_target(&queue.registry, world, &target, kind, camera.as_deref()) {
            Ok(target) => {
                let position = Vec2::from_array(target.center);
                let mut details =
                    serde_json::to_value(&target).expect("resolved target serializes");
                let object = details
                    .as_object_mut()
                    .expect("resolved target serializes as an object");
                object.insert("target_resolved".to_string(), Value::Bool(true));
                object.insert("logical_position".to_string(), json!(target.center));
                queue.resolved_clicks.push_back(ResolvedClick {
                    id,
                    responder,
                    canceled,
                    position,
                    button,
                    frames,
                    details,
                });
            }
            Err(error) => {
                send_diagnostic_error(responder, id, error.code, error.message, &error.details)
            }
        },
        _ => {
            let _ = responder.send(AgentResponse::error(
                id,
                "invalid_diagnostics_command",
                "command was routed to diagnostics incorrectly",
            ));
        }
    }
}

fn matching_abort(
    registry: &PredicateRegistry,
    world: &mut World,
    predicates: &[Predicate],
) -> Result<Option<ObservedPredicate>, EvaluationError> {
    for predicate in predicates {
        let observed = registry.evaluate(world, predicate)?;
        if observed.outcome == PredicateOutcome::Matched {
            return Ok(Some(observed));
        }
    }
    Ok(None)
}

fn send_abort(
    world: &mut World,
    responder: SyncSender<AgentResponse>,
    id: Value,
    observed: ObservedPredicate,
) {
    let context = error_context(world, Some(observed));
    let _ = responder.send(AgentResponse::contextual_error(
        id,
        "predicate_aborted",
        "abort predicate matched",
        context,
    ));
}

fn send_observation(
    world: &mut World,
    responder: SyncSender<AgentResponse>,
    id: Value,
    status: &'static str,
    observed: ObservedPredicate,
) {
    let details = serde_json::to_value(&observed).expect("predicate observation serializes");
    let (latest_capture, snapshot) = diagnostic_snapshot(world);
    let _ = responder.send(AgentResponse::details_with_context(
        id,
        status,
        latest_capture,
        snapshot,
        details,
    ));
}

fn send_evaluation_error(responder: SyncSender<AgentResponse>, id: Value, error: EvaluationError) {
    send_diagnostic_error(responder, id, error.code, error.message, &error.details);
}

fn send_diagnostic_error(
    responder: SyncSender<AgentResponse>,
    id: Value,
    code: &'static str,
    message: String,
    details: &Value,
) {
    let context = AgentErrorContext {
        diagnostic: Some(bounded_diagnostic_details(details)),
        ..Default::default()
    };
    let _ = responder.send(AgentResponse::contextual_error(id, code, message, context));
}

fn bounded_diagnostic_details(details: &Value) -> DiagnosticErrorContext {
    let strings = |key: &str, limit: usize| {
        details
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .take(limit)
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    DiagnosticErrorContext {
        reason: details
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_string),
        entity: details
            .get("entity")
            .and_then(Value::as_str)
            .map(str::to_string),
        resource: details
            .get("resource")
            .and_then(Value::as_str)
            .map(str::to_string),
        field: details
            .get("field")
            .and_then(Value::as_str)
            .map(str::to_string),
        limit: details
            .get("limit")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok()),
        candidates: strings("candidates", 16),
        registered: {
            let mut values = strings("registered", 128);
            values.extend(strings(
                "registered_fields",
                128usize.saturating_sub(values.len()),
            ));
            values
        },
    }
}

fn send_serialized<T: Serialize>(
    responder: SyncSender<AgentResponse>,
    id: Value,
    status: &'static str,
    details: &T,
) {
    let details = serde_json::to_value(details).expect("bounded diagnostic details serialize");
    send_details(responder, id, status, details);
}

fn send_details(
    responder: SyncSender<AgentResponse>,
    id: Value,
    status: &'static str,
    details: Value,
) {
    let _ = responder.send(AgentResponse::details(id, status, details));
}

fn diagnostic_snapshot(world: &mut World) -> (Option<crate::protocol::CaptureInfo>, AgentSnapshot) {
    let state = world.resource::<AgentFeedbackState>();
    let latest_capture = state.latest_capture.clone();
    let mut snapshot = AgentSnapshot {
        frame: state.frame,
        game_time_secs: state.game_time_secs,
        window: None,
        mouse_position: state.cursor_position.map(|position| position.to_array()),
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
    };
    let mut windows = world.query_filtered::<&Window, With<PrimaryWindow>>();
    snapshot.window = windows.iter(world).next().map(WindowInfo::from_window);
    (latest_capture, snapshot)
}

fn error_context(world: &mut World, observed: Option<ObservedPredicate>) -> AgentErrorContext {
    let (latest_capture, snapshot) = diagnostic_snapshot(world);
    AgentErrorContext {
        latest_capture,
        snapshot: Some(snapshot),
        observed_predicate: observed,
        ecs_summary: Some(ecs_summary_context(world)),
        ..Default::default()
    }
}

fn ecs_summary_context(world: &World) -> EcsSummaryContext {
    let count = world
        .iter_entities()
        .take(MAX_DIAGNOSTIC_ENTITIES + 1)
        .count();
    EcsSummaryContext {
        entity_count: count.min(MAX_DIAGNOSTIC_ENTITIES),
        entity_count_is_lower_bound: count > MAX_DIAGNOSTIC_ENTITIES,
        component_count: world.components().iter_registered().count(),
        archetype_count: world.archetypes().len(),
    }
}

fn state_info(registry: &PredicateRegistry, world: &World) -> Value {
    let states = registry
        .states
        .iter()
        .filter_map(|state| {
            (state.read)(world)
                .ok()
                .map(|value| json!({"type": state.rust_type, "value": value}))
        })
        .collect::<Vec<_>>();
    json!({"states": states})
}

fn marker_info(registry: &PredicateRegistry, world: &mut World) -> Value {
    let markers = registry
        .markers
        .iter()
        .map(|marker| {
            let count = (marker.count)(world);
            json!({
                "name": marker.key,
                "count": count.value,
                "truncated": count.is_lower_bound,
                "count_is_lower_bound": count.is_lower_bound,
                "entities": count.entities.iter().map(|entity| format!("{entity:?}")).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    json!({"markers": markers})
}

fn resolve_target(
    registry: &PredicateRegistry,
    world: &mut World,
    target: &crate::TargetSelector,
    kind: crate::TargetKind,
    camera: Option<&str>,
) -> Result<targets::ResolvedTarget, targets::TargetError> {
    let markers = registry
        .markers
        .iter()
        .map(|marker| TargetMarker {
            name: &marker.key,
            entities: marker.entities,
        })
        .collect::<Vec<_>>();
    targets::resolve_target(world, &markers, target, kind, camera)
}

fn ecs_summary(world: &World) -> Value {
    serde_json::to_value(ecs_summary_context(world)).expect("ECS summary serializes")
}

fn list_entities(world: &World) -> Value {
    let components = world.components();
    let mut entities = Vec::new();
    let mut truncated = false;
    for entity_ref in world.iter_entities().take(MAX_DIAGNOSTIC_ENTITIES + 1) {
        if entities.len() == MAX_DIAGNOSTIC_ENTITIES {
            truncated = true;
            break;
        }
        let names = entity_ref
            .archetype()
            .iter_components()
            .filter_map(|id| components.get_info(id))
            .map(|info| info.name().to_string())
            .collect::<Vec<_>>();
        entities.push(json!({"entity": format!("{:?}", entity_ref.id()), "components": names}));
    }
    json!({
        "total": entities.len() + usize::from(truncated),
        "total_is_lower_bound": truncated,
        "truncated": truncated,
        "entities": entities,
    })
}

fn camera_info(world: &mut World) -> Value {
    let mut query = world.query::<(
        Entity,
        &Camera,
        Option<&GlobalTransform>,
        Option<&Projection>,
    )>();
    let mut cameras = Vec::new();
    let mut truncated = false;
    for (entity, camera, transform, projection) in
        query.iter(world).take(MAX_DIAGNOSTIC_CAMERAS + 1)
    {
        if cameras.len() == MAX_DIAGNOSTIC_CAMERAS {
            truncated = true;
            break;
        }
        cameras.push(json!({
            "entity": format!("{entity:?}"),
            "is_active": camera.is_active,
            "order": camera.order,
            "viewport": camera.viewport.as_ref().map(|viewport| json!({
                "physical_position": viewport.physical_position.to_array(),
                "physical_size": viewport.physical_size.to_array(),
            })),
            "translation": transform.map(|value| value.translation().to_array()),
            "projection": projection.map(|value| format!("{value:?}")),
        }));
    }
    json!({
        "total": cameras.len() + usize::from(truncated),
        "total_is_lower_bound": truncated,
        "truncated": truncated,
        "cameras": cameras,
    })
}

#[cfg(test)]
mod tests;
