use crate::config::Config;
use crate::state::InputEvent;
use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use midir::{Ignore, MidiInput};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{trace, warn};

pub struct MixxxMidiInput {
    config: Config,
    event_tx: Sender<InputEvent>,
}

impl MixxxMidiInput {
    pub fn new(config: Config, event_tx: Sender<InputEvent>) -> Self {
        Self { config, event_tx }
    }

    pub fn spawn(self) -> thread::JoinHandle<Result<()>> {
        thread::spawn(move || self.run())
    }

    fn run(&self) -> Result<()> {
        let mut midi_in =
            MidiInput::new("mixxx-midi-clock-input").context("Failed to create MidiInput")?;
        midi_in.ignore(Ignore::None);

        let port = if let Some(ref pattern) = self.config.input_port {
            crate::ports::find_input_port(&midi_in, pattern)
                .context(format!("Input port matching '{}' not found", pattern))?
        } else {
            crate::ports::auto_discover_input(&midi_in)
                .context("Auto-discovery failed for input port")?
        };

        let port_name = midi_in
            .port_name(&port)
            .unwrap_or_else(|_| "unknown".to_string());
        trace!("Connecting to input port: {}", port_name);

        let last_msg_time = Arc::new(Mutex::new(Instant::now()));
        let last_msg_time_cb = last_msg_time.clone();
        let event_tx = self.event_tx.clone();

        let _conn = midi_in
            .connect(
                &port,
                "mixxx-midi-clock-input-conn",
                move |_stamp, msg, _| {
                    *last_msg_time_cb.lock().unwrap() = Instant::now();
                    decode_message(msg, &event_tx);
                },
                (),
            )
            .map_err(|e| anyhow::anyhow!("Failed to connect to MIDI input port: {:?}", e))?;

        // Watchdog loop
        loop {
            std::thread::sleep(Duration::from_millis(100));
            let last = *last_msg_time.lock().unwrap();
            if last.elapsed() > Duration::from_secs(5) {
                warn!("Input watchdog: no message for 5s, emitting Stopped");
                let _ = self.event_tx.send(InputEvent::Stopped);
                // Reset to avoid spam; if truly disconnected it'll re-trigger in 5s
                *last_msg_time.lock().unwrap() = Instant::now();
            }
        }
    }
}

fn decode_message(msg: &[u8], tx: &Sender<InputEvent>) {
    trace!("Raw MIDI: {:02x?}", msg);
    for event in decode_events(msg) {
        let _ = tx.send(event);
    }
}

fn decode_events(msg: &[u8]) -> Vec<InputEvent> {
    if msg.len() < 3 {
        return vec![];
    }

    let status = msg[0];
    let data1 = msg[1];
    let data2 = msg[2];

    match status {
        0xEA => {
            let bpm = (data1 as f64 + 60.0) + (data2 as f64 / 100.0);
            let clamped = bpm.clamp(30.0, 300.0);
            trace!("Decoded Pitch Bend: bpm={:.2}", clamped);
            vec![InputEvent::BpmUpdate { bpm: clamped }, InputEvent::Playing]
        }
        0x9A => {
            if data1 == 0x77 {
                let beat_distance = 1.0 - (data2 as f64 / 127.0);
                trace!("Decoded Note On: beat_distance={:.4}", beat_distance);
                vec![
                    InputEvent::BeatDistance {
                        value: beat_distance,
                    },
                    InputEvent::Playing,
                ]
            } else {
                vec![]
            }
        }
        0x8A => {
            if data1 == 0x77 {
                trace!("Decoded Note Off: stopped");
                vec![InputEvent::Stopped]
            } else {
                vec![]
            }
        }
        _ => {
            trace!("Unknown MIDI status: 0x{:02x}", status);
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_pitch_bend_message_returns_correct_bpm() {
        // data1=60, data2=0 => BPM = 60 + 60 + 0 = 120
        let msg = vec![0xEA, 60, 0];
        let events = decode_events(&msg);
        assert!(matches!(events[0], InputEvent::BpmUpdate { bpm: 120.0 }));
    }

    #[test]
    fn decode_pitch_bend_clamps_to_30_300() {
        // data1=255 would be 315 BPM, should clamp to 300
        let msg = vec![0xEA, 255, 50];
        let events = decode_events(&msg);
        assert!(matches!(events[0], InputEvent::BpmUpdate { bpm: 300.0 }));

        // Negative would be < 30, clamp to 30
        // data1=0, data2=0 => 60.0, that's fine. Need data1 that gives <30
        // BPM = data1 + 60 + data2/100. Minimum is 60.0. So clamp only affects upper bound in practice.
        // Let's test a very high value
        let msg = vec![0xEA, 250, 99];
        let events = decode_events(&msg);
        if let InputEvent::BpmUpdate { bpm } = events[0] {
            assert!(bpm <= 300.0);
        } else {
            panic!("Expected BpmUpdate");
        }
    }

    #[test]
    fn decode_note_on_returns_inverted_beat_distance() {
        // velocity 0 => beat_distance = 1.0
        let msg = vec![0x9A, 0x77, 0];
        let events = decode_events(&msg);
        assert!(matches!(events[0], InputEvent::BeatDistance { value: 1.0 }));

        // velocity 127 => beat_distance ~0.0
        let msg = vec![0x9A, 0x77, 127];
        let events = decode_events(&msg);
        if let InputEvent::BeatDistance { value } = events[0] {
            assert!(value < 0.01);
        } else {
            panic!("Expected BeatDistance");
        }

        // velocity 64 => beat_distance ~0.496
        let msg = vec![0x9A, 0x77, 64];
        let events = decode_events(&msg);
        if let InputEvent::BeatDistance { value } = events[0] {
            assert!((value - 0.496).abs() < 0.01);
        } else {
            panic!("Expected BeatDistance");
        }
    }

    #[test]
    fn decode_note_off_returns_stopped_event() {
        let msg = vec![0x8A, 0x77, 0];
        let events = decode_events(&msg);
        assert!(matches!(events[0], InputEvent::Stopped));
    }
}
