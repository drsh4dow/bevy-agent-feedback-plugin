#!/usr/bin/env bash
# The driving-bevy-games skill ships a self-contained copy of the Python client
# so it works when installed outside this repo. Keep it byte-identical to the
# canonical client; this guard fails on drift.
set -euo pipefail

canonical="clients/python/bevy_feedback.py"
bundled="skills/driving-bevy-games/bevy_feedback.py"

if ! diff -u "$canonical" "$bundled"; then
  echo "skill bundle drift: refresh with 'cp $canonical $bundled'" >&2
  exit 1
fi
