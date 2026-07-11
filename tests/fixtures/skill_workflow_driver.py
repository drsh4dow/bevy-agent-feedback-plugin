#!/usr/bin/env python3
"""End-to-end driver for the ignored rendered skill workflow test."""
from __future__ import annotations

import os
import math
from decimal import Decimal
from pathlib import Path
from typing import Any

import bevy_feedback
from bevy_feedback import BevyFeedbackClient, fail

BUTTON_REGION = (160, 150, 320, 180)
READY_COLOR = (20, 80, 220)
CLICKED_COLOR = (20, 210, 70)
ADVANCE_NANOSECONDS = 125_000_000


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def exact_nanoseconds(value: Any, field: str) -> int:
    try:
        nanoseconds = Decimal(str(value)) * Decimal(1_000_000_000)
    except Exception as error:
        fail(f"{field} was not a numeric second value: {value!r} ({error})")
    integral = nanoseconds.to_integral_value()
    require(nanoseconds == integral, f"{field} was not an exact integer nanosecond value: {value!r}")
    return int(integral)


def result_details(response: dict[str, Any]) -> dict[str, Any]:
    result = response.get("result")
    details = result.get("details") if isinstance(result, dict) else None
    require(isinstance(details, dict), f"response omitted result.details: {response}")
    return details


def validate_capture(
    path: Path,
    info: dict[str, Any] | None,
    *,
    expected_label: str | None,
) -> None:
    require(info is not None, f"capture {path} omitted retained metadata")
    assert info is not None
    require(Path(info.get("path", "")) == path, f"capture path metadata disagreed with {path}: {info}")
    sequence = info.get("sequence")
    require(
        isinstance(sequence, int) and not isinstance(sequence, bool) and sequence >= 0,
        f"capture sequence was invalid: {info}",
    )
    require(info.get("label") == expected_label, f"capture label mismatch: {info}")
    require(info.get("completion") == "screenshot_captured", f"capture readback was incomplete: {info}")
    requested = info.get("requested_frame")
    completed = info.get("completed_frame")
    require(isinstance(requested, int) and requested >= 0, f"invalid request frame: {info}")
    require(
        isinstance(completed, int) and completed >= requested,
        f"capture completed before it was requested: {info}",
    )
    image_width = info.get("image_width")
    image_height = info.get("image_height")
    require(
        isinstance(image_width, int)
        and not isinstance(image_width, bool)
        and image_width > 0
        and isinstance(image_height, int)
        and not isinstance(image_height, bool)
        and image_height > 0,
        f"capture image dimensions were invalid: {info}",
    )
    request_window = info.get("window_at_request")
    completion_window = info.get("window_at_completion")
    for phase, window in (("request", request_window), ("completion", completion_window)):
        require(isinstance(window, dict), f"capture omitted {phase} window metadata: {info}")
        assert isinstance(window, dict)
        physical_width = window.get("physical_width")
        physical_height = window.get("physical_height")
        require(
            isinstance(physical_width, int)
            and physical_width > 0
            and isinstance(physical_height, int)
            and physical_height > 0,
            f"{phase} window physical dimensions were invalid: {window}",
        )
        logical_width = window.get("logical_width")
        logical_height = window.get("logical_height")
        scale_factor = window.get("scale_factor")
        require(
            isinstance(logical_width, (int, float))
            and not isinstance(logical_width, bool)
            and math.isfinite(logical_width)
            and logical_width > 0
            and isinstance(logical_height, (int, float))
            and not isinstance(logical_height, bool)
            and math.isfinite(logical_height)
            and logical_height > 0,
            f"{phase} window logical dimensions were invalid: {window}",
        )
        require(
            isinstance(scale_factor, (int, float))
            and not isinstance(scale_factor, bool)
            and math.isfinite(scale_factor)
            and scale_factor == 1.0,
            f"{phase} window scale was not deterministic: {window}",
        )
        require(
            abs(logical_width * scale_factor - physical_width) <= 1.0
            and abs(logical_height * scale_factor - physical_height) <= 1.0,
            f"{phase} logical/physical window dimensions disagreed: {window}",
        )
        if phase == "completion":
            require(
                physical_width == image_width and physical_height == image_height,
                f"completion window dimensions disagreed with the PNG: {window}, {info}",
            )
        require(isinstance(window.get("focused"), bool), f"{phase} focus metadata was invalid: {window}")
        require(window.get("mode") == "windowed", f"{phase} window mode was not windowed: {window}")
        cursor = window.get("cursor_position")
        if cursor is not None:
            require(
                isinstance(cursor, list)
                and len(cursor) == 2
                and all(
                    isinstance(value, (int, float))
                    and not isinstance(value, bool)
                    and math.isfinite(value)
                    for value in cursor
                )
                and 0.0 <= cursor[0] < logical_width
                and 0.0 <= cursor[1] < logical_height,
                f"{phase} cursor metadata was invalid: {window}",
            )
        require(window.get("visible") is True, f"{phase} window was not visible: {window}")
    require(path.is_file() and path.stat().st_size > 0, f"capture PNG was not persisted: {path}")


