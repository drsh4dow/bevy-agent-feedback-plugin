use crate::{args, python_client::BundledPythonClient};
use bevy_agent_feedback_plugin::client::AgentClient;
use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
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

    match AgentClient::connect(protocol_file) {
        Ok(_client) => {
            emit(
                "ok",
                format!(
                    "protocol file {} fresh and reachable",
                    protocol_file.display()
                ),
            );
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
