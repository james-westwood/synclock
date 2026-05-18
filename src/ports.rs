use anyhow::{Context, Result};
use midir::{MidiInput, MidiOutput};
use tracing::{info, warn};

pub fn list_ports() -> Result<()> {
    let midi_in = MidiInput::new("mixxx-midi-clock-list").context("Failed to create MidiInput")?;
    let midi_out =
        MidiOutput::new("mixxx-midi-clock-list").context("Failed to create MidiOutput")?;

    println!("MIDI Input ports:");
    for (i, port) in midi_in.ports().iter().enumerate() {
        let name = midi_in
            .port_name(port)
            .unwrap_or_else(|_| format!("unknown-{i}"));
        println!("  [{i}] {name}");
    }

    println!("\nMIDI Output ports:");
    for (i, port) in midi_out.ports().iter().enumerate() {
        let name = midi_out
            .port_name(port)
            .unwrap_or_else(|_| format!("unknown-{i}"));
        println!("  [{i}] {name}");
    }

    Ok(())
}

pub fn find_input_port(midi_in: &MidiInput, pattern: &str) -> Option<midir::MidiInputPort> {
    for port in midi_in.ports() {
        if let Ok(name) = midi_in.port_name(&port) {
            if name.to_lowercase().contains(&pattern.to_lowercase()) {
                info!("Found input port: {}", name);
                return Some(port);
            }
        }
    }
    None
}

pub fn find_output_port(midi_out: &MidiOutput, pattern: &str) -> Option<midir::MidiOutputPort> {
    for port in midi_out.ports() {
        if let Ok(name) = midi_out.port_name(&port) {
            if name.to_lowercase().contains(&pattern.to_lowercase()) {
                info!("Found output port: {}", name);
                return Some(port);
            }
        }
    }
    None
}

pub fn auto_discover_input(midi_in: &MidiInput) -> Option<midir::MidiInputPort> {
    let patterns = &["mixxx_midi_clock"];
    for pat in patterns {
        if let Some(port) = find_input_port(midi_in, pat) {
            return Some(port);
        }
    }
    warn!("Auto-discovery failed for input port; available ports:");
    for (i, port) in midi_in.ports().iter().enumerate() {
        if let Ok(name) = midi_in.port_name(port) {
            warn!("  [{i}] {name}");
        }
    }
    None
}

pub fn auto_discover_output(midi_out: &MidiOutput) -> Option<midir::MidiOutputPort> {
    let patterns = &["Focusrite-Novation", "Circuit Rhythm"];
    for pat in patterns {
        if let Some(port) = find_output_port(midi_out, pat) {
            return Some(port);
        }
    }
    warn!("Auto-discovery failed for output port; available ports:");
    for (i, port) in midi_out.ports().iter().enumerate() {
        if let Ok(name) = midi_out.port_name(port) {
            warn!("  [{i}] {name}");
        }
    }
    None
}
