use crate::config::Config;
use crate::state::{DaemonStatus, InputEvent, OutputCommand, PlayState};
use crossbeam_channel::{Receiver, Sender, TryRecvError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, info, trace, warn};

const BPM_TIMEOUT: Duration = Duration::from_secs(3);
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(2);

/// Compute phase correction in nanoseconds.
/// Positive return value = slow down (add to next tick deadline).
/// Negative return value = speed up (subtract from next tick deadline).
pub fn compute_phase_correction(
    phase_error: f64,
    beat_duration_ns: f64,
    tick_interval_ns: u64,
    gain: f64,
) -> i64 {
    // Invert sign: positive phase_error (behind Mixxx) -> negative correction (speed up)
    let raw_correction = -phase_error * beat_duration_ns * gain;
    let max_correction = (beat_duration_ns * 0.05).min(tick_interval_ns as f64 - 1.0);
    let capped = raw_correction.clamp(-max_correction, max_correction);
    capped as i64
}

pub struct ClockEngine {
    bpm: f64,
    play_state: PlayState,
    pulse_count: u8,
    last_beat_distance: f64,
    last_mixxx_msg_time: Instant,
    last_bpm_update_time: Option<Instant>,
    last_coast_warn: Option<Instant>,
    next_tick: Instant,
    tick_interval_ns: u64,
    config: Config,
    input_rx: Receiver<InputEvent>,
    output_tx: Sender<OutputCommand>,
    status: Arc<Mutex<DaemonStatus>>,
    running: Arc<AtomicBool>,
}

impl ClockEngine {
    pub fn new(
        config: Config,
        input_rx: Receiver<InputEvent>,
        output_tx: Sender<OutputCommand>,
        status: Arc<Mutex<DaemonStatus>>,
        running: Arc<AtomicBool>,
    ) -> Self {
        let bpm = config.fallback_bpm;
        let tick_interval_ns = Config::tick_interval_ns(bpm);
        Self {
            bpm,
            play_state: PlayState::Idle,
            pulse_count: 0,
            last_beat_distance: 0.0,
            last_mixxx_msg_time: Instant::now(),
            last_bpm_update_time: None,
            last_coast_warn: None,
            next_tick: Instant::now(),
            tick_interval_ns,
            config,
            input_rx,
            output_tx,
            status,
            running,
        }
    }

