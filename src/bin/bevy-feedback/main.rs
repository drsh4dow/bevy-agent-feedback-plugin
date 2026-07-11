mod args;
mod doctor;
mod python_client;
mod runner;

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

use runner::run;
#[cfg(test)]
use runner::spawn_error;

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
