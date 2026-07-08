#!/usr/bin/env python3
from __future__ import annotations

import sys
import unittest
from pathlib import Path

CLIENTS_PYTHON = Path(__file__).resolve().parents[1] / "clients" / "python"
sys.path.insert(0, str(CLIENTS_PYTHON))

from bevy_feedback import BevyFeedbackClient, BevyFeedbackError


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


if __name__ == "__main__":
    unittest.main()
