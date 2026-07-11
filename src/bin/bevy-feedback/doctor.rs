use crate::{args, python_client::BundledPythonClient};
use bevy_agent_feedback_plugin::client::AgentClient;
use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::{self, Command, ExitCode},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) fn run(args: &args::DoctorArgs) -> Result<ExitCode, String> {
    let mut failed = false;

    emit("ok", format!("version {}", args::version()));
    if !check_protocol(&args.protocol_file) {
        failed = true;
    }
    if !check_capture_dir() {
        failed = true;
    }
    if !check_python_version() {
        failed = true;
    }
    if !check_python_import() {
        failed = true;
    }

    Ok(if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn check_protocol(protocol_file: &Path) -> bool {
    if !protocol_file.exists() {
        emit(
            "warn",
            format!(
                "protocol file {} not present (no game running)",
                protocol_file.display()
            ),
        );
        return true;
    }

    let connection = match AgentClient::connect(protocol_file) {
        Ok(client) => Ok(client),
        Err(original_error) => match connect_without_additive_capabilities(protocol_file) {
            Some(Ok(client)) => Ok(client),
            Some(Err(error)) => Err(error),
            None => Err(original_error.to_string()),
        },
    };
    match connection {
        Ok(_client) => {
            emit(
                "ok",
                format!(
                    "protocol file {} fresh and reachable",
                    protocol_file.display()
                ),
            );
            report_protocol_capabilities(protocol_file);
            true
        }
        Err(error) => {
            emit(
                "fail",
                format!(
                    "protocol file {} stale or invalid: {error}",
                    protocol_file.display()
                ),
            );
            false
        }
    }
}

static DOCTOR_PROTOCOL_COPY_ID: AtomicU64 = AtomicU64::new(0);

fn connect_without_additive_capabilities(
    protocol_file: &Path,
) -> Option<Result<AgentClient, String>> {
    let bytes = fs::read(protocol_file).ok()?;
    let mut protocol: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let object = protocol.as_object_mut()?;
    object.insert("deterministic_time".to_string(), false.into());
    object.insert("max_wait_frames".to_string(), 1.into());
    object.insert("max_time_advance_steps".to_string(), 1.into());
    object.insert("max_time_advance_seconds".to_string(), 1.0.into());

    let copy_id = DOCTOR_PROTOCOL_COPY_ID.fetch_add(1, Ordering::Relaxed);
    let copy_path = env::temp_dir().join(format!(
        "bevy-feedback-doctor-{}-{}-{copy_id}.json",
        process::id(),
        unix_ms()
    ));
    let mut copy = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&copy_path)
    {
        Ok(copy) => copy,
        Err(_) => return None,
    };
    if serde_json::to_writer(&mut copy, &protocol).is_err() || copy.flush().is_err() {
        drop(copy);
        let _ = fs::remove_file(&copy_path);
        return None;
    }
    drop(copy);

    let connection = AgentClient::connect(&copy_path).map_err(|error| error.to_string());
    if let Err(error) = fs::remove_file(&copy_path) {
        emit(
            "warn",
            format!(
                "temporary protocol copy {} could not be removed: {error}",
                copy_path.display()
            ),
        );
    }
    Some(connection)
}
fn report_protocol_capabilities(protocol_file: &Path) {
    let bytes = match fs::read(protocol_file) {
        Ok(bytes) => bytes,
        Err(error) => {
            emit(
                "warn",
                format!("protocol capabilities unavailable: {error}"),
            );
            return;
        }
    };
    let protocol: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(protocol) => protocol,
        Err(error) => {
            emit(
                "warn",
                format!("protocol capabilities unavailable: {error}"),
            );
            return;
        }
    };

    match protocol
        .get("deterministic_time")
        .and_then(serde_json::Value::as_bool)
    {
        Some(enabled) => emit("ok", format!("protocol deterministic_time={enabled}")),
        None => emit(
            "warn",
            "protocol deterministic_time missing or malformed; omitted".to_string(),
        ),
    }

    match protocol
        .get("max_wait_frames")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
        .filter(|value| *value > 0)
    {
        Some(value) => emit("ok", format!("protocol max_wait_frames={value}")),
        None => emit(
            "warn",
            "protocol max_wait_frames missing or malformed; omitted".to_string(),
        ),
    }

    match protocol
        .get("max_time_advance_steps")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
        .filter(|value| *value > 0)
    {
        Some(value) => emit("ok", format!("protocol max_time_advance_steps={value}")),
        None => emit(
            "warn",
            "protocol max_time_advance_steps missing or malformed; omitted".to_string(),
        ),
    }

    match protocol
        .get("max_time_advance_seconds")
        .and_then(serde_json::Value::as_f64)
        .filter(|value| value.is_finite() && *value > 0.0)
    {
        Some(value) => emit("ok", format!("protocol max_time_advance_seconds={value}")),
        None => emit(
            "warn",
            "protocol max_time_advance_seconds missing or malformed; omitted".to_string(),
        ),
    }
}

