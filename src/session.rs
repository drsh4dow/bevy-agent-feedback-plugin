use crate::config::AgentFeedbackConfig;
use bevy::prelude::Resource;
use serde_json::Value;
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(crate) const PROTOCOL_VERSION: &str = "bevy-agent-feedback/3";

#[derive(Clone, Debug, Resource)]
pub(crate) struct AgentFeedbackSession {
    pub(crate) session_id: String,
    pub(crate) pid: u32,
    pub(crate) started_at_unix_ms: u128,
    pub(crate) heartbeat_file: PathBuf,
    pub(crate) heartbeat_interval: Duration,
    pub(crate) stale_after: Duration,
}

impl AgentFeedbackSession {
    pub(crate) fn new(config: &AgentFeedbackConfig) -> Self {
        let started_at_unix_ms = unix_ms();
        let pid = std::process::id();
        Self {
            session_id: format!("{pid}-{started_at_unix_ms}"),
            pid,
            started_at_unix_ms,
            heartbeat_file: heartbeat_file(&config.protocol_file),
            heartbeat_interval: clamp_duration(
                config.heartbeat_interval,
                Duration::from_millis(50),
                Duration::from_secs(5),
            ),
            stale_after: clamp_duration(
                config.session_stale_after,
                Duration::from_millis(250),
                Duration::from_secs(120),
            ),
        }
    }

    pub(crate) fn write_heartbeat(&self) -> io::Result<()> {
        if let Some(parent) = self.heartbeat_file.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.heartbeat_file, unix_ms().to_string())
    }

    pub(crate) fn cleanup(&self, protocol_file: &Path) {
        if protocol_belongs_to_session(protocol_file, &self.session_id) {
            let _ = fs::remove_file(protocol_file);
        }
        let _ = fs::remove_file(&self.heartbeat_file);
    }
}

pub(crate) fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn heartbeat_file(protocol_file: &Path) -> PathBuf {
    let file_name = protocol_file
        .file_name()
        .and_then(|file_name| file_name.to_str())
        .unwrap_or("agent-feedback.json");
    protocol_file.with_file_name(format!("{file_name}.heartbeat"))
}

fn clamp_duration(value: Duration, min: Duration, max: Duration) -> Duration {
    value.max(min).min(max)
}

fn protocol_belongs_to_session(protocol_file: &Path, session_id: &str) -> bool {
    let Ok(bytes) = fs::read(protocol_file) else {
        return false;
    };
    let Ok(protocol) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    protocol["session_id"] == session_id
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    #[test]
    fn cleanup_only_removes_current_session_protocol() {
        let root = std::env::temp_dir().join(format!("bevy-agent-session-{}", unix_ms()));
        let protocol_file = root.join("agent.json");
        let config = AgentFeedbackConfig {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            protocol_file: protocol_file.clone(),
            ..Default::default()
        };
        let session = AgentFeedbackSession::new(&config);
        fs::create_dir_all(&root).expect("temp root");
        fs::write(&protocol_file, r#"{"session_id":"other"}"#).expect("protocol");
        session.write_heartbeat().expect("heartbeat");

        session.cleanup(&protocol_file);

        assert!(protocol_file.exists());
        assert!(!session.heartbeat_file.exists());
        let _ = fs::remove_dir_all(root);
    }
}
