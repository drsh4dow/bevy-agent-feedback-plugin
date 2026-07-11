use super::*;
use std::net::TcpListener;

#[test]
fn mismatch_is_stable_and_records_required_and_actual_dimensions() {
    let root = std::env::temp_dir().join(format!(
        "bevy-feedback-window-contract-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir_all(&root).expect("root");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
    let protocol_file = root.join("protocol.json");
    let heartbeat = root.join("heartbeat");
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis();
    fs::write(&heartbeat, now_ms.to_string()).expect("heartbeat");
    fs::write(
        &protocol_file,
        serde_json::to_vec(&serde_json::json!({
            "protocol": "bevy-agent-feedback/3",
            "session_id": "window-contract",
            "pid": std::process::id(),
            "heartbeat_file": heartbeat,
            "stale_after_ms": 10_000,
            "socket_addr": listener.local_addr().expect("address"),
            "deterministic_time": false,
            "max_wait_frames": 60,
            "max_abort_predicates": 16,
            "max_time_advance_steps": 60,
            "max_time_advance_seconds": 1.0
        }))
        .expect("protocol JSON"),
    )
    .expect("protocol");
    let server = thread::spawn(move || serve_window_info(listener));
    let required = RequiredWindowSize {
        width: 1280,
        height: 720,
    };
    let args = RunArgs {
        protocol_file,
        artifacts: root.clone(),
        required_window_size: Some(required),
        prepare_timeout: Duration::from_secs(1),
        protocol_timeout: Duration::from_secs(1),
        shutdown_timeout: Duration::from_secs(1),
        driver_timeout: Duration::from_secs(1),
        game_cwd: None,
        prepare: None,
        game: vec!["game".to_string()],
        driver: None,
        used_legacy_ready_timeout: false,
    };
    let mut summary = empty_summary(&root, required);

    let failure = inspect_window_contract(
        &args,
        &root.join("transcript.jsonl"),
        required,
        &mut summary,
    )
    .expect_err("window must mismatch");
    server.join().expect("server");

    assert_eq!(failure.code, "window_size_mismatch");
    assert_eq!(
        failure.message,
        "required logical window 1280x720, observed 955x1170"
    );
    assert_eq!(
        window_diagnostic(
            required,
            summary.window.actual.as_ref().expect("actual window")
        ),
        "bevy-feedback window: required_logical=1280x720 actual_logical=955x1170 actual_physical=1910x2340 scale_factor=2"
    );
    let json = serde_json::to_value(&summary).expect("summary JSON");
    assert_eq!(json["window"]["required_logical"]["width"], 1280);
    assert_eq!(json["window"]["required_logical"]["height"], 720);
    assert_eq!(json["window"]["actual"]["logical_width"], 955.0);
    assert_eq!(json["window"]["actual"]["logical_height"], 1170.0);
    assert_eq!(json["window"]["actual"]["physical_width"], 1910);
    assert_eq!(json["window"]["actual"]["physical_height"], 2340);
    assert_eq!(json["window"]["actual"]["scale_factor"], 2.0);
    let _ = fs::remove_dir_all(root);
}

fn serve_window_info(listener: TcpListener) {
    let (mut stream, _) = listener.accept().expect("accept");
    let mut request = String::new();
    BufReader::new(stream.try_clone().expect("clone"))
        .read_line(&mut request)
        .expect("request");
    let request: Value = serde_json::from_str(&request).expect("request JSON");
    assert_eq!(request["command"], "window_info");
    writeln!(
        stream,
        "{}",
        serde_json::json!({
            "id": request["id"],
            "ok": true,
            "result": {
                "status": "ok",
                "window": {
                    "logical_width": 955.0,
                    "logical_height": 1170.0,
                    "physical_width": 1910,
                    "physical_height": 2340,
                    "scale_factor": 2.0,
                    "cursor_position": [100.0, 100.0],
                    "focused": true,
                    "visible": true,
                    "mode": "windowed"
                }
            }
        })
    )
    .expect("response");
}

fn empty_summary(root: &Path, required: RequiredWindowSize) -> RunSummary {
    RunSummary {
        schema_version: 1,
        result: RunResult {
            success: false,
            code: "runner_internal",
            message: String::new(),
        },
        phase: "setup",
        elapsed_ms: 0,
        timings_ms: std::collections::BTreeMap::new(),
        launch: LaunchSummary {
            prepare_command: None,
            game_command: vec!["game".to_string()],
            driver_command: None,
            caller_cwd: root.to_path_buf(),
            game_cwd: root.to_path_buf(),
        },
        artifacts: ArtifactSummary {
            directory: root.to_path_buf(),
            run_summary: root.join("run-summary.json"),
            failure_summary: None,
            game_log: root.join("game.log"),
            prepare_log: None,
            driver_log: None,
            protocol: root.join("protocol-artifact.json"),
            transcript: root.join("transcript.jsonl"),
            screenshots: root.join("screenshots"),
        },
        window: WindowSummary {
            required_logical: Some(required),
            actual: None,
        },
        process_exit: "not_started",
        teardown: TeardownSummary::default(),
        warnings: Vec::new(),
    }
}
