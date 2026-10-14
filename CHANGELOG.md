# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-10-14

### Added

- The MIDI out list labels Microsoft GS Wavetable Synth as **PC soundcard (GS Wavetable)**.
- MIDI thru turns on by itself when the output is not the MPK.
- MCP tool `set_midi_out`, and the status fields `midi_thru` and `midi_out_ports`.
- MCP tool `add_notes`, which writes notes into the loop as one take.
- MCP tool `set_instrument`, and the status field `instruments`. The app sends the chosen
  instruments again when the output changes.
- The `--headless` flag runs the app without a window, with the MCP server as the only
  control. A headless copy exits when the server address is taken.
- MCP tools `set_midi_in`, `set_metronome`, `set_midi_thru`, `undo_take`, `clear`,
  `open_export_folder`, and `quit`, so an MCP client can do everything the window does.
- The status fields `midi_in_ports`, `port_message`, and `position.beats`.
- The sequencer-panel Claude Code mod in `mod/sequencer-panel`. It starts the app headless,
  connects Claude to its MCP server once it answers, and draws its controls and piano roll
  in a pane.

### Changed

- The MCP server starts before the first MIDI port scan, so it answers sooner.

## [0.1.0] - 2026-10-02

### Added

- Loop recorder with overdub, undo of the last take, and clear.
- Playback through any MIDI output, with automatic connection to the AKAI MPK mini play 3.
- Metronome on the drum channel and optional MIDI thru.
- Piano roll view of the loop with a playhead.
- Export of the loop as a format 0 Standard MIDI File.
- MCP server on `http://127.0.0.1:7391/mcp` with the tools `get_status`, `play`, `stop`,
  `set_recording`, `set_tempo`, `set_loop_length`, `get_notes`, and `export_midi`.
