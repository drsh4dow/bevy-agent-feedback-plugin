use crate::protocol::{TargetKind, TargetSelector};
use bevy::{
    a11y::AccessibilityNode,
    camera::{RenderTarget, primitives::Aabb},
    prelude::*,
    ui::{CalculatedClip, ComputedNode, ComputedUiTargetCamera, UiGlobalTransform},
    window::{PrimaryWindow, WindowRef},
};
use serde::Serialize;
use serde_json::{Value, json};

const MAX_TARGET_ENTITIES: usize = 256;
const MAX_CAMERAS: usize = 32;
const MAX_MARKERS: usize = 256;
const MAX_CANDIDATE_DETAILS: usize = 16;

pub(super) type MarkerEntitiesReader = fn(&mut World) -> MarkerEntities;

pub(super) struct MarkerEntities {
    pub(super) entities: Vec<Entity>,
    pub(super) truncated: bool,
}

/// Adapter used by the diagnostics registry without exposing component types here.
#[derive(Clone, Copy)]
pub(super) struct RegisteredMarker<'a> {
    pub(super) name: &'a str,
    pub(super) entities: MarkerEntitiesReader,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ResolvedKind {
    Ui,
    World,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub(super) struct ScreenRect {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) width: f32,
    pub(super) height: f32,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct ResolvedTarget {
    pub(super) entity: String,
    pub(super) selector_source: &'static str,
    pub(super) selector_value: String,
    pub(super) kind: ResolvedKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) marker: Option<String>,
    pub(super) camera: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) camera_name: Option<String>,
    /// Logical primary-window coordinates, suitable for input commands.
    pub(super) center: [f32; 2],
    /// Visible logical bounds with a top-left origin.
    pub(super) bounds: Option<ScreenRect>,
    pub(super) visible: bool,
    pub(super) clipped: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct TargetError {
    pub(super) code: &'static str,
    pub(super) message: String,
    pub(super) details: Value,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(super) enum TargetResolvability {
    Resolved { target: ResolvedTarget },
    Absent,
    PresentUnresolved,
    Indeterminate,
}

impl TargetError {
    fn new(code: &'static str, message: impl Into<String>, details: Value) -> Self {
        Self {
            code,
            message: message.into(),
            details,
        }
    }

    fn unavailable(entity: Entity, reason: &'static str) -> Self {
        Self::new(
            "target_unavailable",
            "target exists but cannot be resolved to a visible primary-window position",
            json!({"entity": entity_label(entity), "reason": reason}),
        )
    }
}

/// Resolves one exact selector without mutating components, resources, or input state.
pub(super) fn resolve_target(
    world: &mut World,
    markers: &[RegisteredMarker<'_>],
    selector: &TargetSelector,
    kind: TargetKind,
    camera_name: Option<&str>,
) -> Result<ResolvedTarget, TargetError> {
    let entity = find_target(world, markers, selector, kind)?;
    let primary_window = find_primary_window(world)?;
    let camera_entity = find_camera(world, primary_window, camera_name)?;
    let mut resolved = if world.get::<ComputedNode>(entity).is_some() {
        project_ui(world, entity, camera_entity, primary_window)?
    } else {
        project_world(world, entity, camera_entity, primary_window)?
    };
    let (selector_source, selector_value, marker) = match selector {
        TargetSelector::Name(value) => ("name", value.clone(), None),
        TargetSelector::AccessibilityLabel(value) => ("accessibility_label", value.clone(), None),
        TargetSelector::Marker(value) => ("marker", value.clone(), Some(value.clone())),
    };
    resolved.selector_source = selector_source;
    resolved.selector_value = selector_value;
    resolved.marker = marker;
    resolved.name = world
        .get::<Name>(entity)
        .map(|name| name.as_str().to_string());
    resolved.camera_name = world
        .get::<Name>(camera_entity)
        .map(|name| name.as_str().to_string());
    Ok(resolved)
}

/// Predicate integration distinguishes identity presence from resolvability.
/// Any exact candidate disproves absence, while a bounded truncated miss cannot
/// prove either existence or absence.
pub(super) fn target_resolvability(
    world: &mut World,
    markers: &[RegisteredMarker<'_>],
    selector: &TargetSelector,
    kind: TargetKind,
    camera_name: Option<&str>,
) -> Result<TargetResolvability, TargetError> {
    match resolve_target(world, markers, selector, kind, camera_name) {
        Ok(target) => Ok(TargetResolvability::Resolved { target }),
        Err(error) if error.code == "target_not_found" => Ok(TargetResolvability::Absent),
        Err(error) if error.code == "target_search_truncated" => {
            let has_candidate = error
                .details
                .get("candidates")
                .and_then(Value::as_array)
                .is_some_and(|candidates| !candidates.is_empty());
            Ok(if has_candidate {
                TargetResolvability::PresentUnresolved
            } else {
                TargetResolvability::Indeterminate
            })
        }
        Err(error)
            if matches!(
                error.code,
                "ambiguous_target"
                    | "target_unavailable"
                    | "camera_unavailable"
                    | "camera_ambiguous"
                    | "camera_search_truncated"
                    | "primary_window_unavailable"
                    | "primary_window_ambiguous"
            ) =>
        {
            Ok(TargetResolvability::PresentUnresolved)
        }
        Err(error) => Err(error),
    }
}

fn find_target(
    world: &mut World,
    markers: &[RegisteredMarker<'_>],
    selector: &TargetSelector,
    kind: TargetKind,
) -> Result<Entity, TargetError> {
    let search = match selector {
        TargetSelector::Name(expected) => {
            let mut query = world.query::<(Entity, &Name, Option<&ComputedNode>)>();
            bounded_target_search(
                query
                    .iter(world)
                    .filter(|(_, _, node)| kind_matches(&kind, node.is_some()))
                    .map(|(entity, name, _)| (entity, name.as_str() == expected)),
            )
        }
        TargetSelector::AccessibilityLabel(expected) => {
            let mut query = world.query::<(Entity, &AccessibilityNode, Option<&ComputedNode>)>();
            bounded_target_search(
                query
                    .iter(world)
                    .filter(|(_, _, node)| kind_matches(&kind, node.is_some()))
                    .map(|(entity, node, _)| {
                        (entity, node.label().is_some_and(|label| label == expected))
                    }),
            )
        }
        TargetSelector::Marker(name) => {
            let reader = find_marker(markers, name)?;
            let matched = reader(world);
            let mut search = bounded_target_search(
                matched
                    .entities
                    .into_iter()
                    .filter(|entity| {
                        kind_matches(&kind, world.get::<ComputedNode>(*entity).is_some())
                    })
                    .map(|entity| (entity, true)),
            );
            search.truncated |= matched.truncated;
            search
        }
    };
    finish_target_search(search)
}

struct TargetSearch {
    candidates: Vec<Entity>,
    scanned: usize,
    truncated: bool,
}

fn bounded_target_search(iter: impl Iterator<Item = (Entity, bool)>) -> TargetSearch {
    let mut candidates = Vec::new();
    let mut scanned = 0usize;
    for (entity, matches) in iter.take(MAX_TARGET_ENTITIES + 1) {
        if scanned == MAX_TARGET_ENTITIES {
            return TargetSearch {
                candidates,
                scanned,
                truncated: true,
            };
        }
        scanned += 1;
        if matches {
            candidates.push(entity);
        }
    }
    TargetSearch {
        candidates,
        scanned,
        truncated: false,
    }
}

fn finish_target_search(search: TargetSearch) -> Result<Entity, TargetError> {
    if search.truncated {
        return Err(TargetError::new(
            "target_search_truncated",
            "target search exceeded the bounded selector-bearing entity scan",
            json!({
                "limit": MAX_TARGET_ENTITIES,
                "candidates": candidate_details(&search.candidates),
                "candidate_count_is_lower_bound": true,
            }),
        ));
    }
    match search.candidates.as_slice() {
        [] => Err(TargetError::new(
            "target_not_found",
            "no entity exactly matched the target selector and kind",
            json!({"scanned": search.scanned}),
        )),
        [entity] => Ok(*entity),
        _ => Err(TargetError::new(
            "ambiguous_target",
            "multiple entities exactly matched the target selector and kind",
            json!({
                "count": search.candidates.len(),
                "candidates": candidate_details(&search.candidates),
                "candidate_details_truncated": search.candidates.len() > MAX_CANDIDATE_DETAILS,
            }),
        )),
    }
}

fn kind_matches(kind: &TargetKind, is_ui: bool) -> bool {
    match kind {
        TargetKind::Any => true,
        TargetKind::Ui => is_ui,
        TargetKind::World => !is_ui,
    }
}

fn find_marker(
    markers: &[RegisteredMarker<'_>],
    requested: &str,
) -> Result<MarkerEntitiesReader, TargetError> {
    if markers.len() > MAX_MARKERS {
        return Err(TargetError::new(
            "marker_registry_truncated",
            "registered marker search exceeded its bound",
            json!({"limit": MAX_MARKERS, "registered_is_lower_bound": true}),
        ));
    }
    let matches = markers
        .iter()
        .filter(|marker| marker.name == requested)
        .take(2)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(TargetError::new(
            "marker_not_registered",
            "target marker name is not registered for diagnostics",
            json!({"marker": requested}),
        )),
        [marker] => Ok(marker.entities),
        _ => Err(TargetError::new(
            "marker_registration_ambiguous",
            "target marker name was registered more than once",
            json!({"marker": requested}),
        )),
    }
}

fn find_primary_window(world: &mut World) -> Result<Entity, TargetError> {
    let mut query = world.query_filtered::<Entity, (With<Window>, With<PrimaryWindow>)>();
    let windows = query.iter(world).take(2).collect::<Vec<_>>();
    match windows.as_slice() {
        [window] => Ok(*window),
        [] => Err(TargetError::new(
            "primary_window_unavailable",
            "no primary window is available",
            json!({}),
        )),
        _ => Err(TargetError::new(
            "primary_window_ambiguous",
            "multiple primary windows are present",
            json!({"windows": candidate_details(&windows)}),
        )),
    }
}

fn find_camera(
    world: &mut World,
    primary_window: Entity,
    requested_name: Option<&str>,
) -> Result<Entity, TargetError> {
    let expected_target =
        RenderTarget::Window(WindowRef::Entity(primary_window)).normalize(Some(primary_window));
    let mut query = world.query::<(Entity, &Camera, &RenderTarget, Option<&Name>)>();
    let mut candidates = Vec::new();
    let mut camera_count = 0usize;
    let mut truncated = false;
    for (entity, camera, target, name) in query.iter(world).take(MAX_CAMERAS + 1) {
        if camera_count == MAX_CAMERAS {
            truncated = true;
            break;
        }
        camera_count += 1;
        let name_matches = requested_name
            .map(|expected| name.is_some_and(|name| name.as_str() == expected))
            .unwrap_or(true);
        if camera.is_active
            && target.normalize(Some(primary_window)) == expected_target
            && name_matches
        {
            candidates.push(entity);
        }
    }
    if truncated {
        return Err(TargetError::new(
            "camera_search_truncated",
            "camera search exceeded the bounded camera scan",
            json!({"limit": MAX_CAMERAS, "candidates": candidate_details(&candidates)}),
        ));
    }
    match candidates.as_slice() {
        [camera] => Ok(*camera),
        [] => Err(TargetError::new(
            "camera_unavailable",
            "no active primary-window camera exactly matched the request",
            json!({"name": requested_name, "scanned": camera_count}),
        )),
        _ => Err(TargetError::new(
            "camera_ambiguous",
            "multiple active primary-window cameras matched the request",
            json!({
                "name": requested_name,
                "count": candidates.len(),
                "candidates": candidate_details(&candidates),
                "candidate_details_truncated": candidates.len() > MAX_CANDIDATE_DETAILS,
            }),
        )),
    }
}

fn project_ui(
    world: &World,
    entity: Entity,
    camera_entity: Entity,
    primary_window: Entity,
) -> Result<ResolvedTarget, TargetError> {
    let node = world
        .get::<ComputedNode>(entity)
        .ok_or_else(|| TargetError::unavailable(entity, "missing_computed_node"))?;
    let size = node.size();
    if !size.is_finite() || !size.cmpgt(Vec2::ZERO).all() {
        return Err(TargetError::unavailable(entity, "empty_computed_node"));
    }
    require_visible(world, entity)?;
    let transform = world
        .get::<UiGlobalTransform>(entity)
        .ok_or_else(|| TargetError::unavailable(entity, "missing_ui_global_transform"))?;
    let target_camera = world
        .get::<ComputedUiTargetCamera>(entity)
        .and_then(ComputedUiTargetCamera::get)
        .ok_or_else(|| TargetError::unavailable(entity, "missing_ui_target_camera"))?;
    if target_camera != camera_entity {
        return Err(TargetError::unavailable(entity, "ui_camera_mismatch"));
    }
    let camera = world
        .get::<Camera>(camera_entity)
        .ok_or_else(|| TargetError::unavailable(entity, "camera_component_missing"))?;
    let window = world
        .get::<Window>(primary_window)
        .ok_or_else(|| TargetError::unavailable(entity, "primary_window_missing"))?;
    let viewport = physical_viewport(camera, window)
        .ok_or_else(|| TargetError::unavailable(entity, "empty_camera_viewport"))?;

    let half = size * 0.5;
    let affine = transform.affine();
    let corners = [
        affine.transform_point2(Vec2::new(-half.x, -half.y)),
        affine.transform_point2(Vec2::new(half.x, -half.y)),
        affine.transform_point2(Vec2::new(half.x, half.y)),
        affine.transform_point2(Vec2::new(-half.x, half.y)),
    ]
    .map(|point| point + viewport.min);
    if corners.iter().any(|corner| !corner.is_finite()) {
        return Err(TargetError::unavailable(entity, "invalid_ui_transform"));
    }
    let raw = points_rect(&corners);
    let mut visible = intersect_rect(raw, viewport)
        .ok_or_else(|| TargetError::unavailable(entity, "fully_clipped"))?;
    if let Some(clip) = world.get::<CalculatedClip>(entity) {
        let clip = Rect::from_corners(clip.clip.min + viewport.min, clip.clip.max + viewport.min);
        visible = intersect_rect(visible, clip)
            .ok_or_else(|| TargetError::unavailable(entity, "fully_clipped"))?;
    }
    let inverse_scale = node.inverse_scale_factor();
    if !inverse_scale.is_finite() || inverse_scale <= 0.0 {
        return Err(TargetError::unavailable(entity, "invalid_ui_scale_factor"));
    }
    let logical = Rect::from_corners(visible.min * inverse_scale, visible.max * inverse_scale);
    Ok(resolved(
        entity,
        ResolvedKind::Ui,
        camera_entity,
        logical,
        raw != visible,
    ))
}

fn project_world(
    world: &World,
    entity: Entity,
    camera_entity: Entity,
    primary_window: Entity,
) -> Result<ResolvedTarget, TargetError> {
    require_visible(world, entity)?;
    let transform = world
        .get::<GlobalTransform>(entity)
        .ok_or_else(|| TargetError::unavailable(entity, "missing_global_transform"))?;
    let camera = world
        .get::<Camera>(camera_entity)
        .ok_or_else(|| TargetError::unavailable(entity, "camera_component_missing"))?;
    let camera_transform = world
        .get::<GlobalTransform>(camera_entity)
        .ok_or_else(|| TargetError::unavailable(entity, "camera_transform_missing"))?;
    let window = world
        .get::<Window>(primary_window)
        .ok_or_else(|| TargetError::unavailable(entity, "primary_window_missing"))?;
    let visible_viewport = logical_viewport(camera, window)
        .ok_or_else(|| TargetError::unavailable(entity, "empty_camera_viewport"))?;
    let projected_center = camera
        .world_to_viewport(camera_transform, transform.translation())
        .map_err(|_| TargetError::unavailable(entity, "center_projection_failed"))?;

    let Some(aabb) = world.get::<Aabb>(entity) else {
        if !visible_viewport.contains(projected_center) {
            return Err(TargetError::unavailable(entity, "fully_clipped"));
        }
        return Ok(ResolvedTarget {
            entity: entity_label(entity),
            selector_source: "",
            selector_value: String::new(),
            kind: ResolvedKind::World,
            name: None,
            marker: None,
            camera: entity_label(camera_entity),
            camera_name: None,
            center: projected_center.to_array(),
            bounds: None,
            visible: true,
            clipped: false,
        });
    };

    let local_center: Vec3 = aabb.center.into();
    let half: Vec3 = aabb.half_extents.into();
    let mut projected = [Vec2::ZERO; 8];
    let mut projected_count = 0usize;
    for index in 0..8 {
        let sign = Vec3::new(
            if index & 1 == 0 { -1.0 } else { 1.0 },
            if index & 2 == 0 { -1.0 } else { 1.0 },
            if index & 4 == 0 { -1.0 } else { 1.0 },
        );
        let world_corner = transform.transform_point(local_center + half * sign);
        if let Ok(point) = camera.world_to_viewport(camera_transform, world_corner) {
            projected[projected_count] = point;
            projected_count += 1;
        }
    }
    if projected_count != projected.len() {
        if !visible_viewport.contains(projected_center) {
            return Err(TargetError::unavailable(entity, "fully_clipped"));
        }
        return Ok(ResolvedTarget {
            entity: entity_label(entity),
            selector_source: "",
            selector_value: String::new(),
            kind: ResolvedKind::World,
            name: None,
            marker: None,
            camera: entity_label(camera_entity),
            camera_name: None,
            center: projected_center.to_array(),
            bounds: None,
            visible: true,
            clipped: true,
        });
    }
    let raw = points_rect(&projected);
    let visible = intersect_rect(raw, visible_viewport)
        .ok_or_else(|| TargetError::unavailable(entity, "fully_clipped"))?;
    Ok(resolved(
        entity,
        ResolvedKind::World,
        camera_entity,
        visible,
        raw != visible,
    ))
}

fn require_visible(world: &World, entity: Entity) -> Result<(), TargetError> {
    match world.get::<InheritedVisibility>(entity) {
        Some(visibility) if visibility.get() => Ok(()),
        Some(_) => Err(TargetError::unavailable(entity, "hidden")),
        None => Err(TargetError::unavailable(
            entity,
            "missing_inherited_visibility",
        )),
    }
}

fn physical_viewport(camera: &Camera, window: &Window) -> Option<Rect> {
    let viewport = camera.physical_viewport_rect()?.as_rect();
    let window_rect = Rect::from_corners(
        Vec2::ZERO,
        Vec2::new(
            window.physical_width() as f32,
            window.physical_height() as f32,
        ),
    );
    intersect_rect(viewport, window_rect)
}

fn logical_viewport(camera: &Camera, window: &Window) -> Option<Rect> {
    let viewport = camera.logical_viewport_rect()?;
    let window_rect = Rect::from_corners(Vec2::ZERO, Vec2::new(window.width(), window.height()));
    intersect_rect(viewport, window_rect)
}

fn points_rect<const N: usize>(points: &[Vec2; N]) -> Rect {
    let mut min = points[0];
    let mut max = points[0];
    for point in points.iter().skip(1) {
        min = min.min(*point);
        max = max.max(*point);
    }
    Rect::from_corners(min, max)
}

fn intersect_rect(left: Rect, right: Rect) -> Option<Rect> {
    let intersection = Rect {
        min: left.min.max(right.min),
        max: left.max.min(right.max),
    };
    (intersection.min.cmplt(intersection.max).all()).then_some(intersection)
}

fn resolved(
    entity: Entity,
    kind: ResolvedKind,
    camera: Entity,
    bounds: Rect,
    clipped: bool,
) -> ResolvedTarget {
    ResolvedTarget {
        entity: entity_label(entity),
        selector_source: "",
        selector_value: String::new(),
        kind,
        name: None,
        marker: None,
        camera: entity_label(camera),
        camera_name: None,
        center: bounds.center().to_array(),
        bounds: Some(ScreenRect {
            x: bounds.min.x,
            y: bounds.min.y,
            width: bounds.width(),
            height: bounds.height(),
        }),
        visible: true,
        clipped,
    }
}

fn candidate_details(candidates: &[Entity]) -> Vec<String> {
    candidates
        .iter()
        .take(MAX_CANDIDATE_DETAILS)
        .copied()
        .map(entity_label)
        .collect()
}

fn entity_label(entity: Entity) -> String {
    format!("{entity:?}")
}
