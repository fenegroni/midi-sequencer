# Plan: transport controls inside Claude Code

This plan describes a Claude Code mod that puts the sequencer's transport controls inside
Claude Code. It was written on 2026-10-04, before the refactor, against the MCP server in
`src/mcp.rs` at commit `2953f0f`. Nothing in this plan has been built yet.

## Problem

The user plays the MPK and works with Claude in the same session. To start, stop or record a
loop, they either switch to the app window or ask Claude in a prompt. Switching windows
breaks the flow at the keyboard. Asking Claude costs a model turn for an action that should
take one keystroke. The user also cannot see the transport state and the playhead while they
talk to Claude about the loop.

## Context

- The app is a Rust program for Windows. It serves MCP over HTTP at
  `http://127.0.0.1:7391/mcp` while it is open, and it accepts connections from the local
  machine only.
- The repository's `.mcp.json` registers that server for Claude Code under the name
  `midi-sequencer`.
- A mod is a Claude Code plugin made of function hooks. It can call a connected MCP server's
  tools directly with `$.mcp.call(server, tool, args)`, so the controls need no model turn.
- The mod must run in a Claude Code session on the same machine as the app. A cloud session
  cannot reach `127.0.0.1:7391` on the user's computer.

## Forces in tension

| Force | Pulls toward | Pulls against |
| --- | --- | --- |
| Immediacy at the keyboard | One-keystroke controls with no model turn | Buttons alone, which are awkward to reach in a terminal |
| Visibility of the transport | A readout that is always on screen | A side pane, which only opens unasked at 144 terminal columns or more |
| A live playhead | Frequent polling of `get_status` | Steady traffic to the app and redraws in the session |
| Robustness | A mod that degrades quietly when the app is closed | Error messages in the transcript every time a poll fails |
| Versioning with the server | Keeping the mod in this repository | An extra folder that the app's build ignores |
| The refactor in progress | Waiting until the MCP interface settles | Starting now and chasing renamed tools afterwards |

## Decisions

These are the forces that prevail and the choices that follow from them.

1. **Immediacy prevails over richness.** The controls are a band above the prompt plus slash
   commands. The band holds the readout and two buttons. The commands are the reliable input
   in a terminal. A side pane is out of scope for the first version.
2. **A live playhead prevails, but only while it moves.** The mod polls `get_status` about
   twice a second while the state is `playing` or `recording`, and about every five seconds
   while it is `stopped`. Every control action also updates the band from its own result.
3. **Robustness prevails over diagnostics.** When the server is unreachable, the band shows
   one dim line, "sequencer offline", and polling backs off to about ten seconds. The mod
   never throws into the session because the app is closed.
4. **Versioning with the server prevails.** The mod lives in this repository, so a change to
   the MCP interface and the matching change to the mod land in the same commit.
5. **The refactor prevails over starting now.** Work starts after the refactor is merged into
   `master`, from this plan and the "Before starting" checks below.

## Interface the mod depends on

The mod depends on these MCP tools. Each one returns the status object below as a single
text content block that contains JSON. A failed call returns `isError: true` with a plain
text message.

| Tool | Arguments | Used for |
| --- | --- | --- |
| `get_status` | none | Polling the readout |
| `play` | none | The Play button and `/play` |
| `stop` | none | The Stop button and `/stop` |
| `set_recording` | `enabled: boolean` | The Record button and `/rec`; turning it on also starts playback |
| `set_tempo` | `bpm: number`, from 20 to 300 | `/bpm <n>` |

```json
{
  "state": "stopped",
  "bpm": 120.0,
  "bars": 4,
  "beats_per_bar": 4,
  "position": { "bar": 1, "beat": 1 },
  "note_count": 0,
  "metronome": false,
  "midi_in": "MPK mini Play mk3",
  "midi_out": "MPK mini Play mk3"
}
```

`state` is one of `stopped`, `playing` or `recording`. The mod reads `state`, `bpm`, `bars`,
`position.bar`, `position.beat` and `note_count`, and it ignores the other fields.

## Before starting (after the refactor)

1. Compare the tool names, arguments and status fields above with the refactored
   `src/mcp.rs`. Update this plan where they differ before writing any mod code.
2. Check that the server address and the `midi-sequencer` name in `.mcp.json` are unchanged.
3. Start a local Claude Code session on the Windows machine with the app running, and check
   that `/mcp` lists `midi-sequencer` as connected.

## Design

### Band above the prompt

The band is one row. It shows the state, the position as `bar.beat / bars`, the tempo and
the note count, followed by a Play/Stop button and a Record button. Recording is drawn in a
colour that stands out from playing.

```
● REC  3.2 / 4  120 BPM  37 notes   [ Stop ]  [ Rec ]
▶ PLAY 1.4 / 4  120 BPM  37 notes   [ Stop ]  [ Rec ]
■ STOP 1.1 / 4  120 BPM  37 notes   [ Play ]  [ Rec ]
  sequencer offline
```

### Slash commands

| Command | Effect |
| --- | --- |
| `/play` | Calls `play`. |
| `/stop` | Calls `stop`. |
| `/rec` | Toggles recording with `set_recording`, based on the last known state. |
| `/bpm <n>` | Calls `set_tempo`. It rejects a value outside 20 to 300 before calling. |

Each command answers with a short text line, for example "Recording on", and updates the
band.

### State and polling

- The last status, or an offline marker, is kept in `$.state` and declared in the mod's
  `types/index.d.ts`. The band reads it and every poll or action writes it.
- One timer on `$.clock` drives polling. Its interval depends on the last state: about
  500 ms while playing or recording, about 5 s while stopped, and about 10 s while offline.
- One function parses a tool result into either a status or an error. The band, the
  commands and the tests all use it.

## Files

| Path | Content |
| --- | --- |
| `claude-plugin/midi-transport/.claude-plugin/plugin.json` | Name, version `0.1.0`, description, and `"types": "./types/index.d.ts"` |
| `claude-plugin/midi-transport/hooks/hooks.json` | `{ "modules": ["./register.tsx"] }` |
| `claude-plugin/midi-transport/hooks/register.tsx` | The band, the commands, polling and parsing |
| `claude-plugin/midi-transport/types/index.d.ts` | The `$.state` contract |
| `claude-plugin/midi-transport/hooks/*.test.ts` | Tests for parsing and for the offline fallback |
| `README.md` | A short "Claude Code mod" section after "MCP server" |
| `CHANGELOG.md` | An entry under `[Unreleased]` |

## Steps

1. Load the `plugin-authoring` skill in Claude Code. It holds the API, the type definitions
   for the installed build, the band and pane examples, and the validate and test commands.
2. Run the "Before starting" checks above.
3. Write the parser and its tests first, using status and error results copied from the
   refactored server.
4. Write the band, then the commands, then polling.
5. Run `claude plugin validate claude-plugin/midi-transport`, type-check the mod, and run
   `claude plugin test claude-plugin/midi-transport`.
6. Load the mod with `claude --plugin-dir claude-plugin/midi-transport` and try it against
   the running app with the MPK connected.
7. Update `README.md` and `CHANGELOG.md`, then commit.

## Done means

- Validation, type-checking and the tests pass.
- With the app running, the band shows the live playhead, and the buttons and commands
  start, stop and record.
- With the app closed, the band shows "sequencer offline" and the transcript stays clean.
- The README explains how to load the mod and lists the commands.

## Out of scope for the first version

- A side pane with BPM and Bars fields or a list of takes.
- Export, undo and clear from the mod. Claude can already do these through the MCP tools.
- Keyboard shortcuts bound outside the slash commands.
