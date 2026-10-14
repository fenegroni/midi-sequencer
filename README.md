# MIDI Sequencer

A small MIDI loop recorder for the AKAI MPK mini play 3. It records what you play, loops it
back through the MPK's internal sounds, and lets you overdub layers. You do not need a DAW.

The app also runs an MCP server, so an assistant such as Claude can control the transport,
read the recorded loop, and export it.

## Requirements

- Windows 10 or 11
- A [Rust toolchain](https://rustup.rs) to build the app
- An AKAI MPK mini play 3 on USB (any other MIDI device also works)

## Build and run

```bash
cargo run --release
```

The executable is `target\release\midi-sequencer.exe`.

## Usage

1. Plug in the MPK. The app connects to its input and output within two seconds.
2. Press **Record** (or `R`) and play. Every pass through the loop overdubs a new layer.
3. Press **Space** to stop and start playback.
4. Press **Export .mid** to write the loop into `Documents\MidiSequencer`.

| Control | Effect |
| --- | --- |
| BPM, Bars | Set the tempo and the loop length in 4/4 bars. |
| Metronome | Plays a side stick on the drum channel on every beat. |
| MIDI out | Chooses where the notes play. **PC soundcard (GS Wavetable)** plays them through your speakers. |
| MIDI thru | Forwards what you play to the output. It turns on by itself for every output except the MPK. |
| Undo last take | Removes the notes of the latest recording pass. |
| Clear | Removes every note. |

The app records notes only. It ignores pitch bend and controller messages.

### Play through the PC soundcard

Windows includes a General MIDI synth called Microsoft GS Wavetable Synth. Choose
**PC soundcard (GS Wavetable)** under **MIDI out** to hear the loop and the metronome on
your PC instead of the MPK. The app turns MIDI thru on, so you also hear your live playing.

- The synth adds about 100 ms of latency, so live playing feels a little late.
- The MPK still plays its own sound when you press a key. Turn its volume down to hear only
  the PC.

## MCP server

The server listens on `http://127.0.0.1:7391/mcp` while the app is open. It accepts
connections from the local machine only. The `.mcp.json` file in this repository registers
it for Claude Code sessions that start in this folder.

| Tool | Effect |
| --- | --- |
| `get_status` | Returns the transport state, tempo, loop length, position, and MIDI ports. |
| `play` | Starts playback from the first bar. |
| `stop` | Stops playback and recording. |
| `set_recording` | Turns overdub recording on or off. |
| `set_tempo` | Sets the tempo in beats per minute. |
| `set_loop_length` | Sets the loop length in bars. |
| `set_midi_out` | Switches the MIDI output, for example to the PC soundcard. |
| `set_instrument` | Selects a General MIDI instrument for one channel. |
| `add_notes` | Writes notes into the loop as one take, so **Undo last take** removes them. |
| `get_notes` | Returns every recorded note. |
| `export_midi` | Exports the loop as a `.mid` file and returns its path. |

## Versioning

This project follows [Semantic Versioning](https://semver.org). The public interface is the
MCP server and the exported file format. [CHANGELOG.md](CHANGELOG.md) lists every release.

- **Major**: a change that breaks an MCP client. Examples are a removed or renamed tool, a
  removed or renamed argument or result field, and a changed server address.
- **Minor**: a backwards compatible addition. Examples are a new tool, a new optional
  argument, a new result field, and a new feature in the window.
- **Patch**: a bug fix that leaves the interface unchanged.

While the version is below 1.0.0, a minor release may contain breaking changes.

## License

Copyright (C) 2026 Filippo Negroni

This program is free software: you can redistribute it and/or modify it under the terms of
the GNU General Public License as published by the Free Software Foundation, either version
3 of the License, or (at your option) any later version.

This program is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY;
without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.
See the [GNU General Public License](LICENSE) for more details.
