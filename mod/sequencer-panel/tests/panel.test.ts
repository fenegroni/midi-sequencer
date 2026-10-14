import { expect, mock, test } from 'claude-code/testing'
import type { On } from 'claude-code'

// Stands in for the engine beneath the plugin.
function engine(on: On) {
  const clock = mock.clock(on)
  on('session.start', async (_$, e) => ({ cwd: e.cwd }))
  on('command.register', async (_$, e) => ({ value: { command: e.name } }) as never)
  on('ui.open', async () => ({ value: { isPlaced: true } }) as never)
  const connected: string[] = []
  on('mcp.connect', async (_$, e) => {
    connected.push(e.server)
    return { value: { isConnected: true, server: e.server } } as never
  })
  // The project's own server failed at session start.
  on('mcp.call', async () => ({ deny: 'ECONNREFUSED' }) as never)
  return { clock, connected }
}

const PANE = { title: 'Sequencer', isFocused: false, bodyColumns: 80, placement: 'dock' } as never

function fakeSequencer() {
  const calls: string[] = []
  const status = {
    state: 'stopped', bpm: 120, bars: 4, beats_per_bar: 4,
    position: { bar: 1, beat: 1, beats: 0 }, note_count: 1, metronome: false, midi_thru: false,
    midi_in: null, midi_out: 'Microsoft GS Wavetable Synth', midi_in_ports: [],
    midi_out_ports: ['Microsoft GS Wavetable Synth'], instruments: {}, port_message: null,
  }
  const notes = { notes: [{ start_beat: 0, duration_beats: 1, pitch: 60, velocity: 100, channel: 1, take: 1 }] }
  const answer = (body: string) => {
    const { name, arguments: args } = JSON.parse(body).params
    calls.push(name)
    if (name === 'play') status.state = 'playing'
    if (name === 'stop') status.state = 'stopped'
    if (name === 'set_tempo') status.bpm = args.bpm
    const value = name === 'get_notes' ? notes : status
    const text = JSON.stringify(value)
    return { value: {
      status: 200, ok: true, headers: {},
      text: JSON.stringify({ jsonrpc: '2.0', id: 1, result: { content: [{ type: 'text', text }], isError: false } }),
    } }
  }
  return { calls, answer }
}

test('the pane drives the sequencer on desktop and terminal', async ($, on) => {
  const { clock, connected } = engine(on)
  const fake = fakeSequencer()
  on('http.fetch', async (_$, e) => fake.answer(String(e.init?.body)))
  await $.session.start({ cwd: 'C:/x', surface: 'desktop', isInteractive: true })
  await clock.advance(0)
  expect(connected).toEqual(['sequencer'])

  for (const surface of ['desktop', 'terminal'] as const) {
    const ui = await $.ui.mount({ plugin: 'sequencer-panel', surface, component: 'Pane', props: PANE, requestId: 'sequencer' })
    expect(await ui.find({ type: 'Text', text: 'Stopped' })).toBeDefined()
    await ui.press({ key: 'play' })
    expect(fake.calls).toContain('play')
    expect(await ui.find({ type: 'Text', text: 'Playing' })).toBeDefined()
    await ui.press({ key: 'play' })
    expect(fake.calls).toContain('stop')
    await ui.input({ key: 'bpm', text: '90' })
    expect(await ui.find({ type: 'Text', text: '90 BPM' })).toBeDefined()
    await ui.unmount()
  }
})

test('the pane offers a start button while the sequencer is offline', async ($, on) => {
  const { clock, connected } = engine(on)
  on('http.fetch', async () => {
    return { deny: 'ECONNREFUSED' } as never
  })
  const launched: string[][] = []
  on('process.run', async (_$, e) => {
    launched.push([...e.argv])
    return { value: { exitCode: 0, stdout: '', stderr: '' } } as never
  })
  await $.session.start({ cwd: 'C:/x', surface: 'desktop', isInteractive: true })
  await clock.advance(0)
  expect(launched.length).toBe(1)
  expect(connected).toEqual([])

  const ui = await $.ui.mount({ plugin: 'sequencer-panel', surface: 'desktop', component: 'Pane', props: PANE, requestId: 'sequencer' })
  expect(await ui.find({ type: 'Text', text: /Starting the sequencer/ })).toBeDefined()
  await ui.unmount()
})
