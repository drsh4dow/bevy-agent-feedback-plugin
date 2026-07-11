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
fn parses_required_window_size_from_flag_and_environment() {
    let flagged = run_args(
        parse_args(&[
            "run".into(),
            "--require-window-size".into(),
            "1280x720".into(),
            "--".into(),
            "game".into(),
        ])
        .expect("flagged size"),
    );
    let from_env = run_args(
        parse_args_with_env(&["run".into(), "--".into(), "game".into()], |name| {
            (name == "BEVY_FEEDBACK_REQUIRED_WINDOW_SIZE").then(|| OsString::from("955x1170"))
        })
        .expect("environment size"),
    );

    assert_eq!(
        flagged.required_window_size,
        Some(RequiredWindowSize {
            width: 1280,
            height: 720
        })
    );
    assert_eq!(
        from_env.required_window_size,
        Some(RequiredWindowSize {
            width: 955,
            height: 1170
        })
    );
}

#[test]
fn rejects_invalid_required_window_sizes() {
    for size in ["1280", "1280X720", "0x720", "1280x0", "a x b"] {
        let error = parse_args(&[
            "run".into(),
            "--require-window-size".into(),
            size.into(),
            "--".into(),
            "game".into(),
        ])
        .expect_err("invalid size");
        assert!(error.contains("WIDTHxHEIGHT"), "{size}: {error}");
    }
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

    assert!(error.contains("protocol timeout must be a positive integer number of milliseconds"));
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
    let args = run_args(parse_args(&["run".into(), "--".into(), "--help".into()]).expect("args"));

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
        usage.contains("--game cargo run --features agent --driver python3 tests/drive_camera.py")
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
