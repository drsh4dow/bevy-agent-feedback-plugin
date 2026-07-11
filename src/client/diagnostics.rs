use super::{AgentClient, ClientError};
use crate::DiagnosticValue;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};

/// Exact selector used by semantic target commands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TargetSelector {
    /// Exact Bevy `Name`.
    Name(String),
    /// Exact AccessKit accessibility label.
    AccessibilityLabel(String),
    /// Exact registered marker key.
    Marker(String),
}

impl Serialize for TargetSelector {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut object = Map::new();
        match self {
            Self::Name(value) => object.insert("name".to_string(), Value::String(value.clone())),
            Self::AccessibilityLabel(value) => object.insert(
                "accessibility_label".to_string(),
                Value::String(value.clone()),
            ),
            Self::Marker(value) => {
                object.insert("marker".to_string(), Value::String(value.clone()))
            }
        };
        object.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TargetSelector {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let object = Map::<String, Value>::deserialize(deserializer)?;
        let candidates = [
            ("name", "Name", 0u8),
            ("accessibility_label", "AccessibilityLabel", 1u8),
            ("marker", "Marker", 2u8),
        ];
        let mut selected = None;
        for (wire, response, kind) in candidates {
            if let Some(value) = object.get(wire).or_else(|| object.get(response)) {
                let value = value
                    .as_str()
                    .ok_or_else(|| serde::de::Error::custom("target selector must be a string"))?;
                if selected.is_some() {
                    return Err(serde::de::Error::custom(
                        "target selector must contain exactly one field",
                    ));
                }
                selected = Some((kind, value.to_string()));
            }
        }
        match selected {
            Some((0, value)) => Ok(Self::Name(value)),
            Some((1, value)) => Ok(Self::AccessibilityLabel(value)),
            Some((2, value)) => Ok(Self::Marker(value)),
            _ => Err(serde::de::Error::custom(
                "target selector must contain exactly one known field",
            )),
        }
    }
}

/// Target projection category.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    /// UI or world target.
    #[default]
    Any,
    /// Bevy UI target.
    Ui,
    /// World-space target.
    World,
}

/// Registered scalar comparison operator.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum ComparisonOperator {
    Eq,
    Ne,
    Lt,
    Lte,
    Gt,
    Gte,
}

/// Predicate payload evaluated by the diagnostics plugin.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum Predicate {
    StateEquals {
        state: String,
        value: DiagnosticValue,
    },
    ResourceField {
        resource: String,
        field: String,
        operator: ComparisonOperator,
        value: DiagnosticValue,
    },
    MarkerCount {
        marker: String,
        min: Option<u32>,
        max: Option<u32>,
    },
    TargetExists {
        target: TargetSelector,
        #[serde(default)]
        kind: TargetKind,
        camera: Option<String>,
    },
    TargetAbsent {
        target: TargetSelector,
        #[serde(default)]
        kind: TargetKind,
        camera: Option<String>,
    },
}

/// Server predicate outcome. Only `Matched` satisfies waits and assertions.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum PredicateOutcome {
    Matched,
    NotMatched,
    Indeterminate,
}

/// Bounded observation returned by predicate evaluation.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[allow(missing_docs)]
pub struct ObservedPredicate {
    pub predicate: Predicate,
    pub outcome: PredicateOutcome,
    pub value: Option<DiagnosticValue>,
    pub count: Option<u32>,
    #[serde(default)]
    pub count_is_lower_bound: bool,
}

/// Visible logical target bounds.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[allow(missing_docs)]
pub struct TargetBounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Resolved semantic target information.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[allow(missing_docs)]
pub struct TargetInfo {
    pub entity: String,
    pub selector_source: String,
    pub selector_value: String,
    pub kind: ResolvedTargetKind,
    pub name: Option<String>,
    pub marker: Option<String>,
    pub camera: String,
    pub camera_name: Option<String>,
    pub center: [f32; 2],
    pub bounds: Option<TargetBounds>,
    pub visible: bool,
    pub clipped: bool,
}

/// Resolved target category.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum ResolvedTargetKind {
    Ui,
    World,
}

#[allow(missing_docs)]
impl AgentClient {
    /// Returns the latest bounded predicate observation from a result or command error.
    pub fn last_observation(&self) -> Option<&ObservedPredicate> {
        self.last_observation.as_ref()
    }

    /// Resolves exactly one semantic target.
    pub fn target_info(
        &mut self,
        target: TargetSelector,
        kind: TargetKind,
        camera: Option<&str>,
    ) -> Result<TargetInfo, ClientError> {
        let response = self.request(json!({
            "command": "target_info",
            "target": target,
            "kind": kind,
            "camera": camera,
        }))?;
        serde_json::from_value(response["result"]["details"].clone()).map_err(|error| {
            ClientError::Protocol(format!(
                "invalid target_info response ({error}): {response}"
            ))
        })
    }

    /// Resolves and clicks a semantic target in one ordered server operation.
    pub fn click_target(
        &mut self,
        target: TargetSelector,
        kind: TargetKind,
        camera: Option<&str>,
        button: &str,
        frames: u16,
    ) -> Result<Value, ClientError> {
        self.validate_frames(frames)?;
        self.request(json!({
            "command": "click_target",
            "target": target,
            "kind": kind,
            "camera": camera,
            "button": button,
            "frames": frames,
        }))
    }

