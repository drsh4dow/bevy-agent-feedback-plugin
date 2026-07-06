use bevy::{
    app::AppExit,
    prelude::*,
    render::RenderPlugin,
    window::{ExitCondition, WindowResolution},
    winit::WinitPlugin,
};
use bevy_agent_feedback_plugin::AgentFeedbackConfig;
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool, mpsc::Sender},
    time::Duration,
};

#[derive(Resource)]
pub struct Probe {
    pub capture_done: Arc<AtomicBool>,
    pub result: Option<Sender<Result<(), String>>>,
    pub max_frames: u32,
}

pub fn add_render_plugins(app: &mut App, title: &str) {
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: title.into(),
                    resolution: WindowResolution::new(640, 480).with_scale_factor_override(1.0),
                    ..default()
                }),
                exit_condition: ExitCondition::DontExit,
                ..default()
            })
            .set(RenderPlugin {
                synchronous_pipeline_compilation: true,
                ..default()
            })
            .set(WinitPlugin {
                run_on_any_thread: true,
            }),
    );
}

pub fn skip_without_window_server() -> bool {
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("skipping rendered Bevy test: DISPLAY/WAYLAND_DISPLAY is not set");
        return true;
    }

    false
}

pub fn artifact_root(name: &str) -> PathBuf {
    std::env::var_os("AGENT_FEEDBACK_ARTIFACT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/agent-feedback"))
        .join(name)
}

pub fn agent_config(root: &Path) -> AgentFeedbackConfig {
    AgentFeedbackConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        protocol_file: root.join("agent-feedback.json"),
        capture_dir: root.join("captures"),
        max_wait_frames: 600,
        command_timeout: Duration::from_secs(30),
        ..Default::default()
    }
}

pub fn socket_addr(config: &AgentFeedbackConfig) -> SocketAddr {
    let protocol: Value = serde_json::from_slice(
        &fs::read(&config.protocol_file).expect("protocol file should be written"),
    )
    .expect("protocol file should be JSON");
    protocol["socket_addr"]
        .as_str()
        .expect("protocol should expose socket address")
        .parse()
        .expect("socket address should parse")
}

pub fn connect_agent(socket_addr: SocketAddr) -> Result<(TcpStream, BufReader<TcpStream>), String> {
    let stream = TcpStream::connect(socket_addr).map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(30)))
        .map_err(|error| error.to_string())?;
    let reader_stream = stream.try_clone().map_err(|error| error.to_string())?;
    Ok((stream, BufReader::new(reader_stream)))
}

pub fn send_request(
    stream: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    request: &str,
) -> Result<Value, String> {
    writeln!(stream, "{request}").map_err(|error| error.to_string())?;

    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    if line.is_empty() {
        return Err("agent socket closed before response".to_string());
    }

    let response: Value = serde_json::from_str(&line).map_err(|error| error.to_string())?;
    if response["ok"] != Value::Bool(true) {
        return Err(format!("agent command failed: {response}"));
    }
    Ok(response)
}

pub fn expect_png(response: &Value) -> Result<(PathBuf, Vec<u8>), String> {
    let path = response["result"]["capture"]["path"]
        .as_str()
        .ok_or_else(|| format!("capture response did not include a path: {response}"))?;
    let path = PathBuf::from(path);
    expect_latest_capture(response, &path)?;

    let image = image::ImageReader::open(&path)
        .map_err(|error| error.to_string())?
        .decode()
        .map_err(|error| error.to_string())?;
    if image.width() == 0 || image.height() == 0 {
        return Err(format!(
            "capture had invalid dimensions: {}",
            path.display()
        ));
    }
    Ok((path, image.to_rgba8().into_raw()))
}

pub fn expect_latest_capture(response: &Value, capture_path: &Path) -> Result<(), String> {
    let latest = response["result"]["latest_capture"]["path"]
        .as_str()
        .ok_or_else(|| format!("response did not include latest_capture: {response}"))?;
    if Path::new(latest) != capture_path {
        return Err(format!(
            "latest_capture was {}, expected {}",
            latest,
            capture_path.display()
        ));
    }
    Ok(())
}

pub fn finish_probe(
    probe: &mut Probe,
    app_exit: &mut MessageWriter<AppExit>,
    result: Result<(), String>,
) {
    let success = result.is_ok();
    if let Some(sender) = probe.result.take() {
        let _ = sender.send(result);
    }
    app_exit.write(if success {
        AppExit::Success
    } else {
        AppExit::error()
    });
}
