# Linux CI for rendered agent feedback

Rendered Bevy captures need a display. On Linux CI, use a real display, Wayland, or Xvfb.

## Packages

Ubuntu-style baseline:

```sh
sudo apt-get update
sudo apt-get install -y clang mold xvfb mesa-vulkan-drivers libx11-dev libxkbcommon-x11-0 libwayland-dev libasound2-dev libudev-dev pkg-config python3-pil
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

Rendered smoke (the same externally owned Xvfb recipe used by `.github/workflows/ci.yml`):

```sh
env -u WAYLAND_DISPLAY WGPU_BACKEND=vulkan \
  xvfb-run -a -s '-screen 0 1280x720x24' scripts/verify-rendered-agent-feedback.sh
```

`xvfb-run` creates and tears down the display around the runner. `bevy-feedback` deliberately does neither. Unsetting `WAYLAND_DISPLAY` ensures local Wayland sessions do not bypass Xvfb; omit that wrapper when intentionally testing the real session.

Pin capture dimensions in examples/tests:

```rust
Window {
    resolution: bevy::window::WindowResolution::new(1280, 720)
        .with_scale_factor_override(1.0),
    ..default()
}
```

If WGPU backend selection is flaky in the runner, set one explicitly before running tests:

```sh
export WGPU_BACKEND=vulkan   # or gl
```

## Artifacts to upload

Upload `target/agent-feedback` (or `$BEVY_FEEDBACK_ARTIFACTS` when set) on failure and success when useful. Wrapper run artifacts include:

- `prepare.log` when `--prepare` is used
- `game.log`
- `run-summary.json`
- `protocol.json`
- `transcript.jsonl`
- live `captures/`
- final copied `screenshots/`
- `failure-summary.txt` on wrapper failures

## Wrapper example

```sh
env -u WAYLAND_DISPLAY xvfb-run -a -s '-screen 0 1280x720x24' \
  cargo run --bin bevy-feedback -- run \
  --require-window-size 640x480 \
  --prepare cargo build --example minimal \
  --game-cwd "$PWD" \
  --game cargo run --example minimal \
  --driver python3 my_driver.py
```

The wrapper exports `BEVY_FEEDBACK_PROTOCOL`, `BEVY_FEEDBACK_CAPTURE_DIR`, `BEVY_FEEDBACK_ARTIFACTS`, and `BEVY_FEEDBACK_TRANSCRIPT`, then releases inputs and requests shutdown during cleanup. `run-summary.json` records acknowledgment, socket closure, child exit, forced termination, and required/actual logical and physical window dimensions plus scale factor. A compositor override produces stable `window_size_mismatch`; normalized client coordinates still use observed logical dimensions. `BEVY_FEEDBACK_REQUIRED_WINDOW_SIZE=640x480` is equivalent to the flag. `--ready-timeout` is a deprecated alias for `--protocol-timeout`; prepare has a separate timeout.
