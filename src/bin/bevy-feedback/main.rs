mod args;
mod doctor;
mod python_client;

use args::{Command as CliCommand, RunArgs};
use bevy_agent_feedback_plugin::client::{AgentClient, AgentClientConfig};
use python_client::BundledPythonClient;
use serde_json::Value;
use std::{
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitCode, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const DIAGNOSTIC_BLOCK_MAX_BYTES: usize = 8 * 1024;
const TRANSCRIPT_READ_CHUNK_BYTES: usize = 8 * 1024;
const TRANSCRIPT_LINE_MAX_BYTES: usize = 64 * 1024;

fn main() -> ExitCode {
    match real_main() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("bevy-feedback: {error}");
            ExitCode::from(1)
        }
    }
}

fn real_main() -> Result<ExitCode, String> {
    let raw_args = std::env::args().skip(1).collect::<Vec<_>>();
    match args::parse_args(&raw_args)? {
        CliCommand::Help => {
            println!("{}", args::usage());
            Ok(ExitCode::SUCCESS)
        }
        CliCommand::Version => {
            println!("{}", args::version());
            Ok(ExitCode::SUCCESS)
        }
        CliCommand::Doctor(doctor_args) => doctor::run(&doctor_args),
        CliCommand::Run(run_args) => run(run_args).map(|()| ExitCode::SUCCESS),
    }
}

fn run(args: RunArgs) -> Result<(), String> {
    fs::create_dir_all(&args.artifacts).map_err(|error| error.to_string())?;
    let python = BundledPythonClient::materialize(&args.artifacts.join("python"))
        .map_err(|error| error.to_string())?;
    if let Some(parent) = args.protocol_file.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let _ = fs::remove_file(&args.protocol_file);

    let game_log = args.artifacts.join("game.log");
    let game_log_file = Arc::new(Mutex::new(
        File::create(&game_log).map_err(|error| error.to_string())?,
    ));
    let wrapper_capture_dir = args.artifacts.join("captures");
    fs::create_dir_all(&wrapper_capture_dir).map_err(|error| error.to_string())?;
    let transcript_file = args.artifacts.join("transcript.jsonl");
    File::create(&transcript_file).map_err(|error| error.to_string())?;
    let mut game = spawn_command(
        "--game",
        &args.game,
        &args,
        &python,
        &wrapper_capture_dir,
        &transcript_file,
        true,
    )?;
    stream_child_logs(&mut game, game_log_file);

    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_signal = stop.clone();
    ctrlc::set_handler(move || stop_for_signal.store(true, Ordering::Relaxed))
        .map_err(|error| error.to_string())?;

    let ready = match wait_ready(&args.protocol_file, args.ready_timeout, &mut game, &stop) {
        Ok(ready) => ready,
        Err(error) => {
            let _ = game.kill();
            let _ = game.wait();
            copy_if_exists(&args.protocol_file, &args.artifacts.join("protocol.json"));
            let _ = copy_captures(&wrapper_capture_dir, &args.artifacts.join("screenshots"));
            return fail_run(
                &args.artifacts,
                &game_log,
                None,
                &wrapper_capture_dir,
                &transcript_file,
                format!("{error}\n"),
            );
        }
    };
    println!(
        "bevy-feedback ready: session={} socket={} protocol={} (protocol ready != game ready; use semantic readiness for animated games or strict stability for static scenes before capturing)",
        ready.session_id,
        ready.socket_addr,
        args.protocol_file.display()
    );
    let live_capture_dir = ready
        .capture_dir
        .clone()
        .unwrap_or_else(|| wrapper_capture_dir.clone());

    let mut game_status = None;
    let mut game_exit_noted = false;
    let mut failure_summary = String::new();
    let mut driver_failed = false;
    let mut driver_log = None;
    if let Some(driver) = &args.driver {
        let path = args.artifacts.join("driver.log");
        let log_file = Arc::new(Mutex::new(
            File::create(&path).map_err(|error| error.to_string())?,
        ));
        let mut driver = spawn_command(
            "--driver",
            driver,
            &args,
            &python,
            &wrapper_capture_dir,
            &transcript_file,
            true,
        )?;
        stream_child_logs(&mut driver, log_file);
        match wait_child(&mut driver, args.driver_timeout).map_err(|error| error.to_string())? {
            Some(status) => {
                driver_failed = !status.success();
                if driver_failed {
                    failure_summary.push_str(&format!("driver exited with status {status}\n"));
                }
            }
            None => {
                driver_failed = true;
                failure_summary.push_str(&format!(
                    "driver timed out after {} ms\n",
                    args.driver_timeout.as_millis()
                ));
                let _ = driver.kill();
                let _ = driver.wait();
            }
        }
        if let Ok(Some(status)) = game.try_wait() {
            game_exit_noted = true;
            game_status = Some(status);
            failure_summary.push_str(&format!(
                "game exited during the run with status {status} (before shutdown was requested); see game.log tail below\n"
            ));
        }
        driver_log = Some(path);
    } else {
        game_status = wait_game_or_signal(&mut game, &stop).map_err(|error| error.to_string())?;
    }

    copy_if_exists(&args.protocol_file, &args.artifacts.join("protocol.json"));
    shutdown_game(&args.protocol_file, &transcript_file);
    if game_status.is_none() {
        game_status =
            wait_child(&mut game, args.shutdown_timeout).map_err(|error| error.to_string())?;
    }
    if game_status.is_none() {
        let _ = game.kill();
        game_status = game.wait().ok();
    }
    let screenshot_dir = args.artifacts.join("screenshots");
    let screenshots =
        copy_captures(&live_capture_dir, &screenshot_dir).map_err(|error| error.to_string())?;

    let game_failed = game_status.as_ref().is_some_and(|status| !status.success());
    if game_failed
        && !game_exit_noted
        && let Some(status) = &game_status
    {
        failure_summary.push_str(&format!("game exited with status {status}\n"));
    }
    if driver_failed || game_failed {
        return fail_run(
            &args.artifacts,
            &game_log,
            driver_log.as_deref(),
            &live_capture_dir,
            &transcript_file,
            failure_summary,
        );
    }
    println!("bevy-feedback ok: artifacts={}", args.artifacts.display());
    print_screenshots(&screenshots);
    Ok(())
}

