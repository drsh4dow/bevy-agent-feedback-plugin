#!/usr/bin/env python3
"""Compatibility wrapper for the maintained Python client."""
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "clients" / "python"))

from bevy_feedback import drive_stdio  # noqa: E402

if len(sys.argv) != 2:
    raise SystemExit("usage: drive.py PROTOCOL_FILE < commands.jsonl")

drive_stdio(sys.argv[1], sys.stdin)
