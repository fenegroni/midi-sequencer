import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register } from 'claude-code'

import type { Anchor, Note, Status, View } from '../types'

const URL = 'http://127.0.0.1:7391/mcp'
const PANE = 'sequencer'

const view = atom({ plugin: 'sequencer-panel', key: 'view' } as const, { kind: 'offline', isStarting: false } as View)
const notes = atom({ plugin: 'sequencer-panel', key: 'notes' } as const, [] as Note[])
const position = atom({ plugin: 'sequencer-panel', key: 'position' } as const, { bar: 1, beat: 1 })
const anchor = atom({ plugin: 'sequencer-panel', key: 'anchor' } as const, { beats: 0, atMs: 0 } as Anchor)
const message = atom({ plugin: 'sequencer-panel', key: 'message' } as const, '')

type $ = EngineInterface
type RpcResult = { ok: true; value: any } | { ok: false; isOffline: boolean; error: string }
type Mode = 'fast' | 'slow' | 'offline'

// Polling runs on one 500 ms timer. A mode sets how many ticks pass between polls.
const TICK_MS = 500
const TICKS: Record<Mode, number> = { fast: 1, slow: 4, offline: 10 }
const START_TIMEOUT_MS = 15000

let exePath = 'C:\\Tools\\MidiSequencer\\midi-sequencer.exe'
let autostart = true

// Module memory: it only saves redundant state writes and starts over on a reload.
let mode: Mode = 'offline'
let ticks = 0
let isPolling = false
let isStarting = false
let lastCore = ''
let lastPosition = ''
let lastNoteCount = -1
let lastPlayhead = { state: '', bpm: 0, bars: 0 }
let roll = { key: '', source: '' }
let isServerLinked = false

async function rpc($: $, tool: string, args: Record<string, unknown> = {}): Promise<RpcResult> {
  let text: string
  try {
    const body = { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: tool, arguments: args } }
    const res = await $.http.fetch(URL, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    })
    if (!res.ok) return { ok: false, isOffline: false, error: `The sequencer answered HTTP ${res.status}.` }
    text = res.text
  } catch {
    return { ok: false, isOffline: true, error: 'The sequencer is not running.' }
  }
  try {
    const result = JSON.parse(text).result
    const inner: string = result.content[0].text
    if (result.isError) return { ok: false, isOffline: false, error: inner }
    return { ok: true, value: JSON.parse(inner) }
  } catch {
    return { ok: false, isOffline: false, error: 'The sequencer sent an answer the mod cannot read.' }
  }
}

async function goOffline($: $) {
  mode = 'offline'
  isServerLinked = false
  lastCore = ''
  lastNoteCount = -1
  lastPlayhead = { state: '', bpm: 0, bars: 0 }
  const current = await read($, view)
  if (current.kind !== 'offline' || current.isStarting !== isStarting) {
    await update($, view, () => ({ kind: 'offline', isStarting }))
  }
}

// Claude Code connects MCP servers when the session starts, which is often before the app is
// up, and it never dials a failed server again. When the repository's own midi-sequencer
// server works, the model already has the tools. Otherwise the mod connects its own server,
// whose URL differs by a query string the app ignores, because Claude Code skips a plugin
// server that has the same endpoint as a project server.
async function linkServer($: $) {
  const project = await $.mcp.call('midi-sequencer', 'get_status').catch(err => ({ isError: true, err: String(err) }))
  if (!project.isError) return
  const linked = await $.mcp.connect('sequencer').catch(err => ({ isConnected: false as const, message: String(err) }))
  if (!linked.isConnected) await update($, message, () => `Claude cannot reach the sequencer's tools: ${linked.message}`)
}

