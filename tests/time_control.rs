mod time_control_support;

use bevy::{
    prelude::*,
    time::{Fixed, TimeUpdateStrategy, Virtual},
};
use bevy_agent_feedback_plugin::client::{AgentClient, AgentClientConfig};
use std::{sync::mpsc, thread, time::Duration};
use time_control_support::*;

#[test]
fn disabled_and_unavailable_time_control_return_structured_errors() {
    let (disabled_config, disabled_root) = timing_config("disabled", false);
    let mut disabled = build_harness(disabled_config, disabled_root, true, |_| {});
    let mut wire = Wire::connect(&disabled.config);
    wire.send(advance_request(
        "disabled",
        Duration::from_millis(10),
        Duration::from_millis(10),
    ));
    let response = response_while_updating(disabled.app_mut(), &mut wire);
    assert_timing_error(&response, "time_control_disabled", None);
    assert_eq!(response["error"]["context"]["timing"]["state"], "disabled");

    let (unavailable_config, unavailable_root) = timing_config("unavailable", true);
    let mut unavailable = build_harness(unavailable_config, unavailable_root, false, |_| {});
    let mut wire = Wire::connect(&unavailable.config);
    wire.send(advance_request(
        "unavailable",
        Duration::from_millis(10),
        Duration::from_millis(10),
    ));
    let response = response_while_updating(unavailable.app_mut(), &mut wire);
    assert_timing_error(
        &response,
        "time_control_unavailable",
        Some("time_plugin_missing"),
    );
    assert_eq!(
        response["error"]["context"]["timing"]["state"],
        "unavailable"
    );
}

#[test]
fn deterministic_time_stays_frozen_during_idle_frames_and_host_sleeps() {
    let mut harness = deterministic_harness("frozen-idle", Duration::from_millis(10));
    harness.clear_trace();

    for sleep in [0, 7, 1, 11, 3].map(Duration::from_millis) {
        thread::sleep(sleep);
        harness.app_mut().update();
    }

    let trace = harness.trace();
    assert_eq!(harness.elapsed(), Duration::ZERO);
    assert_eq!(trace.update_deltas, vec![Duration::ZERO; 5]);
    assert_eq!(trace.update_elapsed, vec![Duration::ZERO; 5]);
    assert!(trace.fixed_deltas.is_empty());
    assert_eq!(trace.simulated_nanoseconds, 0);
}

#[test]
fn advance_has_one_frame_arming_latency_and_exact_update_deltas() {
    let mut harness = deterministic_harness("exact-deltas", Duration::from_millis(10));
    let mut wire = Wire::connect(&harness.config);
    let requested = Duration::from_millis(50);
    let expected = [
        Duration::from_millis(16),
        Duration::from_millis(16),
        Duration::from_millis(16),
        Duration::from_millis(2),
    ];

    assert_eq!(
        arm_advance(
            &mut harness,
            &mut wire,
            "exact",
            requested,
            Duration::from_millis(16),
        ),
        expected[0]
    );
    harness.clear_trace();

    let mut cumulative = Duration::ZERO;
    for (index, delta) in expected.into_iter().enumerate() {
        harness.app_mut().update();
        cumulative += delta;
        assert_eq!(harness.elapsed(), cumulative);
        if index + 1 < expected.len() {
            assert!(
                wire.try_response().is_none(),
                "advance_time responded before consuming every exact delta"
            );
        }
    }
    let response = wire.wait_without_updates();

    let trace = harness.trace();
    assert_eq!(trace.update_deltas, expected);
    assert_eq!(
        trace.update_deltas.iter().copied().sum::<Duration>(),
        requested
    );
    assert_eq!(trace.simulated_nanoseconds, requested.as_nanos());
    assert_eq!(trace.fixed_deltas.len(), 5);
    assert_eq!(harness.elapsed(), requested);
    assert_timing_success(&response, "advanced_time");
    assert_eq!(
        response["result"]["details"]["step_count"], 4,
        "unexpected timing response: {response}"
    );
    assert_eq!(duration_detail(&response, "start_seconds"), Duration::ZERO);
    assert_eq!(duration_detail(&response, "end_seconds"), requested);
    assert_eq!(duration_detail(&response, "actual_seconds"), requested);
}

