use super::*;
use crate::DiagnosticValue;
use image::{ImageBuffer, Rgba};

#[test]
fn image_diff_counts_changed_pixels() {
    let root = std::env::temp_dir().join(format!("bevy-agent-client-{}", unix_ms()));
    fs::create_dir_all(&root).expect("temp root");
    let a = root.join("a.png");
    let b = root.join("b.png");
    ImageBuffer::<Rgba<u8>, _>::from_pixel(2, 2, Rgba([0, 0, 0, 255]))
        .save(&a)
        .expect("save a");
    let mut changed = ImageBuffer::<Rgba<u8>, _>::from_pixel(2, 2, Rgba([0, 0, 0, 255]));
    changed.put_pixel(1, 1, Rgba([255, 0, 0, 255]));
    changed.save(&b).expect("save b");

    assert_eq!(AgentClient::pixel_diff(&a, &b).expect("diff"), 1);
    assert_eq!(
        AgentClient::region_diff(
            &a,
            &b,
            Region {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
        )
        .expect("region diff"),
        0
    );
    AgentClient::assert_changed(&a, &b, 1).expect("changed");
    let error = AgentClient::assert_changed(&a, &b, 2).expect_err("threshold");
    assert!(error.to_string().contains("changed 1 pixels"));
    AgentClient::assert_region_changed(
        &a,
        &b,
        Region {
            x: 1,
            y: 1,
            width: 1,
            height: 1,
        },
        1,
    )
    .expect("region changed");
    AgentClient::assert_color_present(&b, [255, 0, 0], None, 0, 1).expect("color present");
    let error = AgentClient::assert_color_present(&b, [255, 0, 0], None, 0, 2)
        .expect_err("color threshold");
    assert!(error.to_string().contains("found 1 pixels"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn rejects_old_protocol_files() {
    let root = std::env::temp_dir().join(format!("bevy-agent-client-protocol-{}", unix_ms()));
    fs::create_dir_all(&root).expect("temp root");
    let protocol = root.join("agent.json");
    fs::write(
        &protocol,
        json!({
            "protocol": "bevy-agent-feedback/1",
            "socket_addr": "127.0.0.1:1",
            "pid": std::process::id(),
            "heartbeat_file": root.join("heartbeat"),
            "stale_after_ms": 1000,
        })
        .to_string(),
    )
    .expect("protocol");

    let error = read_protocol(&protocol).expect_err("v1 should be rejected");
    assert!(error.to_string().contains("expected bevy-agent-feedback/3"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn rejects_unknown_protocol_files() {
    let root = std::env::temp_dir().join(format!("bevy-agent-client-unknown-{}", unix_ms()));
    fs::create_dir_all(&root).expect("temp root");
    let protocol = root.join("agent.json");
    fs::write(&protocol, "{}").expect("protocol");

    let error = read_protocol(&protocol).expect_err("unknown protocol");
    assert!(error.to_string().contains("missing protocol"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn rejects_stale_heartbeat() {
    let root = std::env::temp_dir().join(format!("bevy-agent-client-stale-{}", unix_ms()));
    fs::create_dir_all(&root).expect("temp root");
    let heartbeat = root.join("heartbeat");
    fs::write(&heartbeat, "1").expect("heartbeat");
    let protocol = root.join("agent.json");
    fs::write(
        &protocol,
        json!({
            "protocol": PROTOCOL_VERSION,
            "socket_addr": "127.0.0.1:1",
            "pid": std::process::id(),
            "heartbeat_file": heartbeat,
            "stale_after_ms": 1,
        })
        .to_string(),
    )
    .expect("protocol");

    let error = read_protocol(&protocol).expect_err("stale heartbeat");
    assert!(error.to_string().contains("protocol stale"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn ocr_missing_binary_is_unavailable() {
    let root = std::env::temp_dir().join(format!("bevy-agent-client-ocr-{}", unix_ms()));
    fs::create_dir_all(&root).expect("temp root");
    let image = root.join("text.png");
    ImageBuffer::<Rgba<u8>, _>::from_pixel(2, 2, Rgba([255, 255, 255, 255]))
        .save(&image)
        .expect("save image");
    let config = OcrOptions {
        tesseract: root.join("missing-tesseract"),
        ..Default::default()
    };

    let error = run_tesseract(&config, &image).expect_err("missing binary");
    assert!(matches!(error, ClientError::OcrUnavailable(_)));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn exposes_capabilities_and_rejects_oversized_wait_before_transmission() {
    let (mut client, server, root) = test_client("capabilities", |mut stream| {
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
        let request = read_request(&mut reader);
        assert_eq!(request["command"], "window_info");
        writeln!(
            stream,
            "{}",
            json!({
                "id": request["id"],
                "ok": true,
                "result": {"status": "ok"}
            })
        )
        .expect("response");
    });

    assert_eq!(client.capabilities().max_wait_frames, 7);
    assert_eq!(client.capabilities().max_abort_predicates, 2);
    assert_eq!(client.max_wait_frames(), 7);
    let error = client.wait_frames(8).expect_err("oversized wait");
    let message = error.to_string();
    assert!(message.contains("frames=8"), "{message}");
    assert!(message.contains("server limit 7"), "{message}");
    assert!(message.contains("explicit bounded requests"), "{message}");
    client.window_info().expect("request after local rejection");

    drop(client);
    server.join().expect("server");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn semantic_abort_attaches_a_labeled_capture_to_the_original_error() {
    let (mut client, server, root) = test_client("semantic-capture", |mut stream| {
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
        let wait = read_request(&mut reader);
        assert_eq!(wait["command"], "wait_for");
        assert_eq!(wait["abort_predicates"].as_array().map(Vec::len), Some(1));
        writeln!(
            stream,
            "{}",
            json!({
                "id": wait["id"],
                "ok": false,
                "error": {
                    "code": "predicate_aborted",
                    "message": "abort predicate matched",
                    "context": {"snapshot": {"frame": 42}}
                }
            })
        )
        .expect("abort response");

        let capture = read_request(&mut reader);
        assert_eq!(capture["command"], "capture");
        assert_eq!(capture["label"], "semantic-wait-failure");
        writeln!(stream, "{}", capture_response(capture["id"].clone())).expect("capture response");
    });
    let success = Predicate::StateEquals {
        state: "GamePhase".to_string(),
        value: DiagnosticValue::String("Playing".to_string()),
    };
    let abort = Predicate::StateEquals {
        state: "GamePhase".to_string(),
        value: DiagnosticValue::String("LoadFailed".to_string()),
    };

    let error = client
        .wait_for_with_abort(success, &[abort], 3)
        .expect_err("semantic abort");
    let ClientError::Command { code, context, .. } = error else {
        panic!("expected command error");
    };
    assert_eq!(code, "predicate_aborted");
    assert_eq!(
        context.expect("context")["failure_capture"]["path"],
        "/captures/semantic-wait-failure.png"
    );

    drop(client);
    server.join().expect("server");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn capture_failure_does_not_replace_the_semantic_error() {
    let (mut client, server, root) = test_client("capture-failure", |mut stream| {
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
        let wait = read_request(&mut reader);
        writeln!(
            stream,
            "{}",
            json!({
                "id": wait["id"],
                "ok": false,
                "error": {"code": "predicate_timeout", "message": "deadline"}
            })
        )
        .expect("timeout response");
        let capture = read_request(&mut reader);
        writeln!(
            stream,
            "{}",
            json!({
                "id": capture["id"],
                "ok": false,
                "error": {"code": "capture_failed", "message": "no renderer"}
            })
        )
        .expect("capture failure");
    });
    let predicate = Predicate::StateEquals {
        state: "GamePhase".to_string(),
        value: DiagnosticValue::String("Playing".to_string()),
    };

    let error = client.wait_for(predicate, 3).expect_err("semantic timeout");
    assert!(matches!(
        error,
        ClientError::Command { ref code, .. } if code == "predicate_timeout"
    ));

    drop(client);
    server.join().expect("server");
    let _ = fs::remove_dir_all(root);
}

fn test_client(
    name: &str,
    serve: impl FnOnce(TcpStream) + Send + 'static,
) -> (AgentClient, std::thread::JoinHandle<()>, PathBuf) {
    let root = std::env::temp_dir().join(format!("bevy-agent-client-{name}-{}", unix_ms()));
    fs::create_dir_all(&root).expect("temp root");
    let heartbeat = root.join("heartbeat");
    fs::write(&heartbeat, unix_ms().to_string()).expect("heartbeat");
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("listener");
    let protocol = root.join("agent.json");
    fs::write(
        &protocol,
        json!({
            "protocol": PROTOCOL_VERSION,
            "socket_addr": listener.local_addr().expect("address"),
            "pid": std::process::id(),
            "heartbeat_file": heartbeat,
            "stale_after_ms": 10_000,
            "max_wait_frames": 7,
            "max_abort_predicates": 2,
            "deterministic_time": false,
            "max_time_advance_steps": 4,
            "max_time_advance_seconds": 3.0
        })
        .to_string(),
    )
    .expect("protocol");
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        serve(stream);
    });
    let client = AgentClient::with_config(AgentClientConfig {
        protocol_file: protocol,
        timeout: Duration::from_secs(1),
        ..Default::default()
    })
    .expect("client");
    (client, server, root)
}

fn read_request(reader: &mut BufReader<TcpStream>) -> Value {
    let mut line = String::new();
    reader.read_line(&mut line).expect("request read");
    serde_json::from_str(&line).expect("request JSON")
}

fn capture_response(id: Value) -> Value {
    let window = json!({
        "logical_width": 640.0,
        "logical_height": 480.0,
        "physical_width": 640,
        "physical_height": 480,
        "scale_factor": 1.0,
        "focused": true,
        "visible": true,
        "mode": "windowed"
    });
    let capture = json!({
        "sequence": 1,
        "path": "/captures/semantic-wait-failure.png",
        "label": "semantic-wait-failure",
        "requested_frame": 42,
        "completed_frame": 43,
        "image_width": 640,
        "image_height": 480,
        "window_at_request": window,
        "window_at_completion": window,
        "completion": "screenshot_captured"
    });
    json!({
        "id": id,
        "ok": true,
        "result": {
            "status": "captured",
            "capture": capture,
            "latest_capture": capture
        }
    })
}