async function applyStatus($: $, raw: any, isAfterAction: boolean) {
  const { position: pos, instruments: _instruments, ...core } = raw
  const status = core as Status
  mode = status.state === 'stopped' ? 'slow' : 'fast'
  if (isStarting) {
    isStarting = false
    await update($, message, () => '')
  }

  if (!isServerLinked) {
    isServerLinked = true
    void linkServer($)
  }

  const coreJson = JSON.stringify(status)
  if (coreJson !== lastCore) {
    lastCore = coreJson
    await update($, view, () => ({ kind: 'online', status }))
  }
  const posJson = JSON.stringify({ bar: pos.bar, beat: pos.beat })
  if (posJson !== lastPosition) {
    lastPosition = posJson
    await update($, position, () => ({ bar: pos.bar, beat: pos.beat }))
  }

  // The roll animates the playhead from an anchor. It moves the anchor only when the
  // transport changes or the animation drifts, so the drawing stays the same between polls.
  const now = await $.clock.now()
  const loopBeats = status.bars * status.beats_per_bar
  const current = await read($, anchor)
  const predicted = (current.beats + ((now - current.atMs) / 60000) * status.bpm) % loopBeats
  const drift = Math.min(Math.abs(predicted - pos.beats), loopBeats - Math.abs(predicted - pos.beats))
  const isMoved =
    status.state !== lastPlayhead.state || status.bpm !== lastPlayhead.bpm || status.bars !== lastPlayhead.bars
  if (status.state !== 'stopped' && (isMoved || drift > 0.25)) {
    await update($, anchor, () => ({ beats: pos.beats, atMs: now }))
  }
  lastPlayhead = { state: status.state, bpm: status.bpm, bars: status.bars }

  if (isAfterAction || status.note_count !== lastNoteCount) {
    lastNoteCount = status.note_count
    const got = await rpc($, 'get_notes')
    if (got.ok) await update($, notes, () => got.value.notes as Note[])
  }
}

async function poll($: $): Promise<boolean> {
  const got = await rpc($, 'get_status')
  if (got.ok) {
    await applyStatus($, got.value, false)
    return true
  }
  if (got.isOffline) await goOffline($)
  return false
}

async function tick($: $) {
  ticks += 1
  if (isPolling || (!isStarting && ticks % TICKS[mode] !== 0)) return
  isPolling = true
  try {
    await poll($)
  } finally {
    isPolling = false
  }
}

// Runs one tool from a control, then shows its result or its error.
async function act($: $, tool: string, args: Record<string, unknown> = {}) {
  const got = await rpc($, tool, args)
  if (!got.ok) {
    await update($, message, () => got.error)
    if (got.isOffline) await goOffline($)
    return
  }
  if (tool === 'export_midi') {
    await update($, message, () => `Exported ${got.value.path}`)
    return
  }
  await update($, message, () => got.value.port_message ?? '')
  if (tool === 'quit') {
    await goOffline($)
    return
  }
  await applyStatus($, got.value, true)
}

async function start($: $) {
  if (isStarting) return
  isStarting = true
  await update($, message, () => '')
  await goOffline($)
  // Start-Process detaches the sequencer, so it outlives this call and the session.
  let failure = ''
  try {
    const run = await $.process.run(
      ['powershell', '-NoProfile', '-NonInteractive', '-Command',
        "Start-Process -FilePath $env:SEQ_EXE -ArgumentList '--headless'"],
      { env: { SEQ_EXE: exePath }, timeoutMs: 20000 },
    )
    if (run.exitCode !== 0) failure = run.stderr.trim().split('\n')[0] ?? ''
  } catch (err) {
    failure = String(err)
  }
  if (failure) {
    isStarting = false
    await goOffline($)
    await update($, message, () => `Could not start ${exePath}: ${failure}`)
    return
  }
  $.clock.after(START_TIMEOUT_MS, () => {
    if (!isStarting) return
    isStarting = false
    void goOffline($)
    void update($, message, () => 'The sequencer started, but its server did not answer.')
  })
}

