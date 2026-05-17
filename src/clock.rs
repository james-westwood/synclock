use crate::config::Config;
use crate::state::{DaemonStatus, InputEvent, OutputCommand, PlayState};
use crossbeam_channel::{select, tick, Receiver, Sender, TryRecvError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, info, trace, warn};

const BPM_TIMEOUT: Duration = Duration::from_secs(3);
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(2);

pub struct ClockEngine {
    bpm: f64,
    play_state: PlayState,
    pulse_count: u8,
    last_beat_distance: f64,
    last_mixxx_msg_time: Instant,
    last_bpm_update_time: Option<Instant>,
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

        let watchdog = tick(WATCHDOG_INTERVAL);

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

            // BPM coasting check
            if let Some(last) = self.last_bpm_update_time {
                if last.elapsed() > BPM_TIMEOUT && self.play_state == PlayState::Running {
                    warn!("No BPM update for 3s, coasting at {} BPM", self.bpm);
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

            // Handle watchdog tick (not used directly here; input.rs has its own)
            select! {
                recv(watchdog) -> _ => {},
                default(Duration::from_millis(0)) => {},
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

        // Only send Start on transition from Stopped/Idle
        if old_state == PlayState::Stopped || old_state == PlayState::Idle {
            self.send_output(OutputCommand::Start);
        }

        // Perform initial phase sync
        self.initial_phase_sync();
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

        let correction = phase_error * beat_duration_ns * self.config.phase_gain;
        let max_correction = beat_duration_ns * 0.05;
        let capped_correction = correction.clamp(-max_correction, max_correction);

        debug!(
            "Phase correction: error={:.6}, correction={:.0}ns (capped from {:.0}ns)",
            phase_error, capped_correction, correction
        );

        // Adjust next_tick: positive error (behind) -> subtract (speed up)
        // negative error (ahead) -> add (slow down)
        if phase_error > 0.0 {
            self.next_tick -= Duration::from_nanos(capped_correction.abs() as u64);
        } else {
            self.next_tick += Duration::from_nanos(capped_correction.abs() as u64);
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
