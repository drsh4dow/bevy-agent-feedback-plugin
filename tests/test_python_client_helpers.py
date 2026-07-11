#!/usr/bin/env python3
from __future__ import annotations

import contextlib
import io
import json
import sys
import tempfile
import unittest
from dataclasses import FrozenInstanceError
from pathlib import Path
from typing import Any, Callable
from unittest import mock

CLIENTS_PYTHON = Path(__file__).resolve().parents[1] / "clients" / "python"
sys.path.insert(0, str(CLIENTS_PYTHON))

import bevy_feedback
from bevy_feedback import (
    BevyFeedbackCapabilities,
    BevyFeedbackClient,
    BevyFeedbackError,
    fail,
    run,
)


ROOT = Path(__file__).resolve().parents[1]


class RecordingTransport:
    def __init__(self, responses: list[dict[str, Any]]) -> None:
        self.responses = list(responses)
        self.requests: list[dict[str, Any]] = []

    def __call__(self, request: dict[str, Any]) -> dict[str, Any]:
        self.requests.append(request)
        index = len(self.requests) - 1
        if index >= len(self.responses):
            raise AssertionError(f"unexpected request: {request}")
        return self.responses[index]


class JsonLineStream:
    def __init__(self, responses: list[dict[str, Any]]) -> None:
        self.responses = list(responses)
        self.next_response = 0
        self.requests: list[dict[str, Any]] = []

    def write(self, line: str) -> int:
        self.requests.append(json.loads(line))
        return len(line)

    def flush(self) -> None:
        pass

    def readline(self) -> str:
        if self.next_response >= len(self.responses):
            return ""
        response = self.responses[self.next_response]
        self.next_response += 1
        return json.dumps(response, separators=(",", ":")) + "\n"


def make_client(
    responses: list[dict[str, Any]],
    *,
    max_wait_frames: int = 300,
    max_abort_predicates: int = 16,
    max_time_advance_steps: int = 600,
    max_time_advance_seconds: float = 10.0,
) -> tuple[BevyFeedbackClient, RecordingTransport]:
    client = BevyFeedbackClient.__new__(BevyFeedbackClient)
    client.capabilities = BevyFeedbackCapabilities(
        max_wait_frames=max_wait_frames,
        max_abort_predicates=max_abort_predicates,
        deterministic_time=False,
        max_time_advance_steps=max_time_advance_steps,
        max_time_advance_seconds=max_time_advance_seconds,
    )
    client._timing_advertised = True
    client.last_capture = None
    client.last_capture_info = None
    client.last_observation = None
    client.last_error_context = None
    transport = RecordingTransport(responses)
    client.request = transport
    return client, transport


def capture_metadata(
    path: str,
    *,
    label: str | None = "ready",
    requested_frame: int = 40,
    completed_frame: int = 42,
    image_size: tuple[int, int] = (1280, 720),
) -> dict[str, Any]:
    width, height = image_size
    request_window = {
        "logical_width": width / 2,
        "logical_height": height / 2,
        "physical_width": width,
        "physical_height": height,
        "scale_factor": 2.0,
        "cursor_position": [width / 4, height / 4],
        "focused": True,
        "visible": True,
        "mode": "windowed",
    }
    completion_window = dict(request_window)
    completion_window["cursor_position"] = [width / 3, height / 3]
    capture = {
        "sequence": 5,
        "path": path,
        "requested_frame": requested_frame,
        "completed_frame": completed_frame,
        "image_width": width,
        "image_height": height,
        "window_at_request": request_window,
        "window_at_completion": completion_window,
        "completion": "screenshot_captured",
    }
    if label is not None:
        capture["label"] = label
    return capture


def capture_response(
    path: str,
    *,
    label: str | None = "ready",
    requested_frame: int = 40,
    completed_frame: int = 42,
    image_size: tuple[int, int] = (1280, 720),
) -> dict[str, Any]:
    capture = capture_metadata(
        path,
        label=label,
        requested_frame=requested_frame,
        completed_frame=completed_frame,
        image_size=image_size,
    )
    return {
        "ok": True,
        "result": {
            "status": "captured",
            "frame": completed_frame,
            "game_time_secs": 3.5,
            "window": capture.get("window_at_completion"),
            "mouse_position": [100.0, 50.0],
            "pressed_keys": [],
            "pressed_buttons": [],
            "capture": capture,
            "latest_capture": dict(capture),
        },
    }


def diagnostic_response(observation: dict[str, Any]) -> dict[str, Any]:
    return {
        "ok": True,
        "result": {
            "status": "evaluated",
            "details": dict(observation),
        },
    }


