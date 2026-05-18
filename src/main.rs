mod clock;
mod config;
mod input;
mod output;
mod ports;
mod state;

use anyhow::{Context, Result};
use clap::Parser;
use crossbeam_channel::bounded;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{error, info, warn};

use crate::clock::ClockEngine;
use crate::config::Config;
use crate::input::MixxxMidiInput;
use crate::output::CircuitRhythmOutput;
use crate::state::{DaemonStatus, OutputCommand};

fn main() -> Result<()> {
    let config = Config::parse();

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // --list-ports
    if config.list_ports {
        ports::list_ports()?;
        return Ok(());
    }

    // --stop
    if config.stop {
        return send_stop(&config);
    }

    // --status
    if config.status {
        return print_status(&config);
    }

    info!("Starting mixxx-midi-clock v{}", env!("CARGO_PKG_VERSION"));
    info!("Config: {:?}", config);

    // Bounded channels
    let (input_tx, input_rx) = bounded::<crate::state::InputEvent>(1024);
    let (output_tx, output_rx) = bounded::<OutputCommand>(1024);

    let running = Arc::new(AtomicBool::new(true));
    let status = Arc::new(Mutex::new(DaemonStatus {
        play_state: "Idle".to_string(),
        bpm: config.fallback_bpm,
        phase_error: 0.0,
        input_port: config.input_port.clone(),
        output_port: config.output_port.clone(),
        pulse_count: 0,
    }));

    // Status socket
    let socket_path = config
        .status_socket
        .clone()
        .unwrap_or_else(get_default_socket_path);
    let socket_path_clone = socket_path.clone();
    let status_clone = status.clone();
    let _status_thread = std::thread::spawn(move || {
        if let Err(e) = run_status_server(&socket_path_clone, status_clone) {
            warn!("Status server error: {}", e);
        }
    });

    // Spawn threads
    let input_handle = MixxxMidiInput::new(config.clone(), input_tx, running.clone()).spawn();
    let output_handle = CircuitRhythmOutput::new(config.clone(), output_rx).spawn();

    let clock_engine = ClockEngine::new(
        config.clone(),
        input_rx,
        output_tx,
        status.clone(),
        running.clone(),
    );
    let clock_handle = std::thread::spawn(move || clock_engine.run());

    // Signal handler
    let r = running.clone();
    ctrlc::set_handler(move || {
        info!("Received SIGINT/SIGTERM, shutting down gracefully...");
        r.store(false, Ordering::Relaxed);
    })
    .context("Failed to set Ctrl-C handler")?;

    // Wait for threads
    if let Err(e) = input_handle.join() {
        error!("Input thread panicked: {:?}", e);
    }
    if let Err(e) = clock_handle.join() {
        error!("Clock thread panicked: {:?}", e);
    }
    if let Err(e) = output_handle.join() {
        error!("Output thread panicked: {:?}", e);
    }

    // Cleanup socket
    let _ = std::fs::remove_file(&socket_path);
    info!("Shutdown complete");
    Ok(())
}

fn send_stop(config: &Config) -> Result<()> {
    use midir::MidiOutput;
    let midi_out = MidiOutput::new("mixxx-midi-clock-stop")
        .context("Failed to create MidiOutput for --stop")?;

    let port = if let Some(ref pattern) = config.output_port {
        ports::find_output_port(&midi_out, pattern)
            .context(format!("Output port matching '{}' not found", pattern))?
    } else {
        ports::auto_discover_output(&midi_out).context("Auto-discovery failed for output port")?
    };

    let mut conn = midi_out
        .connect(&port, "mixxx-midi-clock-stop-conn")
        .map_err(|e| anyhow::anyhow!("Failed to connect to output port for stop: {:?}", e))?;

    info!("Sending 0xFC (Stop) to output port");
    conn.send(&[0xFC]).context("Failed to send Stop command")?;
    Ok(())
}

fn print_status(config: &Config) -> Result<()> {
    let socket_path = config
        .status_socket
        .clone()
        .unwrap_or_else(get_default_socket_path);

    if !socket_path.exists() {
        anyhow::bail!(
            "Daemon does not appear to be running (socket not found at {:?})",
            socket_path
        );
    }

    let mut stream =
        UnixStream::connect(&socket_path).context("Failed to connect to status socket")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .context("Failed to set read timeout")?;

    let mut buf = String::new();
    stream
        .read_to_string(&mut buf)
        .context("Failed to read from status socket")?;

    println!("{}", buf);
    Ok(())
}

fn run_status_server(socket_path: &PathBuf, status: Arc<Mutex<DaemonStatus>>) -> Result<()> {
    // Remove stale socket
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path).context("Failed to bind status socket")?;

    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let st = status.lock().unwrap();
                let json = serde_json::to_string_pretty(&*st).unwrap_or_default();
                let _ = stream.write_all(json.as_bytes());
                let _ = stream.write_all(b"\n");
                drop(st);
                // Close stream so client gets EOF
                drop(stream);
            }
            Err(e) => {
                warn!("Status socket accept error: {}", e);
            }
        }
    }
}

fn get_default_socket_path() -> PathBuf {
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/run/user/{}/mixxx-midi-clock.sock", uid))
}