    /// Atomically clicks an exact Bevy `Name`.
    pub fn click_named(&mut self, name: &str) -> Result<Value, ClientError> {
        self.click_target(
            TargetSelector::Name(name.to_string()),
            TargetKind::Any,
            None,
            "Left",
            1,
        )
    }

    /// Atomically clicks an exact accessibility label.
    pub fn click_accessibility_label(&mut self, label: &str) -> Result<Value, ClientError> {
        self.click_target(
            TargetSelector::AccessibilityLabel(label.to_string()),
            TargetKind::Any,
            None,
            "Left",
            1,
        )
    }

    /// Atomically clicks an exact registered marker.
    pub fn click_marker(&mut self, marker: &str) -> Result<Value, ClientError> {
        self.click_target(
            TargetSelector::Marker(marker.to_string()),
            TargetKind::Any,
            None,
            "Left",
            1,
        )
    }

    /// Reads registered resource metadata or a registered scalar field.
    pub fn resource_info(
        &mut self,
        resource: Option<&str>,
        field: Option<&str>,
    ) -> Result<Value, ClientError> {
        Ok(self.request(json!({
            "command": "resource_info",
            "resource": resource,
            "field": field,
        }))?["result"]["details"]
            .clone())
    }

    /// Reads one explicitly registered scalar resource field.
    pub fn read_resource_field(
        &mut self,
        resource: &str,
        field: &str,
    ) -> Result<DiagnosticValue, ClientError> {
        let details = self.resource_info(Some(resource), Some(field))?;
        serde_json::from_value(details["value"].clone()).map_err(|error| {
            ClientError::Protocol(format!(
                "invalid resource field response ({error}): {details}"
            ))
        })
    }

    /// Evaluates a typed predicate once without reinterpreting server semantics.
    pub fn evaluate_predicate(
        &mut self,
        predicate: Predicate,
    ) -> Result<ObservedPredicate, ClientError> {
        self.predicate_request("evaluate_predicate", predicate, None, None)
    }

    /// Waits for the server evaluator to report `matched`.
    pub fn wait_for(
        &mut self,
        predicate: Predicate,
        max_frames: u16,
    ) -> Result<ObservedPredicate, ClientError> {
        self.wait_for_with_abort(predicate, &[], max_frames)
    }

    /// Waits for a predicate while aborting on the first matching abort predicate.
    pub fn wait_for_with_abort(
        &mut self,
        predicate: Predicate,
        abort_predicates: &[Predicate],
        max_frames: u16,
    ) -> Result<ObservedPredicate, ClientError> {
        self.validate_semantic_wait(max_frames, abort_predicates.len())?;
        match self.predicate_request(
            "wait_for",
            predicate,
            Some(abort_predicates),
            Some(max_frames),
        ) {
            Ok(observed) => Ok(observed),
            Err(mut error) => {
                if error.is_semantic_wait_failure()
                    && let Ok(capture) = self.capture_labeled("semantic-wait-failure")
                {
                    error.attach_failure_capture(&capture);
                }
                Err(error)
            }
        }
    }

    pub fn wait_for_state(
        &mut self,
        state: &str,
        value: DiagnosticValue,
        max_frames: u16,
    ) -> Result<ObservedPredicate, ClientError> {
        self.wait_for_state_with_abort(state, value, &[], max_frames)
    }

    /// Waits for one state value and aborts on any listed value.
    pub fn wait_for_state_with_abort(
        &mut self,
        state: &str,
        value: DiagnosticValue,
        abort_values: &[DiagnosticValue],
        max_frames: u16,
    ) -> Result<ObservedPredicate, ClientError> {
        self.validate_semantic_wait(max_frames, abort_values.len())?;
        let abort_predicates = abort_values
            .iter()
            .cloned()
            .map(|value| Predicate::StateEquals {
                state: state.to_string(),
                value,
            })
            .collect::<Vec<_>>();
        self.wait_for_with_abort(
            Predicate::StateEquals {
                state: state.to_string(),
                value,
            },
            &abort_predicates,
            max_frames,
        )
    }

    pub fn wait_for_resource(
        &mut self,
        resource: &str,
        field: &str,
        operator: ComparisonOperator,
        value: DiagnosticValue,
        max_frames: u16,
    ) -> Result<ObservedPredicate, ClientError> {
        self.wait_for(
            Predicate::ResourceField {
                resource: resource.to_string(),
                field: field.to_string(),
                operator,
                value,
            },
            max_frames,
        )
    }

    pub fn wait_for_marker_count(
        &mut self,
        marker: &str,
        min: Option<u32>,
        max: Option<u32>,
        max_frames: u16,
    ) -> Result<ObservedPredicate, ClientError> {
        self.wait_for(
            Predicate::MarkerCount {
                marker: marker.to_string(),
                min,
                max,
            },
            max_frames,
        )
    }

