//! Agent-friendly input, timing, diagnostics, and screenshot control for Bevy apps.
//!
//! `bevy-agent-feedback-plugin` exposes a bounded local protocol for input,
//! semantic game readiness, deterministic Bevy time, and completion-confirmed
//! primary-window PNGs. Protocol readiness means only that the socket exists;
//! animated games should wait on a registered predicate/target and then capture
//! after one app update. Strict pixel stability is appropriate only for static
//! content or a bounded static region.
//!
//! # Quick start
//!
//! Add feedback after [`DefaultPlugins`] (or after the plugins that provide
//! Bevy time, window, render, and input resources):
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
//!         deterministic_time: true,
//!         ..Default::default()
//!     }))
//!     .run();
//! ```
//!
//! The generated protocol file is authoritative for the 0.5 socket, heartbeat,
//! commands, deterministic-time mode, and caps. The wire envelope remains
//! `{"id":...,"command":...}` with responses containing `ok` and either
//! `result` or `error`.
//!
//! # Timing, capture, and diagnostics
//!
//! Public clients call `wait_frames` for app-update progress; the compatibility
//! wire command remains `"wait"`. App updates are neither elapsed gameplay time
//! nor compositor-presented frames. `wait_seconds` observes normal Bevy virtual
//! time and is rejected while deterministic time is frozen. `advance_time`
//! progresses deterministic Bevy virtual/fixed time in bounded nominal steps.
//! Deterministic mode does not control direct [`std::time::Instant`], OS/network
//! clocks, unseeded RNG, or other external state.
//!
//! `capture_after_frames` and `wait_until_first_capture` return metadata for
//! screenshot readback: sequence/path/label, request/completion app-update
//! frames, physical image dimensions, request/completion window state, and
//! `screenshot_captured` completion. Readback and PNG persistence do not prove
//! that the OS/window compositor presented the image.
//!
//! With the `diagnostics` feature, `AgentFeedbackDiagnosticsPlugin` explicitly
//! registers states, scalar resource fields, and marker components for
//! predicates and semantic targets. Exact duplicate Name/accessibility/marker
//! targets error rather than choosing the first. Input/target coordinates are
//! logical window pixels; PNG masks and crop regions are physical image pixels.
//!
//! # Scheduling and limits
//!
//! Agent commands enter in `PreUpdate` before Bevy input consumers. Screenshot
//! responses complete only after Bevy render readback. Queues, request lines,
//! waits, actions, time advances, diagnostics scans, captures, and response time
//! are bounded by [`AgentFeedbackConfig`] and advertised in the protocol file.
//! Keep the localhost bind unless the socket is protected from untrusted clients.
//!
//! # Canonical workflow
//!
//! See [`skills/driving-bevy-games/SKILL.md`](https://github.com/njfio/bevy-agent-feedback-plugin/blob/main/skills/driving-bevy-games/SKILL.md)
//! for setup, readiness selection, semantic waits, input defaults, visual
//! fallbacks, and artifact handling. See `examples/agent_driven.rs` for the
//! self-driving Rust client.

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
pub use protocol::DiagnosticValue;
#[cfg(feature = "diagnostics")]
pub(crate) use protocol::{
    ComparisonOperator, ObservedPredicate, Predicate, PredicateOutcome, TargetKind, TargetSelector,
};
use runtime::AgentFeedbackRuntimePlugin;

/// Bevy plugin that exposes local agent input and screenshot control.
///
/// Add this after [`bevy::time::TimePlugin`] and the plugins that create input,
/// window, and render resources. [`DefaultPlugins`] is the usual choice.
/// Deterministic time requires those time resources to exist before this plugin.
/// Add `AgentFeedbackDiagnosticsPlugin` separately for registered semantic
/// diagnostics when the `diagnostics` feature is enabled.
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
