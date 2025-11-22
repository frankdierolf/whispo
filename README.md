# Whispo

Minimal voice-to-text CLI for terminal users. Record your voice, get instant transcription to clipboard.

## Demo

![Whispo Demo](demo.gif)

## Quick Start

```bash
# Install
cargo install whispo

# Set API key (add to ~/.bashrc or ~/.zshrc)
export OPENAI_API_KEY=sk-your-key-here

# Run
whispo
```

## Usage

```bash
whispo
```

1. Recording starts automatically
2. Press Enter to stop
3. Transcription copies to clipboard

That's it. Paste into your AI coding tool.

## Optional: Hotkey Mode (GNOME Only)

For hands-free operation on GNOME desktops (Ubuntu, Omakub, etc.):

```bash
whispo setup-hotkey    # One-time setup
whispo listen          # Start background service

# Press your hotkey (default: Ctrl+Shift+R) anywhere to toggle recording
```

Other commands:
```bash
whispo status          # Check service status
whispo stop            # Stop background service
```

## Requirements

- Rust (latest stable)
- OpenAI API key ([get one here](https://platform.openai.com/api-keys))
- FFmpeg (for audio compression)
- Linux with working microphone
- ALSA or PulseAudio
- GNOME desktop (for hotkey mode only)

### Installing FFmpeg

```bash
# Ubuntu/Debian
sudo apt install ffmpeg

# macOS
brew install ffmpeg
```

## Building from Source

```bash
cargo build --release
```

Binary will be at `./target/release/whispo`

## FAQ

**How does hotkey mode work?**

A lightweight background service communicates via Unix sockets. GNOME's native keyboard shortcuts call `whispo toggle`. No special permissions required. Works on Wayland and X11.

**Can I use hotkey mode on other desktop environments?**

The `setup-hotkey` command is GNOME-specific, but you can manually configure hotkeys in KDE, i3, sway, etc. to call `whispo toggle`.

**Does the simple mode still work?**

Yes! Running `whispo` without arguments works exactly as before. Hotkey mode is completely optional.

## Inspiration

Inspired by [whisp](https://github.com/yummyweb/whisp) - a desktop voice input tool with system tray integration.

## License

MIT
