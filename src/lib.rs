//! Agent-friendly input and screenshot control for Bevy apps.
//!
//! `bevy-agent-feedback-plugin` opens a local JSON-lines TCP socket that lets an
//! external Pi/Codex agent press keys, drive desktop-style mouse input, submit
//! text/file-drop events, wait for rendered frames, query primary-window
//! metadata, and capture the primary window. The plugin writes a protocol file
//! with the actual socket address so agents can discover a running app without
//! hard-coded ports.
//!
//! # Quick start
//!
//! ```no_run
//! use bevy::prelude::*;
//! use bevy_agent_feedback_plugin::{AgentFeedbackConfig, AgentFeedbackPlugin};
//! use std::{net::SocketAddr, path::PathBuf};
//!
//! App::new()
//!     .add_plugins(DefaultPlugins)
//!     .add_plugins(AgentFeedbackPlugin::new(AgentFeedbackConfig {
//!         bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
//!         protocol_file: PathBuf::from("target/agent-feedback/agent-feedback.json"),
//!         capture_dir: PathBuf::from("target/agent-feedback/captures"),
//!         ..Default::default()
//!     }))
//!     .run();
//! ```
//!
//! After startup, read the configured protocol file. It contains protocol v2
//! session metadata, `socket_addr`, heartbeat path, capture directory, supported
//! commands, and example JSON requests. Send one newline-terminated JSON object
//! per request.
//!
//! # Protocol
//!
//! Requests have an `id` field and a `command` field. Valid responses echo the
//! `id`, set `ok`, and include either `result` or `error`; malformed requests
//! may return `id: null`.
//!
//! Supported commands include primitives (`key_down`, `mouse_down`,
//! `cursor_move`, `mouse_scroll`, `wait`, `capture`) and safe compound actions
//! (`click`, `drag`, `scroll`, `key_tap`, `key_hold`, `release_all_inputs`,
//! `shutdown`). Key commands target physical `KeyCode` input; apps should read
//! `ButtonInput<KeyCode>` or `KeyboardInput.key_code`. Cursor coordinates are
//! logical pixels in the primary window, with origin at the top-left.
//! Window/capture responses include frame, game time, window metadata, mouse
//! position, and currently agent-held inputs.
//!
//! # Scheduling
//!
//! Agent commands are drained in `PreUpdate` before Bevy's input systems. Normal
//! `Update` systems can read injected `ButtonInput`, accumulated mouse input,
//! window/input messages, and screenshot results.
//!
//! # Limits
//!
//! Queues, wait durations, request line length, captures, and
//! command response time are bounded by [`AgentFeedbackConfig`]. Keep the default
//! localhost bind address unless the control socket is protected from untrusted
//! clients.
//!
//! # Examples
//!
//! See the `examples/` directory for a minimal instrumented app and a
//! self-driving demo that uses the same protocol a Pi/Codex agent would use.

#![warn(missing_docs, rustdoc::broken_intra_doc_links)]

pub mod client;
mod config;
mod control;
#[cfg(feature = "diagnostics")]
mod diagnostics;
mod key_names;
mod protocol;
mod runtime;
mod session;

use bevy::prelude::*;
pub use config::AgentFeedbackConfig;
use control::AgentFeedbackControlPlugin;
#[cfg(feature = "diagnostics")]
pub use diagnostics::AgentFeedbackDiagnosticsPlugin;
use runtime::AgentFeedbackRuntimePlugin;

/// Bevy plugin that exposes local agent input and screenshot control.
///
/// Add this after the plugins that create input, window, and render resources.
/// `DefaultPlugins` is the usual choice for rendered examples.
#[derive(Default)]
pub struct AgentFeedbackPlugin {
    config: AgentFeedbackConfig,
}

impl AgentFeedbackPlugin {
    /// Creates the plugin with explicit runtime configuration.
    pub fn new(config: AgentFeedbackConfig) -> Self {
        Self { config }
    }
}

impl Plugin for AgentFeedbackPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(self.config.clone())
            .add_plugins((AgentFeedbackRuntimePlugin, AgentFeedbackControlPlugin));
    }
}
