use super::*;
use std::collections::{HashSet, VecDeque};

const TRANSCRIPT_TAIL_MAX_BYTES: u64 = 256 * 1024;
const TRANSCRIPT_LINE_MAX_BYTES: usize = 64 * 1024;
const LOG_TAIL_MAX_BYTES: u64 = 64 * 1024;
const LOG_CANDIDATE_LINES: usize = 100;
const LOG_TAIL_LINES: usize = 20;
const SEMANTIC_EVIDENCE_MAX_BYTES: usize = 8 * 1024;

pub(super) fn render_failure(failure: &RunFailure, summary: &RunSummary) -> String {
    let evidence = read_transcript_evidence(&summary.artifacts.transcript).unwrap_or_default();
    let semantic = evidence
        .semantic
        .as_ref()
        .map(render_semantic_evidence)
        .unwrap_or_else(|| "unavailable".to_string());
    let capture = evidence
        .failure_capture
        .or_else(|| semantic_capture_path(evidence.semantic.as_ref()))
        .or_else(|| newest_png(&summary.artifacts.screenshots));
    let log_tail = deduplicated_log_tail(
        &summary.artifacts.game_log,
        summary.artifacts.driver_log.as_deref(),
        evidence.semantic.as_ref(),
    );

    let mut report = format!(
        "[{}] {}\nphase: {}\nsemantic evidence: {}\n",
        failure.code, failure.message, failure.phase, semantic
    );
    match capture {
        Some(path) => report.push_str(&format!("capture: {}\n", path.display())),
        None => report.push_str("capture: unavailable\n"),
    }
    if log_tail.is_empty() {
        report.push_str("log tail: empty\n");
    } else {
        report.push_str("log tail (deduplicated, max 20):\n");
        for line in log_tail {
            report.push_str("  ");
            report.push_str(&line);
            report.push('\n');
        }
    }
    report.push_str("artifacts:\n");
    report.push_str(&format!(
        "  run summary: {}\n",
        summary.artifacts.run_summary.display()
    ));
    if let Some(path) = &summary.artifacts.prepare_log {
        report.push_str(&format!("  prepare log: {}\n", path.display()));
    }
    report.push_str(&format!(
        "  game log: {}\n",
        summary.artifacts.game_log.display()
    ));
    if let Some(path) = &summary.artifacts.driver_log {
        report.push_str(&format!("  driver log: {}\n", path.display()));
    }
    report.push_str(&format!(
        "  transcript: {}\n  screenshots: {}\n",
        summary.artifacts.transcript.display(),
        summary.artifacts.screenshots.display()
    ));
    report.push_str(&format!(
        "teardown: inputs={} shutdown={} socket={} child={} forced={}\n",
        summary.teardown.input_release,
        summary.teardown.shutdown_acknowledgment,
        summary.teardown.socket_closure,
        summary.teardown.child_exit,
        summary.teardown.forced_termination
    ));
    report
}

#[derive(Default)]
struct TranscriptEvidence {
    semantic: Option<Value>,
    failure_capture: Option<PathBuf>,
}

fn read_transcript_evidence(path: &Path) -> io::Result<TranscriptEvidence> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let offset = length.saturating_sub(TRANSCRIPT_TAIL_MAX_BYTES);
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::with_capacity(usize::try_from(length - offset).unwrap_or(0));
    file.take(TRANSCRIPT_TAIL_MAX_BYTES)
        .read_to_end(&mut bytes)?;

    let mut evidence = TranscriptEvidence::default();
    let mut latest_error = None;
    let mut latest_details = None;
    for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        if (offset > 0 && index == 0) || line.is_empty() || line.len() > TRANSCRIPT_LINE_MAX_BYTES {
            continue;
        }
        let Ok(envelope) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let Some(response) = envelope.get("response") else {
            continue;
        };
        if let Some(context) = response
            .pointer("/error/context")
            .filter(|value| !value.is_null())
        {
            if let Some(path) = semantic_capture_path(Some(context)) {
                evidence.failure_capture = Some(path);
            }
            latest_error = Some(context.clone());
        }
        if response
            .pointer("/result/capture/label")
            .and_then(Value::as_str)
            == Some("semantic-wait-failure")
            && let Some(path) = response
                .pointer("/result/capture/path")
                .and_then(Value::as_str)
        {
            evidence.failure_capture = Some(PathBuf::from(path));
        }
        let Some(status) = response.pointer("/result/status").and_then(Value::as_str) else {
            continue;
        };
        let diagnostic_status = matches!(
            status,
            "target_info"
                | "input_dispatched"
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
                        "ecs_summary"
                            | "list_entities"
                            | "camera_info"
                            | "state_info"
                            | "marker_info"
                    )
                });
        if (diagnostic_status || diagnostic_request)
            && let Some(details) = response
                .pointer("/result/details")
                .filter(|value| !value.is_null())
        {
            latest_details = Some(details.clone());
        }
    }
    evidence.semantic = latest_error.or(latest_details);
    Ok(evidence)
}