def write_png(
    directory: Path,
    name: str,
    size: tuple[int, int],
    pixels: dict[tuple[int, int], tuple[int, int, int, int]] | None = None,
) -> Path:
    from PIL import Image

    path = directory / name
    image = Image.new("RGBA", size, (0, 0, 0, 255))
    for point, color in (pixels or {}).items():
        image.putpixel(point, color)
    image.save(path)
    return path


class BevyFeedbackClientCoordinateHelpersTest(unittest.TestCase):
    def test_window_center_uses_logical_dimensions(self) -> None:
        client = BevyFeedbackClient.__new__(BevyFeedbackClient)
        client.window_info = lambda: {
            "result": {"window": {"logical_width": 955.0, "logical_height": 1170.0}}
        }

        self.assertEqual(client.window_center(), (477.5, 585.0))

    def test_point_uses_actual_logical_dimensions_after_window_manager_override(self) -> None:
        requested_size = (1280.0, 720.0)
        actual_size = (955.0, 1170.0)
        client = BevyFeedbackClient.__new__(BevyFeedbackClient)
        client.window_info = lambda: {
            "result": {
                "window": {
                    "logical_width": actual_size[0],
                    "logical_height": actual_size[1],
                }
            }
        }

        self.assertNotEqual(actual_size, requested_size)
        self.assertEqual(client.point(0.95, 0.5), (907.25, 585.0))

    def test_point_rejects_out_of_range_fractions(self) -> None:
        client = BevyFeedbackClient.__new__(BevyFeedbackClient)
        client.window_info = lambda: {
            "result": {"window": {"logical_width": 955.0, "logical_height": 1170.0}}
        }

        with self.assertRaisesRegex(BevyFeedbackError, "point fractions must satisfy"):
            client.point(1.0, 0.5)

    def test_point_rejects_missing_window_dimensions(self) -> None:
        client = BevyFeedbackClient.__new__(BevyFeedbackClient)
        client.window_info = lambda: {"result": {"window": {}}}

        with self.assertRaisesRegex(BevyFeedbackError, "missing logical window dimensions"):
            client.point(0.95, 0.5)


class BevyFeedbackClientTimingTest(unittest.TestCase):
    def test_capabilities_are_immutable(self) -> None:
        capabilities = BevyFeedbackCapabilities(300, 16, True, 600, 10.0)

        with self.assertRaisesRegex(FrozenInstanceError, "cannot assign"):
            capabilities.max_wait_frames = 600  # type: ignore[misc]

    def test_wait_frames_uses_one_request_and_rejects_oversize_before_io(self) -> None:
        response = {"ok": True, "result": {"frame": 3}}
        client, transport = make_client([response], max_wait_frames=3)

        with self.assertRaises(AttributeError):
            getattr(client, "wait")
        self.assertEqual(client.wait_frames(3), response)
        self.assertEqual(transport.requests, [{"command": "wait", "frames": 3}])

        oversized, oversized_transport = make_client([], max_wait_frames=3)
        with self.assertRaisesRegex(
            BevyFeedbackError,
            r"frames=7 exceeds server limit 3; .*AgentFeedbackConfig.max_wait_frames.*explicit bounded requests",
        ):
            oversized.wait_frames(7)
        self.assertEqual(oversized_transport.requests, [])

    def test_wait_seconds_omits_or_includes_the_requested_frame_cap(self) -> None:
        responses = [
            {"ok": True, "result": {"elapsed_seconds": 0.25}},
            {"ok": True, "result": {"elapsed_seconds": 0.5}},
        ]
        client, transport = make_client(responses, max_wait_frames=12)

        client.wait_seconds(0.25)
        client.wait_seconds(0.5, max_frames=8)

        self.assertEqual(
            transport.requests,
            [
                {"command": "wait_seconds", "seconds": 0.25},
                {"command": "wait_seconds", "seconds": 0.5, "max_frames": 8},
            ],
        )

    def test_advance_time_chunks_on_whole_steps_with_one_final_remainder(self) -> None:
        responses = [
            {"ok": True, "result": {"advanced": 0.2}},
            {"ok": True, "result": {"advanced": 0.2}},
            {"ok": True, "result": {"advanced": 0.05}},
        ]
        client, transport = make_client(
            responses,
            max_time_advance_steps=2,
            max_time_advance_seconds=0.25,
        )

        self.assertEqual(client.advance_time(0.45, step_seconds=0.1), responses)
        self.assertEqual(
            transport.requests,
            [
                {
                    "command": "advance_time",
                    "seconds": 0.2,
                    "step_seconds": 0.1,
                },
                {
                    "command": "advance_time",
                    "seconds": 0.2,
                    "step_seconds": 0.1,
                },
                {
                    "command": "advance_time",
                    "seconds": 0.05,
                    "step_seconds": 0.1,
                },
            ],
        )

    def test_advance_time_omits_step_for_one_server_bounded_request(self) -> None:
        response = {"ok": True, "result": {"advanced": 0.125}}
        client, transport = make_client(
            [response],
            max_time_advance_seconds=0.25,
        )

        self.assertEqual(client.advance_time(0.125), [response])
        self.assertEqual(
            transport.requests,
            [{"command": "advance_time", "seconds": 0.125}],
        )

    def test_advance_time_rejects_more_than_bounded_client_chunks(self) -> None:
        client, transport = make_client(
            [],
            max_time_advance_steps=1,
            max_time_advance_seconds=0.001,
        )

        with self.assertRaisesRegex(
            BevyFeedbackError, "requires 4097 chunks; maximum is 4096"
        ):
            client.advance_time(4.097, step_seconds=0.001)

        self.assertEqual(transport.requests, [])


