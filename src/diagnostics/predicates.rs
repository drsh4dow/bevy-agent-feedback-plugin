use super::targets::{RegisteredMarker as TargetMarker, TargetResolvability, target_resolvability};
use crate::{ComparisonOperator, DiagnosticValue, ObservedPredicate, Predicate, PredicateOutcome};
use bevy::{prelude::*, state::state::State};
use serde::Serialize;
use serde_json::{Value, json};
use std::{any::type_name, sync::Arc};

const MAX_REGISTRATIONS: usize = 128;
const MAX_KEY_BYTES: usize = 128;
const MAX_STRING_BYTES: usize = 1024;
const MAX_MARKER_COUNT: usize = 256;

pub(super) type StateReadResult = Result<DiagnosticValue, ReaderError>;
pub(super) type StateReadFn = fn(&World) -> StateReadResult;
pub(super) type MarkerCountFn = fn(&mut World) -> BoundedCount;
pub(super) type MarkerEntitiesFn = fn(&mut World) -> super::targets::MarkerEntities;
type ResourceReadFn =
    dyn Fn(&World) -> Result<DiagnosticValue, ReaderError> + Send + Sync + 'static;

#[derive(Clone)]
pub(super) struct RegisteredState {
    pub(super) key: String,
    pub(super) rust_type: &'static str,
    pub(super) read: StateReadFn,
}

#[derive(Clone)]
pub(super) struct RegisteredMarker {
    pub(super) key: String,
    pub(super) count: MarkerCountFn,
    pub(super) entities: MarkerEntitiesFn,
}

#[derive(Clone)]
pub(super) struct RegisteredResourceField {
    resource: String,
    rust_type: &'static str,
    field: String,
    present: fn(&World) -> bool,
    read: Arc<ResourceReadFn>,
}

