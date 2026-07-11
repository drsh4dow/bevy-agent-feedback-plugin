use super::*;

#[derive(serde::Serialize)]
struct RunSummary {
    schema_version: u8,
    result: RunResult,
    phase: &'static str,
    elapsed_ms: u128,
    timings_ms: std::collections::BTreeMap<&'static str, u128>,
    launch: LaunchSummary,
    artifacts: ArtifactSummary,
    process_exit: &'static str,
    teardown: TeardownSummary,
    warnings: Vec<String>,
}

#[derive(serde::Serialize)]
struct RunResult {
    success: bool,
    code: &'static str,
    message: String,
}

#[derive(serde::Serialize)]
struct LaunchSummary {
    prepare_command: Option<Vec<String>>,
    game_command: Vec<String>,
    driver_command: Option<Vec<String>>,
    caller_cwd: PathBuf,
    game_cwd: PathBuf,
}

#[derive(serde::Serialize)]
struct ArtifactSummary {
    directory: PathBuf,
    game_log: PathBuf,
    prepare_log: Option<PathBuf>,
    driver_log: Option<PathBuf>,
    protocol: PathBuf,
    transcript: PathBuf,
    screenshots: PathBuf,
}

#[derive(serde::Serialize)]
struct TeardownSummary {
    input_release: &'static str,
    shutdown_acknowledgment: &'static str,
    socket_closure: &'static str,
    child_exit: &'static str,
    forced_termination: bool,
}

impl Default for TeardownSummary {
    fn default() -> Self {
        Self {
            input_release: "not_attempted",
            shutdown_acknowledgment: "not_attempted",
            socket_closure: "not_attempted",
            child_exit: "not_attempted",
            forced_termination: false,
        }
    }
}

struct RunFailure {
    phase: &'static str,
    code: &'static str,
    message: String,
}

pub(super) fn run(args: RunArgs) -> Result<(), String> {
    let started = Instant::now();
    let caller_cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let game_cwd = args.game_cwd.clone().unwrap_or_else(|| caller_cwd.clone());
    fs::create_dir_all(&args.artifacts).map_err(|error| error.to_string())?;
    let game_log = args.artifacts.join("game.log");
    let prepare_log = args
        .prepare
        .as_ref()
        .map(|_| args.artifacts.join("prepare.log"));
    let driver_log_path = args
        .driver
        .as_ref()
        .map(|_| args.artifacts.join("driver.log"));
    let transcript_file = args.artifacts.join("transcript.jsonl");
    let protocol_artifact = args.artifacts.join("protocol.json");
    let screenshot_dir = args.artifacts.join("screenshots");
    let wrapper_capture_dir = args.artifacts.join("captures");
    let summary_path = args.artifacts.join("run-summary.json");
    let mut summary = RunSummary {
        schema_version: 1,
        result: RunResult {
            success: false,
            code: "runner_internal",
            message: String::new(),
        },
        phase: "setup",
        elapsed_ms: 0,
        timings_ms: std::collections::BTreeMap::new(),
        launch: LaunchSummary {
            prepare_command: args.prepare.clone(),
            game_command: args.game.clone(),
            driver_command: args.driver.clone(),
            caller_cwd,
            game_cwd: game_cwd.clone(),
        },
        artifacts: ArtifactSummary {
            directory: args.artifacts.clone(),
            game_log: game_log.clone(),
            prepare_log: prepare_log.clone(),
            driver_log: driver_log_path.clone(),
            protocol: protocol_artifact.clone(),
            transcript: transcript_file.clone(),
            screenshots: screenshot_dir.clone(),
        },
        process_exit: "not_started",
        teardown: TeardownSummary::default(),
        warnings: Vec::new(),
    };

    if args.used_legacy_ready_timeout {
        let warning = "--ready-timeout/BEVY_FEEDBACK_READY_TIMEOUT_MS is deprecated; use --protocol-timeout/BEVY_FEEDBACK_PROTOCOL_TIMEOUT_MS";
        eprintln!("bevy-feedback: warning: {warning}");
        summary.warnings.push(warning.to_string());
    }
    println!(
        "bevy-feedback launch: phase=setup game_command={:?} game_cwd={}",
        args.game,
        game_cwd.display()
    );

    let result = run_lifecycle(
        &args,
        &game_cwd,
        &game_log,
        prepare_log.as_deref(),
        driver_log_path.as_deref(),
        &transcript_file,
        &wrapper_capture_dir,
        &protocol_artifact,
        &screenshot_dir,
        &mut summary,
    );
    summary.elapsed_ms = started.elapsed().as_millis();
    match result {
        Ok(()) => {
            summary.result = RunResult {
                success: true,
                code: "ok",
                message: "run completed gracefully".to_string(),
            };
            summary.phase = "complete";
        }
        Err(failure) => {
            summary.result = RunResult {
                success: false,
                code: failure.code,
                message: failure.message.clone(),
            };
            summary.phase = failure.phase;
        }
    }
    let summary_json = serde_json::to_vec_pretty(&summary).map_err(|error| error.to_string())?;
    fs::write(&summary_path, summary_json).map_err(|error| error.to_string())?;

    if summary.result.success {
        println!(
            "bevy-feedback ok: phase=complete elapsed_ms={} artifacts={}",
            summary.elapsed_ms,
            args.artifacts.display()
        );
        println!(
            "teardown: inputs={} shutdown={} socket={} child={}",
            summary.teardown.input_release,
            summary.teardown.shutdown_acknowledgment,
            summary.teardown.socket_closure,
            summary.teardown.child_exit
        );
        Ok(())
    } else {
        Err(format!(
            "{} [{}] (phase={}); summary={}",
            summary.result.message,
            summary.result.code,
            summary.phase,
            summary_path.display()
        ))
    }
}