fn check_capture_dir() -> bool {
    let capture_dir = env::var_os("BEVY_FEEDBACK_CAPTURE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/agent-feedback/captures"));
    match probe_writable(&capture_dir) {
        Ok(()) => {
            emit(
                "ok",
                format!("capture dir {} writable", capture_dir.display()),
            );
            true
        }
        Err(error) => {
            emit(
                "fail",
                format!(
                    "capture dir {} not writable: {error}",
                    capture_dir.display()
                ),
            );
            false
        }
    }
}

fn check_python_version() -> bool {
    match Command::new("python3").arg("--version").output() {
        Ok(output) if output.status.success() => {
            let version = clean_output(&output.stdout, &output.stderr);
            emit("ok", format!("python3 on PATH: {version}"));
            true
        }
        Ok(output) => {
            let status = output.status;
            let output = clean_output(&output.stdout, &output.stderr);
            emit(
                "fail",
                format!("python3 --version exited with status {status}: {output}"),
            );
            false
        }
        Err(error) => {
            emit("fail", format!("python3 not found on PATH: {error}"));
            false
        }
    }
}

fn check_python_import() -> bool {
    let dir = env::temp_dir().join(format!("bevy-feedback-doctor-{}", std::process::id()));
    let client = match BundledPythonClient::materialize(&dir) {
        Ok(client) => client,
        Err(error) => {
            emit(
                "fail",
                format!("cannot materialize bundled client: {error}"),
            );
            return false;
        }
    };
    let output = Command::new("python3")
        .args(["-c", "import bevy_feedback"])
        .env("PYTHONPATH", client.python_path())
        .output();
    client.remove();

    match output {
        Ok(output) if output.status.success() => {
            emit(
                "ok",
                "python import bevy_feedback works (bundled client)".to_string(),
            );
            true
        }
        Ok(output) => {
            let output_text = clean_output(&output.stdout, &output.stderr);
            emit(
                "fail",
                format!(
                    "python import bevy_feedback failed: {output_text}; bevy-feedback run injects the bundled client automatically; this failure means python3 cannot import it even with PYTHONPATH set"
                ),
            );
            false
        }
        Err(error) => {
            emit(
                "fail",
                format!(
                    "python3 import check failed: {error}; bevy-feedback run injects the bundled client automatically; this failure means python3 cannot import it even with PYTHONPATH set"
                ),
            );
            false
        }
    }
}

fn probe_writable(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let probe = dir.join(format!(
        ".bevy-feedback-doctor-{}-{}.tmp",
        std::process::id(),
        unix_ms()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)?;
        file.write_all(b"ok")?;
        file.flush()
    })();
    let remove_result = fs::remove_file(&probe);
    result?;
    remove_result.or_else(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(error)
        }
    })
}

fn emit(status: &str, message: String) {
    println!("{status}: {message}");
}

