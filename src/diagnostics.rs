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
type MarkerReader = fn(&mut World) -> (usize, Vec<Entity>);

#[derive(Clone)]
struct MarkerDiagnostic {
    name: String,
    reader: MarkerReader,
}

/// Optional diagnostics commands for agent debugging.
///
/// Add this next to [`crate::AgentFeedbackPlugin`] and enable the `diagnostics`
/// Cargo feature to serve `ecs_summary`, `list_entities`, `camera_info`,
/// `state_info`, and registered `marker_info` commands.
#[derive(Default)]
pub struct AgentFeedbackDiagnosticsPlugin {
    state_readers: Vec<StateReader>,
    marker_readers: Vec<MarkerDiagnostic>,
}

impl AgentFeedbackDiagnosticsPlugin {
    /// Registers a Bevy state type for the `state_info` command.
    #[must_use]
    pub fn with_state<S: States>(mut self) -> Self {
        self.state_readers.push(read_state::<S>);
        self
    }

    /// Registers a marker component type for the `marker_info` command.
    #[must_use]
    pub fn with_marker<T: Component>(mut self) -> Self {
        self.marker_readers.push(MarkerDiagnostic {
            name: short_type_name::<T>(),
            reader: read_marker::<T>,
        });
        self
    }
}

impl Plugin for AgentFeedbackDiagnosticsPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(AgentDiagnosticsQueue {
            requests: VecDeque::new(),
            state_readers: self.state_readers.clone(),
            marker_readers: self.marker_readers.clone(),
        })
        .add_systems(PreUpdate, answer_diagnostics);
    }
}

#[derive(Resource)]
pub(crate) struct AgentDiagnosticsQueue {
    requests: VecDeque<AgentRequest>,
    state_readers: Vec<StateReader>,
    marker_readers: Vec<MarkerDiagnostic>,
}

impl AgentDiagnosticsQueue {
    pub(crate) fn enqueue(&mut self, request: AgentRequest, max_requests: usize) -> bool {
        if self.requests.len() >= max_requests {
            let _ = request.responder.send(AgentResponse::error(
                request.id,
                "queue_full",
                "diagnostics command queue is full",
            ));
            return false;
        }
        self.requests.push_back(request);
        true
    }
}

fn answer_diagnostics(world: &mut World) {
    let (requests, state_readers, marker_readers) = {
        let mut queue = world.resource_mut::<AgentDiagnosticsQueue>();
        (
            queue.requests.drain(..).collect::<Vec<_>>(),
            queue.state_readers.clone(),
            queue.marker_readers.clone(),
        )
    };

    for request in requests {
        let result = match request.command {
            AgentCommand::EcsSummary => ecs_summary(world),
            AgentCommand::ListEntities => list_entities(world),
            AgentCommand::CameraInfo => camera_info(world),
            AgentCommand::StateInfo => state_info(world, &state_readers),
            AgentCommand::MarkerInfo => marker_info(world, &marker_readers),
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

fn marker_info(world: &mut World, readers: &[MarkerDiagnostic]) -> Value {
    let markers = readers
        .iter()
        .map(|marker| {
            let (total, entities) = (marker.reader)(world);
            json!({
                "name": marker.name,
                "count": total,
                "entities": entities
                    .iter()
                    .map(|entity| format!("{entity:?}"))
                    .collect::<Vec<_>>(),
                "truncated": total > MAX_DIAGNOSTIC_ENTITIES,
            })
        })
        .collect::<Vec<_>>();
    json!({ "markers": markers })
}

fn read_state<S: States>(world: &World) -> Option<Value> {
    let state = world.get_resource::<State<S>>()?;
    Some(json!({
        "type": type_name::<S>(),
        "value": format!("{:?}", state.get()),
    }))
}

fn read_marker<T: Component>(world: &mut World) -> (usize, Vec<Entity>) {
    let mut query = world.query_filtered::<Entity, With<T>>();
    let mut total = 0usize;
    let mut entities = Vec::new();
    for entity in query.iter(world) {
        total += 1;
        if entities.len() < MAX_DIAGNOSTIC_ENTITIES {
            entities.push(entity);
        }
    }
    (total, entities)
}

fn short_type_name<T>() -> String {
    type_name::<T>()
        .rsplit("::")
        .next()
        .unwrap_or(type_name::<T>())
        .to_string()
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

    #[derive(Component)]
    struct TestMarker;

    #[test]
    fn marker_info_preserves_total_count_when_entity_list_is_capped() {
        let mut app = App::new();
        app.add_plugins(AgentFeedbackDiagnosticsPlugin::default().with_marker::<TestMarker>());
        for _ in 0..MAX_DIAGNOSTIC_ENTITIES + 1 {
            app.world_mut().spawn(TestMarker);
        }
        app.update();

        let marker_readers = app
            .world()
            .resource::<AgentDiagnosticsQueue>()
            .marker_readers
            .clone();
        let result = marker_info(app.world_mut(), &marker_readers);
        let marker = &result["markers"][0];
        let count = marker["count"].as_u64().expect("marker count");
        let entities = marker["entities"].as_array().expect("entities");
        assert_eq!(marker["name"], "TestMarker");
        assert_eq!(count, (MAX_DIAGNOSTIC_ENTITIES + 1) as u64);
        assert_eq!(entities.len(), MAX_DIAGNOSTIC_ENTITIES);
        assert!(
            count > entities.len() as u64,
            "marker_info must keep the full count when capping entity details"
        );
        assert_eq!(marker["truncated"], Value::Bool(true));
    }
}