export const register: Register = (on, options) => {
  exePath = String(options.exePath ?? exePath)
  autostart = options.autostart !== false

  on('session.start', async ($, e, next) => {
    const started = await next(e)
    await $.command.register({
      name: 'sequencer',
      description: 'Open the MIDI Sequencer pane, and start the sequencer when it is not running',
    })
    $.clock.every(TICK_MS, () => void tick($))
    if (e.isInteractive) void $.ui.open({ id: PANE, title: 'Sequencer' })
    void (async () => {
      if (!(await poll($)) && autostart) await start($)
    })()

    return started
  })

  on('command.run', { command: 'sequencer' }, async $ => {
    await $.ui.open({ id: PANE, title: 'Sequencer' })
    if (await poll($)) return { text: 'Sequencer pane opened.' }
    await start($)

    return { text: 'Starting the sequencer in the background.' }
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const { Box, Text, Button } = $.ui.resolve(e)
    const v = await read($, view)
    const note = await read($, message)
    const messageLine = note ? <Text color="warning">{note}</Text> : null

    if (v.kind === 'offline') {
      return (
        <Box flexDirection="column" gap={1}>
          <Text dimColor>
            {v.isStarting ? 'Starting the sequencer in the background...' : 'The sequencer is not running.'}
          </Text>
          {!v.isStarting && (
            <Button key="start" label="Start sequencer" variant="primary" hotkey="s" onPress={() => void start($)} />
          )}
          {messageLine}
        </Box>
      )
    }

    const s = v.status
    const pos = await read($, position)
    const isRunning = s.state !== 'stopped'
    const stateText = s.state === 'recording' ? '● Recording' : s.state === 'playing' ? '▶ Playing' : '■ Stopped'
    const stateColor = s.state === 'recording' ? 'error' : s.state === 'playing' ? 'success' : undefined
    const go = (tool: string, args: Record<string, unknown> = {}) => () => void act($, tool, args)
    const toggle = (name: string, isOn: boolean) => `${name}: ${isOn ? 'on' : 'off'}`

    let portRows = null
    let tempoInput = null
    if (e.surface === 'terminal' || e.surface === 'desktop' || e.surface === 'vscode') {
      const { Select, Input } = $.ui.resolve(e)
      const choices = (ports: string[], current: string | null) => {
        const list = current && !ports.includes(current) ? [current, ...ports] : ports
        return [{ value: '', label: '(not connected)' }, ...list.map(p => ({ value: p, label: portLabel(p) }))]
      }
      portRows = (
        <Box flexDirection="column">
          <Select key="in" label="MIDI in" value={s.midi_in ?? ''} options={choices(s.midi_in_ports, s.midi_in)}
            onSelect={port => { if (port) void act($, 'set_midi_in', { port }) }} />
          <Select key="out" label="MIDI out" value={s.midi_out ?? ''} options={choices(s.midi_out_ports, s.midi_out)}
            onSelect={port => { if (port) void act($, 'set_midi_out', { port }) }} />
        </Box>
      )
      tempoInput = (
        <Input key="bpm" label="BPM" value={String(s.bpm)} submitLabel="set"
          onSubmit={text => {
            const bpm = Number(text)
            if (Number.isFinite(bpm)) void act($, 'set_tempo', { bpm })
            else void update($, message, () => `'${text}' is not a number.`)
          }} />
      )
    }

    const list = await read($, notes)
    let rollElement
    if (e.surface === 'terminal') {
      const { Raster } = $.ui.resolve(e)
      const columns = Math.max(16, Math.min(e.props.bodyColumns ?? 64, 128))
      const loopBeats = s.bars * s.beats_per_bar
      const head = isRunning ? ((pos.bar - 1) * s.beats_per_bar + pos.beat - 1) / loopBeats : -1
      rollElement = <Raster key="roll" columns={columns} rows={ROLL_ROWS} cells={rasterRoll(list, s, columns, head)} />
    } else {
      const { Svg } = $.ui.resolve(e)
      const a = await read($, anchor)
      const key = JSON.stringify([s.state, s.bpm, s.bars, a, list])
      if (key !== roll.key) roll = { key, source: svgRoll(list, s, a, isRunning ? await $.clock.now() : 0) }
      rollElement = <Svg source={roll.source} alt={`Piano roll of ${list.length} notes`} isInteractive />
    }

    return (
      <Box flexDirection="column" gap={1}>
        <Box flexDirection="row" columnGap={2} flexWrap="wrap">
          <Text bold color={stateColor}>{stateText}</Text>
          <Text>Bar {pos.bar} Beat {pos.beat}</Text>
          <Text>{s.bpm} BPM</Text>
          <Text>{s.bars} {s.bars === 1 ? 'bar' : 'bars'}</Text>
          <Text dimColor>{s.note_count} notes</Text>
        </Box>
        <Box flexDirection="row" columnGap={1} flexWrap="wrap">
          <Button key="play" label={isRunning ? 'Stop' : 'Play'} hotkey="p" variant="primary"
            onPress={go(isRunning ? 'stop' : 'play')} />
          <Button key="rec" label={s.state === 'recording' ? 'Stop recording' : 'Record'} hotkey="r"
            onPress={go('set_recording', { enabled: s.state !== 'recording' })} />
          <Button key="metronome" label={toggle('Metronome', s.metronome)} hotkey="m"
            onPress={go('set_metronome', { enabled: !s.metronome })} />
          <Button key="thru" label={toggle('Thru', s.midi_thru)} hotkey="t"
            onPress={go('set_midi_thru', { enabled: !s.midi_thru })} />
        </Box>
        <Box flexDirection="row" columnGap={1} flexWrap="wrap" alignItems="center">
          <Button key="slower" label="−5" onPress={go('set_tempo', { bpm: Math.max(20, s.bpm - 5) })} />
          <Button key="faster" label="+5" onPress={go('set_tempo', { bpm: Math.min(300, s.bpm + 5) })} />
          {tempoInput}
          <Text> Bars</Text>
          <Button key="fewer" label="−" onPress={go('set_loop_length', { bars: Math.max(1, s.bars - 1) })} />
          <Button key="more" label="+" onPress={go('set_loop_length', { bars: Math.min(64, s.bars + 1) })} />
        </Box>
        {portRows}
        {rollElement}
        <Box flexDirection="row" columnGap={1} flexWrap="wrap">
          <Button key="undo" label="Undo last take" hotkey="u" onPress={go('undo_take')} />
          <Button key="clear" label="Clear" onPress={go('clear')} />
          <Button key="export" label="Export .mid" hotkey="e" onPress={go('export_midi')} />
          <Button key="folder" label="Open export folder" onPress={go('open_export_folder')} />
          <Button key="quit" label="Quit sequencer" onPress={go('quit')} />
        </Box>
        {messageLine}
      </Box>
    )
  })
}