def drive(game: BevyFeedbackClient) -> None:
    artifact_dir = Path(os.environ.get("BEVY_FEEDBACK_ARTIFACTS", ""))
    expected_module = (artifact_dir / "python" / "bevy_feedback.py").resolve()
    actual_module = Path(bevy_feedback.__file__).resolve()
    require(
        actual_module == expected_module,
        f"driver did not import the CLI-injected bundled client: {actual_module} != {expected_module}",
    )
    require(game.deterministic_time is True, "game did not advertise deterministic time")

    first = game.wait_until_first_capture()
    first_info = dict(game.last_capture_info or {})
    validate_capture(first, first_info, expected_label=None)
    game.assert_color_present(
        first,
        READY_COLOR,
        BUTTON_REGION,
        tolerance=12,
        min_pixels=40_000,
    )

    state = game.wait_for_state("WorkflowState", "Ready", max_frames=120)
    require(state.get("outcome") == "matched", f"state readiness did not match: {state}")
    marker = game.wait_for_marker_present("WorkflowButton", max_frames=120)
    require(marker.get("outcome") == "matched" and marker.get("count") == 1, f"marker readiness failed: {marker}")
    target = game.wait_for_target(
        {"name": "WorkflowButton"},
        kind="ui",
        camera="WorkflowCamera",
        max_frames=120,
    )
    require(target.get("outcome") == "matched", f"semantic target readiness failed: {target}")
    ready = game.wait_for_resource(
        "WorkflowStatus", "clicked", "eq", False, max_frames=30
    )
    require(ready.get("outcome") == "matched", f"workflow was clicked before the named action: {ready}")
    require(
        game.read_resource_field("WorkflowStatus", "elapsed_nanoseconds") == 0,
        "deterministic virtual time advanced while the workflow waited for semantic readiness",
    )

    click_response = game.click_named(
        "WorkflowButton",
        kind="ui",
        camera="WorkflowCamera",
        button="Left",
        frames=1,
    )
    click_result = click_response.get("result")
    require(isinstance(click_result, dict), f"named click omitted result metadata: {click_response}")
    assert isinstance(click_result, dict)
    click_details = click_result.get("details")
    require(
        click_result.get("status") == "input_dispatched"
        and click_result.get("mouse_position") == [320.0, 240.0]
        and isinstance(click_details, dict)
        and click_details.get("target_resolved") is True
        and click_details.get("input_dispatched") is True
        and click_details.get("logical_position") == [320.0, 240.0]
        and click_details.get("button") == "Left"
        and click_details.get("name") == "WorkflowButton"
        and click_details.get("camera_name") == "WorkflowCamera"
        and click_details.get("kind") == "ui"
        and click_details.get("center") == [320.0, 240.0]
        and click_details.get("bounds")
        == {"x": 160.0, "y": 150.0, "width": 320.0, "height": 180.0},
        f"named click did not atomically resolve the exact visible target: {click_response}",
    )
    clicked = game.wait_for_resource(
        "WorkflowStatus", "clicked", "eq", True, max_frames=30
    )
    require(clicked.get("outcome") == "matched", f"same-PreUpdate named click was not consumed: {clicked}")
    after_click = game.capture_after_frames(1, label="workflow-clicked")
    after_click_info = dict(game.last_capture_info or {})
    validate_capture(
        after_click,
        after_click_info,
        expected_label="workflow-clicked",
    )
    game.assert_region_changed(first, after_click, BUTTON_REGION, min_pixels=40_000)
    game.assert_color_present(
        after_click,
        CLICKED_COLOR,
        BUTTON_REGION,
        tolerance=12,
        min_pixels=40_000,
    )

    responses = game.advance_time(0.125, step_seconds=0.04)
    require(len(responses) == 1, f"unexpected deterministic client chunking: {responses}")
    timing = result_details(responses[0])
    require(timing.get("step_count") == 4, f"advance response reported wrong step count: {timing}")
    require(
        exact_nanoseconds(timing.get("actual_seconds"), "actual_seconds")
        == ADVANCE_NANOSECONDS,
        f"advance response reported the wrong exact duration: {timing}",
    )
    require(
        exact_nanoseconds(timing.get("end_seconds"), "end_seconds")
        - exact_nanoseconds(timing.get("start_seconds"), "start_seconds")
        == ADVANCE_NANOSECONDS,
        f"advance response endpoints did not span exactly 125ms: {timing}",
    )
    elapsed = game.wait_for_resource(
        "WorkflowStatus",
        "elapsed_nanoseconds",
        "eq",
        ADVANCE_NANOSECONDS,
        max_frames=30,
    )
    require(elapsed.get("outcome") == "matched", f"virtual elapsed time was not exact: {elapsed}")
    steps = game.wait_for_resource(
        "WorkflowStatus", "advance_steps", "eq", 4, max_frames=30
    )
    require(steps.get("outcome") == "matched", f"game observed the wrong delta sequence length: {steps}")

    final = game.capture_after_frames(1, label="workflow-final")
    final_info = dict(game.last_capture_info or {})
    validate_capture(
        final,
        final_info,
        expected_label="workflow-final",
    )
    require(
        final_info["sequence"] > after_click_info["sequence"] > first_info["sequence"],
        f"capture sequence did not increase monotonically: {first_info}, {after_click_info}, {final_info}",
    )
    require(
        final_info["requested_frame"] > after_click_info["completed_frame"],
        f"final capture was not requested after deterministic advancement: {after_click_info}, {final_info}",
    )
    game.assert_color_present(
        final,
        CLICKED_COLOR,
        BUTTON_REGION,
        tolerance=12,
        min_pixels=40_000,
    )


bevy_feedback.run(drive)