    pub fn wait_for_marker_present(
        &mut self,
        marker: &str,
        max_frames: u16,
    ) -> Result<ObservedPredicate, ClientError> {
        self.wait_for_marker_count(marker, Some(1), None, max_frames)
    }

    pub fn wait_for_marker_absent(
        &mut self,
        marker: &str,
        max_frames: u16,
    ) -> Result<ObservedPredicate, ClientError> {
        self.wait_for_marker_count(marker, None, Some(0), max_frames)
    }

    pub fn wait_for_target(
        &mut self,
        target: TargetSelector,
        kind: TargetKind,
        camera: Option<&str>,
        max_frames: u16,
    ) -> Result<ObservedPredicate, ClientError> {
        self.wait_for(
            Predicate::TargetExists {
                target,
                kind,
                camera: camera.map(str::to_string),
            },
            max_frames,
        )
    }

    pub fn wait_for_target_absent(
        &mut self,
        target: TargetSelector,
        kind: TargetKind,
        camera: Option<&str>,
        max_frames: u16,
    ) -> Result<ObservedPredicate, ClientError> {
        self.wait_for(
            Predicate::TargetAbsent {
                target,
                kind,
                camera: camera.map(str::to_string),
            },
            max_frames,
        )
    }

    pub fn assert_state(&mut self, state: &str, value: DiagnosticValue) -> Result<(), ClientError> {
        self.assert_predicate(Predicate::StateEquals {
            state: state.to_string(),
            value,
        })
    }

    pub fn assert_resource(
        &mut self,
        resource: &str,
        field: &str,
        operator: ComparisonOperator,
        value: DiagnosticValue,
    ) -> Result<(), ClientError> {
        self.assert_predicate(Predicate::ResourceField {
            resource: resource.to_string(),
            field: field.to_string(),
            operator,
            value,
        })
    }

    pub fn assert_marker_count(
        &mut self,
        marker: &str,
        min: Option<u32>,
        max: Option<u32>,
    ) -> Result<(), ClientError> {
        self.assert_predicate(Predicate::MarkerCount {
            marker: marker.to_string(),
            min,
            max,
        })
    }

    pub fn assert_marker_present(&mut self, marker: &str) -> Result<(), ClientError> {
        self.assert_marker_count(marker, Some(1), None)
    }

    pub fn assert_marker_absent(&mut self, marker: &str) -> Result<(), ClientError> {
        self.assert_marker_count(marker, None, Some(0))
    }

    pub fn assert_target_exists(
        &mut self,
        target: TargetSelector,
        kind: TargetKind,
        camera: Option<&str>,
    ) -> Result<(), ClientError> {
        self.assert_predicate(Predicate::TargetExists {
            target,
            kind,
            camera: camera.map(str::to_string),
        })
    }

    pub fn assert_target_absent(
        &mut self,
        target: TargetSelector,
        kind: TargetKind,
        camera: Option<&str>,
    ) -> Result<(), ClientError> {
        self.assert_predicate(Predicate::TargetAbsent {
            target,
            kind,
            camera: camera.map(str::to_string),
        })
    }

    fn assert_predicate(&mut self, predicate: Predicate) -> Result<(), ClientError> {
        let observation = self.evaluate_predicate(predicate)?;
        if observation.outcome == PredicateOutcome::Matched {
            return Ok(());
        }
        Err(ClientError::Assertion(format!(
            "predicate assertion did not match: {observation:?}"
        )))
    }

    fn predicate_request(
        &mut self,
        command: &str,
        predicate: Predicate,
        abort_predicates: Option<&[Predicate]>,
        max_frames: Option<u16>,
    ) -> Result<ObservedPredicate, ClientError> {
        let mut request = json!({"command": command, "predicate": predicate});
        if let Some(abort_predicates) = abort_predicates
            && !abort_predicates.is_empty()
        {
            request["abort_predicates"] = json!(abort_predicates);
        }
        if let Some(max_frames) = max_frames {
            request["max_frames"] = Value::from(max_frames);
        }
        let response = self.request(request)?;
        serde_json::from_value(response["result"]["details"].clone()).map_err(|error| {
            ClientError::Protocol(format!(
                "invalid predicate observation ({error}): {response}"
            ))
        })
    }

    fn validate_frames(&self, frames: u16) -> Result<(), ClientError> {
        if frames == 0 || frames > self.capabilities.max_wait_frames {
            return Err(ClientError::Protocol(format!(
                "frames must be in 1..={}",
                self.capabilities.max_wait_frames
            )));
        }
        Ok(())
    }

    fn validate_semantic_wait(
        &self,
        max_frames: u16,
        abort_predicates: usize,
    ) -> Result<(), ClientError> {
        if max_frames == 0 {
            return Err(ClientError::Protocol(
                "max_frames must be greater than zero".to_string(),
            ));
        }
        self.validate_wait_limit("max_frames", u64::from(max_frames))?;
        if abort_predicates > self.capabilities.max_abort_predicates {
            return Err(ClientError::Protocol(format!(
                "abort_predicates has {abort_predicates} items, but server supports {}; reduce abort predicates or configure separate explicit waits",
                self.capabilities.max_abort_predicates
            )));
        }
        Ok(())
    }
}
