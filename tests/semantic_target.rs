#![cfg(feature = "diagnostics")]

mod semantic_target_support;

use bevy::prelude::Vec2;
use bevy_agent_feedback_plugin::client::{
    AgentClient, ClientError, TargetBounds, TargetKind, TargetSelector,
};
use semantic_target_support::{RenderGeometry, run_rendered_contract};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

const EPSILON: f32 = 0.05;

#[test]
#[ignore = "requires a graphics-capable environment"]
fn semantic_target_rendered_contract() {
    run_rendered_contract(drive_semantic_targets);
}
fn drive_semantic_targets(
    protocol_file: &Path,
    geometry_requested: &AtomicBool,
    geometry_receiver: &mpsc::Receiver<RenderGeometry>,
) -> Result<(), String> {
    let mut client = AgentClient::connect(protocol_file).map_err(client_error)?;
    client.wait_frames(8).map_err(client_error)?;
    geometry_requested.store(true, Ordering::Release);
    let geometry = geometry_receiver
        .recv_timeout(Duration::from_secs(2))
        .map_err(|error| format!("render geometry was unavailable after warmup: {error}"))?;

    let rotated = target_info(&mut client, "RotatedScaled", TargetKind::Ui, "LeftUiCamera")?;
    expect_bounds(
        "rotated/scaled UI",
        rotated.bounds,
        TargetBounds {
            x: geometry.left_ui_viewport.min.x + 130.0,
            y: geometry.left_ui_viewport.min.y + 40.0,
            width: 20.0,
            height: 160.0,
        },
    )?;
    expect_point(
        "rotated/scaled UI center",
        rotated.center,
        [
            geometry.left_ui_viewport.min.x + 140.0,
            geometry.left_ui_viewport.min.y + 120.0,
        ],
    )?;
    if rotated.clipped || !rotated.visible {
        return Err(format!(
            "rotated/scaled UI visibility was wrong: {rotated:?}"
        ));
    }

    let overflow = target_info(&mut client, "OverflowChild", TargetKind::Ui, "LeftUiCamera")?;
    expect_bounds(
        "overflow-clipped UI",
        overflow.bounds,
        TargetBounds {
            x: geometry.left_ui_viewport.min.x + 280.0,
            y: geometry.left_ui_viewport.min.y + 320.0,
            width: 20.0,
            height: 40.0,
        },
    )?;
    expect_point(
        "overflow-clipped UI center",
        overflow.center,
        [
            geometry.left_ui_viewport.min.x + 290.0,
            geometry.left_ui_viewport.min.y + 340.0,
        ],
    )?;
    if !overflow.clipped || !overflow.visible {
        return Err(format!("overflow clipping flags were wrong: {overflow:?}"));
    }

    let accessible = client
        .target_info(
            TargetSelector::AccessibilityLabel("Launch Mission".to_string()),
            TargetKind::Ui,
            Some("LeftUiCamera"),
        )
        .map_err(client_error)?;
    if accessible.selector_source != "accessibility_label"
        || accessible.selector_value != "Launch Mission"
        || accessible.name.as_deref() != Some("AccessibilityFixture")
    {
        return Err(format!(
            "accessibility-label resolution returned the wrong target: {accessible:?}"
        ));
    }
    expect_bounds(
        "accessibility-labeled UI",
        accessible.bounds,
        TargetBounds {
            x: geometry.left_ui_viewport.min.x + 20.0,
            y: geometry.left_ui_viewport.min.y + 220.0,
            width: 100.0,
            height: 30.0,
        },
    )?;

    let right = target_info(
        &mut client,
        "RightViewportTarget",
        TargetKind::Ui,
        "RightUiCamera",
    )?;
    expect_bounds(
        "right split-screen UI",
        right.bounds,
        TargetBounds {
            x: geometry.right_ui_viewport.min.x + 25.0,
            y: geometry.right_ui_viewport.min.y + 35.0,
            width: 70.0,
            height: 30.0,
        },
    )?;
    if right.camera_name.as_deref() != Some("RightUiCamera") {
        return Err(format!(
            "ComputedUiTargetCamera selected the wrong camera: {right:?}"
        ));
    }
    expect_command_error(
        client.target_info(
            TargetSelector::Name("RightViewportTarget".to_string()),
            TargetKind::Ui,
            Some("LeftUiCamera"),
        ),
        "target_unavailable",
        Some("ui_camera_mismatch"),
    )?;

    let world_viewport_center = (geometry.world_viewport.min + geometry.world_viewport.max) * 0.5;
    let world_target_center = world_viewport_center + Vec2::new(30.0, 40.0);
    let world = target_info(
        &mut client,
        "TransformedAabb",
        TargetKind::World,
        "WorldOrthoCamera",
    )?;
    expect_bounds(
        "transformed world AABB",
        world.bounds,
        TargetBounds {
            x: world_target_center.x - 30.0,
            y: world_target_center.y - 40.0,
            width: 60.0,
            height: 80.0,
        },
    )?;
    expect_point(
        "transformed world center",
        world.center,
        world_target_center.to_array(),
    )?;
    if world.clipped || !world.visible {
        return Err(format!("transformed world visibility was wrong: {world:?}"));
    }

    let near = target_info(
        &mut client,
        "NearPlaneAabb",
        TargetKind::World,
        "NearCamera",
    )?;
    let near_viewport_center = (geometry.near_viewport.min + geometry.near_viewport.max) * 0.5;
    expect_point(
        "partial near-plane center",
        near.center,
        near_viewport_center.to_array(),
    )?;
    if near.bounds.is_some() || !near.clipped || !near.visible {
        return Err(format!(
            "partial near-plane AABB must be center-only and clipped: {near:?}"
        ));
    }

    expect_command_error(
        client.target_info(
            TargetSelector::Name("DuplicateWorld".to_string()),
            TargetKind::World,
            Some("WorldOrthoCamera"),
        ),
        "ambiguous_target",
        None,
    )?;
    expect_command_error(
        client.target_info(
            TargetSelector::Name("TransformedAabb".to_string()),
            TargetKind::World,
            None,
        ),
        "camera_ambiguous",
        None,
    )?;
    expect_command_error(
        client.target_info(
            TargetSelector::Name("HiddenWorld".to_string()),
            TargetKind::World,
            Some("WorldOrthoCamera"),
        ),
        "target_unavailable",
        Some("hidden"),
    )?;
    expect_command_error(
        client.target_info(
            TargetSelector::Name("OffscreenWorld".to_string()),
            TargetKind::World,
            Some("WorldOrthoCamera"),
        ),
        "target_unavailable",
        Some("fully_clipped"),
    )?;

    let click = client
        .click_target(
            TargetSelector::Name("MovingButton".to_string()),
            TargetKind::Ui,
            Some("LeftUiCamera"),
            "Left",
            1,
        )
        .map_err(client_error)?;
    if click["result"]["status"] != "clicked_target"
        || click["result"]["details"]["name"] != "MovingButton"
    {
        return Err(format!(
            "named click did not retain its exact resolved target: {click}"
        ));
    }

    Ok(())
}

