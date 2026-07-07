# Linux CI for rendered agent feedback

Rendered Bevy captures need a display. On Linux CI, use a real display, Wayland, or Xvfb.

## Packages

Ubuntu-style baseline:

```sh
sudo apt-get update
sudo apt-get install -y xvfb libx11-dev libasound2-dev libudev-dev pkg-config
# Optional OCR assertions:
sudo apt-get install -y tesseract-ocr tesseract-ocr-eng
```

## Checks

Compile/lint/test without a display:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo check --examples
```

Rendered smoke:

```sh
xvfb-run -s '-screen 0 1280x720x24' scripts/verify-rendered-agent-feedback.sh
```

If WGPU backend selection is flaky in the runner, set one explicitly before running tests:

```sh
export WGPU_BACKEND=vulkan   # or gl
```

## Artifacts to upload

Upload `target/agent-feedback` (or `$AGENT_FEEDBACK_ARTIFACT_ROOT`) on failure and success when useful. It contains:

- protocol/session copy
- heartbeat metadata
- game logs
- request transcript
- captures/screenshots
- failure summary from `bevy-feedback run`

## Wrapper example

```sh
xvfb-run -s '-screen 0 1280x720x24' \
  cargo run --bin bevy-feedback -- run \
  --game cargo run --example minimal \
  --driver python3 my_driver.py
```

The wrapper exports `BEVY_FEEDBACK_PROTOCOL`, `BEVY_FEEDBACK_CAPTURE_DIR`, `BEVY_FEEDBACK_ARTIFACTS`, and `BEVY_FEEDBACK_TRANSCRIPT`, then releases inputs and sends `shutdown` during cleanup.
