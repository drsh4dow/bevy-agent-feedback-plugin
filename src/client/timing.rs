use super::{AgentClient, Capture, ClientError, capture_from_response};
use serde_json::{Value, json};
use std::time::Duration;

const MAX_CLIENT_CHUNKS: u64 = 4_096;

impl AgentClient {
    /// Whether the running game advertised deterministic Bevy time.
    pub fn deterministic_time_enabled(&self) -> bool {
        self.deterministic_time
    }

    /// Maximum app-update frames accepted by one wire request.
    pub fn max_wait_frames(&self) -> u16 {
        self.max_wait_frames
    }

    /// Maximum deterministic updates accepted by one wire request.
    pub fn max_time_advance_steps(&self) -> u16 {
        self.max_time_advance_steps
    }

    /// Maximum deterministic duration accepted by one wire request.
    pub fn max_time_advance(&self) -> Duration {
        self.max_time_advance
    }

    /// Waits for a positive number of Bevy app updates using bounded wire chunks.
    pub fn wait_frames(&mut self, frames: u64) -> Result<Value, ClientError> {
        if frames == 0 {
            return Err(ClientError::Protocol(
                "frames must be greater than zero".to_string(),
            ));
        }
        let cap = u64::from(self.max_wait_frames);
        let chunk_count = frames.div_ceil(cap);
        if chunk_count > MAX_CLIENT_CHUNKS {
            return Err(ClientError::Protocol(format!(
                "frame wait requires {chunk_count} chunks, maximum is {MAX_CLIENT_CHUNKS}"
            )));
        }

        let mut remaining = frames;
        let mut response = Value::Null;
        for _ in 0..chunk_count {
            let chunk = remaining.min(cap);
            response = self.request(json!({"command": "wait", "frames": chunk}))?;
            remaining -= chunk;
        }
        Ok(response)
    }

    /// Observes positive gameplay time without changing the game's clock.
    pub fn wait_seconds(
        &mut self,
        seconds: f64,
        max_frames: Option<u16>,
    ) -> Result<Value, ClientError> {
        let duration = positive_duration("seconds", seconds)?;
        let max_frames = max_frames.unwrap_or(self.max_wait_frames);
        if max_frames == 0 || max_frames > self.max_wait_frames {
            return Err(ClientError::Protocol(format!(
                "max_frames must be in 1..={}",
                self.max_wait_frames
            )));
        }
        self.request(json!({
            "command": "wait_seconds",
            "seconds": duration.as_secs_f64(),
            "max_frames": max_frames,
        }))
    }

    /// Advances deterministic Bevy time, preserving an exact nanosecond step sequence.
    pub fn advance_time(
        &mut self,
        seconds: f64,
        step_seconds: Option<f64>,
    ) -> Result<Value, ClientError> {
        let duration = positive_duration("seconds", seconds)?;
        let Some(step_seconds) = step_seconds else {
            if duration > self.max_time_advance {
                return Err(ClientError::Protocol(
                    "advance_time requires an explicit step_seconds when protocol caps require chunking"
                        .to_string(),
                ));
            }
            return self.request(json!({
                "command": "advance_time",
                "seconds": duration.as_secs_f64(),
            }));
        };
        let step = positive_duration("step_seconds", step_seconds)?;
        self.advance_time_chunked(duration, step)
    }

    fn advance_time_chunked(
        &mut self,
        duration: Duration,
        step: Duration,
    ) -> Result<Value, ClientError> {
        let cap_ns = self.max_time_advance.as_nanos();
        let step_ns = step.as_nanos();
        let max_steps = u128::from(self.max_time_advance_steps);
        let mut remaining_ns = duration.as_nanos();

        if request_fits(remaining_ns, step_ns, cap_ns, max_steps) {
            return self.advance_time_request(remaining_ns, step);
        }
        let regular_steps_per_chunk = (cap_ns / step_ns).min(max_steps);
        if regular_steps_per_chunk == 0 {
            return Err(ClientError::Protocol(
                "advance_time cannot form a non-final chunk that is an exact multiple of step_seconds"
                    .to_string(),
            ));
        }

        for _ in 0..MAX_CLIENT_CHUNKS {
            if request_fits(remaining_ns, step_ns, cap_ns, max_steps) {
                return self.advance_time_request(remaining_ns, step);
            }
            let remaining_full_steps = remaining_ns / step_ns;
            let chunk_steps = remaining_full_steps.min(regular_steps_per_chunk);
            if chunk_steps == 0 {
                return Err(ClientError::Protocol(
                    "advance_time cannot preserve the nominal step across protocol chunks"
                        .to_string(),
                ));
            }
            let chunk_ns = chunk_steps.checked_mul(step_ns).ok_or_else(|| {
                ClientError::Protocol("advance_time chunk duration overflowed".to_string())
            })?;
            self.advance_time_request(chunk_ns, step)?;
            remaining_ns -= chunk_ns;
        }
        Err(ClientError::Protocol(format!(
            "advance_time requires more than {MAX_CLIENT_CHUNKS} protocol chunks"
        )))
    }

