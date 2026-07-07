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
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn main() -> ExitCode {
    match real_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bevy-feedback: {error}");
            ExitCode::from(1)
        }
    }
}

fn real_main() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let args = parse_args(&args)?;
    fs::create_dir_all(&args.artifacts).map_err(|error| error.to_string())?;
    if let Some(parent) = args.protocol_file.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let _ = fs::remove_file(&args.protocol_file);

    let game_log = args.artifacts.join("game.log");
    let log_file = Arc::new(Mutex::new(
        File::create(&game_log).map_err(|error| error.to_string())?,
    ));
    let capture_dir = args.artifacts.join("captures");
    let transcript_file = args.artifacts.join("transcript.jsonl");
    let mut game = spawn_command(&args.game, &args, &capture_dir, &transcript_file, true)?;
    stream_child_logs(&mut game, log_file);

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
            copy_captures(&capture_dir, &args.artifacts.join("screenshots"));
            return fail_run(
                &args.artifacts,
                &game_log,
                &capture_dir,
                format!("{error}\n"),
            );
        }
    };
    println!(
        "bevy-feedback ready: session={} socket={} protocol={}",
        ready.session_id,
        ready.socket_addr,
        args.protocol_file.display()
    );

    let mut game_status = None;
    let mut failure_summary = String::new();
    let mut driver_failed = false;
    if let Some(driver) = &args.driver {
        let mut driver = spawn_command(driver, &args, &capture_dir, &transcript_file, false)?;
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
    copy_captures(&capture_dir, &args.artifacts.join("screenshots"));

    let game_failed = game_status.as_ref().is_some_and(|status| !status.success());
    if game_failed && let Some(status) = &game_status {
        failure_summary.push_str(&format!("game exited with status {status}\n"));
    }
    if driver_failed || game_failed {
        return fail_run(&args.artifacts, &game_log, &capture_dir, failure_summary);
    }
    println!("bevy-feedback artifacts: {}", args.artifacts.display());
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct RunArgs {
    protocol_file: PathBuf,
    artifacts: PathBuf,
    ready_timeout: Duration,
    shutdown_timeout: Duration,
    driver_timeout: Duration,
    game: Vec<String>,
    driver: Option<Vec<String>>,
}

fn parse_args(args: &[String]) -> Result<RunArgs, String> {
    if args.first().map(String::as_str) != Some("run") {
        return Err(usage());
    }
    let mut protocol_file = std::env::var_os("BEVY_FEEDBACK_PROTOCOL")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/agent-feedback/agent-feedback.json"));
    let mut artifacts = std::env::var_os("BEVY_FEEDBACK_ARTIFACTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(format!("target/agent-feedback/artifacts/run-{}", unix_ms()))
        });
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--protocol" => {
                index += 1;
                protocol_file = args.get(index).map(PathBuf::from).ok_or_else(usage)?;
                index += 1;
            }
            "--artifacts" => {
                index += 1;
                artifacts = args.get(index).map(PathBuf::from).ok_or_else(usage)?;
                index += 1;
            }
            "--game" => return parse_game_driver(&args[index + 1..], protocol_file, artifacts),
            "--" => {
                let game = args[index + 1..].to_vec();
                if game.is_empty() {
                    return Err(usage());
                }
                return Ok(RunArgs {
                    protocol_file,
                    artifacts,
                    ready_timeout: Duration::from_secs(60),
                    shutdown_timeout: Duration::from_secs(5),
                    driver_timeout: Duration::from_secs(300),
                    game,
                    driver: None,
                });
            }
            _ => return Err(usage()),
        }
    }
    Err(usage())
}

fn parse_game_driver(
    args: &[String],
    protocol_file: PathBuf,
    artifacts: PathBuf,
) -> Result<RunArgs, String> {
    let driver_index = args.iter().position(|arg| arg == "--driver");
    let (game, driver) = match driver_index {
        Some(index) => (&args[..index], Some(args[index + 1..].to_vec())),
        None => (args, None),
    };
    if game.is_empty() || driver.as_ref().is_some_and(Vec::is_empty) {
        return Err(usage());
    }
    Ok(RunArgs {
        protocol_file,
        artifacts,
        ready_timeout: Duration::from_secs(60),
        shutdown_timeout: Duration::from_secs(5),
        driver_timeout: Duration::from_secs(300),
        game: game.to_vec(),
        driver,
    })
}

fn usage() -> String {
    "usage:
  bevy-feedback run [--protocol FILE] [--artifacts DIR] -- <game command...>
  bevy-feedback run [--protocol FILE] [--artifacts DIR] --game <game...> --driver <driver...>

env:
  BEVY_FEEDBACK_PROTOCOL    protocol file (default target/agent-feedback/agent-feedback.json)
  BEVY_FEEDBACK_ARTIFACTS   artifact dir (default target/agent-feedback/artifacts/run-<unix-ms>)
  BEVY_FEEDBACK_CAPTURE_DIR exported to game/driver as the capture dir
  BEVY_FEEDBACK_TRANSCRIPT  exported to game/driver as transcript.jsonl

timeouts:
  readiness 60s, driver 300s, shutdown 5s"
        .to_string()
}

fn spawn_command(
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
        .map_err(|error| format!("spawn {:?}: {error}", command))
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
            });
        }
        thread::sleep(Duration::from_millis(100));
    }
    let _ = game.kill();
    Err(format!(
        "protocol was not ready within {} ms",
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
    capture_dir: &Path,
    mut failure_summary: String,
) -> Result<(), String> {
    append_failure_context(&mut failure_summary, game_log, capture_dir);
    fs::write(artifacts.join("failure-summary.txt"), &failure_summary)
        .map_err(|error| error.to_string())?;
    Err(format!(
        "run failed; artifacts: {}\n{}",
        artifacts.display(),
        failure_summary.trim_end()
    ))
}

fn append_failure_context(failure_summary: &mut String, game_log: &Path, capture_dir: &Path) {
    failure_summary.push_str(&format!("game log: {}\n", game_log.display()));
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
            .unwrap_or(UNIX_EPOCH);
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

fn copy_captures(from: &Path, to: &Path) {
    let Ok(entries) = fs::read_dir(from) else {
        return;
    };
    let _ = fs::create_dir_all(to);
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("png")
            && let Some(file_name) = path.file_name()
        {
            let _ = fs::copy(&path, to.join(file_name));
        }
    }
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

    #[test]
    fn parses_game_only_command() {
        let args = parse_args(&[
            "run".into(),
            "--".into(),
            "cargo".into(),
            "run".into(),
            "--example".into(),
            "minimal".into(),
        ])
        .expect("args");

        assert_eq!(args.game, ["cargo", "run", "--example", "minimal"]);
        assert_eq!(args.driver, None);
    }

    #[test]
    fn parses_game_and_driver_command() {
        let args = parse_args(&[
            "run".into(),
            "--protocol".into(),
            "target/agent.json".into(),
            "--game".into(),
            "cargo".into(),
            "run".into(),
            "--driver".into(),
            "python3".into(),
            "drive.py".into(),
        ])
        .expect("args");

        assert_eq!(args.protocol_file, PathBuf::from("target/agent.json"));
        assert_eq!(args.game, ["cargo", "run"]);
        assert_eq!(args.driver, Some(vec!["python3".into(), "drive.py".into()]));
    }
}