fn clean_output(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    let text = if stdout.trim().is_empty() {
        stderr.trim().to_string()
    } else if stderr.trim().is_empty() {
        stdout.trim().to_string()
    } else {
        format!("{}; {}", stdout.trim(), stderr.trim())
    };
    text.replace(['\r', '\n'], "; ")
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Map, Value, json};
    use std::net::TcpListener;

    const DOCTOR_PROBE_PROTOCOL: &str = "BEVY_FEEDBACK_TEST_DOCTOR_PROTOCOL";
    const DOCTOR_PROBE_MODE: &str = "BEVY_FEEDBACK_TEST_DOCTOR_MODE";

    #[test]
    fn live_protocol_reports_deterministic_mode_and_advertised_caps() {
        let root = temp_root("live-capabilities");
        fs::create_dir_all(&root).expect("temp root");
        let protocol_file = root.join("agent.json");
        let listener = write_live_protocol(
            &protocol_file,
            json!({
                "deterministic_time": true,
                "max_wait_frames": 240,
                "max_time_advance_steps": 480,
                "max_time_advance_seconds": 7.5
            }),
        );

        let output = doctor_probe(&protocol_file, "check");
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(
            output.status.success(),
            "doctor probe failed\nstdout:\n{stdout}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains(&format!(
                "ok: protocol file {} fresh and reachable",
                protocol_file.display()
            )),
            "{stdout}"
        );
        assert!(
            stdout.contains("ok: protocol deterministic_time=true"),
            "{stdout}"
        );
        assert!(
            stdout.contains("ok: protocol max_wait_frames=240"),
            "{stdout}"
        );
        assert!(
            stdout.contains("ok: protocol max_time_advance_steps=480"),
            "{stdout}"
        );
        assert!(
            stdout.contains("ok: protocol max_time_advance_seconds=7.5"),
            "{stdout}"
        );
        drop(listener);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn live_protocol_warns_when_additive_capability_metadata_is_missing() {
        let root = temp_root("missing-capabilities");
        fs::create_dir_all(&root).expect("temp root");
        let protocol_file = root.join("agent.json");
        let listener = write_live_protocol(&protocol_file, json!({}));

        let output = doctor_probe(&protocol_file, "check");
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(
            output.status.success(),
            "doctor probe failed\nstdout:\n{stdout}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("warn: protocol deterministic_time missing or malformed; omitted"),
            "{stdout}"
        );
        assert!(
            stdout.contains("warn: protocol max_wait_frames missing or malformed; omitted"),
            "{stdout}"
        );
        assert!(
            stdout.contains("warn: protocol max_time_advance_steps missing or malformed; omitted"),
            "{stdout}"
        );
        assert!(
            stdout
                .contains("warn: protocol max_time_advance_seconds missing or malformed; omitted"),
            "{stdout}"
        );
        drop(listener);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn live_protocol_warns_when_additive_capability_metadata_is_malformed() {
        let root = temp_root("malformed-capabilities");
        fs::create_dir_all(&root).expect("temp root");
        let protocol_file = root.join("agent.json");
        let listener = write_live_protocol(
            &protocol_file,
            json!({
                "deterministic_time": "enabled",
                "max_wait_frames": 0,
                "max_time_advance_steps": -1,
                "max_time_advance_seconds": "unbounded"
            }),
        );

        let output = doctor_probe(&protocol_file, "check");
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(
            output.status.success(),
            "doctor probe failed\nstdout:\n{stdout}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains(&format!(
                "ok: protocol file {} fresh and reachable",
                protocol_file.display()
            )),
            "{stdout}"
        );
        assert!(!stdout.contains("stale or invalid"), "{stdout}");
        assert!(
            stdout.contains("warn: protocol deterministic_time missing or malformed; omitted"),
            "{stdout}"
        );
        assert!(
            stdout.contains("warn: protocol max_wait_frames missing or malformed; omitted"),
            "{stdout}"
        );
        assert!(
            stdout.contains("warn: protocol max_time_advance_steps missing or malformed; omitted"),
            "{stdout}"
        );
        assert!(
            stdout
                .contains("warn: protocol max_time_advance_seconds missing or malformed; omitted"),
            "{stdout}"
        );
        drop(listener);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[ignore = "subprocess stdout harness for doctor output tests"]
    fn doctor_output_probe() {
        let Some(protocol_file) = std::env::var_os(DOCTOR_PROBE_PROTOCOL) else {
            return;
        };
        let protocol_file = PathBuf::from(protocol_file);
        match std::env::var(DOCTOR_PROBE_MODE)
            .expect("doctor probe mode")
            .as_str()
        {
            "check" => assert!(check_protocol(&protocol_file)),
            "report" => report_protocol_capabilities(&protocol_file),
            mode => panic!("unknown doctor probe mode {mode}"),
        }
    }

    fn doctor_probe(protocol_file: &Path, mode: &str) -> std::process::Output {
        Command::new(std::env::current_exe().expect("current test executable"))
            .args([
                "--ignored",
                "--exact",
                "doctor::tests::doctor_output_probe",
                "--nocapture",
            ])
            .env(DOCTOR_PROBE_PROTOCOL, protocol_file)
            .env(DOCTOR_PROBE_MODE, mode)
            .output()
            .expect("run doctor output probe")
    }

    fn write_live_protocol(protocol_file: &Path, metadata: Value) -> TcpListener {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind doctor protocol");
        let heartbeat = protocol_file.with_extension("heartbeat");
        fs::write(&heartbeat, unix_ms().to_string()).expect("heartbeat");
        let mut protocol = Map::from_iter([
            (
                "protocol".to_string(),
                Value::String("bevy-agent-feedback/0.5".to_string()),
            ),
            (
                "socket_addr".to_string(),
                Value::String(listener.local_addr().expect("listener address").to_string()),
            ),
            ("pid".to_string(), Value::from(std::process::id())),
            (
                "heartbeat_file".to_string(),
                Value::String(heartbeat.to_string_lossy().into_owned()),
            ),
            ("stale_after_ms".to_string(), Value::from(5_000)),
        ]);
        for (name, value) in metadata
            .as_object()
            .expect("metadata object")
            .iter()
            .take(4)
        {
            protocol.insert(name.clone(), value.clone());
        }
        fs::write(
            protocol_file,
            serde_json::to_vec(&protocol).expect("protocol JSON"),
        )
        .expect("protocol file");
        listener
    }

    fn temp_root(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        env::temp_dir().join(format!(
            "bevy-feedback-doctor-test-{name}-{}-{nonce}",
            std::process::id()
        ))
    }
}