fn spawn_command(
    label: &str,
    command: &[String],
    args: &RunArgs,
    python: &BundledPythonClient,
    capture_dir: &Path,
    transcript_file: &Path,
    piped_logs: bool,
) -> Result<Child, String> {
    let mut child = Command::new(&command[0]);
    child
        .args(&command[1..])
        .env("BEVY_FEEDBACK_PROTOCOL", &args.protocol_file)
        .env("BEVY_FEEDBACK_CAPTURE_DIR", capture_dir)
        .env("BEVY_FEEDBACK_ARTIFACTS", &args.artifacts)
        .env("BEVY_FEEDBACK_TRANSCRIPT", transcript_file)
        .env("PYTHONPATH", python.python_path());
    if piped_logs {
        child.stdout(Stdio::piped()).stderr(Stdio::piped());
    }
    child
        .spawn()
        .map_err(|error| spawn_error(label, command, error))
}

fn spawn_error(label: &str, command: &[String], error: io::Error) -> String {
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

fn stream_child_logs(child: &mut Child, log_file: Arc<Mutex<File>>) {
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
struct ReadyInfo {
    session_id: String,
    socket_addr: String,
    capture_dir: Option<PathBuf>,
}

fn wait_ready(
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
    let _ = game.kill();
    Err(format!(
        "protocol was not ready within {} ms; increase with --ready-timeout MS or BEVY_FEEDBACK_READY_TIMEOUT_MS if the game is still compiling",
        timeout.as_millis()
    ))
}

fn wait_game_or_signal(game: &mut Child, stop: &AtomicBool) -> io::Result<Option<ExitStatus>> {
    while !stop.load(Ordering::Relaxed) {
        if let Some(status) = game.try_wait()? {
            return Ok(Some(status));
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(None)
}

fn shutdown_game(protocol_file: &Path, transcript_file: &Path) {
    let config = AgentClientConfig {
        protocol_file: protocol_file.to_path_buf(),
        transcript_file: Some(transcript_file.to_path_buf()),
        timeout: Duration::from_secs(2),
        ..Default::default()
    };
    if let Ok(mut client) = AgentClient::with_config(config) {
        let _ = client.release_all_inputs();
        let _ = client.shutdown();
    }
}

fn wait_child(child: &mut Child, timeout: Duration) -> io::Result<Option<ExitStatus>> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(None)
}

fn fail_run(
    artifacts: &Path,
    game_log: &Path,
    driver_log: Option<&Path>,
    capture_dir: &Path,
    transcript_file: &Path,
    mut failure_summary: String,
) -> Result<(), String> {
    append_failure_context(
        &mut failure_summary,
        game_log,
        driver_log,
        capture_dir,
        transcript_file,
    );
    fs::write(artifacts.join("failure-summary.txt"), &failure_summary)
        .map_err(|error| error.to_string())?;
    Err(format!(
        "run failed; artifacts: {}\n{}",
        artifacts.display(),
        failure_summary.trim_end()
    ))
}

fn append_failure_context(
    failure_summary: &mut String,
    game_log: &Path,
    driver_log: Option<&Path>,
    capture_dir: &Path,
    transcript_file: &Path,
) {
    failure_summary.push_str(&format!("game log: {}\n", game_log.display()));
    if let Some(driver_log) = driver_log {
        failure_summary.push_str(&format!("driver log: {}\n", driver_log.display()));
    }
    if let Some(capture) = newest_png(capture_dir) {
        failure_summary.push_str(&format!("newest capture: {}\n", capture.display()));
    }
    if let Ok(lines) = tail_lines(game_log, 20)
        && !lines.is_empty()
    {
        failure_summary.push_str("last 20 game.log lines:\n");
        failure_summary.push_str(&lines);
        failure_summary.push('\n');
    }
    if let Some(driver_log) = driver_log
        && let Ok(lines) = tail_lines(driver_log, 20)
        && !lines.is_empty()
    {
        failure_summary.push_str("last 20 driver.log lines:\n");
        failure_summary.push_str(&lines);
        failure_summary.push('\n');
    }
    append_transcript_context(failure_summary, transcript_file);
}

fn append_transcript_context(failure_summary: &mut String, transcript_file: &Path) {
    let Ok(Some(context)) = transcript_diagnostic_context(transcript_file) else {
        failure_summary.push_str("diagnostic context unavailable\n");
        return;
    };
    let Ok(json) = serde_json::to_string(&context) else {
        failure_summary.push_str("diagnostic context unavailable\n");
        return;
    };
    const HEADER: &str = "diagnostic context:\n";
    let payload_limit = DIAGNOSTIC_BLOCK_MAX_BYTES.saturating_sub(HEADER.len() + 1);
    let rendered = if json.len() <= payload_limit {
        json
    } else {
        let empty_wrapper = serde_json::json!({"truncated": true, "context_json_prefix": ""});
        let Ok(empty_wrapper_json) = serde_json::to_string(&empty_wrapper) else {
            failure_summary.push_str("diagnostic context unavailable\n");
            return;
        };
        let prefix_limit = payload_limit.saturating_sub(empty_wrapper_json.len()) / "\\u0000".len();
        let prefix_boundary = json.floor_char_boundary(json.len().min(prefix_limit));
        let wrapper = serde_json::json!({
            "truncated": true,
            "context_json_prefix": &json[..prefix_boundary],
        });
        let Ok(wrapper_json) = serde_json::to_string(&wrapper) else {
            failure_summary.push_str("diagnostic context unavailable\n");
            return;
        };
        wrapper_json
    };
    failure_summary.push_str(HEADER);
    failure_summary.push_str(&rendered);
    failure_summary.push('\n');
}

fn transcript_diagnostic_context(path: &Path) -> io::Result<Option<Value>> {
    let mut file = File::open(path)?;
    let mut chunk = [0_u8; TRANSCRIPT_READ_CHUNK_BYTES];
    let mut line = Vec::with_capacity(TRANSCRIPT_READ_CHUNK_BYTES);
    let mut discarding_oversized_line = false;
    let mut last_error_context = None;
    let mut last_diagnostic_details = None;

    loop {
        let count = file.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        for &byte in &chunk[..count] {
            if byte == b'\n' {
                if !discarding_oversized_line {
                    remember_transcript_context(
                        &line,
                        &mut last_error_context,
                        &mut last_diagnostic_details,
                    );
                }
                line.clear();
                discarding_oversized_line = false;
            } else if !discarding_oversized_line {
                if line.len() < TRANSCRIPT_LINE_MAX_BYTES {
                    line.push(byte);
                } else {
                    line.clear();
                    discarding_oversized_line = true;
                }
            }
        }
    }

    Ok(last_error_context.or(last_diagnostic_details))
}

fn remember_transcript_context(
    line: &[u8],
    last_error_context: &mut Option<Value>,
    last_diagnostic_details: &mut Option<Value>,
) {
    let Ok(envelope) = serde_json::from_slice::<Value>(line) else {
        return;
    };
    let Some(response) = envelope.get("response") else {
        return;
    };
    if let Some(context) = response.pointer("/error/context")
        && !context.is_null()
    {
        *last_error_context = Some(context.clone());
        return;
    }
    let Some(status) = response.pointer("/result/status").and_then(Value::as_str) else {
        return;
    };
    let diagnostic_status = matches!(
        status,
        "target_info"
            | "clicked_target"
            | "resource_info"
            | "predicate_evaluated"
            | "predicate_matched"
    );
    let diagnostic_request = status == "ok"
        && envelope
            .pointer("/request/command")
            .and_then(Value::as_str)
            .is_some_and(|command| {
                matches!(
                    command,
                    "ecs_summary" | "list_entities" | "camera_info" | "state_info" | "marker_info"
                )
            });
    if (diagnostic_status || diagnostic_request)
        && let Some(details) = response.pointer("/result/details")
        && !details.is_null()
    {
        *last_diagnostic_details = Some(details.clone());
    }
}

fn tail_lines(path: &Path, max_lines: usize) -> io::Result<String> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(64 * 1024)))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.lines().rev().take(max_lines).collect::<Vec<_>>();
    lines.reverse();
    Ok(lines.join("\n"))
}