#[test]
fn advance_shorter_than_fixed_timestep_runs_update_but_not_fixed_update() {
    let mut harness = deterministic_harness("sub-fixed", Duration::from_millis(10));
    let mut wire = Wire::connect(&harness.config);
    let requested = Duration::from_millis(4);

    assert_eq!(
        arm_advance(
            &mut harness,
            &mut wire,
            "sub-fixed",
            requested,
            Duration::from_millis(10),
        ),
        requested
    );
    harness.clear_trace();
    harness.app_mut().update();
    let response = wire.wait_without_updates();

    let trace = harness.trace();
    assert_eq!(trace.update_deltas, vec![requested]);
    assert!(trace.fixed_deltas.is_empty());
    assert_eq!(harness.elapsed(), requested);
    assert_timing_success(&response, "advanced_time");
    assert_eq!(
        response["result"]["details"]["step_count"], 1,
        "unexpected timing response: {response}"
    );
    assert_eq!(duration_detail(&response, "actual_seconds"), requested);
}

#[test]
fn client_chunking_preserves_full_steps_and_emits_one_final_remainder() {
    let (mut config, root) = timing_config("client-chunks", true);
    config.max_time_advance = Duration::from_millis(25);
    config.max_time_advance_steps = 2;
    config.command_timeout = Duration::from_secs(5);
    let protocol_file = config.protocol_file.clone();
    let mut harness = build_harness(config, root, true, |app| {
        app.world_mut()
            .resource_mut::<Time<Fixed>>()
            .set_timestep(Duration::from_millis(10));
    });
    harness.clear_trace();

    let (sender, receiver) = mpsc::sync_channel(1);
    let client_thread = thread::spawn(move || {
        let result = AgentClient::with_config(AgentClientConfig {
            protocol_file,
            timeout: Duration::from_secs(5),
            ..Default::default()
        })
        .and_then(|mut client| client.advance_time(0.055, Some(0.01)));
        sender
            .send(result)
            .expect("test receiver should remain open");
    });

    let response = (0..UPDATE_ATTEMPTS)
        .find_map(|_| {
            harness.app_mut().update();
            let result = receiver.try_recv().ok();
            if result.is_none() {
                thread::sleep(Duration::from_millis(2));
            }
            result
        })
        .expect("chunked client advance should finish within bounded updates")
        .expect("chunked client advance should succeed");
    client_thread
        .join()
        .expect("client thread should not panic");

    let trace = harness.trace();
    let nonzero = trace
        .update_deltas
        .iter()
        .copied()
        .filter(|delta| !delta.is_zero())
        .collect::<Vec<_>>();
    assert_eq!(
        nonzero,
        vec![
            Duration::from_millis(10),
            Duration::from_millis(10),
            Duration::from_millis(10),
            Duration::from_millis(10),
            Duration::from_millis(10),
            Duration::from_millis(5),
        ]
    );
    assert_eq!(
        nonzero
            .iter()
            .filter(|delta| **delta != Duration::from_millis(10))
            .count(),
        1,
        "client chunk boundaries must not create intermediate remainders"
    );
    assert_eq!(
        nonzero.iter().copied().sum::<Duration>(),
        Duration::from_millis(55)
    );
    assert_eq!(harness.elapsed(), Duration::from_millis(55));
    assert_eq!(trace.fixed_deltas.len(), 5);
    assert_timing_success(&response, "advanced_time");
    assert_eq!(
        duration_detail(&response, "actual_seconds"),
        Duration::from_millis(15)
    );
}

fn pause_virtual_time(app: &mut App) {
    app.world_mut().resource_mut::<Time<Virtual>>().pause();
}

fn speed_up_virtual_time(app: &mut App) {
    app.world_mut()
        .resource_mut::<Time<Virtual>>()
        .set_relative_speed(2.0);
}

fn lower_max_delta(app: &mut App) {
    app.world_mut()
        .resource_mut::<Time<Virtual>>()
        .set_max_delta(Duration::from_millis(5));
}

type TimeConflictCase = (&'static str, fn(&mut App), &'static str);