function portLabel(name: string): string {
  return name.toLowerCase().includes('gs wavetable') ? 'PC soundcard (GS Wavetable)' : name
}

// The pitch range the roll shows: the notes' own range, at least one octave.
function pitchRange(list: Note[]): { lo: number; hi: number } {
  const pitches = list.map(n => n.pitch)
  const lo = (pitches.length ? Math.min(...pitches) : 48) - 1
  const hi = Math.max((pitches.length ? Math.max(...pitches) : 72) + 1, lo + 12)
  return { lo, hi }
}

const W = 640
const H = 160
const ROLL_ROWS = 10

export function svgRoll(list: Note[], s: Status, a: Anchor, nowMs: number): string {
  const loopBeats = s.bars * s.beats_per_bar
  const x = (beat: number) => (beat / loopBeats) * W
  const { lo, hi } = pitchRange(list)
  const row = H / (hi - lo + 1)
  const parts: string[] = []

  for (let beat = 0; beat < loopBeats; beat++) {
    const isBar = beat % s.beats_per_bar === 0
    parts.push(`<line x1="${x(beat)}" x2="${x(beat)}" y1="0" y2="${H}" stroke="#888" stroke-opacity="${isBar ? 0.6 : 0.2}"/>`)
  }
  for (const n of list) {
    if (n.start_beat >= loopBeats) continue
    const y = H - (n.pitch - lo + 1) * row
    const color = n.channel === 10 ? '#f0a03c' : '#5aaafa'
    const bar = (from: number, to: number) =>
      parts.push(`<rect x="${x(from)}" y="${y}" width="${Math.max(x(to) - x(from), 2)}" height="${Math.max(row, 2)}" rx="1" fill="${color}"/>`)
    const end = n.start_beat + n.duration_beats
    bar(n.start_beat, Math.min(end, loopBeats))
    if (end > loopBeats) bar(0, end - loopBeats)
  }

  if (s.state !== 'stopped') {
    // SMIL moves the playhead between polls. A negative begin starts it mid-loop.
    const loopSeconds = (loopBeats * 60) / s.bpm
    const beatsNow = (a.beats + ((nowMs - a.atMs) / 60000) * s.bpm) % loopBeats
    const begin = -((beatsNow * 60) / s.bpm)
    const color = s.state === 'recording' ? '#e04848' : '#3fb950'
    parts.push(
      `<line x1="0" x2="0" y1="0" y2="${H}" stroke="${color}" stroke-width="2">` +
        `<animateTransform attributeName="transform" type="translate" from="0 0" to="${W} 0" ` +
        `dur="${loopSeconds.toFixed(3)}s" begin="${begin.toFixed(3)}s" repeatCount="indefinite"/></line>`,
    )
  }

  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${W} ${H}" width="100%" preserveAspectRatio="none">` +
    `<rect width="${W}" height="${H}" rx="4" fill="#888" fill-opacity="0.1"/>${parts.join('')}</svg>`
}

