#!/usr/bin/env python3
from __future__ import annotations

import contextlib
import io
import json
import sys
import unittest
from unittest import mock
from pathlib import Path

CLIENTS_PYTHON = Path(__file__).resolve().parents[1] / "clients" / "python"
sys.path.insert(0, str(CLIENTS_PYTHON))

import bevy_feedback
from bevy_feedback import BevyFeedbackClient, BevyFeedbackError, fail, run


class BevyFeedbackClientCoordinateHelpersTest(unittest.TestCase):
    def test_window_center_uses_logical_dimensions(self) -> None:
        client = BevyFeedbackClient.__new__(BevyFeedbackClient)
        client.window_info = lambda: {
            "result": {"window": {"logical_width": 955.0, "logical_height": 1170.0}}
        }

        self.assertEqual(client.window_center(), (477.5, 585.0))

    def test_point_uses_fractional_logical_dimensions(self) -> None:
        client = BevyFeedbackClient.__new__(BevyFeedbackClient)
        client.window_info = lambda: {
            "result": {"window": {"logical_width": 955.0, "logical_height": 1170.0}}
        }

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


class BevyFeedbackClientWaitTest(unittest.TestCase):
    def test_wait_chunks_requests_above_server_frame_cap(self) -> None:
        client = BevyFeedbackClient.__new__(BevyFeedbackClient)
        client.max_wait_frames = 300
        responses = [
            {"ok": True, "result": {"step": 1}},
            {"ok": True, "result": {"step": 2}},
            {"ok": True, "result": {"step": 3}},
        ]
        client.request = mock.Mock(side_effect=responses)

        response = client.wait(750)

        self.assertEqual(response, responses[-1])
        self.assertEqual(
            [call.args[0]["frames"] for call in client.request.call_args_list],
            [300, 300, 150],
        )

    def test_wait_under_frame_cap_uses_one_request(self) -> None:
        client = BevyFeedbackClient.__new__(BevyFeedbackClient)
        client.max_wait_frames = 300
        response = {"ok": True, "result": {"frames": 10}}
        client.request = mock.Mock(return_value=response)

        self.assertEqual(client.wait(10), response)
        client.request.assert_called_once_with({"command": "wait", "frames": 10})

    def test_wait_zero_passthrough_preserves_server_rejection(self) -> None:
        client = BevyFeedbackClient.__new__(BevyFeedbackClient)
        client.max_wait_frames = 300
        response = {"ok": True, "result": {"frames": 0}}
        client.request = mock.Mock(return_value=response)

        self.assertEqual(client.wait(0), response)
        client.request.assert_called_once_with({"command": "wait", "frames": 0})


class BevyFeedbackClientWaitUntilStableTest(unittest.TestCase):
    def test_static_screen_returns_after_stable_polls(self) -> None:
        client = BevyFeedbackClient.__new__(BevyFeedbackClient)
        captures = [
            Path("initial.png"),
            Path("stable_1.png"),
            Path("stable_2.png"),
        ]
        client.capture = mock.Mock(side_effect=captures)
        client.wait = mock.Mock()

        with mock.patch.object(bevy_feedback, "pixel_diff", return_value=0):
            result = client.wait_until_stable(
                frames=7, attempts=5, stable=2, label="boot"
            )

        self.assertEqual(result, captures[2])
        self.assertEqual(client.wait.call_count, 2)

    def test_change_resets_stable_streak(self) -> None:
        client = BevyFeedbackClient.__new__(BevyFeedbackClient)
        captures = [
            Path("initial.png"),
            Path("changed.png"),
            Path("stable_1.png"),
            Path("stable_2.png"),
        ]
        client.capture = mock.Mock(side_effect=captures)
        client.wait = mock.Mock()

        with mock.patch.object(bevy_feedback, "pixel_diff", side_effect=[1, 0, 0]):
            result = client.wait_until_stable(frames=7, attempts=3, stable=2)

        self.assertEqual(result, captures[3])
        self.assertEqual(client.wait.call_count, 3)

    def test_raises_after_attempts_without_stabilizing(self) -> None:
        client = BevyFeedbackClient.__new__(BevyFeedbackClient)
        client.capture = mock.Mock(
            side_effect=[
                Path("initial.png"),
                Path("changed_1.png"),
                Path("changed_2.png"),
                Path("changed_3.png"),
            ]
        )
        client.wait = mock.Mock()

        with (
            mock.patch.object(bevy_feedback, "pixel_diff", return_value=1),
            self.assertRaisesRegex(BevyFeedbackError, "did not stabilize"),
        ):
            client.wait_until_stable(frames=7, attempts=3, stable=2)

    def test_dimension_mismatch_is_treated_as_change(self) -> None:
        client = BevyFeedbackClient.__new__(BevyFeedbackClient)
        captures = [
            Path("initial.png"),
            Path("resized.png"),
            Path("stable_1.png"),
            Path("stable_2.png"),
        ]
        client.capture = mock.Mock(side_effect=captures)
        client.wait = mock.Mock()

        with mock.patch.object(
            bevy_feedback,
            "pixel_diff",
            side_effect=[
                BevyFeedbackError("image dimensions differ: 1x1 vs 2x2"),
                0,
                0,
            ],
        ):
            result = client.wait_until_stable(frames=7, attempts=3, stable=2)

        self.assertEqual(result, captures[3])


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