#[test]
fn advance_rejects_paused_scaled_and_max_delta_conflicts() {
    let cases: [TimeConflictCase; 3] = [
        ("paused", pause_virtual_time, "virtual_time_paused"),
        (
            "scaled",
            speed_up_virtual_time,
            "virtual_time_speed_not_one",
        ),
        (
            "max-delta",
            lower_max_delta,
            "nominal_step_exceeds_max_delta",
        ),
    ];

    for (name, configure, reason) in cases {
        let mut harness = deterministic_harness(name, Duration::from_millis(10));
        configure(harness.app_mut());
        let mut wire = Wire::connect(&harness.config);
        wire.send(advance_request(
            name,
            Duration::from_millis(10),
            Duration::from_millis(10),
        ));
        let response = response_while_updating(harness.app_mut(), &mut wire);
        assert_timing_error(&response, "invalid_time_advance", Some(reason));
        assert_eq!(harness.elapsed(), Duration::ZERO, "case {name}");
    }
}

#[test]
fn wait_seconds_observes_virtual_time_and_times_out_without_changing_it() {
    let (config, root) = timing_config("observational-wait", false);
    let mut harness = build_harness(config, root, true, |app| {
        *app.world_mut().resource_mut::<TimeUpdateStrategy>() =
            TimeUpdateStrategy::ManualDuration(Duration::from_millis(5));
    });
    harness.app_mut().update();
    harness.clear_trace();
    let mut wire = Wire::connect(&harness.config);
    wire.send(wait_seconds_request(
        "observed",
        Duration::from_millis(12),
        4,
    ));
    let response = response_while_updating(harness.app_mut(), &mut wire);
    let trace = harness.trace();

    assert_timing_success(&response, "waited");
    assert_eq!(
        response["result"]["details"]["frames"], 3,
        "unexpected timing response: {response}"
    );
    assert_eq!(
        duration_detail(&response, "observed_seconds"),
        Duration::from_millis(15)
    );
    assert!(
        trace
            .update_deltas
            .iter()
            .all(|delta| *delta == Duration::from_millis(5)),
        "wait_seconds must observe the app's clock without injecting a custom delta"
    );
    assert_eq!(
        trace.update_deltas.iter().copied().sum::<Duration>(),
        harness.elapsed()
    );

    let (timeout_config, timeout_root) = timing_config("observational-timeout", false);
    let mut timeout = build_harness(timeout_config, timeout_root, true, |app| {
        *app.world_mut().resource_mut::<TimeUpdateStrategy>() =
            TimeUpdateStrategy::ManualDuration(Duration::ZERO);
    });
    let mut wire = Wire::connect(&timeout.config);
    wire.send(wait_seconds_request("timeout", Duration::from_millis(1), 2));
    let response = response_while_updating(timeout.app_mut(), &mut wire);
    assert_timing_error(&response, "wait_seconds_timeout", None);
    assert_eq!(
        response["error"]["context"]["timing"]["observed_seconds"],
        0.0
    );
    assert_eq!(response["error"]["context"]["timing"]["frames"], 2);
    assert_eq!(timeout.elapsed(), Duration::ZERO);

    let mut frozen = deterministic_harness("frozen-wait", Duration::from_millis(10));
    let mut wire = Wire::connect(&frozen.config);
    wire.send(wait_seconds_request("frozen", Duration::from_millis(1), 2));
    let response = response_while_updating(frozen.app_mut(), &mut wire);
    assert_timing_error(&response, "time_control_frozen", None);
    assert_eq!(response["error"]["context"]["timing"]["state"], "frozen");
    assert_eq!(frozen.elapsed(), Duration::ZERO);
}

#[test]
fn command_timeout_cancels_an_armed_advance_before_it_changes_time() {
    let (mut config, root) = timing_config("command-timeout", true);
    config.command_timeout = Duration::from_millis(200);
    let mut harness = build_harness(config, root, true, |app| {
        app.world_mut()
            .resource_mut::<Time<Fixed>>()
            .set_timestep(Duration::from_millis(10));
    });
    let mut wire = Wire::connect(&harness.config);

    assert_eq!(
        arm_advance(
            &mut harness,
            &mut wire,
            "timeout",
            Duration::from_millis(30),
            Duration::from_millis(10),
        ),
        Duration::from_millis(10)
    );
    thread::sleep(Duration::from_millis(250));
    let response = wire.wait_without_updates();
    assert_timing_error(&response, "timeout", None);

    harness.clear_trace();
    harness.app_mut().update();
    assert_eq!(harness.elapsed(), Duration::ZERO);
    assert_eq!(harness.trace().update_deltas, vec![Duration::ZERO]);
    assert!(matches!(
        *harness
            .app()
            .world()
            .resource::<TimeUpdateStrategy>(),
        TimeUpdateStrategy::ManualDuration(delta) if delta.is_zero()
    ));
}