class BevyFeedbackClientCaptureTest(unittest.TestCase):
    def test_capture_payloads_retain_complete_capture_metadata(self) -> None:
        immediate = capture_response("/captures/capture-000004.png", label=None)
        delayed = capture_response(
            "/captures/capture-000005-ready.png",
            label="ready",
            requested_frame=40,
            completed_frame=42,
        )
        client, transport = make_client([immediate, delayed])

        self.assertEqual(
            client.capture(),
            Path("/captures/capture-000004.png"),
        )
        self.assertEqual(
            client.last_capture_info,
            capture_metadata("/captures/capture-000004.png", label=None),
        )
        self.assertEqual(
            client.last_capture,
            Path("/captures/capture-000004.png"),
        )
        self.assertEqual(
            client.capture_after_frames(4, "ready"),
            Path("/captures/capture-000005-ready.png"),
        )

        expected = capture_metadata(
            "/captures/capture-000005-ready.png",
            label="ready",
            requested_frame=40,
            completed_frame=42,
        )
        self.assertEqual(client.last_capture_info, expected)
        self.assertEqual(client.last_capture, Path(expected["path"]))
        self.assertEqual(
            transport.requests,
            [
                {"command": "capture"},
                {
                    "command": "capture_after_frames",
                    "frames": 4,
                    "label": "ready",
                },
            ],
        )

    def test_animated_readiness_then_first_capture_uses_two_atomic_requests(self) -> None:
        predicate = {
            "type": "state_equals",
            "state": "AppState",
            "value": "MainMenu",
        }
        observation = {
            "predicate": predicate,
            "outcome": "matched",
            "observed": "MainMenu",
            "frames_observed": 3,
        }
        capture = capture_response(
            "/captures/capture-000001.png",
            label=None,
            requested_frame=3,
            completed_frame=4,
        )
        client, transport = make_client(
            [diagnostic_response(observation), capture],
            max_wait_frames=20,
        )

        self.assertEqual(
            client.wait_for_state("AppState", "MainMenu", max_frames=12),
            observation,
        )
        self.assertEqual(
            client.wait_until_first_capture(),
            Path("/captures/capture-000001.png"),
        )
        self.assertEqual(
            transport.requests,
            [
                {
                    "command": "wait_for",
                    "predicate": predicate,
                    "max_frames": 12,
                },
                {"command": "capture_after_frames", "frames": 1},
            ],
        )


