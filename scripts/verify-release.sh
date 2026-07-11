#!/usr/bin/env bash
set -euo pipefail

cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo check --examples --all-features
python3 -m unittest tests/test_python_client_helpers.py
node --experimental-strip-types --test tests/test_typescript_client_helpers.ts
scripts/check-skill-bundle.sh

if grep -RIn 'clicked_target\|bevy-agent-feedback/3' \
  --exclude-dir=.git --exclude-dir=target --exclude=REPORT.md \
  src clients skills tests README.md examples docs; then
  echo 'obsolete pre-0.5 contract found' >&2
  exit 1
fi

grep -q 'version = "0.5.0"' Cargo.toml
grep -q 'bevy-agent-feedback/0.5' src/session.rs
grep -q -- '--prepare' README.md
grep -q -- '--protocol-timeout' README.md
grep -q -- '--game-cwd' README.md
grep -q 'tests/fixtures/skill_workflow_driver.py' scripts/verify-rendered-agent-feedback.sh
grep -q 'bevy-agent-feedback/0.5' tests/fixtures/skill_workflow_driver.py