// Packs the roll into Raster cells: [codePoint, foreground, background] per cell.
export function rasterRoll(list: Note[], s: Status, columns: number, head: number): string {
  const DEFAULT = 0x01000000
  const loopBeats = s.bars * s.beats_per_bar
  const { lo, hi } = pitchRange(list)
  const words = new Uint32Array(columns * ROLL_ROWS * 3)
  const put = (col: number, rowIndex: number, ch: string, fg: number) => {
    const i = (rowIndex * columns + col) * 3
    words[i] = ch.codePointAt(0) ?? 32
    words[i + 1] = fg
    words[i + 2] = DEFAULT
  }
  for (let r = 0; r < ROLL_ROWS; r++) {
    for (let c = 0; c < columns; c++) {
      const beat = (c / columns) * loopBeats
      const isGrid = Math.floor(beat) !== Math.floor(((c - 1) / columns) * loopBeats) || c === 0
      put(c, r, isGrid ? '┊' : ' ', 0x555555)
    }
  }
  const col = (beat: number) => Math.min(columns - 1, Math.floor((beat / loopBeats) * columns))
  for (const n of list) {
    if (n.start_beat >= loopBeats) continue
    const r = ROLL_ROWS - 1 - Math.floor(((n.pitch - lo) / (hi - lo + 1)) * ROLL_ROWS)
    const fg = n.channel === 10 ? 0xf0a03c : 0x5aaafa
    const end = n.start_beat + n.duration_beats
    for (let c = col(n.start_beat); c <= col(Math.min(end, loopBeats) - 0.001); c++) put(c, r, '█', fg)
    if (end > loopBeats) for (let c = 0; c <= col(end - loopBeats - 0.001); c++) put(c, r, '█', fg)
  }
  if (head >= 0) for (let r = 0; r < ROLL_ROWS; r++) put(col(head * loopBeats), r, '│', 0xffffff)

  const bytes = new Uint8Array(words.buffer)
  let binary = ''
  for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i] ?? 0)
  return btoa(binary)
}