fn newest_png(dir: &Path) -> Option<PathBuf> {
    let mut newest = None;
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("png") {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        match &newest {
            Some((newest_modified, _)) if modified <= *newest_modified => {}
            _ => newest = Some((modified, path)),
        }
    }
    newest.map(|(_, path)| path)
}

fn copy_if_exists(from: &Path, to: &Path) {
    if from.exists() {
        let _ = fs::copy(from, to);
    }
}

fn copy_captures(from: &Path, to: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(from) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut pngs = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("png") {
            pngs.push(path);
        }
    }
    pngs.sort();
    if !pngs.is_empty() {
        fs::create_dir_all(to)?;
    }
    let mut copied = Vec::with_capacity(pngs.len());
    for path in pngs {
        if let Some(file_name) = path.file_name() {
            let target = to.join(file_name);
            fs::copy(&path, &target)?;
            copied.push(target);
        }
    }
    Ok(copied)
}

fn print_screenshots(paths: &[PathBuf]) {
    if paths.is_empty() {
        println!("screenshots: none captured");
        return;
    }
    println!("screenshots ({}):", paths.len());
    for path in paths.iter().take(10) {
        println!("  {}", path.display());
    }
    if paths.len() > 10 {
        println!("  +{} more", paths.len() - 10);
    }
}

#[cfg(test)]
mod tests;