    fn advance_time_request(
        &mut self,
        nanoseconds: u128,
        step: Duration,
    ) -> Result<Value, ClientError> {
        let duration = duration_from_nanos(nanoseconds)?;
        let seconds = exact_wire_seconds("advance_time chunk", duration)?;
        let step_seconds = exact_wire_seconds("step_seconds", step)?;
        self.request(json!({
            "command": "advance_time",
            "seconds": seconds,
            "step_seconds": step_seconds,
        }))
    }

    /// Captures the primary window as a PNG.
    pub fn capture(&mut self) -> Result<Capture, ClientError> {
        self.capture_request("capture", 0, None)
    }

    /// Captures the primary window as a labeled PNG.
    pub fn capture_labeled(&mut self, label: &str) -> Result<Capture, ClientError> {
        self.capture_request("capture", 0, Some(label))
    }

    /// Atomically waits for app updates and completes a render-readback capture.
    pub fn capture_after_frames(
        &mut self,
        frames: u16,
        label: Option<&str>,
    ) -> Result<Capture, ClientError> {
        if frames == 0 || frames > self.max_wait_frames {
            return Err(ClientError::Protocol(format!(
                "capture frames must be in 1..={}",
                self.max_wait_frames
            )));
        }
        self.capture_request("capture_after_frames", frames, label)
    }

    /// Waits for the first completion-confirmed render-readback capture.
    pub fn wait_until_first_capture(&mut self) -> Result<Capture, ClientError> {
        self.capture_after_frames(1, None)
    }

    /// Returns metadata for the most recent capture reported by the game.
    pub fn last_capture_info(&self) -> Option<&Capture> {
        self.last_capture.as_ref()
    }

    fn capture_request(
        &mut self,
        command: &str,
        frames: u16,
        label: Option<&str>,
    ) -> Result<Capture, ClientError> {
        let mut request = json!({"command": command});
        if frames > 0 {
            request["frames"] = Value::from(frames);
        }
        if let Some(label) = label {
            request["label"] = Value::from(label);
        }
        let capture = capture_from_response(&self.request(request)?)?;
        self.last_capture = Some(capture.clone());
        Ok(capture)
    }
}

fn positive_duration(name: &str, seconds: f64) -> Result<Duration, ClientError> {
    let duration = Duration::try_from_secs_f64(seconds).map_err(|_| {
        ClientError::Protocol(format!("{name} must be finite and greater than zero"))
    })?;
    if duration.is_zero() {
        return Err(ClientError::Protocol(format!(
            "{name} must be at least one nanosecond"
        )));
    }
    Ok(duration)
}

fn exact_wire_seconds(name: &str, duration: Duration) -> Result<f64, ClientError> {
    let seconds = duration.as_secs_f64();
    if Duration::try_from_secs_f64(seconds).ok() != Some(duration) {
        return Err(ClientError::Protocol(format!(
            "{name} cannot be represented as exact integer nanoseconds on the f64 wire"
        )));
    }
    Ok(seconds)
}

fn duration_from_nanos(nanoseconds: u128) -> Result<Duration, ClientError> {
    let seconds = u64::try_from(nanoseconds / 1_000_000_000)
        .map_err(|_| ClientError::Protocol("advance_time duration overflowed".to_string()))?;
    let nanos = u32::try_from(nanoseconds % 1_000_000_000)
        .expect("nanosecond remainder is always below one second");
    Ok(Duration::new(seconds, nanos))
}

fn request_fits(duration_ns: u128, step_ns: u128, cap_ns: u128, max_steps: u128) -> bool {
    if duration_ns > cap_ns {
        return false;
    }
    duration_ns.div_ceil(step_ns) <= max_steps
}