class BevyFeedbackClientSemanticTargetTest(unittest.TestCase):
    def test_atomic_named_click_and_disappearance_each_use_one_request(self) -> None:
        predicate = {
            "type": "target_absent",
            "target": {"name": "Play"},
            "kind": "ui",
            "camera": "HudCamera",
        }
        observation = {
            "predicate": predicate,
            "outcome": "matched",
            "observed": {"matches": 0},
            "frames_observed": 2,
        }
        responses = [
            {"ok": True, "result": {"status": "clicked"}},
            diagnostic_response(observation),
        ]
        client, transport = make_client(responses, max_wait_frames=30)

        client.click_named("Play")
        self.assertEqual(
            client.wait_for_target_absent(
                {"name": "Play"},
                kind="ui",
                camera="HudCamera",
                max_frames=10,
            ),
            observation,
        )

        self.assertEqual(
            transport.requests,
            [
                {"command": "click_target", "target": {"name": "Play"}},
                {
                    "command": "wait_for",
                    "predicate": predicate,
                    "max_frames": 10,
                },
            ],
        )

    def test_semantic_target_methods_emit_exact_selector_payloads(self) -> None:
        cases: list[
            tuple[
                str,
                Callable[[BevyFeedbackClient], object],
                dict[str, Any],
            ]
        ] = [
            (
                "target info",
                lambda client: client.target_info(
                    {"accessibility_label": "Play"},
                    kind="ui",
                    camera="HudCamera",
                ),
                {
                    "command": "target_info",
                    "target": {"accessibility_label": "Play"},
                    "kind": "ui",
                    "camera": "HudCamera",
                },
            ),
            (
                "target wait",
                lambda client: client.wait_for_target(
                    {"marker": "Clickable"},
                    kind="world",
                    camera="WorldCamera",
                    max_frames=9,
                ),
                {
                    "command": "wait_for",
                    "predicate": {
                        "type": "target_exists",
                        "target": {"marker": "Clickable"},
                        "kind": "world",
                        "camera": "WorldCamera",
                    },
                    "max_frames": 9,
                },
            ),
            (
                "generic click target",
                lambda client: client.click_target(
                    {"marker": "Enemy"},
                    kind="world",
                    camera="WorldCamera",
                    button="Right",
                    frames=3,
                ),
                {
                    "command": "click_target",
                    "target": {"marker": "Enemy"},
                    "kind": "world",
                    "camera": "WorldCamera",
                    "button": "Right",
                    "frames": 3,
                },
            ),
            (
                "accessibility click",
                lambda client: client.click_accessibility_label(
                    "Resume", button="Left", frames=2
                ),
                {
                    "command": "click_target",
                    "target": {"accessibility_label": "Resume"},
                    "button": "Left",
                    "frames": 2,
                },
            ),
            (
                "marker click",
                lambda client: client.click_marker("Clickable"),
                {
                    "command": "click_target",
                    "target": {"marker": "Clickable"},
                },
            ),
        ]

        for name, invoke, expected in cases:
            with self.subTest(name=name):
                if expected["command"] == "wait_for":
                    predicate = expected["predicate"]
                    response = diagnostic_response(
                        {
                            "predicate": predicate,
                            "outcome": "matched",
                            "observed": {"matches": 1},
                        }
                    )
                else:
                    response = {"ok": True, "result": {"status": "ok"}}
                client, transport = make_client([response], max_wait_frames=10)

                invoke(client)

                self.assertEqual(transport.requests, [expected])

    def test_selector_with_multiple_exact_keys_is_rejected_before_io(self) -> None:
        client, transport = make_client([])

        with self.assertRaisesRegex(
            BevyFeedbackError,
            "target must contain exactly one of name, accessibility_label, or marker",
        ):
            client.target_info({"name": "Play", "marker": "Clickable"})

        self.assertEqual(transport.requests, [])


