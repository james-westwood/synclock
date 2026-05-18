use crate::config::Config;
use crate::state::OutputCommand;
use anyhow::{Context, Result};
use crossbeam_channel::Receiver;
use midir::MidiOutput;
use std::thread;
use tracing::{info, trace, warn};

pub struct CircuitRhythmOutput {
    config: Config,
    command_rx: Receiver<OutputCommand>,
}

impl CircuitRhythmOutput {
    pub fn new(config: Config, command_rx: Receiver<OutputCommand>) -> Self {
        Self { config, command_rx }
    }

    pub fn spawn(self) -> thread::JoinHandle<Result<()>> {
        thread::spawn(move || self.run())
    }

    fn run(&self) -> Result<()> {
        if self.config.dry_run {
            return self.run_dry();
        }

        let midi_out =
            MidiOutput::new("mixxx-midi-clock-output").context("Failed to create MidiOutput")?;

        let port = if let Some(ref pattern) = self.config.output_port {
            crate::ports::find_output_port(&midi_out, pattern)
                .context(format!("Output port matching '{}' not found", pattern))?
        } else {
            crate::ports::auto_discover_output(&midi_out)
                .context("Auto-discovery failed for output port")?
        };

        let port_name = midi_out
            .port_name(&port)
            .unwrap_or_else(|_| "unknown".to_string());
        info!("Connecting to output port: {}", port_name);

        let mut conn = midi_out
            .connect(&port, "mixxx-midi-clock-output-conn")
            .map_err(|e| anyhow::anyhow!("Failed to connect to MIDI output port: {:?}", e))?;

        while let Ok(cmd) = self.command_rx.recv() {
            if self.config.dry_run {
                trace!("[dry-run] Output: {:?}", cmd);
                continue;
            }

            let bytes = match cmd {
                OutputCommand::ClockPulse => {
                    trace!("Sending 0xF8 (ClockPulse)");
                    vec![0xF8]
                }
                OutputCommand::Start => {
                    info!("Sending 0xFA (Start)");
                    vec![0xFA]
                }
                OutputCommand::Stop => {
                    info!("Sending 0xFC (Stop)");
                    vec![0xFC]
                }
                OutputCommand::Continue => {
                    info!("Sending 0xFB (Continue)");
                    vec![0xFB]
                }
            };

            if let Err(e) = conn.send(&bytes) {
                warn!("MIDI send error: {}", e);
            }
        }

        info!("Output thread shutting down");
        Ok(())
    }

    fn run_dry(&self) -> Result<()> {
        trace!("[dry-run] Output thread active — logging commands only");
        while let Ok(cmd) = self.command_rx.recv() {
            trace!("[dry-run] Would send: {:?}", cmd);
        }
        trace!("[dry-run] Output thread shutting down (channel closed)");
        Ok(())
    }
}
