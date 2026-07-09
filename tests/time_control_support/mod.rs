use bevy::{
    input::InputPlugin,
    prelude::*,
    time::{Fixed, TimePlugin, TimeUpdateStrategy, Virtual},
    window::PrimaryWindow,
};
use bevy_agent_feedback_plugin::{AgentFeedbackConfig, AgentFeedbackPlugin};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::Duration,
};

const IO_ATTEMPTS: usize = 2_500;
pub(crate) const UPDATE_ATTEMPTS: usize = 2_500;
static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Default, Eq, PartialEq, Resource)]
pub(crate) struct ClockTrace {
    pub(crate) update_deltas: Vec<Duration>,
    pub(crate) update_elapsed: Vec<Duration>,
    pub(crate) fixed_deltas: Vec<Duration>,
    pub(crate) fixed_elapsed: Vec<Duration>,
    pub(crate) simulated_nanoseconds: u128,
}

fn record_update(time: Res<Time>, mut trace: ResMut<ClockTrace>) {
    trace.update_deltas.push(time.delta());
    trace.update_elapsed.push(time.elapsed());
    trace.simulated_nanoseconds += time.delta().as_nanos();
}

fn record_fixed_update(time: Res<Time>, mut trace: ResMut<ClockTrace>) {
    trace.fixed_deltas.push(time.delta());
    trace.fixed_elapsed.push(time.elapsed());
}

pub(crate) struct Harness {
    app: Option<App>,
    pub(crate) config: AgentFeedbackConfig,
    root: PathBuf,
}

impl Harness {
    pub(crate) fn app(&self) -> &App {
        self.app.as_ref().expect("test app should still exist")
    }

    pub(crate) fn app_mut(&mut self) -> &mut App {
        self.app.as_mut().expect("test app should still exist")
    }

    pub(crate) fn elapsed(&self) -> Duration {
        self.app().world().resource::<Time<Virtual>>().elapsed()
    }

    pub(crate) fn trace(&self) -> ClockTrace {
        self.app().world().resource::<ClockTrace>().clone()
    }

    pub(crate) fn clear_trace(&mut self) {
        *self.app_mut().world_mut().resource_mut::<ClockTrace>() = ClockTrace::default();
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        drop(self.app.take());
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub(crate) fn timing_config(
    name: &str,
    deterministic_time: bool,
) -> (AgentFeedbackConfig, PathBuf) {
    let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "bevy-agent-feedback-time-{name}-{}-{sequence}",
        std::process::id()
    ));
    let config = AgentFeedbackConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        protocol_file: root.join("agent.json"),
        capture_dir: root.join("captures"),
        deterministic_time,
        max_wait_frames: 64,
        max_time_advance_steps: 64,
        max_time_advance: Duration::from_secs(2),
        command_timeout: Duration::from_secs(5),
        ..Default::default()
    };
    (config, root)
}

pub(crate) fn build_harness(
    config: AgentFeedbackConfig,
    root: PathBuf,
    with_time: bool,
    configure_before_feedback: impl FnOnce(&mut App),
) -> Harness {
    let mut app = App::new();
    if with_time {
        app.add_plugins(TimePlugin);
    }
    app.add_plugins(InputPlugin);
    app.world_mut().spawn((
        Window {
            resolution: bevy::window::WindowResolution::new(640, 480)
                .with_scale_factor_override(1.0),
            ..default()
        },
        PrimaryWindow,
    ));
    configure_before_feedback(&mut app);
    if with_time {
        app.init_resource::<ClockTrace>()
            .add_systems(Update, record_update)
            .add_systems(FixedUpdate, record_fixed_update);
    }
    app.add_plugins(AgentFeedbackPlugin::new(config.clone()));
    Harness {
        app: Some(app),
        config,
        root,
    }
}

pub(crate) fn deterministic_harness(name: &str, fixed_step: Duration) -> Harness {
    let (config, root) = timing_config(name, true);
    build_harness(config, root, true, |app| {
        app.world_mut()
            .resource_mut::<Time<Fixed>>()
            .set_timestep(fixed_step);
    })
}

pub(crate) struct Wire {
    stream: TcpStream,
    pending: Vec<u8>,
}

impl Wire {
    pub(crate) fn connect(config: &AgentFeedbackConfig) -> Self {
        let protocol: Value = serde_json::from_slice(
            &fs::read(&config.protocol_file).expect("protocol file should be written"),
        )
        .expect("protocol file should contain JSON");
        let stream = TcpStream::connect(
            protocol["socket_addr"]
                .as_str()
                .expect("protocol should advertise socket_addr"),
        )
        .expect("agent socket should accept a local connection");
        stream
            .set_nonblocking(true)
            .expect("test socket should be nonblocking");
        Self {
            stream,
            pending: Vec::new(),
        }
    }

