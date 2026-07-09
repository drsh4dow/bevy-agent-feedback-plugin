use super::*;
use crate::{Predicate, TargetKind, TargetSelector};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::AtomicBool,
    mpsc::{Receiver, TryRecvError, sync_channel},
};

#[derive(States, Clone, Debug, Default, Eq, Hash, PartialEq)]
enum TestPhase {
    #[default]
    Loading,
}

#[derive(Component)]
struct TestMarker;

#[derive(Resource)]
struct TestResource(u32);

fn request(command: AgentCommand) -> (AgentRequest, Receiver<AgentResponse>) {
    let (responder, receiver) = sync_channel(1);
    (
        AgentRequest {
            id: json!("request"),
            command,
            responder,
            canceled: Arc::new(AtomicBool::new(false)),
        },
        receiver,
    )
}

fn diagnostics_world(registry: PredicateRegistry, request: AgentRequest) -> World {
    let mut world = World::new();
    world.insert_resource(AgentFeedbackState::default());
    world.insert_resource(AgentDiagnosticsQueue {
        requests: VecDeque::from([request]),
        waits: VecDeque::new(),
        resolved_clicks: VecDeque::new(),
        registry,
        capacity: 1,
    });
    world
}

fn response_value(receiver: &Receiver<AgentResponse>) -> Value {
    serde_json::to_value(
        receiver
            .try_recv()
            .expect("diagnostics evaluation should have responded"),
    )
    .expect("agent response should serialize")
}

#[test]
fn wait_for_matches_during_its_admission_evaluation() {
    let mut registry = PredicateRegistry::default();
    registry
        .register_marker::<TestMarker>()
        .expect("marker registration should succeed");
    let predicate = Predicate::MarkerCount {
        marker: "TestMarker".to_string(),
        min: Some(1),
        max: None,
    };
    let (request, receiver) = request(AgentCommand::WaitFor {
        predicate: predicate.clone(),
        max_frames: 4,
    });
    let mut world = diagnostics_world(registry, request);
    world.spawn(TestMarker);

    answer_diagnostics(&mut world);

    let response = response_value(&receiver);
    assert_eq!(response["result"]["status"], "predicate_matched");
    assert_eq!(
        response["result"]["details"],
        json!({
            "predicate": predicate,
            "outcome": "matched",
            "count": 1
        })
    );
}

#[test]
fn wait_for_times_out_only_on_the_exact_future_evaluation_boundary() {
    let mut registry = PredicateRegistry::default();
    registry
        .register_marker::<TestMarker>()
        .expect("marker registration should succeed");
    let predicate = Predicate::MarkerCount {
        marker: "TestMarker".to_string(),
        min: Some(2),
        max: None,
    };
    let (request, receiver) = request(AgentCommand::WaitFor {
        predicate: predicate.clone(),
        max_frames: 2,
    });
    let mut world = diagnostics_world(registry, request);
    world.spawn(TestMarker);
    for _ in 0..256 {
        world.spawn_empty();
    }

    answer_diagnostics(&mut world);
    assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));

    answer_diagnostics(&mut world);
    assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));

    answer_diagnostics(&mut world);
    let response = response_value(&receiver);
    assert_eq!(response["error"]["code"], "predicate_timeout");
    assert_eq!(
        response["error"]["message"],
        "predicate did not match after exactly 2 future evaluations"
    );
    assert_eq!(
        response["error"]["context"]["observed_predicate"],
        json!({
            "predicate": predicate,
            "outcome": "not_matched",
            "count": 1
        })
    );
    assert_eq!(
        response["error"]["context"]["ecs_summary"]["entity_count"],
        256
    );
    assert_eq!(
        response["error"]["context"]["ecs_summary"]["entity_count_is_lower_bound"],
        true
    );
}

