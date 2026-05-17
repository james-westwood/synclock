use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "mixxx-midi-clock",
    about = "Bridge Mixxx BPM/beat_distance to MIDI clock for Circuit Rhythm",
    version
)]
pub struct Config {
    /// Substring or index of ALSA MIDI input port (Mixxx output)
    #[arg(long, env = "MIXXX_MIDI_CLOCK_INPUT_PORT")]
    pub input_port: Option<String>,

    /// Substring or index of ALSA MIDI output port (Circuit Rhythm input)
    #[arg(long, env = "MIXXX_MIDI_CLOCK_OUTPUT_PORT")]
    pub output_port: Option<String>,

    /// Fallback BPM when no Mixxx signal
    #[arg(long, default_value = "120.0")]
    pub fallback_bpm: f64,

    /// Phase correction gain (0.0–1.0)
    #[arg(long, default_value = "0.15")]
    pub phase_gain: f64,

    /// List all ALSA MIDI ports and exit
    #[arg(long)]
    pub list_ports: bool,

    /// Dry-run mode: log but do not send MIDI
    #[arg(long)]
    pub dry_run: bool,

    /// Send MIDI Stop (0xFC) to output port and exit
    #[arg(long)]
    pub stop: bool,

    /// Print daemon status JSON via Unix domain socket and exit
    #[arg(long)]
    pub status: bool,

    /// Unix domain socket path for status server
    #[arg(long)]
    pub status_socket: Option<PathBuf>,
}

impl Config {
    pub fn clamp_bpm(&self, bpm: f64) -> f64 {
        bpm.clamp(30.0, 300.0)
    }

    pub fn tick_interval_ns(bpm: f64) -> u64 {
        (60_000_000_000.0 / (bpm * 24.0)) as u64
    }

    pub fn beat_duration_ns(bpm: f64) -> u64 {
        (60_000_000_000.0 / bpm) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_interval_ns_computes_correctly_for_120_bpm() {
        assert_eq!(Config::tick_interval_ns(120.0), 20_833_333);
    }

    #[test]
    fn tick_interval_ns_computes_correctly_for_160_bpm() {
        assert_eq!(Config::tick_interval_ns(160.0), 15_625_000);
    }

    #[test]
    fn tick_interval_ns_computes_correctly_for_60_bpm() {
        assert_eq!(Config::tick_interval_ns(60.0), 41_666_666);
    }

    #[test]
    fn clamp_bpm_limits_to_30_300() {
        let config = Config {
            input_port: None,
            output_port: None,
            fallback_bpm: 120.0,
            phase_gain: 0.15,
            list_ports: false,
            dry_run: false,
            stop: false,
            status: false,
            status_socket: None,
        };
        assert_eq!(config.clamp_bpm(20.0), 30.0);
        assert_eq!(config.clamp_bpm(400.0), 300.0);
        assert_eq!(config.clamp_bpm(120.0), 120.0);
    }
}