fn target_info(
    client: &mut AgentClient,
    name: &str,
    kind: TargetKind,
    camera: &str,
) -> Result<bevy_agent_feedback_plugin::client::TargetInfo, String> {
    client
        .target_info(TargetSelector::Name(name.to_string()), kind, Some(camera))
        .map_err(|error| format!("target_info {name:?} via {camera:?} failed: {error}"))
}

fn expect_bounds(
    label: &str,
    actual: Option<TargetBounds>,
    expected: TargetBounds,
) -> Result<(), String> {
    let actual = actual.ok_or_else(|| format!("{label} had no visible bounds"))?;
    expect_near(label, "x", actual.x, expected.x)?;
    expect_near(label, "y", actual.y, expected.y)?;
    expect_near(label, "width", actual.width, expected.width)?;
    expect_near(label, "height", actual.height, expected.height)
}

fn expect_point(label: &str, actual: [f32; 2], expected: [f32; 2]) -> Result<(), String> {
    expect_near(label, "x", actual[0], expected[0])?;
    expect_near(label, "y", actual[1], expected[1])
}

fn expect_near(label: &str, field: &str, actual: f32, expected: f32) -> Result<(), String> {
    if (actual - expected).abs() <= EPSILON {
        Ok(())
    } else {
        Err(format!(
            "{label} {field} was {actual}, expected {expected} ± {EPSILON}"
        ))
    }
}

fn expect_command_error<T>(
    result: Result<T, ClientError>,
    expected_code: &str,
    expected_reason: Option<&str>,
) -> Result<(), String> {
    match result {
        Err(ClientError::Command { code, context, .. }) if code == expected_code => {
            if let Some(expected_reason) = expected_reason {
                let actual = context
                    .as_ref()
                    .and_then(|value| value["diagnostic"]["reason"].as_str());
                if actual != Some(expected_reason) {
                    return Err(format!(
                        "command error {expected_code} reason was {actual:?}, expected {expected_reason:?}; context={context:?}"
                    ));
                }
            }
            Ok(())
        }
        Err(error) => Err(format!(
            "expected command error {expected_code}, received {error}"
        )),
        Ok(_) => Err(format!(
            "expected command error {expected_code}, command succeeded"
        )),
    }
}

fn client_error(error: ClientError) -> String {
    error.to_string()
}
