#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" == "Linux" && -z "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]]; then
  echo "rendered agent feedback tests need DISPLAY or WAYLAND_DISPLAY" >&2
  exit 1
fi

export AGENT_FEEDBACK_ARTIFACT_ROOT="${AGENT_FEEDBACK_ARTIFACT_ROOT:-target/agent-feedback}"
mkdir -p "${AGENT_FEEDBACK_ARTIFACT_ROOT}"
echo "agent feedback artifacts: ${AGENT_FEEDBACK_ARTIFACT_ROOT}"

cargo test --test alien_cake_addict -- --ignored --test-threads=1 --nocapture
cargo test --test physics_in_fixed_timestep -- --ignored --test-threads=1 --nocapture
