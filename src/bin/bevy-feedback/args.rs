use std::{
    ffi::OsString,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct RequiredWindowSize {
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RunArgs {
    pub(crate) protocol_file: PathBuf,
    pub(crate) artifacts: PathBuf,
    pub(crate) required_window_size: Option<RequiredWindowSize>,
    pub(crate) prepare_timeout: Duration,
    pub(crate) protocol_timeout: Duration,
    pub(crate) shutdown_timeout: Duration,
    pub(crate) driver_timeout: Duration,
    pub(crate) game_cwd: Option<PathBuf>,
    pub(crate) prepare: Option<Vec<String>>,
    pub(crate) game: Vec<String>,
    pub(crate) driver: Option<Vec<String>>,
    pub(crate) used_legacy_ready_timeout: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DoctorArgs {
    pub(crate) protocol_file: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Help,
    Version,
    Run(RunArgs),
    Doctor(DoctorArgs),
}

pub(crate) fn parse_args(args: &[String]) -> Result<Command, String> {
    parse_args_with_env(args, |name| std::env::var_os(name))
}

pub(crate) fn version() -> String {
    format!("bevy-feedback {}", env!("CARGO_PKG_VERSION"))
}

pub(crate) fn usage() -> String {
    "usage:
  bevy-feedback --help
  bevy-feedback --version
  bevy-feedback doctor [--protocol FILE]
  bevy-feedback run [options] -- <game command...>
  bevy-feedback run [options] [--prepare <prepare...>] --game <game...> [--driver <driver...>]

options:
  --prepare-timeout MS   prepare timeout (default 300000)
  --protocol-timeout MS  protocol startup timeout after game spawn (default 60000)
  --game-cwd DIR         working directory for the game only
  --require-window-size WIDTHxHEIGHT
                         fail unless the actual logical window has this size
  --ready-timeout MS     deprecated alias for --protocol-timeout

env:
  BEVY_FEEDBACK_PROTOCOL             protocol file (default target/agent-feedback/agent-feedback.json)
  BEVY_FEEDBACK_ARTIFACTS            artifact dir (default target/agent-feedback/artifacts/run-<unix-ms>)
  BEVY_FEEDBACK_PREPARE_TIMEOUT_MS   prepare timeout in milliseconds (default 300000)
  BEVY_FEEDBACK_PROTOCOL_TIMEOUT_MS  protocol timeout in milliseconds (default 60000)
  BEVY_FEEDBACK_READY_TIMEOUT_MS     deprecated protocol-timeout fallback
  BEVY_FEEDBACK_DRIVER_TIMEOUT_MS    driver timeout in milliseconds (default 300000)
  BEVY_FEEDBACK_SHUTDOWN_TIMEOUT_MS  shutdown timeout in milliseconds (default 5000)
  BEVY_FEEDBACK_REQUIRED_WINDOW_SIZE required logical window size (for example 1280x720)
  BEVY_FEEDBACK_CAPTURE_DIR         exported to game/driver as the capture dir
  BEVY_FEEDBACK_TRANSCRIPT          exported to game/driver as transcript.jsonl

timeouts:
  prepare 300s, protocol 60s, driver 300s, shutdown 5s

artifacts:
  successful runs print artifacts=<dir> and copy captured PNGs to <dir>/screenshots/
  live captures remain in the protocol file's capture_dir during the run

examples:
  bevy-feedback --version
  bevy-feedback doctor
  bevy-feedback run -- cargo run --example minimal
  bevy-feedback run --prepare cargo build --features agent --game cargo run --features agent --driver python3 tests/drive_camera.py

note:
  Protocol-ready is not game-ready; use skills/driving-bevy-games/SKILL.md to choose readiness and time control.
  Frame waits count app updates, not gameplay time. For animated games use predicates; use deterministic advance for gameplay time.
  Capture completion proves screenshot readback, not OS compositor presentation.
  Do not quote the whole game or driver command; pass each argv word separately."
        .to_string()
}

pub(crate) fn parse_args_with_env(
    args: &[String],
    get_env: impl Fn(&str) -> Option<OsString>,
) -> Result<Command, String> {
    match args {
        [arg] if arg == "--help" || arg == "-h" => Ok(Command::Help),
        [arg] if arg == "--version" || arg == "-V" => Ok(Command::Version),
        [] => Err(
            "missing command; expected 'run' or 'doctor' (see 'bevy-feedback --help')".to_string(),
        ),
        [command, ..] if command == "run" => parse_run_args(&args[1..], &get_env),
        [command, ..] if command == "doctor" => parse_doctor_args(&args[1..], &get_env),
        [option, ..] if option.starts_with('-') => Err(format!(
            "unknown option '{option}' (see 'bevy-feedback --help')"
        )),
        [command, ..] => Err(format!(
            "unknown command '{command}'; expected 'run' or 'doctor' (see 'bevy-feedback --help')"
        )),
    }
}

fn parse_run_args(
    args: &[String],
    get_env: &impl Fn(&str) -> Option<OsString>,
) -> Result<Command, String> {
    if matches!(args, [arg] if arg == "--help" || arg == "-h") {
        return Ok(Command::Help);
    }
    if matches!(args, [arg] if arg == "--version" || arg == "-V") {
        return Ok(Command::Version);
    }

    let mut protocol_file = default_protocol_file(get_env);
    let mut artifacts = get_env("BEVY_FEEDBACK_ARTIFACTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(format!("target/agent-feedback/artifacts/run-{}", unix_ms()))
        });
    let mut prepare_timeout = default_timeout(
        "prepare timeout",
        "BEVY_FEEDBACK_PREPARE_TIMEOUT_MS",
        300_000,
        get_env,
    )?;
    let protocol_env = get_env("BEVY_FEEDBACK_PROTOCOL_TIMEOUT_MS");
    let legacy_protocol_env = get_env("BEVY_FEEDBACK_READY_TIMEOUT_MS");
    let mut used_legacy_ready_timeout = protocol_env.is_none() && legacy_protocol_env.is_some();
    let mut protocol_timeout = match protocol_env.or(legacy_protocol_env) {
        Some(value) => {
            let value = value.into_string().map_err(|_| {
                "protocol timeout environment value must be valid UTF-8".to_string()
            })?;
            parse_timeout_ms(&value, "protocol timeout")?
        }
        None => Duration::from_millis(60_000),
    };
    let mut protocol_flag_seen = false;
    let mut ready_flag_seen = false;
    let mut game_cwd = None;
    let mut required_window_size = get_env("BEVY_FEEDBACK_REQUIRED_WINDOW_SIZE")
        .map(|value| {
            value
                .into_string()
                .map_err(|_| "BEVY_FEEDBACK_REQUIRED_WINDOW_SIZE must be valid UTF-8".to_string())
                .and_then(|value| parse_window_size(&value))
        })
        .transpose()?;
    let mut driver_timeout = default_timeout(
        "driver timeout",
        "BEVY_FEEDBACK_DRIVER_TIMEOUT_MS",
        300_000,
        get_env,
    )?;
    let mut shutdown_timeout = default_timeout(
        "shutdown timeout",
        "BEVY_FEEDBACK_SHUTDOWN_TIMEOUT_MS",
        5_000,
        get_env,
    )?;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--protocol" => {
                let value = option_value(args, index, "--protocol")?;
                protocol_file = PathBuf::from(value);
                index += 2;
            }
            "--artifacts" => {
                let value = option_value(args, index, "--artifacts")?;
                artifacts = PathBuf::from(value);
                index += 2;
            }
            "--prepare-timeout" => {
                let value = option_value(args, index, "--prepare-timeout")?;
                prepare_timeout = parse_timeout_ms(value, "prepare timeout")?;
                index += 2;
            }
            "--protocol-timeout" => {
                if ready_flag_seen {
                    return Err(
                        "--protocol-timeout conflicts with deprecated --ready-timeout".to_string(),
                    );
                }
                let value = option_value(args, index, "--protocol-timeout")?;
                protocol_timeout = parse_timeout_ms(value, "protocol timeout")?;
                protocol_flag_seen = true;
                used_legacy_ready_timeout = false;
                index += 2;
            }
            "--ready-timeout" => {
                if protocol_flag_seen {
                    return Err(
                        "deprecated --ready-timeout conflicts with --protocol-timeout".to_string(),
                    );
                }
                let value = option_value(args, index, "--ready-timeout")?;
                protocol_timeout = parse_timeout_ms(value, "protocol timeout")?;
                ready_flag_seen = true;
                used_legacy_ready_timeout = true;
                index += 2;
            }
            "--game-cwd" => {
                game_cwd = Some(PathBuf::from(option_value(args, index, "--game-cwd")?));
                index += 2;
            }
            "--require-window-size" => {
                required_window_size = Some(parse_window_size(option_value(
                    args,
                    index,
                    "--require-window-size",
                )?)?);
                index += 2;
            }
            "--driver-timeout" => {
                let value = option_value(args, index, "--driver-timeout")?;
                driver_timeout = parse_timeout_ms(value, "driver timeout")?;
                index += 2;
            }
            "--shutdown-timeout" => {
                let value = option_value(args, index, "--shutdown-timeout")?;
                shutdown_timeout = parse_timeout_ms(value, "shutdown timeout")?;
                index += 2;
            }
            "--prepare" => {
                let rest = &args[index + 1..];
                let Some(game_index) = rest.iter().position(|arg| arg == "--game") else {
                    return Err("--prepare requires a following --game command".to_string());
                };
                if game_index == 0 {
                    return Err("missing prepare command after '--prepare'".to_string());
                }
                return parse_game_driver(
                    &rest[game_index + 1..],
                    protocol_file,
                    artifacts,
                    required_window_size,
                    prepare_timeout,
                    protocol_timeout,
                    shutdown_timeout,
                    driver_timeout,
                    game_cwd,
                    Some(rest[..game_index].to_vec()),
                    used_legacy_ready_timeout,
                );
            }
            "--game" => {
                return parse_game_driver(
                    &args[index + 1..],
                    protocol_file,
                    artifacts,
                    required_window_size,
                    prepare_timeout,
                    protocol_timeout,
                    shutdown_timeout,
                    driver_timeout,
                    game_cwd,
                    None,
                    used_legacy_ready_timeout,
                );
            }
            "--" => {
                let game = args[index + 1..].to_vec();
                if game.is_empty() {
                    return Err(missing_game());
                }
                return Ok(Command::Run(RunArgs {
                    protocol_file,
                    artifacts,
                    required_window_size,
                    prepare_timeout,
                    protocol_timeout,
                    shutdown_timeout,
                    driver_timeout,
                    game_cwd,
                    prepare: None,
                    game,
                    driver: None,
                    used_legacy_ready_timeout,
                }));
            }
            option if option.starts_with('-') => {
                return Err(format!(
                    "unknown option '{option}' (see 'bevy-feedback run --help')"
                ));
            }
            _ => return Err(missing_game()),
        }
    }
    Err(missing_game())
}

