use super::*;

pub(super) fn spawn_logged_command(
    label: &str,
    command: &[String],
    cwd: Option<&Path>,
    log_path: &Path,
) -> Result<Child, String> {
    let mut child = Command::new(&command[0]);
    child
        .args(&command[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        child.current_dir(cwd);
    }
    let mut child = child
        .spawn()
        .map_err(|error| spawn_error(label, command, error))?;
    let log = Arc::new(Mutex::new(
        File::create(log_path).map_err(|error| error.to_string())?,
    ));
    stream_child_logs(&mut child, log);
    Ok(child)
}

pub(super) fn spawn_command(
    label: &str,
    command: &[String],
    args: &RunArgs,
    python: &BundledPythonClient,
    capture_dir: &Path,
    transcript_file: &Path,
    cwd: Option<&Path>,
) -> Result<Child, String> {
    let mut child = Command::new(&command[0]);
    child
        .args(&command[1..])
        .env("BEVY_FEEDBACK_PROTOCOL", &args.protocol_file)
        .env("BEVY_FEEDBACK_CAPTURE_DIR", capture_dir)
        .env("BEVY_FEEDBACK_ARTIFACTS", &args.artifacts)
        .env("BEVY_FEEDBACK_TRANSCRIPT", transcript_file)
        .env("PYTHONPATH", python.python_path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        child.current_dir(cwd);
    }
    child
        .spawn()
        .map_err(|error| spawn_error(label, command, error))
}

pub(crate) fn spawn_error(label: &str, command: &[String], error: io::Error) -> String {
    let mut message = format!("spawn {label} {command:?}: {error}");
    if command
        .first()
        .is_some_and(|executable| executable.chars().any(char::is_whitespace))
    {
        message.push_str(
            "; executable contains whitespace; if this was a command plus arguments, pass each argv word separately",
        );
    }
    message
}

pub(super) fn stream_child_logs(child: &mut Child, log_file: Arc<Mutex<File>>) {
    if let Some(stdout) = child.stdout.take() {
        stream_log(stdout, false, log_file.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        stream_log(stderr, true, log_file);
    }
}

fn stream_log(pipe: impl Read + Send + 'static, stderr: bool, log_file: Arc<Mutex<File>>) {
    thread::spawn(move || {
        let mut reader = BufReader::new(pipe);
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap_or(0) > 0 {
            if stderr {
                eprint!("{line}");
            } else {
                print!("{line}");
            }
            if let Ok(mut file) = log_file.lock() {
                let _ = file.write_all(line.as_bytes());
                let _ = file.flush();
            }
            line.clear();
        }
    });
}

#[derive(Debug)]
pub(super) struct ReadyInfo {
    pub(super) session_id: String,
    pub(super) socket_addr: String,
    pub(super) capture_dir: Option<PathBuf>,
}

pub(super) fn wait_ready(
    protocol_file: &Path,
    timeout: Duration,
    game: &mut Child,
    stop: &AtomicBool,
) -> Result<ReadyInfo, String> {
    let start = Instant::now();
    while start.elapsed() < timeout && !stop.load(Ordering::Relaxed) {
        if let Some(status) = game.try_wait().map_err(|error| error.to_string())? {
            return Err(format!("game exited before protocol was ready: {status}"));
        }
        if protocol_file.exists() && AgentClient::connect(protocol_file).is_ok() {
            let protocol: Value = serde_json::from_slice(
                &fs::read(protocol_file).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            return Ok(ReadyInfo {
                session_id: protocol["session_id"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string(),
                socket_addr: protocol["socket_addr"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string(),
                capture_dir: protocol["capture_dir"]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from),
            });
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(format!(
        "protocol was not ready within {} ms while the game process remained live",
        timeout.as_millis()
    ))
}

pub(super) fn wait_game_or_signal(
    game: &mut Child,
    stop: &AtomicBool,
) -> io::Result<Option<ExitStatus>> {
    while !stop.load(Ordering::Relaxed) {
        if let Some(status) = game.try_wait()? {
            return Ok(Some(status));
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(None)
}

pub(super) fn teardown_game(
    protocol_file: &Path,
    transcript_file: &Path,
    game: &mut Child,
    timeout: Duration,
    summary: &mut RunSummary,
) {
    if let Ok(Some(status)) = game.try_wait() {
        summary.process_exit = exit_category(&status);
        summary.teardown.child_exit = "exited_before_request";
        summary.teardown.input_release = "unavailable";
        summary.teardown.shutdown_acknowledgment = "unavailable";
        summary.teardown.socket_closure = "closed";
        return;
    }
    let config = AgentClientConfig {
        protocol_file: protocol_file.to_path_buf(),
        transcript_file: Some(transcript_file.to_path_buf()),
        timeout: timeout.min(Duration::from_secs(2)),
        ..Default::default()
    };
    if let Ok(mut client) = AgentClient::with_config(config) {
        summary.teardown.input_release = if client.release_all_inputs().is_ok() {
            "acknowledged"
        } else {
            "failed"
        };
        summary.teardown.shutdown_acknowledgment = if client.shutdown().is_ok() {
            "acknowledged"
        } else {
            "failed"
        };
        summary.teardown.socket_closure = if client.wait_for_disconnect().is_ok() {
            "closed"
        } else {
            "not_observed"
        };
    } else {
        summary.teardown.input_release = "unavailable";
        summary.teardown.shutdown_acknowledgment = "unavailable";
        summary.teardown.socket_closure = "not_observed";
    }
    match wait_child(game, timeout) {
        Ok(Some(status)) => {
            summary.process_exit = exit_category(&status);
            summary.teardown.child_exit = "exited";
        }
        _ => terminate_game(game, summary),
    }
}

pub(super) fn terminate_game(game: &mut Child, summary: &mut RunSummary) {
    if let Ok(Some(status)) = game.try_wait() {
        summary.process_exit = exit_category(&status);
        summary.teardown.child_exit = "exited";
        return;
    }
    summary.teardown.forced_termination = true;
    summary.teardown.child_exit = "forced";
    let _ = game.kill();
    if let Ok(status) = game.wait() {
        summary.process_exit = exit_category(&status);
    }
}

pub(super) fn exit_category(status: &ExitStatus) -> &'static str {
    if status.success() {
        "success"
    } else if status.code().is_some() {
        "nonzero"
    } else {
        "signal"
    }
}

pub(super) fn write_failure_artifacts(
    args: &RunArgs,
    game_log: &Path,
    driver_log: Option<&Path>,
    capture_dir: &Path,
    transcript_file: &Path,
    message: &str,
) {
    let _ = fail_run(
        &args.artifacts,
        game_log,
        driver_log,
        capture_dir,
        transcript_file,
        format!("{message}\n"),
    );
}

pub(super) fn wait_child(child: &mut Child, timeout: Duration) -> io::Result<Option<ExitStatus>> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(None)
}
