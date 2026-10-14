export type TransportState = 'stopped' | 'playing' | 'recording'

// The sequencer's get_status result, without the playhead, which lives in its own value so
// that it can change without redrawing the piano roll.
export type Status = {
  state: TransportState
  bpm: number
  bars: number
  beats_per_bar: number
  note_count: number
  metronome: boolean
  midi_thru: boolean
  midi_in: string | null
  midi_out: string | null
  midi_in_ports: string[]
  midi_out_ports: string[]
  port_message: string | null
}

export type View =
  | { kind: 'offline'; isStarting: boolean }
  | { kind: 'online'; status: Status }

export type Note = {
  start_beat: number
  duration_beats: number
  pitch: number
  velocity: number
  channel: number
  take: number
}

// Where the playhead was at a known moment, so the roll can animate it between polls.
export type Anchor = { beats: number; atMs: number }

declare module 'claude-code' {
  interface PluginState {
    'sequencer-panel': {
      view: View
      notes: Note[]
      position: { bar: number; beat: number }
      anchor: Anchor
      message: string
    }
  }
}