#[test]
fn disconnect_cancels_an_armed_advance_and_allows_the_next_request() {
    let mut harness = deterministic_harness("disconnect", Duration::from_millis(10));
    let mut disconnected = Wire::connect(&harness.config);
    assert_eq!(
        arm_advance(
            &mut harness,
            &mut disconnected,
            "disconnect",
            Duration::from_millis(20),
            Duration::from_millis(10),
        ),
        Duration::from_millis(10)
    );
    drop(disconnected);
    thread::sleep(Duration::from_millis(80));

    harness.clear_trace();
    harness.app_mut().update();
    assert_eq!(harness.elapsed(), Duration::ZERO);
    assert_eq!(harness.trace().update_deltas, vec![Duration::ZERO]);

    let mut next = Wire::connect(&harness.config);
    assert_eq!(
        arm_advance(
            &mut harness,
            &mut next,
            "after-disconnect",
            Duration::from_millis(10),
            Duration::from_millis(10),
        ),
        Duration::from_millis(10)
    );
    harness.app_mut().update();
    let response = next.wait_without_updates();
    assert_timing_success(&response, "advanced_time");
    assert_eq!(harness.elapsed(), Duration::from_millis(10));
}

#[test]
fn app_provided_time_strategy_conflict_is_rejected_without_claiming_control() {
    let (config, root) = timing_config("strategy-conflict", true);
    let app_delta = Duration::from_millis(7);
    let mut harness = build_harness(config, root, true, |app| {
        *app.world_mut().resource_mut::<TimeUpdateStrategy>() =
            TimeUpdateStrategy::ManualDuration(app_delta);
    });
    harness.app_mut().update();
    harness.clear_trace();
    let mut wire = Wire::connect(&harness.config);
    wire.send(advance_request(
        "strategy-conflict",
        Duration::from_millis(10),
        Duration::from_millis(10),
    ));
    let response = response_while_updating(harness.app_mut(), &mut wire);

    assert_timing_error(
        &response,
        "time_control_unavailable",
        Some("manual_duration_already_configured"),
    );
    assert!(
        harness
            .trace()
            .update_deltas
            .iter()
            .all(|delta| *delta == app_delta),
        "the app-provided strategy must remain the clock source"
    );
    assert!(harness.elapsed() >= app_delta);
}

#[derive(Debug, Eq, PartialEq)]
struct ScenarioOutcome {
    trace: ClockTrace,
    elapsed: Duration,
}

fn run_sleep_invariant_scenario(name: &str, host_sleeps: [Duration; 4]) -> ScenarioOutcome {
    let mut harness = deterministic_harness(name, Duration::from_millis(7));
    let mut wire = Wire::connect(&harness.config);
    assert_eq!(
        arm_advance(
            &mut harness,
            &mut wire,
            name,
            Duration::from_millis(37),
            Duration::from_millis(10),
        ),
        Duration::from_millis(10)
    );
    harness.clear_trace();

    for sleep in host_sleeps {
        thread::sleep(sleep);
        harness.app_mut().update();
    }
    let response = wire.wait_without_updates();
    assert_timing_success(&response, "advanced_time");
    assert_eq!(
        response["result"]["details"]["step_count"], 4,
        "unexpected timing response: {response}"
    );
    assert_eq!(
        duration_detail(&response, "actual_seconds"),
        Duration::from_millis(37)
    );

    ScenarioOutcome {
        trace: harness.trace(),
        elapsed: harness.elapsed(),
    }
}

#[test]
fn deterministic_advance_is_identical_under_different_host_sleep_patterns() {
    let immediate = run_sleep_invariant_scenario("sleep-none", [Duration::ZERO; 4]);
    let delayed =
        run_sleep_invariant_scenario("sleep-varied", [2, 13, 1, 8].map(Duration::from_millis));

    assert_eq!(immediate, delayed);
    assert_eq!(immediate.elapsed, Duration::from_millis(37));
    assert_eq!(
        immediate.trace.update_deltas,
        vec![
            Duration::from_millis(10),
            Duration::from_millis(10),
            Duration::from_millis(10),
            Duration::from_millis(7),
        ]
    );
    assert_eq!(immediate.trace.fixed_deltas.len(), 5);
    assert_eq!(
        immediate.trace.simulated_nanoseconds,
        Duration::from_millis(37).as_nanos()
    );
}