class BevyFeedbackClientDiagnosticsTest(unittest.TestCase):
    def test_resource_listing_and_scalar_field_read_use_exact_payloads(self) -> None:
        field_observation = {
            "resource": "RoundStats",
            "field": "score",
            "value": 17,
        }
        responses = [
            {"ok": True, "result": {"details": {"resources": ["RoundStats"]}}},
            {
                "ok": True,
                "result": {
                    "details": {
                        "resource": "RoundStats",
                        "fields": ["loaded", "score"],
                    }
                },
            },
            diagnostic_response(field_observation),
        ]
        client, transport = make_client(responses)

        client.resource_info()
        client.resource_info("RoundStats")
        self.assertEqual(client.read_resource_field("RoundStats", "score"), 17)

        self.assertEqual(
            transport.requests,
            [
                {"command": "resource_info"},
                {"command": "resource_info", "resource": "RoundStats"},
                {
                    "command": "resource_info",
                    "resource": "RoundStats",
                    "field": "score",
                },
            ],
        )

    def test_diagnostic_methods_return_structured_observations(self) -> None:
        cases: list[
            tuple[
                str,
                Callable[[BevyFeedbackClient], object],
                dict[str, Any],
            ]
        ] = [
            (
                "evaluate predicate",
                lambda client: client.evaluate_predicate(
                    {
                        "type": "resource_field",
                        "resource": "RoundStats",
                        "field": "loaded",
                        "operator": "eq",
                        "value": True,
                    }
                ),
                {
                    "command": "evaluate_predicate",
                    "predicate": {
                        "type": "resource_field",
                        "resource": "RoundStats",
                        "field": "loaded",
                        "operator": "eq",
                        "value": True,
                    },
                },
            ),
            (
                "generic wait",
                lambda client: client.wait_for(
                    {"type": "state_equals", "state": "AppState", "value": "Ready"}
                ),
                {
                    "command": "wait_for",
                    "predicate": {
                        "type": "state_equals",
                        "state": "AppState",
                        "value": "Ready",
                    },
                },
            ),
            (
                "state wait",
                lambda client: client.wait_for_state(
                    "AppState", "Playing", max_frames=11
                ),
                {
                    "command": "wait_for",
                    "predicate": {
                        "type": "state_equals",
                        "state": "AppState",
                        "value": "Playing",
                    },
                    "max_frames": 11,
                },
            ),
            (
                "resource wait",
                lambda client: client.wait_for_resource(
                    "RoundStats", "score", "gte", 10, max_frames=12
                ),
                {
                    "command": "wait_for",
                    "predicate": {
                        "type": "resource_field",
                        "resource": "RoundStats",
                        "field": "score",
                        "operator": "gte",
                        "value": 10,
                    },
                    "max_frames": 12,
                },
            ),
            (
                "bounded marker count wait",
                lambda client: client.wait_for_marker_count(
                    "Enemy", min_count=2, max_count=5, max_frames=13
                ),
                {
                    "command": "wait_for",
                    "predicate": {
                        "type": "marker_count",
                        "marker": "Enemy",
                        "min": 2,
                        "max": 5,
                    },
                    "max_frames": 13,
                },
            ),
            (
                "marker presence wait",
                lambda client: client.wait_for_marker_present("Clickable"),
                {
                    "command": "wait_for",
                    "predicate": {
                        "type": "marker_count",
                        "marker": "Clickable",
                        "min": 1,
                    },
                },
            ),
            (
                "marker absence wait",
                lambda client: client.wait_for_marker_absent(
                    "LoadingSpinner", max_frames=14
                ),
                {
                    "command": "wait_for",
                    "predicate": {
                        "type": "marker_count",
                        "marker": "LoadingSpinner",
                        "max": 0,
                    },
                    "max_frames": 14,
                },
            ),
        ]

        for name, invoke, expected in cases:
            with self.subTest(name=name):
                predicate = expected["predicate"]
                observation = {
                    "predicate": predicate,
                    "outcome": "matched",
                    "observed": {"value": 17, "count": 2},
                    "frames_observed": 4,
                }
                client, transport = make_client(
                    [diagnostic_response(observation)],
                    max_wait_frames=20,
                )

                self.assertEqual(invoke(client), observation)
                self.assertEqual(client.last_observation, observation)
                self.assertEqual(transport.requests, [expected])

    def test_state_abort_values_use_generic_abort_predicates(self) -> None:
        predicate = {
            "type": "state_equals",
            "state": "GamePhase",
            "value": "Playing",
        }
        abort = {
            "type": "state_equals",
            "state": "GamePhase",
            "value": "LoadFailed",
        }
        observation = {"predicate": predicate, "outcome": "matched", "value": "Playing"}
        client, transport = make_client([diagnostic_response(observation)])

        self.assertEqual(
            client.wait_for_state(
                "GamePhase",
                "Playing",
                abort_values=["LoadFailed"],
                max_frames=30,
            ),
            observation,
        )
        self.assertEqual(
            transport.requests,
            [{
                "command": "wait_for",
                "predicate": predicate,
                "abort_predicates": [abort],
                "max_frames": 30,
            }],
        )

    def test_oversized_semantic_wait_fails_before_io_with_remediation(self) -> None:
        client, transport = make_client([], max_wait_frames=3)

        with self.assertRaisesRegex(
            BevyFeedbackError,
            r"max_frames=7 exceeds server limit 3; .*AgentFeedbackConfig.max_wait_frames.*explicit bounded requests",
        ):
            client.wait_for_state("GamePhase", "Playing", max_frames=7)

        self.assertEqual(transport.requests, [])

    def test_semantic_failure_capture_attaches_metadata_to_original_error(self) -> None:
        failure = BevyFeedbackError(
            "command failed [predicate_aborted]: abort predicate matched",
            code="predicate_aborted",
            context={"snapshot": {"frame": 42}},
        )
        capture = capture_response(
            "/captures/capture-000010-semantic-wait-failure.png",
            label="semantic-wait-failure",
        )
        client, _transport = make_client([])
        requests: list[dict[str, Any]] = []

        def request(payload: dict[str, Any]) -> dict[str, Any]:
            requests.append(payload)
            if payload["command"] == "wait_for":
                raise failure
            return capture

        client.request = request
        with self.assertRaises(BevyFeedbackError) as raised:
            client.wait_for_state("GamePhase", "Playing", max_frames=3)

        self.assertIs(raised.exception, failure)
        self.assertEqual(
            failure.context["failure_capture"],  # type: ignore[index]
            capture["result"]["capture"],
        )
        self.assertEqual(
            requests,
            [
                {
                    "command": "wait_for",
                    "predicate": {
                        "type": "state_equals",
                        "state": "GamePhase",
                        "value": "Playing",
                    },
                    "max_frames": 3,
                },
                {"command": "capture", "label": "semantic-wait-failure"},
            ],
        )

    def test_capture_failure_preserves_the_original_semantic_error(self) -> None:
        failure = BevyFeedbackError(
            "command failed [predicate_timeout]: deadline",
            code="predicate_timeout",
            context={"snapshot": {"frame": 3}},
        )
        capture_failure = BevyFeedbackError("capture failed", code="capture_failed")
        client, _transport = make_client([])

        def request(payload: dict[str, Any]) -> dict[str, Any]:
            if payload["command"] == "wait_for":
                raise failure
            raise capture_failure

        client.request = request
        with self.assertRaises(BevyFeedbackError) as raised:
            client.wait_for_state("GamePhase", "Playing", max_frames=3)

        self.assertIs(raised.exception, failure)
        self.assertEqual(failure.context, {"snapshot": {"frame": 3}})