fn semantic_capture_path(context: Option<&Value>) -> Option<PathBuf> {
    context
        .and_then(|value| {
            value
                .pointer("/failure_capture/path")
                .or_else(|| value.pointer("/latest_capture/path"))
        })
        .and_then(Value::as_str)
        .map(PathBuf::from)
}

fn render_semantic_evidence(context: &Value) -> String {
    let json = serde_json::to_string(context).unwrap_or_else(|_| "unavailable".to_string());
    if json.len() <= SEMANTIC_EVIDENCE_MAX_BYTES {
        return json;
    }
    let empty_wrapper = serde_json::to_string(&serde_json::json!({
        "truncated": true,
        "context_json_prefix": "",
    }))
    .unwrap_or_default();
    let prefix_limit =
        SEMANTIC_EVIDENCE_MAX_BYTES.saturating_sub(empty_wrapper.len()) / "\\u0000".len();
    let boundary = json.floor_char_boundary(prefix_limit.min(json.len()));
    serde_json::to_string(&serde_json::json!({
        "truncated": true,
        "context_json_prefix": &json[..boundary],
    }))
    .unwrap_or_else(|_| "unavailable".to_string())
}

fn deduplicated_log_tail(
    game_log: &Path,
    driver_log: Option<&Path>,
    semantic: Option<&Value>,
) -> Vec<String> {
    let semantic_json = semantic.and_then(|value| serde_json::to_string(value).ok());
    let mut candidates = Vec::with_capacity(LOG_CANDIDATE_LINES * 2);
    append_log_candidates(&mut candidates, "game", game_log);
    if let Some(path) = driver_log {
        append_log_candidates(&mut candidates, "driver", path);
    }

    let mut seen = HashSet::with_capacity(candidates.len());
    let mut tail = VecDeque::with_capacity(LOG_TAIL_LINES);
    for (source, line) in candidates {
        let normalized = normalize_log_line(&line);
        if normalized.is_empty()
            || normalized.contains("diagnostic context:")
            || semantic_json
                .as_deref()
                .is_some_and(|json| normalized.contains(json))
            || !seen.insert(normalized.clone())
        {
            continue;
        }
        if tail.len() == LOG_TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(format!("[{source}] {normalized}"));
    }
    tail.into_iter().collect()
}

fn append_log_candidates(
    candidates: &mut Vec<(&'static str, String)>,
    source: &'static str,
    path: &Path,
) {
    let Ok(lines) = tail_lines(path, LOG_CANDIDATE_LINES) else {
        return;
    };
    candidates.extend(lines.into_iter().map(|line| (source, line)));
}

fn tail_lines(path: &Path, max_lines: usize) -> io::Result<Vec<String>> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let offset = length.saturating_sub(LOG_TAIL_MAX_BYTES);
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::with_capacity(usize::try_from(length - offset).unwrap_or(0));
    file.take(LOG_TAIL_MAX_BYTES).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.lines().map(str::to_string).collect::<Vec<_>>();
    if offset > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    let keep_from = lines.len().saturating_sub(max_lines);
    Ok(lines.drain(keep_from..).collect())
}

fn normalize_log_line(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut plain = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'[') {
            index += 2;
            while index < bytes.len() {
                let byte = bytes[index];
                index += 1;
                if (0x40..=0x7e).contains(&byte) {
                    break;
                }
            }
        } else {
            plain.push(bytes[index]);
            index += 1;
        }
    }
    let plain = String::from_utf8_lossy(&plain);
    let plain = plain.trim();
    let plain = plain
        .split_once(char::is_whitespace)
        .filter(|(token, _)| looks_like_timestamp(token))
        .map_or(plain, |(_, rest)| rest);
    let mut normalized = String::with_capacity(plain.len());
    for word in plain.split_whitespace() {
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(word);
    }
    normalized
}

fn looks_like_timestamp(token: &str) -> bool {
    token.len() >= 8
        && token.contains(':')
        && token.contains('-')
        && (token.contains('T')
            || token
                .chars()
                .take(4)
                .all(|character| character.is_ascii_digit()))
}