fn parse_doctor_args(
    args: &[String],
    get_env: &impl Fn(&str) -> Option<OsString>,
) -> Result<Command, String> {
    if matches!(args, [arg] if arg == "--help" || arg == "-h") {
        return Ok(Command::Help);
    }
    if matches!(args, [arg] if arg == "--version" || arg == "-V") {
        return Ok(Command::Version);
    }

    let mut protocol_file = default_protocol_file(get_env);
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--protocol" => {
                let value = option_value(args, index, "--protocol")?;
                protocol_file = PathBuf::from(value);
                index += 2;
            }
            option if option.starts_with('-') => {
                return Err(format!(
                    "unknown option '{option}' (see 'bevy-feedback doctor --help')"
                ));
            }
            value => {
                return Err(format!(
                    "unexpected argument '{value}' (see 'bevy-feedback doctor --help')"
                ));
            }
        }
    }
    Ok(Command::Doctor(DoctorArgs { protocol_file }))
}

fn option_value<'a>(args: &'a [String], index: usize, option: &str) -> Result<&'a str, String> {
    let Some(value) = args.get(index + 1) else {
        return Err(format!("option '{option}' requires a value"));
    };
    if value == "--" || value.starts_with("--") {
        return Err(format!("option '{option}' requires a value"));
    }
    Ok(value)
}

