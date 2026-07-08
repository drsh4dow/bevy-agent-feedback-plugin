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