#[allow(clippy::too_many_arguments)]
fn run_lifecycle(
    args: &RunArgs,
    game_cwd: &Path,
    game_log: &Path,
    prepare_log: Option<&Path>,
    driver_log_path: Option<&Path>,
    transcript_file: &Path,
    wrapper_capture_dir: &Path,
    protocol_artifact: &Path,
    screenshot_dir: &Path,
    summary: &mut RunSummary,
) -> Result<(), RunFailure> {
    let phase_started = Instant::now();
    if let (Some(command), Some(log_path)) = (&args.prepare, prepare_log) {
        println!(
            "bevy-feedback phase=prepare command={command:?} cwd={}",
            summary.launch.caller_cwd.display()
        );
        let mut child =
            spawn_logged_command("--prepare", command, None, log_path).map_err(|message| {
                RunFailure {
                    phase: "prepare",
                    code: "prepare_spawn_failed",
                    message,
                }
            })?;
        match wait_child(&mut child, args.prepare_timeout).map_err(|error| RunFailure {
            phase: "prepare",
            code: "prepare_wait_failed",
            message: error.to_string(),
        })? {
            Some(status) if status.success() => {}
            Some(status) => {
                return Err(RunFailure {
                    phase: "prepare",
                    code: "prepare_nonzero_exit",
                    message: format!("prepare exited with status {status}"),
                });
            }
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunFailure {
                    phase: "prepare",
                    code: "prepare_timeout",
                    message: format!(
                        "prepare timed out after {} ms",
                        args.prepare_timeout.as_millis()
                    ),
                });
            }
        }
    }
    summary
        .timings_ms
        .insert("prepare", phase_started.elapsed().as_millis());

    let setup_started = Instant::now();
    let python =
        BundledPythonClient::materialize(&args.artifacts.join("python")).map_err(|error| {
            RunFailure {
                phase: "setup",
                code: "artifact_setup_failed",
                message: error.to_string(),
            }
        })?;
    if let Some(parent) = args
        .protocol_file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| RunFailure {
            phase: "setup",
            code: "artifact_setup_failed",
            message: error.to_string(),
        })?;
    }
    let _ = fs::remove_file(&args.protocol_file);
    fs::create_dir_all(wrapper_capture_dir).map_err(|error| RunFailure {
        phase: "setup",
        code: "artifact_setup_failed",
        message: error.to_string(),
    })?;
    File::create(transcript_file).map_err(|error| RunFailure {
        phase: "setup",
        code: "artifact_setup_failed",
        message: error.to_string(),
    })?;
    let game_log_file = Arc::new(Mutex::new(File::create(game_log).map_err(|error| {
        RunFailure {
            phase: "setup",
            code: "artifact_setup_failed",
            message: error.to_string(),
        }
    })?));
    summary
        .timings_ms
        .insert("setup", setup_started.elapsed().as_millis());

    let spawn_started = Instant::now();
    println!(
        "bevy-feedback phase=game_spawn command={:?} cwd={}",
        args.game,
        game_cwd.display()
    );
    let mut game = spawn_command(
        "--game",
        &args.game,
        args,
        &python,
        wrapper_capture_dir,
        transcript_file,
        Some(game_cwd),
    )
    .map_err(|message| RunFailure {
        phase: "game_spawn",
        code: "game_spawn_failed",
        message,
    })?;
    stream_child_logs(&mut game, game_log_file);
    summary.process_exit = "running";
    summary
        .timings_ms
        .insert("game_spawn", spawn_started.elapsed().as_millis());

    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_signal = stop.clone();
    let _ = ctrlc::set_handler(move || stop_for_signal.store(true, Ordering::Relaxed));

    let protocol_started = Instant::now();
    println!(
        "bevy-feedback phase=protocol_startup timeout_ms={}",
        args.protocol_timeout.as_millis()
    );
    let ready = match wait_ready(&args.protocol_file, args.protocol_timeout, &mut game, &stop) {
        Ok(ready) => ready,
        Err(error) => {
            summary
                .timings_ms
                .insert("protocol_startup", protocol_started.elapsed().as_millis());
            summary.process_exit = game
                .try_wait()
                .ok()
                .flatten()
                .as_ref()
                .map(exit_category)
                .unwrap_or("running");
            terminate_game(&mut game, summary);
            copy_if_exists(&args.protocol_file, protocol_artifact);
            let _ = copy_captures(wrapper_capture_dir, screenshot_dir);
            write_failure_artifacts(
                args,
                game_log,
                None,
                wrapper_capture_dir,
                transcript_file,
                &error,
            );
            let code = if error.starts_with("game exited") {
                "protocol_early_exit"
            } else {
                "protocol_timeout"
            };
            return Err(RunFailure {
                phase: "protocol_startup",
                code,
                message: error,
            });
        }
    };
    summary
        .timings_ms
        .insert("protocol_startup", protocol_started.elapsed().as_millis());
    println!(
        "bevy-feedback ready: phase=driver session={} socket={} protocol={} elapsed_ms={} (protocol ready != game ready; use semantic readiness for animated games or strict stability for static scenes before capturing)",
        ready.session_id,
        ready.socket_addr,
        args.protocol_file.display(),
        protocol_started.elapsed().as_millis()
    );
    let live_capture_dir = ready
        .capture_dir
        .clone()
        .unwrap_or_else(|| wrapper_capture_dir.to_path_buf());

    let driver_started = Instant::now();
    let mut primary_failure = None;
    if let (Some(driver_command), Some(log_path)) = (&args.driver, driver_log_path) {
        primary_failure = run_driver(
            driver_command,
            log_path,
            args,
            &python,
            wrapper_capture_dir,
            transcript_file,
        )
        .err();
    } else {
        match wait_game_or_signal(&mut game, &stop).map_err(|error| RunFailure {
            phase: "game",
            code: "game_wait_failed",
            message: error.to_string(),
        })? {
            Some(status) if status.success() => summary.process_exit = exit_category(&status),
            Some(status) => {
                primary_failure = Some(RunFailure {
                    phase: "game",
                    code: "game_nonzero_exit",
                    message: format!("game exited with status {status}"),
                })
            }
            None => {}
        }
    }
    summary
        .timings_ms
        .insert("driver", driver_started.elapsed().as_millis());

    let teardown_started = Instant::now();
    copy_if_exists(&args.protocol_file, protocol_artifact);
    teardown_game(
        &args.protocol_file,
        transcript_file,
        &mut game,
        args.shutdown_timeout,
        summary,
    );
    summary
        .timings_ms
        .insert("teardown", teardown_started.elapsed().as_millis());
    let screenshots =
        copy_captures(&live_capture_dir, screenshot_dir).map_err(|error| RunFailure {
            phase: "artifacts",
            code: "artifact_copy_failed",
            message: error.to_string(),
        })?;
    print_screenshots(&screenshots);

    if let Some(failure) = primary_failure {
        write_failure_artifacts(
            args,
            game_log,
            driver_log_path,
            &live_capture_dir,
            transcript_file,
            &failure.message,
        );
        return Err(failure);
    }
    if summary.teardown.forced_termination {
        let message = "game required forced termination during teardown".to_string();
        write_failure_artifacts(
            args,
            game_log,
            driver_log_path,
            &live_capture_dir,
            transcript_file,
            &message,
        );
        return Err(RunFailure {
            phase: "teardown",
            code: "teardown_forced_termination",
            message,
        });
    }
    if matches!(summary.process_exit, "nonzero" | "signal") {
        let message = format!("game process exited with category {}", summary.process_exit);
        write_failure_artifacts(
            args,
            game_log,
            driver_log_path,
            &live_capture_dir,
            transcript_file,
            &message,
        );
        return Err(RunFailure {
            phase: "teardown",
            code: "game_nonzero_exit",
            message,
        });
    }
    if summary.teardown.shutdown_acknowledgment != "acknowledged" {
        summary
            .warnings
            .push("game exited cleanly before shutdown acknowledgment".to_string());
    }
    Ok(())
}

