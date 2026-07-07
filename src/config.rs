use bevy::prelude::Resource;
use std::{net::SocketAddr, path::PathBuf, time::Duration};

/// Runtime configuration for [`crate::AgentFeedbackPlugin`].
///
/// Defaults bind to localhost on port `15712`, write `agent-feedback.json`,
/// keep captures in `agent-feedback-captures`, and refresh a heartbeat for v2
/// stale-session checks.
#[derive(Clone, Debug, Resource)]
pub struct AgentFeedbackConfig {
    /// TCP address used by the local JSON-lines control socket.
    ///
    /// Use port `0` to let the operating system choose a free port. The chosen
    /// address is written to [`protocol_file`](Self::protocol_file).
    pub bind_addr: SocketAddr,

    /// JSON file written at startup so Pi/Codex agents can discover the socket.
    pub protocol_file: PathBuf,

    /// Directory where `capture` commands write PNG screenshots.
    pub capture_dir: PathBuf,

    /// Maximum queued agent commands drained per frame.
    pub max_pending_commands: usize,

    /// Maximum accepted frame count for a single `wait` command.
    pub max_wait_frames: u16,

    /// Maximum accepted step count for compound actions such as `drag`.
    pub max_action_steps: u16,

    /// How often the plugin refreshes the session heartbeat file.
    pub heartbeat_interval: Duration,

    /// How old the heartbeat may be before clients reject the session as stale.
    pub session_stale_after: Duration,

    /// Maximum number of retained PNG captures.
    ///
    /// Older capture files created by this plugin are removed after this limit.
    pub max_captures: usize,

    /// Maximum time a socket client waits for the game to answer a command.
    pub command_timeout: Duration,
}

impl Default for AgentFeedbackConfig {
    fn default() -> Self {
        Self {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 15712)),
            protocol_file: PathBuf::from("agent-feedback.json"),
            capture_dir: PathBuf::from("agent-feedback-captures"),
            max_pending_commands: 32,
            max_wait_frames: 300,
            max_action_steps: 120,
            heartbeat_interval: Duration::from_millis(250),
            session_stale_after: Duration::from_secs(3),
            max_captures: 32,
            command_timeout: Duration::from_secs(10),
        }
    }
}
