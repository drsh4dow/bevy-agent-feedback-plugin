use bevy::{prelude::*, window::PrimaryWindow};
use bevy_agent_feedback_plugin::{
    AgentFeedbackConfig, AgentFeedbackPlugin,
    client::{AgentClient, AgentClientConfig},
};
use serde_json::Value;
use std::{
    fs,
    net::SocketAddr,
    path::PathBuf,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[test]
fn rust_client_writes_transcript_envelopes_and_replays_compat_formats() {
    let (mut app, config) = agent_app("rust-client-transcript");
    let transcript_file = config
        .protocol_file
        .parent()
        .expect("protocol parent")
        .join("transcript.jsonl");
    let replay_file = config
        .protocol_file
        .parent()
        .expect("protocol parent")
        .join("replay.jsonl");
    let protocol_file = config.protocol_file.clone();
    let client = thread::spawn({
        let transcript_file = transcript_file.clone();
        let replay_file = replay_file.clone();
        move || -> Result<(), String> {
            let mut client = AgentClient::with_config(AgentClientConfig {
                protocol_file,
                transcript_file: Some(transcript_file),
                ..Default::default()
            })
            .map_err(|error| error.to_string())?;
            client.window_info().map_err(|error| error.to_string())?;
            fs::write(
                &replay_file,
                format!(
                    "{}\n{}\n",
                    serde_json::json!({"command": "window_info"}),
                    serde_json::json!({"request": {"command": "window_info"}}),
                ),
            )
            .map_err(|error| error.to_string())?;
            let responses = client
                .replay_jsonl(&replay_file)
                .map_err(|error| error.to_string())?;
            if responses.len() != 2
                || responses
                    .iter()
                    .any(|response| response["ok"] != Value::Bool(true))
            {
                return Err(format!("unexpected replay responses: {responses:?}"));
            }
            Ok(())
        }
    });
    for _ in 0..100 {
        app.update();
        if client.is_finished() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    client
        .join()
        .expect("client thread")
        .expect("client request");

    let transcript = fs::read_to_string(&transcript_file).expect("transcript");
    let envelopes = transcript
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("transcript line"))
        .collect::<Vec<_>>();
    assert!(envelopes.len() >= 3, "transcript: {transcript}");
    assert!(envelopes[0]["ts_ms"].as_u64().is_some());
    assert!(envelopes[0]["duration_ms"].as_u64().is_some());
    assert_eq!(envelopes[0]["request"]["command"], "window_info");
    assert_eq!(envelopes[0]["response"]["ok"], Value::Bool(true));
    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

#[test]
fn rust_client_replay_jsonl_snapshots_transcript_before_appending() {
    let (mut app, config) = agent_app("rust-client-replay-same-transcript");
    let transcript_file = config
        .protocol_file
        .parent()
        .expect("protocol parent")
        .join("transcript.jsonl");
    fs::write(
        &transcript_file,
        format!(
            "{}\n{}\n",
            serde_json::json!({"command": "window_info"}),
            serde_json::json!({"request": {"command": "window_info"}}),
        ),
    )
    .expect("seed transcript");

    let protocol_file = config.protocol_file.clone();
    let client = thread::spawn({
        let transcript_file = transcript_file.clone();
        move || -> Result<(), String> {
            let mut client = AgentClient::with_config(AgentClientConfig {
                protocol_file,
                timeout: Duration::from_millis(250),
                transcript_file: Some(transcript_file.clone()),
                ..Default::default()
            })
            .map_err(|error| error.to_string())?;
            let responses = client
                .replay_jsonl(&transcript_file)
                .map_err(|error| error.to_string())?;
            if responses.len() != 2
                || responses
                    .iter()
                    .any(|response| response["ok"] != Value::Bool(true))
            {
                return Err(format!("unexpected replay responses: {responses:?}"));
            }
            Ok(())
        }
    });

    let finished = update_until(&mut app, Duration::from_secs(2), || client.is_finished());
    if !finished {
        drop(app);
        let _ = client.join();
        panic!("replay_jsonl should stop after the original transcript lines");
    }
    client
        .join()
        .expect("client thread")
        .expect("client replay");

    let transcript = fs::read_to_string(&transcript_file).expect("transcript");
    let envelopes = transcript
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("transcript line"))
        .collect::<Vec<_>>();
    assert_eq!(envelopes.len(), 4, "transcript: {transcript}");
    assert_eq!(envelopes[0]["command"], "window_info");
    assert_eq!(envelopes[1]["request"]["command"], "window_info");
    assert_eq!(envelopes[2]["request"]["command"], "window_info");
    assert_eq!(envelopes[2]["response"]["ok"], Value::Bool(true));
    assert_eq!(envelopes[3]["request"]["command"], "window_info");
    assert_eq!(envelopes[3]["response"]["ok"], Value::Bool(true));
    let _ = fs::remove_dir_all(config.protocol_file.parent().unwrap());
}

fn agent_app(name: &str) -> (App, AgentFeedbackConfig) {
    let root = temp_root(name);
    let config = AgentFeedbackConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        protocol_file: root.join("agent.json"),
        capture_dir: root.join("captures"),
        command_timeout: Duration::from_secs(2),
        ..Default::default()
    };
    let mut app = App::new();
    app.add_plugins(bevy::input::InputPlugin);
    app.world_mut().spawn((
        Window {
            resolution: bevy::window::WindowResolution::new(640, 480)
                .with_scale_factor_override(1.0),
            ..default()
        },
        PrimaryWindow,
    ));
    app.add_plugins(AgentFeedbackPlugin::new(config.clone()));
    (app, config)
}

fn temp_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "bevy-agent-feedback-{name}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos()
    ))
}

fn update_until(app: &mut App, timeout: Duration, done: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        app.update();
        if done() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    done()
}
