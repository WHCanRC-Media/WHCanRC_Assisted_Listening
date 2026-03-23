# WHCanRC Assisted Listening

A low-latency WebRTC audio streaming server for assisted listening. A lecturer speaks into a microphone, the audio is captured on a PC, and listeners on the same WiFi network open a browser on their phone to hear it with minimal latency.

## Quick Start

1. **Run the server:**
   ```bash
   ./whcanrc-assisted-listening
   ```

2. **Open a browser** on any device on the same network:
   ```
   http://<server-ip>:8080/
   ```

3. **Tap "Listen"** to start receiving audio.

## Building from Source

### Prerequisites

- Rust (stable) — [install via rustup](https://rustup.rs/)
- Linux: `libasound2-dev` (ALSA development headers)
  ```bash
  sudo apt-get install libasound2-dev
  ```

### Build

```bash
cargo build --release
```

The binary will be at `target/release/whcanrc-assisted-listening`.

## Configuration

Copy `config.toml.example` to `config.toml` and edit as needed:

```toml
port = 8080
log_level = "info"          # trace, debug, info, warn, error
audio_sample_rate = 48000
audio_channels = 1          # mono is sufficient for speech
```

All values can be overridden via environment variables with the `WHCANRC_` prefix:

```bash
WHCANRC_PORT=9000 ./whcanrc-assisted-listening
```

## Running as a Service

### Linux (systemd)

1. Copy the binary to `/usr/local/bin/`
2. Create the config directory and config file:
   ```bash
   sudo mkdir -p /etc/whcanrc
   sudo cp config.toml.example /etc/whcanrc/config.toml
   ```
3. Install the systemd unit:
   ```bash
   sudo cp whcanrc-assisted-listening.service /etc/systemd/system/
   sudo systemctl daemon-reload
   sudo systemctl enable --now whcanrc-assisted-listening
   ```

### Windows

Run the NSIS installer (`whcanrc-assisted-listening-setup.exe`). It will:
- Install the binary to `C:\Program Files\WHCanRC Assisted Listening\`
- Install and start the Windows Service
- Add a firewall rule for the configured port
- Create an uninstaller

## Running Tests

```bash
cargo test --all
```

## Architecture

```
[USB Audio Interface / Mic]
        |
     cpal capture thread
        |
  broadcast channel
        |
  WebRTC audio track broadcaster
      /    \
  peer1   peer2   peer3 ...  (browser listeners)
```

The server uses HTTP-based WebRTC signalling (no STUN/TURN needed for LAN):

1. Browser loads the HTML page
2. Browser creates an RTCPeerConnection and sends an SDP offer to `POST /offer`
3. Server creates an answer, attaches the audio track, returns SDP answer
4. Audio streams to the browser

## License

MIT