fn run_driver(
    command: &[String],
    log_path: &Path,
    args: &RunArgs,
    python: &BundledPythonClient,
    capture_dir: &Path,
    transcript_file: &Path,
) -> Result<(), RunFailure> {
    let log_file = Arc::new(Mutex::new(File::create(log_path).map_err(|error| {
        RunFailure {
            phase: "driver",
            code: "driver_log_failed",
            message: error.to_string(),
        }
    })?));
    let mut driver = spawn_command(
        "--driver",
        command,
        args,
        python,
        capture_dir,
        transcript_file,
        None,
    )
    .map_err(|message| RunFailure {
        phase: "driver",
        code: "driver_spawn_failed",
        message,
    })?;
    stream_child_logs(&mut driver, log_file);
    match wait_child(&mut driver, args.driver_timeout).map_err(|error| RunFailure {
        phase: "driver",
        code: "driver_wait_failed",
        message: error.to_string(),
    })? {
        Some(status) if status.success() => Ok(()),
        Some(status) => Err(RunFailure {
            phase: "driver",
            code: "driver_nonzero_exit",
            message: format!("driver exited with status {status}"),
        }),
        None => {
            let _ = driver.kill();
            let _ = driver.wait();
            Err(RunFailure {
                phase: "driver",
                code: "driver_timeout",
                message: format!(
                    "driver timed out after {} ms",
                    args.driver_timeout.as_millis()
                ),
            })
        }
    }
}

mod process;
#[cfg(test)]
pub(crate) use process::spawn_error;
use process::*;