fn parse_window_size(value: &str) -> Result<RequiredWindowSize, String> {
    let Some((width, height)) = value.split_once('x') else {
        return Err(
            "required window size must use WIDTHxHEIGHT with positive integers".to_string(),
        );
    };
    let width = width.parse::<u32>().ok().filter(|value| *value > 0);
    let height = height.parse::<u32>().ok().filter(|value| *value > 0);
    match (width, height) {
        (Some(width), Some(height)) => Ok(RequiredWindowSize { width, height }),
        _ => Err("required window size must use WIDTHxHEIGHT with positive integers".to_string()),
    }
}

fn parse_timeout_ms(value: &str, name: &str) -> Result<Duration, String> {
    let milliseconds = value
        .parse::<u64>()
        .ok()
        .filter(|milliseconds| *milliseconds > 0)
        .ok_or_else(|| format!("{name} must be a positive integer number of milliseconds"))?;
    Ok(Duration::from_millis(milliseconds))
}

fn default_timeout(
    name: &str,
    env_name: &str,
    default_ms: u64,
    get_env: &impl Fn(&str) -> Option<OsString>,
) -> Result<Duration, String> {
    let Some(value) = get_env(env_name) else {
        return Ok(Duration::from_millis(default_ms));
    };
    let value = value
        .into_string()
        .map_err(|_| format!("{env_name} must be valid UTF-8"))?;
    parse_timeout_ms(&value, name)
}

