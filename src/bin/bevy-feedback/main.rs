mod args;
mod doctor;

use args::{Command as CliCommand, RunArgs};
use bevy_agent_feedback_plugin::client::{AgentClient, AgentClientConfig};
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
                format!("{error}\n"),
            );
        }
    };
    println!(
        "bevy-feedback ready: session={} socket={} protocol={} (protocol ready != game ready; wait for a stable frame before capturing)",
        ready.session_id,
        ready.socket_addr,
        args.protocol_file.display()
    );
    let live_capture_dir = ready
        .capture_dir
        .clone()
        .unwrap_or_else(|| wrapper_capture_dir.clone());

    let mut game_status = None;
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
    if game_failed && let Some(status) = &game_status {
        failure_summary.push_str(&format!("game exited with status {status}\n"));
    }
    if driver_failed || game_failed {
        return fail_run(
            &args.artifacts,
            &game_log,
            driver_log.as_deref(),
            &live_capture_dir,
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
        .env("BEVY_FEEDBACK_TRANSCRIPT", transcript_file);
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
    mut failure_summary: String,
) -> Result<(), String> {
    append_failure_context(&mut failure_summary, game_log, driver_log, capture_dir);
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
mod tests {
    use super::*;

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
        let root = std::env::temp_dir().join(format!(
            "bevy-feedback-copy-captures-{}",
            std::process::id()
        ));
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
}
