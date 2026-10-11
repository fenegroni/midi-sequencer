# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- The MIDI out list labels Microsoft GS Wavetable Synth as **PC soundcard (GS Wavetable)**.
- MIDI thru turns on by itself when the output is not the MPK.
- MCP tool `set_midi_out`, and the status fields `midi_thru` and `midi_out_ports`.

## [0.1.0] - 2026-10-02

### Added

- Loop recorder with overdub, undo of the last take, and clear.
- Playback through any MIDI output, with automatic connection to the AKAI MPK mini play 3.
- Metronome on the drum channel and optional MIDI thru.
- Piano roll view of the loop with a playhead.
- Export of the loop as a format 0 Standard MIDI File.
- MCP server on `http://127.0.0.1:7391/mcp` with the tools `get_status`, `play`, `stop`,
  `set_recording`, `set_tempo`, `set_loop_length`, `get_notes`, and `export_midi`.
