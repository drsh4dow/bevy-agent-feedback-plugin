use super::ClientError;
use crate::session::{PROTOCOL_VERSION, unix_ms};
use serde::Deserialize;
use serde_json::Value;
use std::{
    fs, io,
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Debug, Deserialize)]
pub(super) struct ProtocolFile {
    pub(super) socket_addr: SocketAddr,
    pid: u32,
    heartbeat_file: PathBuf,
    stale_after_ms: u64,
    #[serde(default = "default_max_wait_frames")]
    pub(super) max_wait_frames: u16,
}

const DEFAULT_MAX_WAIT_FRAMES: u16 = 300;

fn default_max_wait_frames() -> u16 {
    DEFAULT_MAX_WAIT_FRAMES
}

pub(super) fn read_protocol(path: &Path) -> Result<ProtocolFile, ClientError> {
    let bytes = fs::read(path).map_err(|error| {
        ClientError::Protocol(format!(
            "failed to read protocol file {}: {error}",
            path.display()
        ))
    })?;
    let value: Value = serde_json::from_slice(&bytes)?;
    let Some(version) = value["protocol"].as_str() else {
        return Err(ClientError::Protocol(format!(
            "unknown protocol file {}; missing protocol, expected {PROTOCOL_VERSION}",
            path.display()
        )));
    };
    if version != PROTOCOL_VERSION {
        return Err(ClientError::Protocol(format!(
            "unsupported protocol '{version}'; expected {PROTOCOL_VERSION}"
        )));
    }
    let protocol: ProtocolFile = serde_json::from_value(value)?;
    if !process_alive(protocol.pid) {
        return Err(ClientError::Protocol(format!(
            "protocol stale: process {} is not alive",
            protocol.pid
        )));
    }
    let heartbeat = fs::read_to_string(&protocol.heartbeat_file).map_err(|error| {
        ClientError::Protocol(format!(
            "protocol stale: failed to read heartbeat {}: {error}",
            protocol.heartbeat_file.display()
        ))
    })?;
    let heartbeat_ms = heartbeat.trim().parse::<u128>().map_err(|error| {
        ClientError::Protocol(format!("protocol stale: heartbeat is invalid: {error}"))
    })?;
    let age = unix_ms().saturating_sub(heartbeat_ms);
    if age > u128::from(protocol.stale_after_ms) {
        return Err(ClientError::Protocol(format!(
            "protocol stale: heartbeat is {age} ms old, stale after {} ms",
            protocol.stale_after_ms
        )));
    }
    Ok(protocol)
}

pub(super) fn socket_error(error: io::Error, socket_addr: &SocketAddr) -> ClientError {
    if error.kind() == io::ErrorKind::ConnectionRefused {
        ClientError::Protocol(format!(
            "socket refused at {socket_addr}; game probably exited"
        ))
    } else {
        ClientError::Io(format!("connect {socket_addr}: {error}"))
    }
}

fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        Path::new("/proc").join(pid.to_string()).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}
