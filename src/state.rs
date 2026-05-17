use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayState {
    #[default]
    Idle,
    WaitingForBeat,
    Running,
    Stopped,
}

#[derive(Debug, Clone, Copy)]
pub enum InputEvent {
    BpmUpdate { bpm: f64 },
    BeatDistance { value: f64 },
    Playing,
    Stopped,
}

#[derive(Debug, Clone, Copy)]
pub enum OutputCommand {
    ClockPulse,
    Start,
    Stop,
    #[allow(dead_code)]
    Continue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub play_state: String,
    pub bpm: f64,
    pub phase_error: f64,
    pub input_port: Option<String>,
    pub output_port: Option<String>,
    pub pulse_count: u8,
}
