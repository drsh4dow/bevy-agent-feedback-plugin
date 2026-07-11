#!/usr/bin/env python3
"""Stdin JSON-lines to stdout JSON-lines driver for bevy-agent-feedback v3.

Self-contained: imports the bundled client next to this file, so the skill
works when installed outside the source repository.
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))

from bevy_feedback import drive_stdio  # noqa: E402

if len(sys.argv) > 2:
    raise SystemExit("usage: drive.py [PROTOCOL_FILE] < commands.jsonl")

protocol = sys.argv[1] if len(sys.argv) == 2 else None
raise SystemExit(drive_stdio(protocol, sys.stdin))
