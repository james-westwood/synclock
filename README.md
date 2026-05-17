# mixxx-midi-clock

Headless Rust daemon that bridges Mixxx BPM/beat_distance MIDI output to MIDI clock (24 PPQN) for the Novation Circuit Rhythm, running as a systemd user service.

## Requirements

- Linux with ALSA MIDI support
- System build dependencies: `alsa-lib-devel` (Fedora) / `libasound2-dev` (Debian/Ubuntu)
- Rust toolchain

## Build

```bash
cargo build --release
```

## Installation

```bash
./install.sh
```

This builds the release binary, copies it to `~/.local/bin/`, installs the systemd user unit, and starts the service.

## Usage

```bash
# List available ALSA MIDI ports
mixxx-midi-clock --list-ports

# Run with auto-discovery (default)
mixxx-midi-clock

# Specify ports manually
mixxx-midi-clock --input-port "mixxx_midi_clock" --output-port "Circuit Rhythm"

# Dry-run mode (log but don't send MIDI)
mixxx-midi-clock --dry-run

# Print daemon status
mixxx-midi-clock --status

# Send Stop command
mixxx-midi-clock --stop
```

## Environment Variables

- `MIXXX_MIDI_CLOCK_INPUT_PORT` — ALSA MIDI input port pattern
- `MIXXX_MIDI_CLOCK_OUTPUT_PORT` — ALSA MIDI output port pattern
- `RUST_LOG` — Log level (`trace`, `debug`, `info`, `warn`, `error`)

## Systemd Management

```bash
# Check status
systemctl --user status mixxx-midi-clock

# View logs
journalctl --user -u mixxx-midi-clock -f

# Restart
systemctl --user restart mixxx-midi-clock

# Stop
systemctl --user stop mixxx-midi-clock
```

## Troubleshooting

- **No ports found**: Run `mixxx-midi-clock --list-ports` to verify ALSA port names.
- **Auto-discovery fails**: Use `--input-port` and `--output-port` with exact substrings.
- **Permission denied for real-time priority**: Ensure user is in the `audio` group.
