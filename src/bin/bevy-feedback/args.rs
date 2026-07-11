use std::{
    ffi::OsString,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RunArgs {
    pub(crate) protocol_file: PathBuf,
    pub(crate) artifacts: PathBuf,
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
  --ready-timeout MS     deprecated alias for --protocol-timeout

env:
  BEVY_FEEDBACK_PROTOCOL             protocol file (default target/agent-feedback/agent-feedback.json)
  BEVY_FEEDBACK_ARTIFACTS            artifact dir (default target/agent-feedback/artifacts/run-<unix-ms>)
  BEVY_FEEDBACK_PREPARE_TIMEOUT_MS   prepare timeout in milliseconds (default 300000)
  BEVY_FEEDBACK_PROTOCOL_TIMEOUT_MS  protocol timeout in milliseconds (default 60000)
  BEVY_FEEDBACK_READY_TIMEOUT_MS     deprecated protocol-timeout fallback
  BEVY_FEEDBACK_DRIVER_TIMEOUT_MS    driver timeout in milliseconds (default 300000)
  BEVY_FEEDBACK_SHUTDOWN_TIMEOUT_MS  shutdown timeout in milliseconds (default 5000)
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
mod tests {
    use super::*;

    fn run_args(command: Command) -> RunArgs {
        match command {
            Command::Run(args) => args,
            other => panic!("expected run args, got {other:?}"),
        }
    }

    #[test]
    fn parses_game_only_command() {
        let args = run_args(
            parse_args(&[
                "run".into(),
                "--".into(),
                "cargo".into(),
                "run".into(),
                "--example".into(),
                "minimal".into(),
            ])
            .expect("args"),
        );

        assert_eq!(args.game, ["cargo", "run", "--example", "minimal"]);
        assert_eq!(args.driver, None);
    }

    #[test]
    fn parses_game_and_driver_command() {
        let args = run_args(
            parse_args(&[
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
            .expect("args"),
        );

        assert_eq!(args.protocol_file, PathBuf::from("target/agent.json"));
        assert_eq!(args.game, ["cargo", "run"]);
        assert_eq!(args.driver, Some(vec!["python3".into(), "drive.py".into()]));
    }

    #[test]
    fn parses_timeout_flags_for_game_only_command() {
        let args = run_args(
            parse_args(&[
                "run".into(),
                "--ready-timeout".into(),
                "120000".into(),
                "--driver-timeout".into(),
                "400000".into(),
                "--shutdown-timeout".into(),
                "9000".into(),
                "--".into(),
                "cargo".into(),
                "run".into(),
            ])
            .expect("args"),
        );

        assert_eq!(args.protocol_timeout, Duration::from_millis(120_000));
        assert_eq!(args.driver_timeout, Duration::from_millis(400_000));
        assert_eq!(args.shutdown_timeout, Duration::from_millis(9_000));
    }

    #[test]
    fn parses_timeout_flags_for_game_and_driver_command() {
        let args = run_args(
            parse_args(&[
                "run".into(),
                "--ready-timeout".into(),
                "120000".into(),
                "--driver-timeout".into(),
                "400000".into(),
                "--shutdown-timeout".into(),
                "9000".into(),
                "--game".into(),
                "cargo".into(),
                "run".into(),
                "--driver".into(),
                "python3".into(),
                "drive.py".into(),
            ])
            .expect("args"),
        );

        assert_eq!(args.protocol_timeout, Duration::from_millis(120_000));
        assert_eq!(args.driver_timeout, Duration::from_millis(400_000));
        assert_eq!(args.shutdown_timeout, Duration::from_millis(9_000));
        assert_eq!(args.game, ["cargo", "run"]);
        assert_eq!(args.driver, Some(vec!["python3".into(), "drive.py".into()]));
    }

    #[test]
    fn parses_prepare_protocol_timeout_and_game_cwd() {
        let args = run_args(
            parse_args(&[
                "run".into(),
                "--prepare-timeout".into(),
                "7000".into(),
                "--protocol-timeout".into(),
                "8000".into(),
                "--game-cwd".into(),
                "/tmp/game".into(),
                "--prepare".into(),
                "cargo".into(),
                "build".into(),
                "--game".into(),
                "target/debug/game".into(),
                "--driver".into(),
                "python3".into(),
                "drive.py".into(),
            ])
            .expect("args"),
        );

        assert_eq!(args.prepare, Some(vec!["cargo".into(), "build".into()]));
        assert_eq!(args.prepare_timeout, Duration::from_millis(7_000));
        assert_eq!(args.protocol_timeout, Duration::from_millis(8_000));
        assert_eq!(args.game_cwd, Some(PathBuf::from("/tmp/game")));
        assert!(!args.used_legacy_ready_timeout);
    }

    #[test]
    fn legacy_ready_timeout_is_a_deprecated_alias() {
        let args = run_args(
            parse_args(&[
                "run".into(),
                "--ready-timeout".into(),
                "9000".into(),
                "--".into(),
                "game".into(),
            ])
            .expect("args"),
        );

        assert_eq!(args.protocol_timeout, Duration::from_millis(9_000));
        assert!(args.used_legacy_ready_timeout);
    }

    #[test]
    fn rejects_both_protocol_timeout_names() {
        let error = parse_args(&[
            "run".into(),
            "--ready-timeout".into(),
            "9000".into(),
            "--protocol-timeout".into(),
            "8000".into(),
            "--".into(),
            "game".into(),
        ])
        .expect_err("aliases must not conflict");

        assert!(error.contains("conflicts"), "{error}");
    }

    #[test]
    fn rejects_zero_timeout() {
        let error = parse_args(&[
            "run".into(),
            "--ready-timeout".into(),
            "0".into(),
            "--".into(),
            "cargo".into(),
            "run".into(),
        ])
        .expect_err("zero timeout should be rejected");

        assert!(
            error.contains("protocol timeout must be a positive integer number of milliseconds")
        );
    }

    #[test]
    fn uses_timeout_env_defaults() {
        let args = run_args(
            parse_args_with_env(
                &["run".into(), "--".into(), "cargo".into(), "run".into()],
                |name| match name {
                    "BEVY_FEEDBACK_READY_TIMEOUT_MS" => Some(OsString::from("120000")),
                    "BEVY_FEEDBACK_DRIVER_TIMEOUT_MS" => Some(OsString::from("400000")),
                    "BEVY_FEEDBACK_SHUTDOWN_TIMEOUT_MS" => Some(OsString::from("9000")),
                    _ => None,
                },
            )
            .expect("args"),
        );

        assert_eq!(args.protocol_timeout, Duration::from_millis(120_000));
        assert_eq!(args.driver_timeout, Duration::from_millis(400_000));
        assert_eq!(args.shutdown_timeout, Duration::from_millis(9_000));
    }

    #[test]
    fn parses_run_help() {
        assert_eq!(
            parse_args(&["run".into(), "--help".into()]).expect("help"),
            Command::Help
        );
        assert_eq!(
            parse_args(&["run".into(), "-h".into()]).expect("help"),
            Command::Help
        );
    }

    #[test]
    fn parses_global_help() {
        assert_eq!(parse_args(&["--help".into()]).expect("help"), Command::Help);
        assert_eq!(parse_args(&["-h".into()]).expect("help"), Command::Help);
    }

    #[test]
    fn help_does_not_read_timeout_env() {
        assert_eq!(
            parse_args_with_env(&["run".into(), "--help".into()], |name| {
                panic!("help should not read env {name}")
            })
            .expect("help"),
            Command::Help
        );
    }

    #[test]
    fn does_not_consume_help_after_separator() {
        let args =
            run_args(parse_args(&["run".into(), "--".into(), "--help".into()]).expect("args"));

        assert_eq!(args.game, ["--help"]);
    }

    #[test]
    fn parses_separator_game_path_with_spaces() {
        let args = run_args(
            parse_args(&["run".into(), "--".into(), "/tmp/My Game/game".into()]).expect("args"),
        );

        assert_eq!(args.game, ["/tmp/My Game/game"]);
        assert_eq!(args.driver, None);
    }

    #[test]
    fn parses_game_and_driver_paths_with_spaces() {
        let args = run_args(
            parse_args(&[
                "run".into(),
                "--game".into(),
                "/tmp/My Game/game".into(),
                "--driver".into(),
                "/tmp/My Driver/driver.py".into(),
            ])
            .expect("args"),
        );

        assert_eq!(args.game, ["/tmp/My Game/game"]);
        assert_eq!(args.driver, Some(vec!["/tmp/My Driver/driver.py".into()]));
    }

    #[test]
    fn parses_global_version() {
        assert_eq!(
            parse_args(&["--version".into()]).expect("version"),
            Command::Version
        );
        assert_eq!(
            parse_args(&["-V".into()]).expect("version"),
            Command::Version
        );
    }

    #[test]
    fn parses_run_version() {
        assert_eq!(
            parse_args(&["run".into(), "--version".into()]).expect("version"),
            Command::Version
        );
        assert_eq!(
            parse_args(&["run".into(), "-V".into()]).expect("version"),
            Command::Version
        );
    }

    #[test]
    fn parses_doctor_protocol() {
        assert_eq!(
            parse_args(&[
                "doctor".into(),
                "--protocol".into(),
                "target/custom.json".into()
            ])
            .expect("doctor"),
            Command::Doctor(DoctorArgs {
                protocol_file: PathBuf::from("target/custom.json")
            })
        );
    }

    #[test]
    fn rejects_unknown_command_precisely() {
        let error = parse_args(&["bad".into()]).expect_err("unknown command");
        assert_eq!(
            error,
            "unknown command 'bad'; expected 'run' or 'doctor' (see 'bevy-feedback --help')"
        );
    }

    #[test]
    fn rejects_unknown_run_option_precisely() {
        let error = parse_args(&["run".into(), "--bad".into()]).expect_err("unknown option");
        assert_eq!(
            error,
            "unknown option '--bad' (see 'bevy-feedback run --help')"
        );
    }

    #[test]
    fn rejects_missing_value_precisely() {
        let error = parse_args(&["run".into(), "--protocol".into()]).expect_err("missing value");
        assert_eq!(error, "option '--protocol' requires a value");
    }

    #[test]
    fn rejects_missing_game_precisely() {
        let error = parse_args(&["run".into()]).expect_err("missing game");
        assert_eq!(
            error,
            "missing game command; use '-- <game...>' or '--game <game...>'"
        );
    }

    #[test]
    fn usage_shows_cargo_driver_argv_example() {
        let usage = usage();

        assert!(
            usage.contains(
                "--game cargo run --features agent --driver python3 tests/drive_camera.py"
            )
        );
        assert!(usage.contains("Do not quote the whole game or driver command"));
        assert!(usage.contains("doctor"));
        assert!(usage.contains("--version"));
        assert!(usage.contains("screenshots"));
    }
    #[test]
    fn usage_points_to_readiness_time_and_capture_completion_guidance() {
        let usage = usage();

        assert!(
            usage.contains(
                "Protocol-ready is not game-ready; use skills/driving-bevy-games/SKILL.md to choose readiness and time control."
            ),
            "{usage}"
        );
        assert!(
            usage.contains("Frame waits count app updates, not gameplay time."),
            "{usage}"
        );
        assert!(
            usage.contains(
                "For animated games use predicates; use deterministic advance for gameplay time."
            ),
            "{usage}"
        );
        assert!(
            usage.contains("Capture completion proves screenshot readback"),
            "{usage}"
        );
    }
}