class BevyFeedbackClientPredicateAssertionsTest(unittest.TestCase):
    def test_assertion_methods_evaluate_exact_predicates(self) -> None:
        cases: list[
            tuple[
                str,
                Callable[[BevyFeedbackClient], object],
                dict[str, Any],
            ]
        ] = [
            (
                "state",
                lambda client: client.assert_state("AppState", "Playing"),
                {
                    "type": "state_equals",
                    "state": "AppState",
                    "value": "Playing",
                },
            ),
            (
                "resource",
                lambda client: client.assert_resource(
                    "RoundStats", "score", "gte", 20
                ),
                {
                    "type": "resource_field",
                    "resource": "RoundStats",
                    "field": "score",
                    "operator": "gte",
                    "value": 20,
                },
            ),
            (
                "marker count",
                lambda client: client.assert_marker_count(
                    "Enemy", min_count=1, max_count=4
                ),
                {
                    "type": "marker_count",
                    "marker": "Enemy",
                    "min": 1,
                    "max": 4,
                },
            ),
            (
                "marker present",
                lambda client: client.assert_marker_present("Clickable"),
                {
                    "type": "marker_count",
                    "marker": "Clickable",
                    "min": 1,
                },
            ),
            (
                "marker absent",
                lambda client: client.assert_marker_absent("LoadingSpinner"),
                {
                    "type": "marker_count",
                    "marker": "LoadingSpinner",
                    "max": 0,
                },
            ),
            (
                "target exists",
                lambda client: client.assert_target_exists(
                    {"accessibility_label": "Play"}, kind="ui"
                ),
                {
                    "type": "target_exists",
                    "target": {"accessibility_label": "Play"},
                    "kind": "ui",
                },
            ),
            (
                "target absent",
                lambda client: client.assert_target_absent(
                    {"name": "BlockingModal"},
                    kind="ui",
                    camera="HudCamera",
                ),
                {
                    "type": "target_absent",
                    "target": {"name": "BlockingModal"},
                    "kind": "ui",
                    "camera": "HudCamera",
                },
            ),
        ]

        for name, invoke, predicate in cases:
            with self.subTest(name=name):
                observation = {
                    "predicate": predicate,
                    "outcome": "matched",
                    "observed": {"count": 1},
                }
                client, transport = make_client(
                    [diagnostic_response(observation)]
                )

                invoke(client)

                self.assertEqual(
                    transport.requests,
                    [
                        {
                            "command": "evaluate_predicate",
                            "predicate": predicate,
                        }
                    ],
                )

    def test_assertion_rejects_indeterminate_with_structured_observation(self) -> None:
        predicate = {
            "type": "marker_count",
            "marker": "Enemy",
            "max": 0,
        }
        observation = {
            "predicate": predicate,
            "outcome": "indeterminate",
            "reason": "entity_scan_truncated",
            "observed": {"count_lower_bound": 256},
        }
        client, _transport = make_client([diagnostic_response(observation)])

        with self.assertRaisesRegex(
            BevyFeedbackError,
            'predicate assertion failed: .*"outcome":"indeterminate".*'
            '"reason":"entity_scan_truncated"',
        ):
            client.assert_marker_absent("Enemy")

        self.assertEqual(client.last_observation, observation)