fn default_protocol_file(get_env: &impl Fn(&str) -> Option<OsString>) -> PathBuf {
    get_env("BEVY_FEEDBACK_PROTOCOL")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/agent-feedback/agent-feedback.json"))
}

#[allow(clippy::too_many_arguments)]
fn parse_game_driver(
    args: &[String],
    protocol_file: PathBuf,
    artifacts: PathBuf,
    required_window_size: Option<RequiredWindowSize>,
    prepare_timeout: Duration,
    protocol_timeout: Duration,
    shutdown_timeout: Duration,
    driver_timeout: Duration,
    game_cwd: Option<PathBuf>,
    prepare: Option<Vec<String>>,
    used_legacy_ready_timeout: bool,
) -> Result<Command, String> {
    let driver_index = args.iter().position(|arg| arg == "--driver");
    let (game, driver) = match driver_index {
        Some(index) => (&args[..index], Some(args[index + 1..].to_vec())),
        None => (args, None),
    };
    if game.is_empty() {
        return Err(missing_game());
    }
    if driver.as_ref().is_some_and(Vec::is_empty) {
        return Err("missing driver command after '--driver'".to_string());
    }
    Ok(Command::Run(RunArgs {
        protocol_file,
        artifacts,
        required_window_size,
        prepare_timeout,
        protocol_timeout,
        shutdown_timeout,
        driver_timeout,
        game_cwd,
        prepare,
        game: game.to_vec(),
        driver,
        used_legacy_ready_timeout,
    }))
}

fn missing_game() -> String {
    "missing game command; use '-- <game...>' or '--game <game...>'".to_string()
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests;