#[test]
fn duplicate_registrations_return_the_specific_configuration_error() {
    let mut states = PredicateRegistry::default();
    states
        .register_state::<TestPhase>()
        .expect("first state registration should succeed");
    let state_error = states
        .register_state::<TestPhase>()
        .expect_err("duplicate state registration should fail");
    assert_eq!(state_error.code, "duplicate_state_registration");
    assert_eq!(state_error.details, json!({"state": "TestPhase"}));

    let mut markers = PredicateRegistry::default();
    markers
        .register_marker::<TestMarker>()
        .expect("first marker registration should succeed");
    let marker_error = markers
        .register_marker::<TestMarker>()
        .expect_err("duplicate marker registration should fail");
    assert_eq!(marker_error.code, "duplicate_marker_registration");
    assert_eq!(marker_error.details, json!({"marker": "TestMarker"}));

    let mut fields = PredicateRegistry::default();
    fields
        .register_resource_field::<TestResource, _, _>("value", |resource| resource.0)
        .expect("first resource field registration should succeed");
    let field_error = fields
        .register_resource_field::<TestResource, _, _>("value", |resource| resource.0)
        .expect_err("duplicate resource field registration should fail");
    assert_eq!(field_error.code, "duplicate_resource_field_registration");
    assert_eq!(
        field_error.details,
        json!({"resource": "TestResource", "field": "value"})
    );
}

#[test]
fn target_identity_errors_distinguish_miss_ambiguity_and_bounded_search() {
    let selector = TargetSelector::Name("Target".to_string());

    let mut empty_world = World::new();
    let missing = targets::resolve_target(&mut empty_world, &[], &selector, TargetKind::Any, None)
        .expect_err("an empty world cannot contain the target");
    assert_eq!(missing.code, "target_not_found");
    assert_eq!(missing.details, json!({"scanned": 0}));

    let mut ambiguous_world = World::new();
    let candidates = (0..17)
        .map(|_| ambiguous_world.spawn(Name::new("Target")).id())
        .collect::<Vec<_>>();
    let ambiguous =
        targets::resolve_target(&mut ambiguous_world, &[], &selector, TargetKind::Any, None)
            .expect_err("multiple exact names must not resolve by iteration order");
    assert_eq!(ambiguous.code, "ambiguous_target");
    assert_eq!(ambiguous.details["count"], 17);
    assert_eq!(
        ambiguous.details["candidates"],
        json!(
            candidates
                .iter()
                .take(16)
                .map(|entity| format!("{entity:?}"))
                .collect::<Vec<_>>()
        )
    );
    assert_eq!(ambiguous.details["candidate_details_truncated"], true);

    let mut truncated_world = World::new();
    let candidate = truncated_world.spawn(Name::new("Target")).id();
    for _ in 0..256 {
        truncated_world.spawn(Name::new("Filler"));
    }
    let truncated =
        targets::resolve_target(&mut truncated_world, &[], &selector, TargetKind::Any, None)
            .expect_err("a bounded scan cannot claim a unique target");
    assert_eq!(truncated.code, "target_search_truncated");
    assert_eq!(
        truncated.details,
        json!({
            "limit": 256,
            "candidates": [format!("{candidate:?}")],
            "candidate_count_is_lower_bound": true
        })
    );
    let candidate_status =
        targets::target_resolvability(&mut truncated_world, &[], &selector, TargetKind::Any, None)
            .expect("bounded candidate search should produce a tri-state result");
    assert!(
        matches!(
            candidate_status,
            targets::TargetResolvability::PresentUnresolved
        ),
        "unexpected candidate status: {candidate_status:?}"
    );

    let mut indeterminate_world = World::new();
    for _ in 0..257 {
        indeterminate_world.spawn(Name::new("Filler"));
    }
    let miss_status = targets::target_resolvability(
        &mut indeterminate_world,
        &[],
        &selector,
        TargetKind::Any,
        None,
    )
    .expect("bounded miss should produce a tri-state result");
    assert!(
        matches!(miss_status, targets::TargetResolvability::Indeterminate),
        "unexpected bounded-miss status: {miss_status:?}"
    );
}