fn newest_png(dir: &Path) -> Option<PathBuf> {
    let mut newest = None;
    for entry in fs::read_dir(dir).ok()?.take(10_000).flatten() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_ansi_timestamps_and_whitespace() {
        assert_eq!(
            normalize_log_line("\u{1b}[2m2026-07-10T10:11:12Z\u{1b}[0m  INFO   game: ready"),
            "INFO game: ready"
        );
    }

    #[test]
    fn semantic_evidence_is_bounded_and_remains_json() {
        let context =
            serde_json::json!({"message": format!("prefix-{}-suffix", "x".repeat(20 * 1024))});

        let rendered = render_semantic_evidence(&context);

        assert!(rendered.len() <= SEMANTIC_EVIDENCE_MAX_BYTES);
        assert!(rendered.contains("\"truncated\":true"));
        assert!(rendered.contains("prefix-"));
        assert!(!rendered.contains("-suffix"));
        serde_json::from_str::<Value>(&rendered).expect("valid truncated JSON");
    }

    #[test]
    fn failure_report_is_ordered_deduplicated_and_prefers_post_failure_capture() {
        let root = std::env::temp_dir().join(format!(
            "bevy-feedback-evidence-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let screenshots = root.join("screenshots");
        fs::create_dir_all(&screenshots).expect("screenshots");
        let game_log = root.join("game.log");
        let driver_log = root.join("driver.log");
        let transcript = root.join("transcript.jsonl");
        fs::write(
            &game_log,
            "2026-07-10T10:00:00Z INFO game: loading\n2026-07-10T10:00:01Z WARN retry\n",
        )
        .expect("game log");
        fs::write(
            &driver_log,
            "\u{1b}[33m2026-07-10T10:00:02Z\u{1b}[0m WARN retry\ndiagnostic context: repeated\nassertion failed\n",
        )
        .expect("driver log");
        let context = serde_json::json!({
            "latest_capture": {"path": "/captures/loading.png"},
            "observed_predicate": {"outcome": "not_matched", "value": 17}
        });
        let envelopes = [
            serde_json::json!({"response": {"error": {"context": context}}}),
            serde_json::json!({
                "response": {
                    "result": {
                        "status": "captured",
                        "capture": {
                            "label": "semantic-wait-failure",
                            "path": "/captures/semantic-wait-failure.png"
                        }
                    }
                }
            }),
        ];
        let mut transcript_bytes = Vec::new();
        for envelope in envelopes {
            serde_json::to_writer(&mut transcript_bytes, &envelope).expect("envelope");
            transcript_bytes.push(b'\n');
        }
        fs::write(&transcript, transcript_bytes).expect("transcript");

        let summary_path = root.join("run-summary.json");
        let summary = RunSummary {
            schema_version: 1,
            result: RunResult {
                success: false,
                code: "driver_nonzero_exit",
                message: "driver exited with status 7".to_string(),
            },
            phase: "driver",
            elapsed_ms: 1,
            timings_ms: std::collections::BTreeMap::new(),
            launch: LaunchSummary {
                prepare_command: None,
                game_command: vec!["game".to_string()],
                driver_command: Some(vec!["driver".to_string()]),
                caller_cwd: root.clone(),
                game_cwd: root.clone(),
            },
            artifacts: ArtifactSummary {
                directory: root.clone(),
                run_summary: summary_path.clone(),
                failure_summary: Some(root.join("failure-summary.txt")),
                game_log: game_log.clone(),
                prepare_log: None,
                driver_log: Some(driver_log.clone()),
                protocol: root.join("protocol.json"),
                transcript: transcript.clone(),
                screenshots: screenshots.clone(),
            },
            window: WindowSummary {
                required_logical: None,
                actual: None,
            },
            process_exit: "nonzero",
            teardown: TeardownSummary {
                input_release: "acknowledged",
                shutdown_acknowledgment: "acknowledged",
                socket_closure: "closed",
                child_exit: "exited",
                forced_termination: false,
            },
            warnings: Vec::new(),
        };
        let failure = RunFailure {
            phase: "driver",
            code: "driver_nonzero_exit",
            message: "driver exited with status 7".to_string(),
        };

        let report = render_failure(&failure, &summary);
        let expected = format!(
            "[driver_nonzero_exit] driver exited with status 7\nphase: driver\nsemantic evidence: {{\"latest_capture\":{{\"path\":\"/captures/loading.png\"}},\"observed_predicate\":{{\"outcome\":\"not_matched\",\"value\":17}}}}\ncapture: /captures/semantic-wait-failure.png\nlog tail (deduplicated, max 20):\n  [game] INFO game: loading\n  [game] WARN retry\n  [driver] assertion failed\nartifacts:\n  run summary: {}\n  game log: {}\n  driver log: {}\n  transcript: {}\n  screenshots: {}\nteardown: inputs=acknowledged shutdown=acknowledged socket=closed child=exited forced=false\n",
            summary_path.display(),
            game_log.display(),
            driver_log.display(),
            transcript.display(),
            screenshots.display()
        );
        assert_eq!(report, expected);
        assert!(!report.contains("diagnostic context: repeated"));
        assert_eq!(report.matches("WARN retry").count(), 1);
        let _ = fs::remove_dir_all(root);
    }
}
