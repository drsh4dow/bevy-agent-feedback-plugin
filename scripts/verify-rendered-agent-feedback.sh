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
cargo build --bin bevy-feedback
cargo test --test time_control

cargo test --test update_input -- --ignored --test-threads=1 --nocapture
cargo test --test fixed_timestep_input -- --ignored --test-threads=1 --nocapture
cargo test --features diagnostics --test semantic_target -- --ignored --exact semantic_target_rendered_contract --test-threads=1 --nocapture
cargo test --all-features --test stable_pattern -- --ignored --exact stable_pattern_preserves_metadata_and_pixels_across_a_long_frame_run --nocapture
cargo test --all-features --test capture_metadata -- --nocapture
BEVY_FEEDBACK_PROTOCOL="${AGENT_FEEDBACK_ARTIFACT_ROOT}/skill-workflow/agent-feedback.json" \
BEVY_FEEDBACK_ARTIFACTS="${BEVY_FEEDBACK_ARTIFACTS}/skill-workflow" \
  target/debug/bevy-feedback run \
    --game cargo test --all-features --test skill_workflow -- --ignored --exact skill_workflow --nocapture \
    --driver python3 tests/fixtures/skill_workflow_driver.py
