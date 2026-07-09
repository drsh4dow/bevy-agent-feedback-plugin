use super::DiagnosticValue;
use crate::key_names::KEY_CODE_NAMES;
use bevy::{input::mouse::MouseScrollUnit, prelude::*};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::time::Duration;

pub(super) const MAX_SELECTOR_BYTES: usize = 128;
const MAX_SCALAR_STRING_BYTES: usize = 1024;

pub(super) fn scalar(label: &str, value: Value) -> Result<DiagnosticValue, String> {
    match value {
        Value::Null => Ok(DiagnosticValue::Null),
        Value::Bool(value) => Ok(DiagnosticValue::Bool(value)),
        Value::Number(value) => value
            .as_f64()
            .filter(|value| value.is_finite())
            .map(DiagnosticValue::Number)
            .ok_or_else(|| format!("{label} must be a finite scalar number")),
        Value::String(value) => Ok(DiagnosticValue::String(bounded_string(
            label,
            value,
            MAX_SCALAR_STRING_BYTES,
        )?)),
        _ => Err(format!(
            "{label} must be a null, boolean, finite number, or string scalar"
        )),
    }
}

impl From<()> for DiagnosticValue {
    fn from((): ()) -> Self {
        Self::Null
    }
}

impl From<bool> for DiagnosticValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<f32> for DiagnosticValue {
    fn from(value: f32) -> Self {
        Self::Number(f64::from(value))
    }
}

impl From<f64> for DiagnosticValue {
    fn from(value: f64) -> Self {
        Self::Number(value)
    }
}

impl From<i32> for DiagnosticValue {
    fn from(value: i32) -> Self {
        Self::Number(f64::from(value))
    }
}

impl From<u32> for DiagnosticValue {
    fn from(value: u32) -> Self {
        Self::Number(f64::from(value))
    }
}

impl From<String> for DiagnosticValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for DiagnosticValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_string())
    }
}

impl<T: Into<DiagnosticValue>> From<Option<T>> for DiagnosticValue {
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::Null, Into::into)
    }
}

pub(super) fn positive_duration(label: &str, seconds: f64) -> Result<Duration, String> {
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(format!("{label} must be finite and positive"));
    }
    let duration = Duration::try_from_secs_f64(seconds)
        .map_err(|_| format!("{label} is outside the supported duration range"))?;
    if duration.is_zero() {
        return Err(format!("{label} must not round to zero duration"));
    }
    Ok(duration)
}

pub(super) fn validate_step_count(
    duration: Duration,
    step: Duration,
    max_steps: u16,
) -> Result<(), String> {
    let request_ns = duration.as_nanos();
    let step_ns = step.as_nanos();
    let rounded = request_ns
        .checked_add(step_ns - 1)
        .ok_or_else(|| "duration step count overflowed".to_string())?;
    let steps = rounded / step_ns;
    if steps > u128::from(max_steps.max(1)) {
        return Err(format!(
            "advance requires {steps} steps, exceeding max_time_advance_steps {}",
            max_steps.max(1)
        ));
    }
    Ok(())
}

pub(super) fn vec2(label: &str, x: f32, y: f32) -> Result<Vec2, String> {
    if x.is_finite() && y.is_finite() {
        Ok(Vec2::new(x, y))
    } else {
        Err(format!("{label} must contain finite coordinates"))
    }
}

pub(super) fn validate_capture_label(label: Option<String>) -> Result<Option<String>, String> {
    let Some(label) = label else {
        return Ok(None);
    };
    let valid = (1..=40).contains(&label.len())
        && label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    if valid {
        Ok(Some(label))
    } else {
        Err("capture label must match [A-Za-z0-9_-]{1,40}".to_string())
    }
}

pub(super) fn bounded_frames(label: &str, value: u16, max: u16) -> Result<u16, String> {
    if value == 0 || value > max {
        return Err(format!("{label} must be between 1 and {max}, got {value}"));
    }
    Ok(value)
}

pub(super) fn bounded_optional(
    label: &str,
    value: Option<String>,
    max: usize,
) -> Result<Option<String>, String> {
    value
        .map(|value| bounded_string(label, value, max))
        .transpose()
}

pub(super) fn bounded_string(label: &str, value: String, max: usize) -> Result<String, String> {
    if value.is_empty() || value.len() > max {
        return Err(format!("{label} must contain 1..={max} UTF-8 bytes"));
    }
    Ok(value)
}

pub(super) fn parse_key_code(value: &str) -> Result<KeyCode, String> {
    parse_named("key", value, KEY_CODE_NAMES)
}

pub(super) fn parse_mouse_button(value: &str) -> Result<MouseButton, String> {
    parse_named("button", value, MOUSE_BUTTON_NAMES)
}

pub(super) fn parse_scroll_unit(value: Option<&str>) -> Result<MouseScrollUnit, String> {
    match value {
        Some(value) => parse_named("scroll unit", value, MOUSE_SCROLL_UNIT_NAMES),
        None => Ok(MouseScrollUnit::Line),
    }
}

fn parse_named<T: DeserializeOwned>(kind: &str, value: &str, names: &[&str]) -> Result<T, String> {
    let Some(name) = names.iter().find(|name| name.eq_ignore_ascii_case(value)) else {
        let value_lower = value.to_ascii_lowercase();
        let mut best = None;
        for name in names {
            let name_lower = name.to_ascii_lowercase();
            if let Some(distance) = edit_distance_with_cutoff(&value_lower, &name_lower, 2)
                && best.is_none_or(|(best_distance, _)| distance < best_distance)
            {
                best = Some((distance, *name));
            }
        }
        if let Some((_, suggestion)) = best {
            return Err(format!(
                "invalid {kind} '{value}'; did you mean '{suggestion}'?"
            ));
        }
        return Err(format!("invalid {kind} '{value}'"));
    };
    serde_json::from_value(Value::String((*name).to_string()))
        .map_err(|error| format!("invalid {kind} '{value}'; did you mean '{name}'? ({error})"))
}

fn edit_distance_with_cutoff(left: &str, right: &str, cutoff: usize) -> Option<usize> {
    if left.len().abs_diff(right.len()) > cutoff {
        return None;
    }
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0; right.len() + 1];
    for (left_index, left_byte) in left.bytes().enumerate() {
        current[0] = left_index + 1;
        let mut row_minimum = current[0];
        for (right_index, right_byte) in right.bytes().enumerate() {
            let substitution = previous[right_index] + usize::from(left_byte != right_byte);
            let insertion = current[right_index] + 1;
            let deletion = previous[right_index + 1] + 1;
            let distance = substitution.min(insertion).min(deletion);
            current[right_index + 1] = distance;
            row_minimum = row_minimum.min(distance);
        }
        if row_minimum > cutoff {
            return None;
        }
        std::mem::swap(&mut previous, &mut current);
    }
    (previous[right.len()] <= cutoff).then_some(previous[right.len()])
}

const MOUSE_BUTTON_NAMES: &[&str] = &["Left", "Right", "Middle", "Back", "Forward"];
const MOUSE_SCROLL_UNIT_NAMES: &[&str] = &["Line", "Pixel"];
