mod args;
mod doctor;
mod python_client;
mod runner;

use args::{Command as CliCommand, RunArgs};
use bevy_agent_feedback_plugin::client::{AgentClient, AgentClientConfig};
use python_client::BundledPythonClient;
use serde_json::Value;
use std::{
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitCode, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

fn main() -> ExitCode {
    match real_main() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("bevy-feedback: {error}");
            ExitCode::from(1)
        }
    }
}

fn real_main() -> Result<ExitCode, String> {
    let raw_args = std::env::args().skip(1).collect::<Vec<_>>();
    match args::parse_args(&raw_args)? {
        CliCommand::Help => {
            println!("{}", args::usage());
            Ok(ExitCode::SUCCESS)
        }
        CliCommand::Version => {
            println!("{}", args::version());
            Ok(ExitCode::SUCCESS)
        }
        CliCommand::Doctor(doctor_args) => doctor::run(&doctor_args),
        CliCommand::Run(run_args) => run(run_args).map(|()| ExitCode::SUCCESS),
    }
}

use runner::run;
#[cfg(test)]
use runner::spawn_error;

fn copy_if_exists(from: &Path, to: &Path) {
    if from.exists() {
        let _ = fs::copy(from, to);
    }
}

fn copy_captures(from: &Path, to: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(from) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut pngs = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("png") {
            pngs.push(path);
        }
    }
    pngs.sort();
    if !pngs.is_empty() {
        fs::create_dir_all(to)?;
    }
    let mut copied = Vec::with_capacity(pngs.len());
    for path in pngs {
        if let Some(file_name) = path.file_name() {
            let target = to.join(file_name);
            fs::copy(&path, &target)?;
            copied.push(target);
        }
    }
    Ok(copied)
}

fn print_screenshots(paths: &[PathBuf]) {
    if paths.is_empty() {
        println!("screenshots: none captured");
        return;
    }
    println!("screenshots ({}):", paths.len());
    for path in paths.iter().take(10) {
        println!("  {}", path.display());
    }
    if paths.len() > 10 {
        println!("  +{} more", paths.len() - 10);
    }
}

#[cfg(test)]
mod tests;