class BevyFeedbackClientImageHelpersTest(unittest.TestCase):
    def test_physical_include_and_masks_count_only_visible_changed_pixels(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            before = write_png(directory, "before.png", (5, 4))
            after = write_png(
                directory,
                "after.png",
                (5, 4),
                {
                    (0, 0): (255, 0, 0, 255),
                    (1, 1): (255, 0, 0, 255),
                    (2, 1): (255, 0, 0, 255),
                    (4, 3): (255, 0, 0, 255),
                },
            )
            include = (1, 1, 4, 3)
            masks = ((1, 1, 1, 1),)

            self.assertEqual(bevy_feedback.pixel_diff(before, after), 4)
            self.assertEqual(
                bevy_feedback.pixel_diff(before, after, include=include),
                3,
            )
            self.assertEqual(
                bevy_feedback.pixel_diff(
                    before,
                    after,
                    include=include,
                    masks=masks,
                ),
                2,
            )
            self.assertEqual(
                bevy_feedback.region_diff(
                    before,
                    after,
                    include,
                    masks=masks,
                ),
                2,
            )

            client, _transport = make_client([])
            client.assert_changed(
                before,
                after,
                min_pixels=2,
                include=include,
                masks=masks,
            )
            with self.assertRaisesRegex(
                BevyFeedbackError, "changed 2 pixels, expected at least 3"
            ):
                client.assert_region_changed(
                    before,
                    after,
                    include,
                    min_pixels=3,
                    masks=masks,
                )

    def test_resize_is_explicit_and_stability_restarts_on_new_dimensions(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            initial = write_png(directory, "initial.png", (2, 2))
            resized = write_png(directory, "resized.png", (3, 2))
            stable_one = write_png(directory, "stable-one.png", (3, 2))
            stable_two = write_png(directory, "stable-two.png", (3, 2))

            with self.assertRaisesRegex(
                BevyFeedbackError,
                r"image dimensions differ: \(2, 2\) vs \(3, 2\).*window resized",
            ):
                bevy_feedback.pixel_diff(initial, resized)

            responses = [
                capture_response(
                    str(initial), label=None, image_size=(2, 2)
                ),
                capture_response(
                    str(resized), label=None, image_size=(3, 2)
                ),
                capture_response(
                    str(stable_one), label=None, image_size=(3, 2)
                ),
                capture_response(
                    str(stable_two), label=None, image_size=(3, 2)
                ),
            ]
            client, transport = make_client(responses, max_wait_frames=5)

            self.assertEqual(
                client.wait_until_stable(frames=2, attempts=3, stable=2),
                stable_two,
            )
            self.assertEqual(
                transport.requests,
                [
                    {"command": "capture"},
                    {"command": "capture_after_frames", "frames": 2},
                    {"command": "capture_after_frames", "frames": 2},
                    {"command": "capture_after_frames", "frames": 2},
                ],
            )

    def test_change_polling_stops_at_the_attempt_bound(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            before = write_png(directory, "before.png", (2, 2))
            unchanged = write_png(directory, "unchanged.png", (2, 2))
            responses = [
                capture_response(
                    str(unchanged),
                    label="probe",
                    image_size=(2, 2),
                )
                for _ in range(3)
            ]
            client, transport = make_client(responses, max_wait_frames=2)

            with self.assertRaisesRegex(
                BevyFeedbackError, "screenshot did not change"
            ):
                client.wait_until_changed(
                    before,
                    frames=1,
                    attempts=3,
                    label="probe",
                    include=(0, 0, 2, 2),
                    masks=((0, 0, 1, 1),),
                )

            self.assertEqual(
                transport.requests,
                [
                    {
                        "command": "capture_after_frames",
                        "frames": 1,
                        "label": "probe",
                    }
                    for _ in range(3)
                ],
            )

    def test_all_visual_retry_loops_reject_unbounded_attempts_before_io(self) -> None:
        attempts = bevy_feedback.MAX_CLIENT_CHUNKS + 1
        cases: list[
            tuple[str, Callable[[BevyFeedbackClient], object]]
        ] = [
            (
                "changed",
                lambda client: client.wait_until_changed(
                    "before.png", attempts=attempts
                ),
            ),
            (
                "stable",
                lambda client: client.wait_until_stable(attempts=attempts),
            ),
            (
                "color",
                lambda client: client.wait_until_color(
                    (255, 0, 0), (0, 0, 1, 1), attempts=attempts
                ),
            ),
            (
                "text",
                lambda client: client.wait_until_text(
                    "Ready", attempts=attempts
                ),
            ),
        ]

        for name, invoke in cases:
            with self.subTest(name=name):
                client, transport = make_client([])
                with self.assertRaisesRegex(
                    BevyFeedbackError,
                    "attempts must not exceed client bound 4096",
                ):
                    invoke(client)
                self.assertEqual(transport.requests, [])


class BevyFeedbackClientErrorContextTest(unittest.TestCase):
    def test_request_retains_structured_error_capture_and_predicate_context(self) -> None:
        capture = capture_metadata(
            "/captures/capture-000009-failure.png",
            label="failure",
            requested_frame=90,
            completed_frame=92,
        )
        observed_predicate = {
            "predicate": {
                "type": "target_absent",
                "target": {"name": "BlockingModal"},
            },
            "outcome": "not_matched",
            "observed": {"matches": 1},
            "frames_observed": 10,
        }
        context = {
            "latest_capture": capture,
            "observed_predicate": observed_predicate,
            "frame": 92,
        }
        stream = JsonLineStream(
            [
                {
                    "id": 7,
                    "ok": False,
                    "error": {
                        "code": "timeout",
                        "message": "target did not disappear",
                        "context": context,
                    },
                }
            ]
        )
        client = BevyFeedbackClient.__new__(BevyFeedbackClient)
        client._stream = stream
        client._next_id = 7
        client._transcript = None
        client.timeout = 1.0
        client.last_capture = None
        client.last_capture_info = None
        client.last_observation = None
        client.last_error_context = None
        request = {
            "command": "wait_for",
            "predicate": observed_predicate["predicate"],
            "max_frames": 10,
        }

        with self.assertRaises(BevyFeedbackError) as raised:
            client.request(request)

        message = str(raised.exception)
        self.assertIn("command failed [timeout]: target did not disappear", message)
        self.assertIn(
            "last captured frame: /captures/capture-000009-failure.png",
            message,
        )
        self.assertIn('"outcome":"not_matched"', message)
        self.assertEqual(
            stream.requests,
            [{**request, "id": 7}],
        )
        self.assertEqual(client.last_error_context, context)
        self.assertEqual(client.last_capture_info, capture)
        self.assertEqual(client.last_capture, Path(capture["path"]))
        self.assertEqual(client.last_observation, observed_predicate)


class BevyFeedbackClientBundleParityTest(unittest.TestCase):
    def test_bundled_client_is_byte_identical_to_the_canonical_client(self) -> None:
        canonical = ROOT / "clients" / "python" / "bevy_feedback.py"
        bundled = ROOT / "skills" / "driving-bevy-games" / "bevy_feedback.py"

        self.assertEqual(canonical.read_bytes(), bundled.read_bytes())

class BevyFeedbackRunBoundaryTest(unittest.TestCase):
    def test_fail_raises_client_error_with_message(self) -> None:
        with self.assertRaisesRegex(BevyFeedbackError, "missing start button"):
            fail("missing start button")

    def test_run_prints_success_json_and_exits_zero_without_real_socket(self) -> None:
        stdout = io.StringIO()
        seen_clients: list[StubClient] = []

        with (
            mock.patch.object(bevy_feedback, "BevyFeedbackClient", StubClient),
            contextlib.redirect_stdout(stdout),
            self.assertRaises(SystemExit) as raised,
        ):
            run(seen_clients.append)

        self.assertEqual(raised.exception.code, 0)
        self.assertEqual(json.loads(stdout.getvalue()), {"ok": True})
        self.assertEqual(len(stdout.getvalue().splitlines()), 1)
        self.assertEqual(len(seen_clients), 1)
        self.assertIs(seen_clients[0], StubClient.instances[-1])
        self.assertTrue(seen_clients[0].closed)

    def test_run_maps_client_errors_to_error_json_and_exit_one(self) -> None:
        stdout = io.StringIO()

        def driver(_client: StubClient) -> None:
            raise BevyFeedbackError("button did not appear")

        with (
            mock.patch.object(bevy_feedback, "BevyFeedbackClient", StubClient),
            contextlib.redirect_stdout(stdout),
            self.assertRaises(SystemExit) as raised,
        ):
            run(driver)

        self.assertEqual(raised.exception.code, 1)
        self.assertEqual(
            json.loads(stdout.getvalue()),
            {"ok": False, "error": "button did not appear"},
        )
        self.assertEqual(len(stdout.getvalue().splitlines()), 1)

    def test_run_leaves_driver_bugs_to_raise_with_traceback(self) -> None:
        stdout = io.StringIO()

        def driver(_client: StubClient) -> None:
            raise ValueError("driver bug")

        with (
            mock.patch.object(bevy_feedback, "BevyFeedbackClient", StubClient),
            contextlib.redirect_stdout(stdout),
            self.assertRaisesRegex(ValueError, "driver bug"),
        ):
            run(driver)

        self.assertEqual(stdout.getvalue(), "")


class StubClient:
    instances: list["StubClient"] = []

    def __init__(self) -> None:
        self.closed = False
        self.instances.append(self)

    def __enter__(self) -> "StubClient":
        return self

    def __exit__(self, *_: object) -> None:
        self.closed = True


if __name__ == "__main__":
    unittest.main()
