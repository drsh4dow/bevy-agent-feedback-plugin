use super::*;
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
fn rejects_v1_protocol_files() {
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
    assert!(error.to_string().contains("expected bevy-agent-feedback/2"));
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
