#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" == "Linux" && -z "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]]; then
  echo "rendered agent feedback tests need DISPLAY/WAYLAND_DISPLAY; try: xvfb-run -s '-screen 0 1280x720x24' $0" >&2
  exit 1
fi

export AGENT_FEEDBACK_ARTIFACT_ROOT="${AGENT_FEEDBACK_ARTIFACT_ROOT:-target/agent-feedback}"
export BEVY_FEEDBACK_ARTIFACTS="${BEVY_FEEDBACK_ARTIFACTS:-${AGENT_FEEDBACK_ARTIFACT_ROOT}/artifacts}"
mkdir -p "${AGENT_FEEDBACK_ARTIFACT_ROOT}" "${BEVY_FEEDBACK_ARTIFACTS}"
echo "agent feedback artifacts: ${AGENT_FEEDBACK_ARTIFACT_ROOT}"

cargo test --test update_input -- --ignored --test-threads=1 --nocapture
cargo test --test fixed_timestep_input -- --ignored --test-threads=1 --nocapture