#[derive(Clone, Default)]
pub(super) struct PredicateRegistry {
    pub(super) states: Vec<RegisteredState>,
    pub(super) markers: Vec<RegisteredMarker>,
    resource_fields: Vec<RegisteredResourceField>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct RegistryError {
    pub(super) code: &'static str,
    pub(super) message: String,
    pub(super) details: Value,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct EvaluationError {
    pub(super) code: &'static str,
    pub(super) message: String,
    pub(super) details: Value,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct ResourceSummary {
    pub(super) resource: String,
    pub(super) present: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub(super) enum ResourceInfoDetails {
    Resources {
        resources: Vec<ResourceSummary>,
    },
    Resource {
        resource: String,
        present: bool,
        fields: Vec<String>,
    },
    Field {
        resource: String,
        field: String,
        value: DiagnosticValue,
    },
}

#[derive(Clone, Debug)]
pub(super) struct BoundedCount {
    pub(super) value: u32,
    pub(super) is_lower_bound: bool,
    pub(super) entities: Vec<Entity>,
}

#[derive(Clone, Debug)]
pub(super) enum ReaderError {
    Missing,
    InvalidValue { reason: &'static str },
}

impl PredicateRegistry {
    pub(super) fn register_state<S: States>(&mut self) -> Result<(), RegistryError> {
        let key = short_type_name::<S>();
        self.check_registration_key("state", &key)?;
        if self.states.iter().any(|state| state.key == key) {
            return Err(RegistryError::new(
                "duplicate_state_registration",
                "a state is already registered with this exact key",
                json!({"state": key}),
            ));
        }
        self.check_capacity()?;
        self.states.push(RegisteredState {
            key,
            rust_type: type_name::<S>(),
            read: read_state::<S>,
        });
        Ok(())
    }

    pub(super) fn register_marker<T: Component>(&mut self) -> Result<(), RegistryError> {
        let key = short_type_name::<T>();
        self.check_registration_key("marker", &key)?;
        if self.markers.iter().any(|marker| marker.key == key) {
            return Err(RegistryError::new(
                "duplicate_marker_registration",
                "a marker is already registered with this exact key",
                json!({"marker": key}),
            ));
        }
        self.check_capacity()?;
        self.markers.push(RegisteredMarker {
            key,
            count: count_marker::<T>,
            entities: marker_entities::<T>,
        });
        Ok(())
    }

    pub(super) fn register_resource_field<R, V, F>(
        &mut self,
        field: impl Into<String>,
        read_field: F,
    ) -> Result<(), RegistryError>
    where
        R: Resource,
        V: Into<DiagnosticValue>,
        F: Fn(&R) -> V + Send + Sync + 'static,
    {
        let resource = short_type_name::<R>();
        let field = field.into();
        if let Some(existing) = self
            .resource_fields
            .iter()
            .find(|entry| entry.resource == resource)
            && existing.rust_type != type_name::<R>()
        {
            return Err(RegistryError::new(
                "resource_short_name_collision",
                "two resource types have the same short Rust type name",
                json!({"resource": resource, "first_type": existing.rust_type, "second_type": type_name::<R>()}),
            ));
        }
        self.check_registration_key("resource", &resource)?;
        self.check_registration_key("field", &field)?;
        if self
            .resource_fields
            .iter()
            .any(|registered| registered.resource == resource && registered.field == field)
        {
            return Err(RegistryError::new(
                "duplicate_resource_field_registration",
                "a resource field is already registered with these exact keys",
                json!({"resource": resource, "field": field}),
            ));
        }
        self.check_capacity()?;

        let reader = move |world: &World| {
            let resource = world.get_resource::<R>().ok_or(ReaderError::Missing)?;
            let value = read_field(resource).into();
            validate_value(&value)?;
            Ok(value)
        };
        self.resource_fields.push(RegisteredResourceField {
            resource,
            field,
            present: resource_present::<R>,
            rust_type: type_name::<R>(),
            read: Arc::new(reader),
        });
        Ok(())
    }

    pub(super) fn evaluate(
        &self,
        world: &mut World,
        predicate: &Predicate,
    ) -> Result<ObservedPredicate, EvaluationError> {
        match predicate {
            Predicate::StateEquals { state, value } => {
                self.evaluate_state(world, predicate, state, value)
            }
            Predicate::ResourceField {
                resource,
                field,
                operator,
                value,
            } => self.evaluate_resource(world, predicate, resource, field, *operator, value),
            Predicate::MarkerCount { marker, min, max } => {
                self.evaluate_marker(world, predicate, marker, *min, *max)
            }
            Predicate::TargetExists {
                target,
                kind,
                camera,
            } => self.evaluate_target(
                world,
                predicate,
                target,
                kind.clone(),
                camera.as_deref(),
                true,
            ),
            Predicate::TargetAbsent {
                target,
                kind,
                camera,
            } => self.evaluate_target(
                world,
                predicate,
                target,
                kind.clone(),
                camera.as_deref(),
                false,
            ),
        }
    }

    pub(super) fn resource_info(
        &self,
        world: &World,
        resource: Option<&str>,
        field: Option<&str>,
    ) -> Result<ResourceInfoDetails, EvaluationError> {
        match (resource, field) {
            (None, None) => Ok(ResourceInfoDetails::Resources {
                resources: self.resource_summaries(world),
            }),
            (Some(resource), None) => self.resource_fields_info(world, resource),
            (Some(resource), Some(field)) => {
                let registered = self.find_resource_field(resource, field)?;
                let value = self.read_resource_field(world, registered)?;
                Ok(ResourceInfoDetails::Field {
                    resource: resource.to_string(),
                    field: field.to_string(),
                    value,
                })
            }
            (None, Some(field)) => Err(EvaluationError::new(
                "resource_required",
                "a field can only be requested together with a resource",
                json!({"field": field}),
            )),
        }
    }

    fn evaluate_state(
        &self,
        world: &World,
        predicate: &Predicate,
        key: &str,
        expected: &DiagnosticValue,
    ) -> Result<ObservedPredicate, EvaluationError> {
        validate_expected(expected, "state", key, None)?;
        let registered = self.states.iter().find(|state| state.key == key).ok_or_else(|| {
            EvaluationError::new(
                "state_not_registered",
                "no state is registered with this exact key",
                json!({"state": key, "registered": self.states.iter().map(|entry| &entry.key).collect::<Vec<_>>()}),
            )
        })?;
        let observed = (registered.read)(world)
            .map_err(|error| map_reader_error(error, "state", key, None))?;
        let outcome = if observed == *expected {
            PredicateOutcome::Matched
        } else {
            PredicateOutcome::NotMatched
        };
        Ok(observation(predicate, outcome, Some(observed), None, false))
    }

    fn evaluate_resource(
        &self,
        world: &World,
        predicate: &Predicate,
        resource: &str,
        field: &str,
        operator: ComparisonOperator,
        expected: &DiagnosticValue,
    ) -> Result<ObservedPredicate, EvaluationError> {
        validate_expected(expected, "resource", resource, Some(field))?;
        let registered = self.find_resource_field(resource, field)?;
        let observed = self.read_resource_field(world, registered)?;
        let matched = compare_values(&observed, operator, expected).map_err(|reason| {
            EvaluationError::new(
                "comparison_type_mismatch",
                reason,
                json!({
                    "resource": resource,
                    "field": field,
                    "operator": operator,
                    "observed": observed,
                    "expected": expected,
                }),
            )
        })?;
        let outcome = if matched {
            PredicateOutcome::Matched
        } else {
            PredicateOutcome::NotMatched
        };
        Ok(observation(predicate, outcome, Some(observed), None, false))
    }

    fn evaluate_marker(
        &self,
        world: &mut World,
        predicate: &Predicate,
        key: &str,
        min: Option<u32>,
        max: Option<u32>,
    ) -> Result<ObservedPredicate, EvaluationError> {
        let marker = self.markers.iter().find(|marker| marker.key == key).ok_or_else(|| {
            EvaluationError::new(
                "marker_not_registered",
                "no marker is registered with this exact key",
                json!({"marker": key, "registered": self.markers.iter().map(|entry| &entry.key).collect::<Vec<_>>()}),
            )
        })?;
        let count = (marker.count)(world);
        let outcome = marker_count_outcome(&count, min, max);
        Ok(observation(
            predicate,
            outcome,
            None,
            Some(count.value),
            count.is_lower_bound,
        ))
    }

    fn evaluate_target(
        &self,
        world: &mut World,
        predicate: &Predicate,
        target: &crate::TargetSelector,
        kind: crate::TargetKind,
        camera: Option<&str>,
        expect_exists: bool,
    ) -> Result<ObservedPredicate, EvaluationError> {
        let markers = self
            .markers
            .iter()
            .map(|marker| TargetMarker {
                name: &marker.key,
                entities: marker.entities,
            })
            .collect::<Vec<_>>();
        let resolvability =
            target_resolvability(world, &markers, target, kind, camera).map_err(|error| {
                EvaluationError {
                    code: error.code,
                    message: error.message,
                    details: error.details,
                }
            })?;
        let outcome = match (resolvability, expect_exists) {
            (TargetResolvability::Resolved { .. }, true) | (TargetResolvability::Absent, false) => {
                PredicateOutcome::Matched
            }
            (TargetResolvability::Resolved { .. }, false)
            | (TargetResolvability::Absent, true)
            | (TargetResolvability::PresentUnresolved, false) => PredicateOutcome::NotMatched,
            (TargetResolvability::PresentUnresolved, true)
            | (TargetResolvability::Indeterminate, _) => PredicateOutcome::Indeterminate,
        };
        Ok(observation(predicate, outcome, None, None, false))
    }

    fn find_resource_field(
        &self,
        resource: &str,
        field: &str,
    ) -> Result<&RegisteredResourceField, EvaluationError> {
        let resource_registered = self
            .resource_fields
            .iter()
            .any(|registered| registered.resource == resource);
        if !resource_registered {
            return Err(EvaluationError::new(
                "resource_not_registered",
                "no resource is registered with this exact key",
                json!({"resource": resource, "registered": self.resource_fields.iter().map(|entry| &entry.resource).collect::<Vec<_>>()}),
            ));
        }
        self.resource_fields
            .iter()
            .find(|registered| registered.resource == resource && registered.field == field)
            .ok_or_else(|| {
                EvaluationError::new(
                    "resource_field_not_registered",
                    "no field is registered with these exact resource and field keys",
                    json!({"resource": resource, "field": field, "registered_fields": self.resource_fields.iter().filter(|entry| entry.resource == resource).map(|entry| &entry.field).collect::<Vec<_>>()}),
                )
            })
    }

    fn read_resource_field(
        &self,
        world: &World,
        registered: &RegisteredResourceField,
    ) -> Result<DiagnosticValue, EvaluationError> {
        (registered.read)(world).map_err(|error| {
            map_reader_error(
                error,
                "resource",
                &registered.resource,
                Some(&registered.field),
            )
        })
    }

    fn resource_summaries(&self, world: &World) -> Vec<ResourceSummary> {
        let mut resources = Vec::new();
        for registered in &self.resource_fields {
            if resources
                .iter()
                .any(|summary: &ResourceSummary| summary.resource == registered.resource)
            {
                continue;
            }
            resources.push(ResourceSummary {
                resource: registered.resource.clone(),
                present: (registered.present)(world),
            });
        }
        resources
    }

    fn resource_fields_info(
        &self,
        world: &World,
        resource: &str,
    ) -> Result<ResourceInfoDetails, EvaluationError> {
        let mut matching = self
            .resource_fields
            .iter()
            .filter(|registered| registered.resource == resource);
        let first = matching.next().ok_or_else(|| {
            EvaluationError::new(
                "resource_not_registered",
                "no resource is registered with this exact key",
                json!({"resource": resource, "registered": self.resource_fields.iter().map(|entry| &entry.resource).collect::<Vec<_>>()}),
            )
        })?;
        let mut fields = Vec::new();
        fields.push(first.field.clone());
        fields.extend(matching.map(|registered| registered.field.clone()));
        Ok(ResourceInfoDetails::Resource {
            resource: resource.to_string(),
            present: (first.present)(world),
            fields,
        })
    }

    fn check_capacity(&self) -> Result<(), RegistryError> {
        let count = self.states.len() + self.markers.len() + self.resource_fields.len();
        if count >= MAX_REGISTRATIONS {
            return Err(RegistryError::new(
                "registration_limit_reached",
                "diagnostics registrations exceed the bounded registry capacity",
                json!({"limit": MAX_REGISTRATIONS}),
            ));
        }
        Ok(())
    }

    fn check_registration_key(&self, kind: &'static str, key: &str) -> Result<(), RegistryError> {
        if key.is_empty() || key.len() > MAX_KEY_BYTES {
            return Err(RegistryError::new(
                "invalid_registration_key",
                "a diagnostics registration key must be non-empty and at most 128 bytes",
                json!({"kind": kind, "key": key, "max_bytes": MAX_KEY_BYTES}),
            ));
        }
        Ok(())
    }
}

impl RegistryError {
    fn new(code: &'static str, message: impl Into<String>, details: Value) -> Self {
        Self {
            code,
            message: message.into(),
            details,
        }
    }
}

impl EvaluationError {
    fn new(code: &'static str, message: impl Into<String>, details: Value) -> Self {
        Self {
            code,
            message: message.into(),
            details,
        }
    }
}

fn read_state<S: States>(world: &World) -> StateReadResult {
    let state = world
        .get_resource::<State<S>>()
        .ok_or(ReaderError::Missing)?;
    let text = format!("{:?}", state.get());
    let value = DiagnosticValue::String(text);
    validate_value(&value)?;
    Ok(value)
}

fn resource_present<R: Resource>(world: &World) -> bool {
    world.contains_resource::<R>()
}

fn count_marker<T: Component>(world: &mut World) -> BoundedCount {
    let mut query = world.query_filtered::<Entity, With<T>>();
    let mut entities = query
        .iter(world)
        .take(MAX_MARKER_COUNT + 1)
        .collect::<Vec<_>>();
    let is_lower_bound = entities.len() > MAX_MARKER_COUNT;
    let value = entities.len() as u32;
    entities.truncate(MAX_MARKER_COUNT);
    BoundedCount {
        value,
        is_lower_bound,
        entities,
    }
}

fn marker_entities<T: Component>(world: &mut World) -> super::targets::MarkerEntities {
    let count = count_marker::<T>(world);
    super::targets::MarkerEntities {
        entities: count.entities,
        truncated: count.is_lower_bound,
    }
}

fn marker_count_outcome(
    count: &BoundedCount,
    min: Option<u32>,
    max: Option<u32>,
) -> PredicateOutcome {
    if !count.is_lower_bound {
        let above_min = min.is_none_or(|minimum| count.value >= minimum);
        let below_max = max.is_none_or(|maximum| count.value <= maximum);
        return if above_min && below_max {
            PredicateOutcome::Matched
        } else {
            PredicateOutcome::NotMatched
        };
    }
    if max.is_some_and(|maximum| count.value > maximum) {
        return PredicateOutcome::NotMatched;
    }
    if max.is_none() && min.is_none_or(|minimum| count.value >= minimum) {
        return PredicateOutcome::Matched;
    }
    PredicateOutcome::Indeterminate
}

fn compare_values(
    observed: &DiagnosticValue,
    operator: ComparisonOperator,
    expected: &DiagnosticValue,
) -> Result<bool, &'static str> {
    match operator {
        ComparisonOperator::Eq => Ok(observed == expected),
        ComparisonOperator::Ne => Ok(observed != expected),
        ComparisonOperator::Lt
        | ComparisonOperator::Lte
        | ComparisonOperator::Gt
        | ComparisonOperator::Gte => {
            let (DiagnosticValue::Number(observed), DiagnosticValue::Number(expected)) =
                (observed, expected)
            else {
                return Err("ordering comparisons require numeric observed and expected values");
            };
            Ok(match operator {
                ComparisonOperator::Lt => observed < expected,
                ComparisonOperator::Lte => observed <= expected,
                ComparisonOperator::Gt => observed > expected,
                ComparisonOperator::Gte => observed >= expected,
                ComparisonOperator::Eq | ComparisonOperator::Ne => unreachable!(),
            })
        }
    }
}

fn validate_expected(
    value: &DiagnosticValue,
    kind: &'static str,
    key: &str,
    field: Option<&str>,
) -> Result<(), EvaluationError> {
    validate_value(value).map_err(|error| map_reader_error(error, kind, key, field))
}

fn validate_value(value: &DiagnosticValue) -> Result<(), ReaderError> {
    match value {
        DiagnosticValue::Number(number) if !number.is_finite() => Err(ReaderError::InvalidValue {
            reason: "diagnostic numeric values must be finite",
        }),
        DiagnosticValue::String(text) if text.len() > MAX_STRING_BYTES => {
            Err(ReaderError::InvalidValue {
                reason: "diagnostic strings must be at most 1024 bytes",
            })
        }
        _ => Ok(()),
    }
}

fn map_reader_error(
    error: ReaderError,
    kind: &'static str,
    key: &str,
    field: Option<&str>,
) -> EvaluationError {
    let mut details = match (kind, field) {
        ("resource", Some(field)) => json!({"resource": key, "field": field}),
        ("resource", None) => json!({"resource": key}),
        ("state", _) => json!({"state": key}),
        (_, Some(field)) => json!({"kind": kind, "key": key, "field": field}),
        (_, None) => json!({"kind": kind, "key": key}),
    };
    match error {
        ReaderError::Missing => EvaluationError::new(
            if kind == "state" {
                "state_missing"
            } else {
                "resource_missing"
            },
            "the registered Bevy resource is not present in the world",
            details,
        ),
        ReaderError::InvalidValue { reason } => {
            details["reason"] = Value::String(reason.to_string());
            EvaluationError::new("diagnostic_value_invalid", reason, details)
        }
    }
}

fn observation(
    predicate: &Predicate,
    outcome: PredicateOutcome,
    value: Option<DiagnosticValue>,
    count: Option<u32>,
    count_is_lower_bound: bool,
) -> ObservedPredicate {
    ObservedPredicate {
        predicate: predicate.clone(),
        outcome,
        value,
        count,
        count_is_lower_bound,
    }
}

fn short_type_name<T>() -> String {
    type_name::<T>()
        .rsplit("::")
        .next()
        .unwrap_or(type_name::<T>())
        .to_string()
}
