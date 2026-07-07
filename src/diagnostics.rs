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
type MarkerReader = fn(&mut World) -> BoundedEntityList;

struct BoundedEntityList {
    total: usize,
    total_is_lower_bound: bool,
    entities: Vec<Entity>,
}

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
    let entities = bounded_entities(
        world.iter_entities().map(|entity_ref| entity_ref.id()),
        MAX_DIAGNOSTIC_ENTITIES,
    );
    let mut result = json!({
        "entity_count": entities.total,
        "component_count": world.components().iter_registered().count(),
        "archetype_count": world.archetypes().len(),
    });
    if entities.total_is_lower_bound {
        result["entity_count_is_lower_bound"] = Value::Bool(true);
    }
    result
}

fn list_entities(world: &World) -> Value {
    let bounded = bounded_entities(
        world.iter_entities().map(|entity_ref| entity_ref.id()),
        MAX_DIAGNOSTIC_ENTITIES,
    );
    let components = world.components();
    let entities = bounded
        .entities
        .iter()
        .map(|entity| {
            let entity_ref = world.entity(*entity);
            let names = entity_ref
                .archetype()
                .iter_components()
                .filter_map(|id| components.get_info(id))
                .map(|info| info.name().to_string())
                .collect::<Vec<_>>();
            json!({
                "entity": format!("{entity:?}"),
                "components": names,
            })
        })
        .collect::<Vec<_>>();
    let mut result = json!({
        "entities": entities,
        "total": bounded.total,
        "truncated": bounded.total_is_lower_bound,
    });
    if bounded.total_is_lower_bound {
        result["total_is_lower_bound"] = Value::Bool(true);
    }
    result
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
    let mut total_is_lower_bound = false;
    for (entity, camera, transform, projection) in query.iter(world) {
        if cameras.len() >= MAX_DIAGNOSTIC_CAMERAS {
            total = MAX_DIAGNOSTIC_CAMERAS + 1;
            total_is_lower_bound = true;
            break;
        }
        total += 1;
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
    let mut result = json!({
        "cameras": cameras,
        "total": total,
        "truncated": total_is_lower_bound,
    });
    if total_is_lower_bound {
        result["total_is_lower_bound"] = Value::Bool(true);
    }
    result
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
            let bounded = (marker.reader)(world);
            let mut result = json!({
                "name": marker.name,
                "count": bounded.total,
                "entities": bounded
                    .entities
                    .iter()
                    .map(|entity| format!("{entity:?}"))
                    .collect::<Vec<_>>(),
                "truncated": bounded.total_is_lower_bound,
            });
            if bounded.total_is_lower_bound {
                result["count_is_lower_bound"] = Value::Bool(true);
            }
            result
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

fn read_marker<T: Component>(world: &mut World) -> BoundedEntityList {
    let mut query = world.query_filtered::<Entity, With<T>>();
    bounded_entities(query.iter(world), MAX_DIAGNOSTIC_ENTITIES)
}

fn bounded_entities(iter: impl Iterator<Item = Entity>, cap: usize) -> BoundedEntityList {
    let mut entities = Vec::new();
    let mut total = 0usize;
    for entity in iter {
        if entities.len() >= cap {
            return BoundedEntityList {
                total: cap + 1,
                total_is_lower_bound: true,
                entities,
            };
        }
        total += 1;
        entities.push(entity);
    }
    BoundedEntityList {
        total,
        total_is_lower_bound: false,
        entities,
    }
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
    fn bounded_entities_reports_exact_count_below_cap() {
        let mut app = App::new();
        let entities = (0..3)
            .map(|_| app.world_mut().spawn_empty().id())
            .collect::<Vec<_>>();

        let bounded = bounded_entities(entities.iter().copied(), 4);

        assert_eq!(bounded.total, 3);
        assert!(!bounded.total_is_lower_bound);
        assert_eq!(bounded.entities, entities);
    }

    #[test]
    fn bounded_entities_reports_lower_bound_above_cap() {
        let mut app = App::new();
        let entities = (0..3)
            .map(|_| app.world_mut().spawn_empty().id())
            .collect::<Vec<_>>();

        let bounded = bounded_entities(entities.iter().copied(), 2);

        assert_eq!(bounded.total, 3);
        assert!(bounded.total_is_lower_bound);
        assert_eq!(bounded.entities.as_slice(), &entities[..2]);
    }

    #[test]
    fn marker_info_reports_lower_bound_when_entity_list_is_capped() {
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
        let entities = marker["entities"].as_array().expect("entities");
        assert_eq!(marker["name"], "TestMarker");
        assert_eq!(
            marker["count"].as_u64().expect("marker count"),
            (MAX_DIAGNOSTIC_ENTITIES + 1) as u64
        );
        assert_eq!(marker["count_is_lower_bound"], Value::Bool(true));
        assert_eq!(marker["truncated"], Value::Bool(true));
        assert_eq!(entities.len(), MAX_DIAGNOSTIC_ENTITIES);
    }

    #[test]
    fn ecs_summary_reports_entity_lower_bound_when_capped() {
        let mut app = App::new();
        for _ in 0..MAX_DIAGNOSTIC_ENTITIES + 1 {
            app.world_mut().spawn_empty();
        }

        let summary = ecs_summary(app.world());

        assert_eq!(
            summary["entity_count"].as_u64().expect("entity count"),
            (MAX_DIAGNOSTIC_ENTITIES + 1) as u64
        );
        assert_eq!(summary["entity_count_is_lower_bound"], Value::Bool(true));
    }

    #[test]
    fn list_entities_reports_lower_bound_when_capped() {
        let mut app = App::new();
        for _ in 0..MAX_DIAGNOSTIC_ENTITIES + 1 {
            app.world_mut().spawn_empty();
        }

        let result = list_entities(app.world());
        let entities = result["entities"].as_array().expect("entities");

        assert_eq!(
            result["total"].as_u64().expect("entity total"),
            (MAX_DIAGNOSTIC_ENTITIES + 1) as u64
        );
        assert_eq!(result["total_is_lower_bound"], Value::Bool(true));
        assert_eq!(result["truncated"], Value::Bool(true));
        assert_eq!(entities.len(), MAX_DIAGNOSTIC_ENTITIES);
    }

    #[test]
    fn camera_info_reports_lower_bound_when_capped() {
        let mut app = App::new();
        for _ in 0..MAX_DIAGNOSTIC_CAMERAS + 1 {
            app.world_mut()
                .spawn((Camera::default(), Transform::default()));
        }

        let result = camera_info(app.world_mut());
        let cameras = result["cameras"].as_array().expect("cameras");

        assert_eq!(
            result["total"].as_u64().expect("camera total"),
            (MAX_DIAGNOSTIC_CAMERAS + 1) as u64
        );
        assert_eq!(result["total_is_lower_bound"], Value::Bool(true));
        assert_eq!(result["truncated"], Value::Bool(true));
        assert_eq!(cameras.len(), MAX_DIAGNOSTIC_CAMERAS);
    }
}
