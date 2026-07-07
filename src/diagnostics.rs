use crate::{
    protocol::{AgentCommand, AgentResponse},
    runtime::AgentRequest,
};
use bevy::{prelude::*, state::state::State};
use serde_json::{Value, json};
use std::{any::type_name, collections::VecDeque};

const MAX_DIAGNOSTIC_ENTITIES: usize = 256;
const MAX_DIAGNOSTIC_CAMERAS: usize = 32;

type StateReader = fn(&World) -> Option<Value>;

/// Optional diagnostics commands for agent debugging.
///
/// Add this next to [`crate::AgentFeedbackPlugin`] and enable the `diagnostics`
/// Cargo feature to serve `ecs_summary`, `list_entities`, `camera_info`, and
/// `state_info` commands.
#[derive(Default)]
pub struct AgentFeedbackDiagnosticsPlugin {
    state_readers: Vec<StateReader>,
}

impl AgentFeedbackDiagnosticsPlugin {
    /// Registers a Bevy state type for the `state_info` command.
    #[must_use]
    pub fn with_state<S: States>(mut self) -> Self {
        self.state_readers.push(read_state::<S>);
        self
    }
}

impl Plugin for AgentFeedbackDiagnosticsPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(AgentDiagnosticsQueue {
            requests: VecDeque::new(),
            state_readers: self.state_readers.clone(),
        })
        .add_systems(PreUpdate, answer_diagnostics);
    }
}

#[derive(Resource)]
pub(crate) struct AgentDiagnosticsQueue {
    requests: VecDeque<AgentRequest>,
    state_readers: Vec<StateReader>,
}

impl AgentDiagnosticsQueue {
    pub(crate) fn enqueue(&mut self, request: AgentRequest, max_requests: usize) {
        if self.requests.len() >= max_requests {
            let _ = request.responder.send(AgentResponse::error(
                request.id,
                "queue_full",
                "diagnostics command queue is full",
            ));
            return;
        }
        self.requests.push_back(request);
    }
}

fn answer_diagnostics(world: &mut World) {
    let (requests, state_readers) = {
        let mut queue = world.resource_mut::<AgentDiagnosticsQueue>();
        (
            queue.requests.drain(..).collect::<Vec<_>>(),
            queue.state_readers.clone(),
        )
    };

    for request in requests {
        let result = match request.command {
            AgentCommand::EcsSummary => ecs_summary(world),
            AgentCommand::ListEntities => list_entities(world),
            AgentCommand::CameraInfo => camera_info(world),
            AgentCommand::StateInfo => state_info(world, &state_readers),
            _ => json!({"error": "not a diagnostics command"}),
        };
        let _ = request
            .responder
            .send(AgentResponse::details(request.id, "ok", result));
    }
}

fn ecs_summary(world: &World) -> Value {
    json!({
        "entity_count": world.iter_entities().count(),
        "component_count": world.components().iter_registered().count(),
        "archetype_count": world.archetypes().len(),
    })
}

fn list_entities(world: &World) -> Value {
    let components = world.components();
    let mut entities = Vec::new();
    let mut total = 0usize;
    for entity in world.iter_entities() {
        total += 1;
        if entities.len() >= MAX_DIAGNOSTIC_ENTITIES {
            continue;
        }
        let names = entity
            .archetype()
            .iter_components()
            .filter_map(|id| components.get_info(id))
            .map(|info| info.name().to_string())
            .collect::<Vec<_>>();
        entities.push(json!({
            "entity": format!("{:?}", entity.id()),
            "components": names,
        }));
    }
    json!({
        "entities": entities,
        "total": total,
        "truncated": total > MAX_DIAGNOSTIC_ENTITIES,
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
    let mut total = 0usize;
    for (entity, camera, transform, projection) in query.iter(world) {
        total += 1;
        if cameras.len() >= MAX_DIAGNOSTIC_CAMERAS {
            continue;
        }
        let translation = transform.map(|transform| transform.translation().to_array());
        cameras.push(json!({
            "entity": format!("{:?}", entity),
            "is_active": camera.is_active,
            "order": camera.order,
            "viewport": camera.viewport.as_ref().map(|viewport| json!({
                "physical_position": [viewport.physical_position.x, viewport.physical_position.y],
                "physical_size": [viewport.physical_size.x, viewport.physical_size.y],
            })),
            "translation": translation,
            "projection": projection.map(|projection| format!("{projection:?}")),
        }));
    }
    json!({
        "cameras": cameras,
        "total": total,
        "truncated": total > MAX_DIAGNOSTIC_CAMERAS,
    })
}

fn state_info(world: &World, readers: &[StateReader]) -> Value {
    let states = readers
        .iter()
        .filter_map(|reader| reader(world))
        .collect::<Vec<_>>();
    json!({ "states": states })
}

fn read_state<S: States>(world: &World) -> Option<Value> {
    let state = world.get_resource::<State<S>>()?;
    Some(json!({
        "type": type_name::<S>(),
        "value": format!("{:?}", state.get()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_summarize_world_entities() {
        let mut app = App::new();
        app.world_mut().spawn_empty();

        let summary = ecs_summary(app.world());
        assert!(summary["entity_count"].as_u64().unwrap() >= 1);

        let entities = list_entities(app.world());
        assert!(entities["total"].as_u64().unwrap() >= 1);
    }
}