    pub fn run(mut self) {
        info!("ClockEngine started: BPM={}", self.bpm);

        while self.running.load(Ordering::Relaxed) {
            // Process pending input events (non-blocking)
            loop {
                match self.input_rx.try_recv() {
                    Ok(event) => self.handle_input_event(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        warn!("Input channel disconnected");
                        self.running.store(false, Ordering::Relaxed);
                        return;
                    }
                }
            }

            // BPM coasting check (rate-limited to once per WATCHDOG_INTERVAL)
            if let Some(last) = self.last_bpm_update_time {
                if last.elapsed() > BPM_TIMEOUT
                    && self.play_state == PlayState::Running
                    && self
                        .last_coast_warn
                        .map_or(true, |t| t.elapsed() >= WATCHDOG_INTERVAL)
                {
                    warn!("No BPM update for 3s, coasting at {} BPM", self.bpm);
                    self.last_coast_warn = Some(Instant::now());
                }
            }

            // Update status
            if let Ok(mut st) = self.status.try_lock() {
                st.play_state = format!("{:?}", self.play_state);
                st.bpm = self.bpm;
                st.pulse_count = self.pulse_count;
            }

            // State-specific behavior
            match self.play_state {
                PlayState::Idle => {
                    // Just wait for events
                    std::thread::sleep(Duration::from_millis(1));
                }
                PlayState::WaitingForBeat => {
                    // We should have already done initial phase sync before entering this state
                    // If we're here without proper sync, start immediately
                    self.transition_to_running();
                }
                PlayState::Running => {
                    self.run_clock_tick();
                }
                PlayState::Stopped => {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }

        info!("ClockEngine shutting down");
    }

    fn handle_input_event(&mut self, event: InputEvent) {
        self.last_mixxx_msg_time = Instant::now();
        trace!("ClockEngine received: {:?}", event);

        match event {
            InputEvent::BpmUpdate { bpm } => {
                let clamped = self.config.clamp_bpm(bpm);
                let delta = (clamped - self.bpm).abs();
                if delta > 0.05 {
                    info!("BPM changed: {} -> {} (delta={})", self.bpm, clamped, delta);
                    self.bpm = clamped;
                    self.tick_interval_ns = Config::tick_interval_ns(self.bpm);
                }
                self.last_bpm_update_time = Some(Instant::now());
                self.last_coast_warn = None;

                // Wake state machine if Idle or Stopped
                if self.play_state == PlayState::Idle || self.play_state == PlayState::Stopped {
                    self.transition_to_waiting();
                }
            }
            InputEvent::BeatDistance { value } => {
                self.last_beat_distance = value;
                if self.play_state == PlayState::Idle || self.play_state == PlayState::Stopped {
                    self.transition_to_waiting();
                }
                // Phase correction happens on beat boundary in run_clock_tick
            }
            InputEvent::Playing => {
                if self.play_state == PlayState::Idle || self.play_state == PlayState::Stopped {
                    self.transition_to_waiting();
                }
            }
            InputEvent::Stopped => {
                if self.play_state == PlayState::Running {
                    self.transition_to_stopped();
                }
            }
        }
    }

    fn transition_to_waiting(&mut self) {
        if self.play_state == PlayState::WaitingForBeat {
            return;
        }
        info!("State transition: {:?} -> WaitingForBeat", self.play_state);
        let old_state = self.play_state;
        self.play_state = PlayState::WaitingForBeat;

        // Perform initial phase sync BEFORE sending Start so 0xFA and first 0xF8 arrive tightly together
        self.initial_phase_sync();

        // Only send Start on transition from Stopped/Idle
        if old_state == PlayState::Stopped || old_state == PlayState::Idle {
            self.send_output(OutputCommand::Start);
        }

        self.transition_to_running();
    }

    fn transition_to_running(&mut self) {
        if self.play_state == PlayState::Running {
            return;
        }
        info!("State transition: {:?} -> Running", self.play_state);
        self.play_state = PlayState::Running;
        self.pulse_count = 0;
        self.next_tick = Instant::now();
    }

    fn transition_to_stopped(&mut self) {
        if self.play_state == PlayState::Stopped {
            return;
        }
        info!("State transition: {:?} -> Stopped", self.play_state);
        self.play_state = PlayState::Stopped;
        self.send_output(OutputCommand::Stop);
    }

    fn initial_phase_sync(&mut self) {
        if self.last_beat_distance <= 0.0 {
            return;
        }
        let beat_duration_ns = Config::beat_duration_ns(self.bpm);
        let sleep_ns = ((1.0 - self.last_beat_distance) * beat_duration_ns as f64) as u64;

        if self.last_beat_distance > 0.95 {
            debug!(
                "Phase sync: beat_distance={:.4} > 0.95, skipping sleep",
                self.last_beat_distance
            );
            return;
        }

        debug!(
            "Phase sync: sleeping {}us until beat boundary (beat_distance={:.4})",
            sleep_ns / 1000,
            self.last_beat_distance
        );
        spin_sleep::sleep(Duration::from_nanos(sleep_ns));
    }

    fn run_clock_tick(&mut self) {
        let now = Instant::now();
        if now < self.next_tick {
            spin_sleep::sleep(self.next_tick - now);
        }

        self.pulse_count += 1;
        if self.pulse_count > 24 {
            self.pulse_count = 1;
        }

        trace!(
            "Clock pulse {} @ {} BPM (interval={}ns)",
            self.pulse_count,
            self.bpm,
            self.tick_interval_ns
        );
        self.send_output(OutputCommand::ClockPulse);

        // On beat boundary, apply phase correction
        if self.pulse_count == 24 {
            self.apply_phase_correction();
        }

        self.next_tick += Duration::from_nanos(self.tick_interval_ns);
    }

    fn apply_phase_correction(&mut self) {
        let beat_duration_ns = Config::beat_duration_ns(self.bpm) as f64;
        let staleness = self.last_mixxx_msg_time.elapsed().as_nanos() as f64 / beat_duration_ns;
        let adjusted_beat_distance = (self.last_beat_distance + staleness) % 1.0;
        let expected = 0.0;
        let phase_error = ((adjusted_beat_distance - expected) + 0.5) % 1.0 - 0.5;

        let correction_ns = compute_phase_correction(
            phase_error,
            beat_duration_ns,
            self.tick_interval_ns,
            self.config.phase_gain,
        );

        debug!(
            "Phase correction: error={:.6}, correction={}ns",
            phase_error, correction_ns
        );

        // Apply correction: positive ns = slow down (add to next_tick)
        // negative ns = speed up (subtract from next_tick)
        if correction_ns >= 0 {
            self.next_tick += Duration::from_nanos(correction_ns as u64);
        } else {
            self.next_tick -= Duration::from_nanos((-correction_ns) as u64);
        }

        if let Ok(mut st) = self.status.try_lock() {
            st.phase_error = phase_error;
        }
    }

    fn send_output(&self, cmd: OutputCommand) {
        if self.config.dry_run {
            trace!("[dry-run] Would send: {:?}", cmd);
            return;
        }
        if let Err(e) = self.output_tx.try_send(cmd) {
            warn!("Failed to send output command: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_error_wraps_correctly_at_boundary() {
        // beat_distance jumps from 0.99 to 0.01 (boundary crossing)
        // Without wrap-safe math, error would be ~0.98
        // With wrap-safe: ((0.01 - 0.0) + 0.5) % 1.0 - 0.5 = 0.01
        let adjusted: f64 = 0.01;
        let expected: f64 = 0.0;
        let phase_error: f64 = ((adjusted - expected) + 0.5) % 1.0 - 0.5;
        assert!(
            phase_error.abs() < 0.05,
            "phase_error should be small, got {}",
            phase_error
        );
    }

    #[test]
    fn correction_capped_at_min_of_5pct_beat_and_tick_interval() {
        // At 120 BPM: beat_duration = 500ms, tick_interval = ~20.8ms
        // 5% of beat = 25ms, but tick_interval - 1 = ~20.8ms - 1
        // So max correction should be ~20.8ms - 1, not 25ms
        let beat_duration_ns = 500_000_000.0; // 500ms
        let tick_interval_ns = 20_833_333; // ~20.8ms
        let gain = 0.15;

        // Large positive phase_error (behind) -> negative correction (speed up), capped
        let correction = compute_phase_correction(1.0, beat_duration_ns, tick_interval_ns, gain);
        let max_allowed = (beat_duration_ns * 0.05).min(tick_interval_ns as f64 - 1.0) as i64;
        assert!(
            correction.abs() <= max_allowed,
            "correction {} should be <= {}",
            correction,
            max_allowed
        );
        assert!(
            correction < 0,
            "positive phase_error should produce negative correction (speed up), got {}",
            correction
        );
    }

    #[test]
    fn zero_phase_error_produces_zero_correction() {
        let correction = compute_phase_correction(0.0, 500_000_000.0, 20_833_333, 0.15);
        assert_eq!(correction, 0);
    }

    #[test]
    fn negative_phase_error_increases_next_tick_deadline() {
        // Negative phase_error means we're ahead of Mixxx -> slow down (positive correction)
        let correction = compute_phase_correction(-0.1, 500_000_000.0, 20_833_333, 0.15);
        assert!(
            correction > 0,
            "negative phase_error should produce positive correction (slow down), got {}",
            correction
        );
    }

    #[test]
    fn positive_phase_error_decreases_next_tick_deadline() {
        // Positive phase_error means we're behind Mixxx -> speed up (negative correction)
        let correction = compute_phase_correction(0.1, 500_000_000.0, 20_833_333, 0.15);
        assert!(
            correction < 0,
            "positive phase_error should produce negative correction (speed up), got {}",
            correction
        );
    }

    #[test]
    fn correction_cannot_push_next_tick_into_past() {
        // At 120 BPM: tick_interval = ~20.8ms. Max correction = tick_interval - 1.
        // If we apply this to next_tick which is one tick_interval in the future,
        // the earliest next_tick can be is 1ns from now (not negative/past).
        let tick_interval_ns = 20_833_333u64;
        let max_correction = (500_000_000.0f64 * 0.05).min(tick_interval_ns as f64 - 1.0) as i64;
        let now = Instant::now();
        let next_tick = now + Duration::from_nanos(tick_interval_ns);

        // Simulate applying the max negative correction (speed up most)
        let corrected = next_tick - Duration::from_nanos(max_correction as u64);
        assert!(
            corrected >= now,
            "corrected next_tick should not be in the past: corrected={:?}, now={:?}",
            corrected,
            now
        );
    }
}
