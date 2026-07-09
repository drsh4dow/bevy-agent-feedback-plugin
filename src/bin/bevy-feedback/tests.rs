use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

const RUN_PROBE_SCENARIO: &str = "BEVY_FEEDBACK_TEST_RUN_SCENARIO";
const RUN_PROBE_ARTIFACTS: &str = "BEVY_FEEDBACK_TEST_RUN_ARTIFACTS";

#[test]
fn spawn_error_hints_when_executable_contains_whitespace() {
    let error = spawn_error(
        "--game",
        &["cargo run --features agent".into()],
        std::io::Error::new(std::io::ErrorKind::NotFound, "missing"),
    );

    assert!(
        error.contains("spawn --game [\"cargo run --features agent\"]: missing"),
        "{error}"
    );
    assert!(error.contains("pass each argv word separately"), "{error}");
}

#[test]
fn copies_only_png_captures_sorted() {
    let root = temp_root("copy-captures");
    let from = root.join("from");
    let to = root.join("to");
    fs::create_dir_all(&from).expect("from dir");
    fs::write(from.join("capture-000002-b.png"), b"b").expect("capture b");
    fs::write(from.join("capture-000001-a.png"), b"a").expect("capture a");
    fs::write(from.join("notes.txt"), b"skip").expect("notes");

    let copied = copy_captures(&from, &to).expect("copy captures");

    assert_eq!(
        copied,
        vec![
            to.join("capture-000001-a.png"),
            to.join("capture-000002-b.png")
        ]
    );
    assert_eq!(fs::read(to.join("capture-000001-a.png")).expect("a"), b"a");
    assert!(!to.join("notes.txt").exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn transcript_prefers_latest_error_context_over_later_diagnostic_details() {
    let root = temp_root("latest-error");
    fs::create_dir_all(&root).expect("temp root");
    let transcript = root.join("transcript.jsonl");
    write_envelopes(
        &transcript,
        &[
            serde_json::json!({
                "response": {"error": {"context": {"error": "older"}}}
            }),
            serde_json::json!({
                "response": {"error": {"context": {"error": "latest", "frame": 41}}}
            }),
            serde_json::json!({
                "request": {"command": "evaluate_predicate"},
                "response": {
                    "result": {
                        "status": "predicate_evaluated",
                        "details": {"matched": true, "frame": 42}
                    }
                }
            }),
        ],
    );

    let context = transcript_diagnostic_context(&transcript).expect("read transcript");

    assert_eq!(
        context,
        Some(serde_json::json!({"error": "latest", "frame": 41}))
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn transcript_uses_latest_diagnostic_details_without_error_context() {
    let root = temp_root("diagnostic-fallback");
    fs::create_dir_all(&root).expect("temp root");
    let transcript = root.join("transcript.jsonl");
    write_envelopes(
        &transcript,
        &[
            serde_json::json!({
                "response": {
                    "result": {
                        "status": "target_info",
                        "details": {"target": "older"}
                    }
                }
            }),
            serde_json::json!({
                "request": {"command": "ecs_summary"},
                "response": {
                    "result": {
                        "status": "ok",
                        "details": {"entities": 17, "frame": 73}
                    }
                }
            }),
            serde_json::json!({
                "request": {"command": "wait_seconds"},
                "response": {
                    "result": {
                        "status": "waited",
                        "details": {"frames": 2}
                    }
                }
            }),
        ],
    );

    let context = transcript_diagnostic_context(&transcript).expect("read transcript");

    assert_eq!(
        context,
        Some(serde_json::json!({"entities": 17, "frame": 73}))
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn diagnostic_context_block_is_capped_at_eight_kibibytes() {
    let root = temp_root("context-cap");
    fs::create_dir_all(&root).expect("temp root");
    let transcript = root.join("transcript.jsonl");
    write_envelopes(
        &transcript,
        &[serde_json::json!({
            "response": {
                "error": {
                    "context": {
                        "message": format!("prefix-{}-suffix", "x".repeat(20 * 1024))
                    }
                }
            }
        })],
    );
    let mut summary = String::new();

    append_transcript_context(&mut summary, &transcript);

    assert!(
        summary.len() <= DIAGNOSTIC_BLOCK_MAX_BYTES,
        "diagnostic block was {} bytes",
        summary.len()
    );
    assert!(summary.starts_with("diagnostic context:\n"));
    assert!(summary.contains("\"truncated\":true"));
    assert!(summary.contains("prefix-"));
    assert!(!summary.contains("-suffix"));
    let rendered = summary
        .strip_prefix("diagnostic context:\n")
        .and_then(|value| value.strip_suffix('\n'))
        .expect("bounded diagnostic block");
    serde_json::from_str::<Value>(rendered).expect("truncated context remains valid JSON");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn transcript_ignores_malformed_lines_and_trailing_partial_record() {
    let root = temp_root("malformed-and-partial");
    fs::create_dir_all(&root).expect("temp root");
    let transcript = root.join("transcript.jsonl");
    let valid = serde_json::json!({
        "response": {
            "result": {
                "status": "resource_info",
                "details": {"resource": "Score", "value": 99}
            }
        }
    });
    fs::write(
        &transcript,
        format!("not JSON\n{valid}\n{{\"response\":{{\"error\":"),
    )
    .expect("transcript");

    let context = transcript_diagnostic_context(&transcript).expect("read transcript");

    assert_eq!(
        context,
        Some(serde_json::json!({
            "resource": "Score",
            "value": 99
        }))
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn transcript_without_complete_diagnostic_context_reports_unavailable() {
    let root = temp_root("unavailable-context");
    fs::create_dir_all(&root).expect("temp root");
    let cases = [
        ("empty", Some(Vec::new())),
        ("malformed", Some(b"not JSON\n\xff\n".to_vec())),
        (
            "partial",
            Some(br#"{"response":{"error":{"context":{"reason":"unfinished"}}}}"#.to_vec()),
        ),
        (
            "request-only",
            Some(br#"{"request":{"command":"ecs_summary"}}"#.to_vec()),
        ),
        ("missing", None),
    ];

    for (name, bytes) in cases {
        let transcript = root.join(format!("{name}.jsonl"));
        if let Some(bytes) = bytes {
            fs::write(&transcript, bytes).expect("transcript");
        }
        let mut summary = String::new();

        append_transcript_context(&mut summary, &transcript);

        assert_eq!(
            summary, "diagnostic context unavailable\n",
            "unexpected context for {name}"
        );
    }
    let _ = fs::remove_dir_all(root);
}

#[test]
fn fail_run_preserves_original_failure_capture_and_log_tails() {
    let root = temp_root("failure-artifacts");
    let artifacts = root.join("artifacts");
    let capture_dir = root.join("captures");
    fs::create_dir_all(&artifacts).expect("artifacts");
    fs::create_dir_all(&capture_dir).expect("captures");
    let game_log = artifacts.join("game.log");
    let driver_log = artifacts.join("driver.log");
    let transcript = artifacts.join("transcript.jsonl");
    let game_lines = (0..25)
        .map(|line| format!("game-line-{line:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let driver_lines = (0..23)
        .map(|line| format!("driver-line-{line:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&game_log, game_lines).expect("game log");
    fs::write(&driver_log, driver_lines).expect("driver log");
    fs::write(capture_dir.join("failure-frame.png"), b"png").expect("capture");
    write_envelopes(
        &transcript,
        &[serde_json::json!({
            "response": {
                "error": {
                    "context": {"predicate": "target_exists", "matched": false}
                }
            }
        })],
    );
    let original = "driver exited with status exit status: 7\n".to_string();

    let error = fail_run(
        &artifacts,
        &game_log,
        Some(&driver_log),
        &capture_dir,
        &transcript,
        original.clone(),
    )
    .expect_err("run should fail");
    let summary =
        fs::read_to_string(artifacts.join("failure-summary.txt")).expect("failure summary");

    assert!(summary.starts_with(&original), "{summary}");
    assert!(error.contains(&original), "{error}");
    assert!(
        summary.contains(&format!("game log: {}", game_log.display())),
        "{summary}"
    );
    assert!(
        summary.contains(&format!("driver log: {}", driver_log.display())),
        "{summary}"
    );
    assert!(
        summary.contains(&format!(
            "newest capture: {}",
            capture_dir.join("failure-frame.png").display()
        )),
        "{summary}"
    );
    assert!(!summary.contains("game-line-04"), "{summary}");
    assert!(summary.contains("game-line-05"), "{summary}");
    assert!(!summary.contains("driver-line-02"), "{summary}");
    assert!(summary.contains("driver-line-03"), "{summary}");
    assert!(
        summary.contains(r#"{"matched":false,"predicate":"target_exists"}"#),
        "{summary}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn both_cli_failure_paths_write_focused_failure_artifacts() {
    let cases = [
        (
            "readiness",
            "game exited before protocol was ready",
            r#"{"phase":"startup","reason":"protocol-missing"}"#,
            false,
        ),
        (
            "dead-game",
            "game exited with status",
            "diagnostic context unavailable",
            true,
        ),
    ];

    for (scenario, original_failure, diagnostic, copies_protocol) in cases {
        let root = temp_root(scenario);
        let artifacts = root.join("artifacts");
        let output = Command::new(std::env::current_exe().expect("current test executable"))
            .args([
                "--ignored",
                "--exact",
                "tests::cli_run_failure_probe",
                "--nocapture",
            ])
            .env(RUN_PROBE_SCENARIO, scenario)
            .env(RUN_PROBE_ARTIFACTS, &artifacts)
            .output()
            .expect("run CLI failure probe");
        assert!(
            output.status.success(),
            "{scenario} probe failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if scenario == "dead-game" {
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                stdout.contains(
                    "protocol ready != game ready; use semantic readiness for animated games or strict stability for static scenes before capturing"
                ),
                "{stdout}"
            );
            assert!(
                !stdout.contains("wait for a stable frame before capturing"),
                "{stdout}"
            );
        }
        let summary =
            fs::read_to_string(artifacts.join("failure-summary.txt")).expect("failure summary");

        assert!(summary.contains(original_failure), "{scenario}: {summary}");
        assert!(summary.contains(diagnostic), "{scenario}: {summary}");
        assert!(
            summary.contains(&format!(
                "game log: {}",
                artifacts.join("game.log").display()
            )),
            "{scenario}: {summary}"
        );
        assert_eq!(
            artifacts.join("protocol.json").exists(),
            copies_protocol,
            "{scenario}: protocol artifact"
        );
        let _ = fs::remove_dir_all(root);
    }
}

#[test]
#[ignore = "subprocess harness for both_cli_failure_paths_write_focused_failure_artifacts"]
fn cli_run_failure_probe() {
    let Some(scenario) = std::env::var_os(RUN_PROBE_SCENARIO) else {
        return;
    };
    let artifacts =
        PathBuf::from(std::env::var_os(RUN_PROBE_ARTIFACTS).expect("probe artifacts directory"));
    let executable = std::env::current_exe().expect("current test executable");
    let game = vec![
        executable.to_string_lossy().into_owned(),
        "--ignored".to_string(),
        "--exact".to_string(),
        "tests::cli_fake_game_probe".to_string(),
        "--nocapture".to_string(),
    ];
    let args = RunArgs {
        protocol_file: artifacts.join("live-protocol.json"),
        artifacts,
        ready_timeout: Duration::from_secs(3),
        shutdown_timeout: Duration::from_millis(100),
        driver_timeout: Duration::from_secs(1),
        game,
        driver: None,
    };

    let error = run(args).expect_err("probe run should fail");

    match scenario.to_str().expect("UTF-8 scenario") {
        "readiness" => assert!(error.contains("game exited before protocol was ready")),
        "dead-game" => assert!(error.contains("game exited with status")),
        other => panic!("unknown run probe scenario {other}"),
    }
}

#[test]
#[ignore = "fake game subprocess for cli_run_failure_probe"]
fn cli_fake_game_probe() {
    let scenario = std::env::var(RUN_PROBE_SCENARIO).expect("probe scenario");
    let transcript =
        PathBuf::from(std::env::var_os("BEVY_FEEDBACK_TRANSCRIPT").expect("transcript path"));
    if scenario == "readiness" {
        write_envelopes(
            &transcript,
            &[serde_json::json!({
                "response": {
                    "error": {
                        "context": {
                            "phase": "startup",
                            "reason": "protocol-missing"
                        }
                    }
                }
            })],
        );
        std::process::exit(17);
    }

    assert_eq!(scenario, "dead-game");
    let protocol_file =
        PathBuf::from(std::env::var_os("BEVY_FEEDBACK_PROTOCOL").expect("protocol path"));
    let capture_dir =
        PathBuf::from(std::env::var_os("BEVY_FEEDBACK_CAPTURE_DIR").expect("capture path"));
    let heartbeat = protocol_file.with_extension("heartbeat");
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind fake game");
    listener
        .set_nonblocking(true)
        .expect("nonblocking fake game");
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis();
    fs::write(&heartbeat, now_ms.to_string()).expect("heartbeat");
    fs::write(
        &protocol_file,
        serde_json::to_vec(&serde_json::json!({
            "protocol": "bevy-agent-feedback/2",
            "session_id": "dead-game-probe",
            "pid": std::process::id(),
            "heartbeat_file": heartbeat,
            "stale_after_ms": 5_000,
            "socket_addr": listener.local_addr().expect("listener address"),
            "capture_dir": capture_dir,
            "deterministic_time": false,
            "max_wait_frames": 60,
            "max_time_advance_steps": 60,
            "max_time_advance_seconds": 1.0
        }))
        .expect("protocol JSON"),
    )
    .expect("protocol file");

    for _ in 0..300 {
        match listener.accept() {
            Ok((_stream, _address)) => std::process::exit(9),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("fake game accept failed: {error}"),
        }
    }
    panic!("CLI did not connect to fake game within three seconds");
}

fn write_envelopes(path: &Path, envelopes: &[Value]) {
    let mut bytes = Vec::new();
    for envelope in envelopes {
        serde_json::to_writer(&mut bytes, envelope).expect("serialize transcript envelope");
        bytes.push(b'\n');
    }
    fs::write(path, bytes).expect("write transcript");
}

fn temp_root(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "bevy-feedback-cli-{name}-{}-{nonce}",
        std::process::id()
    ))
}