    pub(crate) fn send(&mut self, request: Value) {
        serde_json::to_writer(&mut self.stream, &request).expect("request should serialize");
        self.stream
            .write_all(b"\n")
            .expect("request should be written");
        self.stream.flush().expect("request should flush");
    }

    pub(crate) fn try_response(&mut self) -> Option<Value> {
        if let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            return Some(self.take_response(newline));
        }

        let mut buffer = [0_u8; 1024];
        for _ in 0..8 {
            match self.stream.read(&mut buffer) {
                Ok(0) => panic!("agent socket closed before a response"),
                Ok(read) => {
                    self.pending.extend_from_slice(&buffer[..read]);
                    if let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
                        return Some(self.take_response(newline));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return None,
                Err(error) => panic!("response read failed: {error}"),
            }
        }
        None
    }

    fn take_response(&mut self, newline: usize) -> Value {
        let line = self.pending.drain(..=newline).collect::<Vec<_>>();
        serde_json::from_slice(&line[..line.len() - 1]).expect("response should contain JSON")
    }

    pub(crate) fn wait_without_updates(&mut self) -> Value {
        for _ in 0..IO_ATTEMPTS {
            if let Some(response) = self.try_response() {
                return response;
            }
            thread::sleep(Duration::from_millis(2));
        }
        panic!("agent did not write a response within the bounded wait")
    }
}

pub(crate) fn response_while_updating(app: &mut App, wire: &mut Wire) -> Value {
    for _ in 0..UPDATE_ATTEMPTS {
        app.update();
        if let Some(response) = wire.try_response() {
            return response;
        }
        thread::sleep(Duration::from_millis(2));
    }
    panic!("agent did not answer within the bounded update loop")
}

fn armed_delta(app: &App) -> Option<Duration> {
    match *app.world().resource::<TimeUpdateStrategy>() {
        TimeUpdateStrategy::ManualDuration(delta) if !delta.is_zero() => Some(delta),
        _ => None,
    }
}

pub(crate) fn advance_request(id: &str, duration: Duration, step: Duration) -> Value {
    json!({
        "id": id,
        "command": "advance_time",
        "seconds": duration.as_secs_f64(),
        "step_seconds": step.as_secs_f64()
    })
}

pub(crate) fn wait_seconds_request(id: &str, duration: Duration, max_frames: u16) -> Value {
    json!({
        "id": id,
        "command": "wait_seconds",
        "seconds": duration.as_secs_f64(),
        "max_frames": max_frames
    })
}

pub(crate) fn arm_advance(
    harness: &mut Harness,
    wire: &mut Wire,
    id: &str,
    duration: Duration,
    step: Duration,
) -> Duration {
    let before = harness.elapsed();
    wire.send(advance_request(id, duration, step));
    for _ in 0..UPDATE_ATTEMPTS {
        harness.app_mut().update();
        if let Some(delta) = armed_delta(harness.app()) {
            assert_eq!(
                harness.elapsed(),
                before,
                "the admission frame must only arm the next delta"
            );
            assert!(
                wire.try_response().is_none(),
                "advance_time must not respond before the armed delta is consumed"
            );
            return delta;
        }
        assert_eq!(
            harness.elapsed(),
            before,
            "frozen frames before admission must not advance virtual time"
        );
        thread::sleep(Duration::from_millis(2));
    }
    panic!("advance_time was not armed within the bounded update loop")
}

pub(crate) fn assert_timing_success(response: &Value, status: &str) {
    assert_eq!(
        response["ok"],
        Value::Bool(true),
        "unexpected timing response: {response}"
    );
    assert_eq!(
        response["result"]["status"], status,
        "unexpected timing response: {response}"
    );
}

pub(crate) fn assert_timing_error(response: &Value, code: &str, reason: Option<&str>) {
    assert_eq!(
        response["ok"],
        Value::Bool(false),
        "unexpected timing response: {response}"
    );
    assert_eq!(
        response["error"]["code"], code,
        "unexpected timing response: {response}"
    );
    if let Some(reason) = reason {
        assert_eq!(response["error"]["context"]["timing"]["reason"], reason);
    }
}

pub(crate) fn duration_detail(response: &Value, field: &str) -> Duration {
    Duration::try_from_secs_f64(
        response["result"]["details"][field]
            .as_f64()
            .unwrap_or_else(|| panic!("timing result should contain {field}: {response}")),
    )
    .unwrap_or_else(|_| panic!("timing result {field} should be a duration: {response}"))
}
