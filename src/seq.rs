use midir::{MidiOutput, MidiOutputConnection};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub const PPQ: u32 = 480;
pub const BEATS_PER_BAR: u32 = 4;
pub const MIN_BPM: f32 = 20.0;
pub const MAX_BPM: f32 = 300.0;
pub const MAX_BARS: u32 = 64;

pub const DEVICE_HINT: &str = "mpk";
// Windows ships this software synth. It plays MIDI through the PC soundcard.
pub const SOUNDCARD_HINT: &str = "gs wavetable";

// The metronome plays a side stick on the General MIDI drum channel.
const CLICK_CH: u8 = 9;
const CLICK_NOTE: u8 = 37;

#[derive(Clone, Copy)]
pub struct Note {
    pub start: u32,
    pub dur: u32,
    pub pitch: u8,
    pub vel: u8,
    pub ch: u8,
    pub take: u32,
}

pub struct Seq {
    pub bpm: f32,
    pub bars: u32,
    pub playing: bool,
    pub recording: bool,
    pub metronome: bool,
    pub thru: bool,
    pub pos: f64,
    pub notes: Vec<Note>,
    pub in_name: Option<String>,
    pub out_name: Option<String>,
    take: u32,
    pending: HashMap<(u8, u8), (u32, u8)>,
    sounding: Vec<(u8, u8)>,
    click_on: bool,
    // Only the channels that were set are sent, so the MPK keeps its own sounds otherwise.
    pub programs: [Option<u8>; 16],
    out: Option<MidiOutputConnection>,
}

impl Seq {
    pub fn new() -> Self {
        Seq {
            bpm: 100.0,
            bars: 4,
            playing: false,
            recording: false,
            metronome: true,
            thru: false,
            pos: 0.0,
            notes: Vec::new(),
            in_name: None,
            out_name: None,
            take: 0,
            pending: HashMap::new(),
            sounding: Vec::new(),
            click_on: false,
            programs: [None; 16],
            out: None,
        }
    }

    pub fn len_ticks(&self) -> u32 {
        self.bars * BEATS_PER_BAR * PPQ
    }

    pub fn set_bpm(&mut self, bpm: f32) {
        self.bpm = bpm.clamp(MIN_BPM, MAX_BPM);
    }

    pub fn set_bars(&mut self, bars: u32) {
        self.bars = bars.clamp(1, MAX_BARS);
        let len = self.len_ticks() as f64;
        if self.pos >= len {
            self.pos %= len;
        }
    }

    pub fn set_out(&mut self, out: Option<MidiOutputConnection>, name: Option<String>) {
        self.silence();
        self.out = out;
        self.out_name = name;
        // A new output starts with its default sounds, so the chosen ones go out again.
        for ch in 0..16u8 {
            if let Some(program) = self.programs[ch as usize] {
                self.send(&[0xC0 | ch, program]);
            }
        }
    }

    // Selects a General MIDI instrument, numbered from 0, on a channel numbered from 0.
    pub fn set_program(&mut self, ch: u8, program: u8) {
        self.programs[ch as usize] = Some(program);
        self.send(&[0xC0 | ch, program]);
    }

    // Opens the named output. MIDI thru turns on for every output except the MPK,
    // because only the MPK already plays what you press on it.
    pub fn connect_out(&mut self, name: &str) -> Result<(), String> {
        // The old connection must close first because Windows opens a port only once.
        self.set_out(None, None);
        let midi = MidiOutput::new("MidiSequencer out").map_err(|e| e.to_string())?;
        let port = midi
            .ports()
            .into_iter()
            .find(|p| midi.port_name(p).ok().as_deref() == Some(name))
            .ok_or("The port is gone.".to_string())?;
        let conn = midi.connect(&port, "out").map_err(|e| e.to_string())?;
        self.set_out(Some(conn), Some(name.to_string()));
        self.thru = !is_device(name);
        Ok(())
    }

    fn send(&mut self, msg: &[u8]) {
        if let Some(out) = self.out.as_mut() {
            let _ = out.send(msg);
        }
    }

    fn silence(&mut self) {
        for (ch, pitch) in std::mem::take(&mut self.sounding) {
            self.send(&[0x80 | ch, pitch, 0]);
        }
        if self.click_on {
            self.send(&[0x80 | CLICK_CH, CLICK_NOTE, 0]);
            self.click_on = false;
        }
    }

    pub fn play(&mut self) {
        if !self.playing {
            self.pos = 0.0;
            self.playing = true;
        }
    }

    pub fn stop(&mut self) {
        self.set_recording(false);
        self.playing = false;
        self.pos = 0.0;
        self.silence();
    }

    pub fn set_recording(&mut self, on: bool) {
        if on == self.recording {
            return;
        }
        if on {
            self.take += 1;
            self.play();
        } else {
            // Close the notes that are still held down.
            let held: Vec<(u8, u8)> = self.pending.keys().copied().collect();
            for (ch, pitch) in held {
                self.finish_note(ch, pitch);
            }
        }
        self.recording = on;
    }

    // Adds written notes as one new take, so undo removes them together.
    pub fn add_take(&mut self, notes: &[Note]) {
        self.take += 1;
        let take = self.take;
        self.notes.extend(notes.iter().map(|n| Note { take, ..*n }));
    }

    // Starts the given notes that the playhead is already inside, so they sound before the loop
    // comes round to their start. A key that already sounds is left alone, so it does not retrigger.
    pub fn sound_now(&mut self, notes: &[Note]) {
        for n in notes {
            let key = (n.ch, n.pitch);
            if self.covers(n) && !self.sounding.contains(&key) {
                self.send(&[0x90 | n.ch, n.pitch, n.vel]);
                self.sounding.push(key);
            }
        }
    }

    // Removes the notes that match and returns how many went. Every other note keeps ringing.
    // A removed key gets a note-off only when it sounds now and no kept note on it covers the
    // playhead, because one note-off stops the key for every note on it.
    pub fn remove_notes(&mut self, matches: impl Fn(&Note) -> bool) -> usize {
        let before = self.notes.len();
        let mut keys = Vec::new();
        self.notes.retain(|n| {
            let hit = matches(n);
            if hit {
                keys.push((n.ch, n.pitch));
            }
            !hit
        });
        keys.sort_unstable();
        keys.dedup();
        for key in keys {
            let kept = self.notes.iter().any(|n| (n.ch, n.pitch) == key && self.covers(n));
            if self.sounding.contains(&key) && !kept {
                self.send(&[0x80 | key.0, key.1, 0]);
                self.sounding.retain(|s| *s != key);
            }
        }
        before - self.notes.len()
    }

    // True when the playhead is past the note's start and not yet past its end. fire() sends
    // the note-on once the playhead has passed the start, and the note-off once it has passed the end.
    fn covers(&self, n: &Note) -> bool {
        let len = self.len_ticks();
        if !self.playing || n.start >= len {
            return false;
        }
        let elapsed = (self.pos - n.start as f64).rem_euclid(len as f64);
        elapsed > 0.0 && elapsed <= n.dur as f64
    }

    pub fn undo_take(&mut self) {
        if let Some(last) = self.notes.iter().map(|n| n.take).max() {
            self.silence();
            self.notes.retain(|n| n.take != last);
        }
    }

    pub fn clear(&mut self) {
        self.silence();
        self.notes.clear();
        self.pending.clear();
    }

    fn finish_note(&mut self, ch: u8, pitch: u8) {
        if let Some((start, vel)) = self.pending.remove(&(ch, pitch)) {
            let len = self.len_ticks();
            let now = self.pos as u32 % len;
            let dur = ((now + len - start % len) % len).clamp(1, len - 1);
            self.notes.push(Note { start: start % len, dur, pitch, vel, ch, take: self.take });
        }
    }

    pub fn on_midi_in(&mut self, msg: &[u8]) {
        if self.thru {
            self.send(msg);
        }
        if msg.len() < 3 {
            return;
        }
        let (kind, ch, pitch, vel) = (msg[0] & 0xF0, msg[0] & 0x0F, msg[1], msg[2]);
        match kind {
            0x90 if vel > 0 => {
                if self.recording {
                    self.pending.insert((ch, pitch), (self.pos as u32, vel));
                }
            }
            0x80 | 0x90 => self.finish_note(ch, pitch),
            _ => {}
        }
    }

    pub fn advance(&mut self, dt: Duration) {
        if !self.playing {
            return;
        }
        let len = self.len_ticks() as f64;
        let prev = self.pos;
        let mut next = prev + dt.as_secs_f64() * self.bpm as f64 / 60.0 * PPQ as f64;
        if next >= len {
            self.fire(prev, len);
            next = (next - len).min(len - 1.0);
            self.fire(0.0, next);
        } else {
            self.fire(prev, next);
        }
        self.pos = next;
    }

    // Sends every event whose tick lies in the half-open range [from, to).
    fn fire(&mut self, from: f64, to: f64) {
        let len = self.len_ticks();
        let hit = |t: u32| (t as f64) >= from && (t as f64) < to;
        let mut offs = Vec::new();
        let mut ons = Vec::new();
        for n in &self.notes {
            if n.start >= len {
                continue;
            }
            if hit((n.start + n.dur) % len) {
                offs.push((n.ch, n.pitch));
            }
            if hit(n.start) {
                ons.push((n.ch, n.pitch, n.vel));
            }
        }
        for (ch, pitch) in offs {
            self.send(&[0x80 | ch, pitch, 0]);
            self.sounding.retain(|s| *s != (ch, pitch));
        }
        for (ch, pitch, vel) in ons {
            self.send(&[0x90 | ch, pitch, vel]);
            self.sounding.push((ch, pitch));
        }
        if self.metronome {
            let beat = (from / PPQ as f64).ceil() as u32;
            if hit(beat * PPQ) {
                if self.click_on {
                    self.send(&[0x80 | CLICK_CH, CLICK_NOTE, 0]);
                }
                let vel = if beat % BEATS_PER_BAR == 0 { 127 } else { 70 };
                self.send(&[0x90 | CLICK_CH, CLICK_NOTE, vel]);
                self.click_on = true;
            }
        }
    }

    // Builds a format 0 Standard MIDI File of one pass through the loop.
    pub fn to_smf(&self) -> Vec<u8> {
        let len = self.len_ticks();
        let mut events: Vec<(u32, u8, [u8; 3])> = Vec::new();
        for n in self.notes.iter().filter(|n| n.start < len) {
            events.push((n.start, 1, [0x90 | n.ch, n.pitch, n.vel]));
            events.push(((n.start + n.dur).min(len), 0, [0x80 | n.ch, n.pitch, 0]));
        }
        events.sort_by_key(|e| (e.0, e.1));

        let mut track = Vec::new();
        let tempo = (60_000_000.0 / self.bpm as f64).round() as u32;
        track.extend_from_slice(&[0x00, 0xFF, 0x51, 0x03]);
        track.extend_from_slice(&tempo.to_be_bytes()[1..]);
        track.extend_from_slice(&[0x00, 0xFF, 0x58, 0x04, BEATS_PER_BAR as u8, 2, 24, 8]);
        // Other players start every channel on piano, so the chosen instruments go first.
        for (ch, program) in self.programs.iter().enumerate() {
            if let Some(p) = program {
                track.extend_from_slice(&[0x00, 0xC0 | ch as u8, *p]);
            }
        }
        let mut last = 0;
        for (tick, _, msg) in events {
            write_vlq(&mut track, tick - last);
            track.extend_from_slice(&msg);
            last = tick;
        }
        write_vlq(&mut track, len - last);
        track.extend_from_slice(&[0xFF, 0x2F, 0x00]);

        let mut smf = Vec::new();
        smf.extend_from_slice(b"MThd");
        smf.extend_from_slice(&6u32.to_be_bytes());
        smf.extend_from_slice(&0u16.to_be_bytes());
        smf.extend_from_slice(&1u16.to_be_bytes());
        smf.extend_from_slice(&(PPQ as u16).to_be_bytes());
        smf.extend_from_slice(b"MTrk");
        smf.extend_from_slice(&(track.len() as u32).to_be_bytes());
        smf.extend_from_slice(&track);
        smf
    }

    // Writes the loop into the export folder. The name can not leave that folder.
    pub fn export(&self, name: Option<&str>) -> Result<PathBuf, String> {
        let stem: String = name
            .unwrap_or("")
            .trim()
            .trim_end_matches(".mid")
            .chars()
            .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' '))
            .collect();
        let stem = if stem.trim().is_empty() {
            format!("loop-{}", chrono::Local::now().format("%Y%m%d-%H%M%S"))
        } else {
            stem.trim().to_string()
        };
        let dir = export_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{stem}.mid"));
        std::fs::write(&path, self.to_smf()).map_err(|e| e.to_string())?;
        Ok(path)
    }

    // Replaces the loop with an imported file as one take. It returns how many notes started
    // past the longest loop and were dropped.
    pub fn load_smf(&mut self, smf: Smf) -> usize {
        self.stop();
        self.clear();
        self.set_bpm(smf.bpm);
        self.set_bars(smf.end.div_ceil(BEATS_PER_BAR * PPQ));
        let len = self.len_ticks();
        let total = smf.notes.len();
        let notes: Vec<Note> = smf
            .notes
            .into_iter()
            .filter(|n| n.start < len)
            .map(|n| Note { dur: n.dur.min(len - n.start).clamp(1, len - 1), ..n })
            .collect();
        let dropped = total - notes.len();
        self.add_take(&notes);
        self.programs = [None; 16];
        for (ch, program) in smf.programs.iter().enumerate() {
            if let Some(p) = program {
                self.set_program(ch as u8, *p);
            }
        }
        dropped
    }
}

// What import_midi takes from a Standard MIDI File. Every tick is already at PPQ.
pub struct Smf {
    pub bpm: f32,
    pub end: u32,
    pub notes: Vec<Note>,
    pub programs: [Option<u8>; 16],
}

// Finds the file for import_midi: an absolute path as given, or a name in the export folder.
// A name without an extension gets .mid, as export_midi adds it.
pub fn import_path(name: &str) -> PathBuf {
    let given = PathBuf::from(name.trim());
    let mut path = if given.is_absolute() { given } else { export_dir().join(given) };
    if path.extension().is_none() {
        path.set_extension("mid");
    }
    path
}

pub fn read_smf(name: &str) -> Result<(PathBuf, Smf), String> {
    let path = import_path(name);
    let data = std::fs::read(&path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    Ok((path, parse_smf(&data)?))
}

// Reads a format 0 or 1 Standard MIDI File. It keeps the notes, the earliest tempo and the
// earliest Program Change on each channel, and skips every other event.
pub fn parse_smf(data: &[u8]) -> Result<Smf, String> {
    let mut r = Reader { data, pos: 0 };
    if data.len() < 14 || r.take(4)? != b"MThd" {
        return Err("The file is not a Standard MIDI File.".into());
    }
    let header_len = r.u32()? as usize;
    if header_len < 6 {
        return Err("The file header is too short.".into());
    }
    let header = r.take(header_len)?;
    let format = u16::from_be_bytes([header[0], header[1]]);
    let division = u16::from_be_bytes([header[4], header[5]]) as u64;
    if format > 1 {
        return Err(format!("The file is format {format}. Only formats 0 and 1 can be imported."));
    }
    if division & 0x8000 != 0 {
        return Err("The file counts time in SMPTE frames. Only files that count in beats can be imported.".into());
    }
    if division == 0 {
        return Err("The file header gives 0 ticks per beat.".into());
    }

    let mut p = Parsed::default();
    let mut tracks = 0;
    // Some files carry a few bytes of padding after the last chunk.
    while data.len() - r.pos >= 8 {
        let id = r.take(4)?;
        let len = r.u32()? as usize;
        let body = r.take(len)?;
        if id == b"MTrk" {
            p.track(body)?;
            tracks += 1;
        }
    }
    if tracks == 0 {
        return Err("The file has no tracks.".into());
    }

    let scale = |t: u32| ((t as u64 * PPQ as u64 + division / 2) / division).min(u32::MAX as u64) as u32;
    let notes = p
        .notes
        .iter()
        .map(|n| {
            let start = scale(n.start);
            let end = scale(n.start.saturating_add(n.dur));
            Note { start, dur: (end - start).max(1), ..*n }
        })
        .collect();
    // A file without a tempo plays at 120 beats per minute.
    let us = p.tempo.map_or(500_000, |(_, us)| us);
    Ok(Smf {
        bpm: (60_000_000.0 / us as f64) as f32,
        end: scale(p.end),
        notes,
        programs: p.programs.map(|p| p.map(|(_, program)| program)),
    })
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).filter(|e| *e <= self.data.len()).ok_or("The file is cut short.")?;
        let bytes = &self.data[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }

    fn byte(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn vlq(&mut self) -> Result<u32, String> {
        let mut value = 0u32;
        for _ in 0..4 {
            let b = self.byte()?;
            value = (value << 7) | (b & 0x7F) as u32;
            if b & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err("The file holds a number longer than four bytes.".into())
    }
}

// The events of every track so far, in the file's own ticks. Tempo and programs keep their
// tick, so the earliest one wins across tracks.
#[derive(Default)]
struct Parsed {
    tempo: Option<(u32, u32)>,
    programs: [Option<(u32, u8)>; 16],
    notes: Vec<Note>,
    end: u32,
}

impl Parsed {
    fn track(&mut self, body: &[u8]) -> Result<(), String> {
        let mut r = Reader { data: body, pos: 0 };
        let mut tick = 0u32;
        let mut running: Option<u8> = None;
        // A repeated note-on stacks up, and each note-off closes the oldest one.
        let mut held: HashMap<(u8, u8), VecDeque<(u32, u8)>> = HashMap::new();
        while r.pos < body.len() {
            tick = tick.saturating_add(r.vlq()?);
            let mut status = r.byte()?;
            if status < 0x80 {
                status = running.ok_or("A track has a data byte where an event should start.")?;
                r.pos -= 1;
            }
            match status {
                0xFF => {
                    let kind = r.byte()?;
                    let len = r.vlq()? as usize;
                    let d = r.take(len)?;
                    if kind == 0x51 && len == 3 && self.tempo.map_or(true, |(t, _)| tick < t) {
                        let us = u32::from_be_bytes([0, d[0], d[1], d[2]]);
                        if us > 0 {
                            self.tempo = Some((tick, us));
                        }
                    }
                    if kind == 0x2F {
                        break;
                    }
                }
                0xF0 | 0xF7 => {
                    let len = r.vlq()? as usize;
                    r.take(len)?;
                }
                0x80..=0xEF => {
                    running = Some(status);
                    let (kind, ch) = (status & 0xF0, status & 0x0F);
                    let a = r.byte()?;
                    let b = if matches!(kind, 0xC0 | 0xD0) { 0 } else { r.byte()? };
                    if a >= 0x80 || b >= 0x80 {
                        return Err("The file holds a broken channel event.".into());
                    }
                    match kind {
                        0x90 if b > 0 => held.entry((ch, a)).or_default().push_back((tick, b)),
                        0x80 | 0x90 => {
                            if let Some((start, vel)) = held.get_mut(&(ch, a)).and_then(VecDeque::pop_front) {
                                self.notes.push(Note { start, dur: tick - start, pitch: a, vel, ch, take: 0 });
                            }
                        }
                        0xC0 if self.programs[ch as usize].map_or(true, |(t, _)| tick < t) => {
                            self.programs[ch as usize] = Some((tick, a));
                        }
                        _ => {}
                    }
                }
                _ => return Err(format!("The file holds the unknown event byte 0x{status:02X}.")),
            }
        }
        // Notes still held at the end of the track stop there.
        for ((ch, pitch), starts) in held {
            for (start, vel) in starts {
                self.notes.push(Note { start, dur: tick - start, pitch, vel, ch, take: 0 });
            }
        }
        self.end = self.end.max(tick);
        Ok(())
    }
}

pub fn out_ports() -> Vec<String> {
    MidiOutput::new("MidiSequencer scan")
        .map(|m| m.ports().iter().filter_map(|p| m.port_name(p).ok()).collect())
        .unwrap_or_default()
}

pub fn is_device(name: &str) -> bool {
    name.to_lowercase().contains(DEVICE_HINT)
}

pub fn is_soundcard(name: &str) -> bool {
    name.to_lowercase().contains(SOUNDCARD_HINT)
}

pub fn export_dir() -> PathBuf {
    let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Documents").join("MidiSequencer")
}

fn write_vlq(buf: &mut Vec<u8>, value: u32) {
    let mut bytes = vec![(value & 0x7F) as u8];
    let mut rest = value >> 7;
    while rest > 0 {
        bytes.push((rest & 0x7F) as u8 | 0x80);
        rest >>= 7;
    }
    bytes.reverse();
    buf.extend_from_slice(&bytes);
}

pub fn note_name(pitch: u8) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    format!("{}{}", NAMES[pitch as usize % 12], pitch as i32 / 12 - 1)
}

pub fn spawn_engine(seq: Arc<Mutex<Seq>>) {
    thread::spawn(move || {
        let mut last = Instant::now();
        loop {
            thread::sleep(Duration::from_millis(1));
            let now = Instant::now();
            seq.lock().unwrap().advance(now - last);
            last = now;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_a_note_and_exports_it() {
        let mut s = Seq::new();
        s.bpm = 120.0;
        s.set_recording(true);
        s.advance(Duration::from_millis(500));
        s.on_midi_in(&[0x90, 60, 100]);
        s.advance(Duration::from_millis(250));
        s.on_midi_in(&[0x80, 60, 0]);
        assert_eq!(s.notes.len(), 1);
        assert_eq!((s.notes[0].start, s.notes[0].dur), (480, 240));

        let smf = s.to_smf();
        assert_eq!(&smf[..4], b"MThd");
        let track_len = u32::from_be_bytes(smf[18..22].try_into().unwrap()) as usize;
        assert_eq!(smf.len(), 22 + track_len);
        assert_eq!(&smf[smf.len() - 3..], &[0xFF, 0x2F, 0x00]);
    }

    #[test]
    fn export_selects_the_chosen_instruments_at_the_start() {
        let mut s = Seq::new();
        s.set_program(2, 32);
        s.set_program(9, 0);
        s.add_take(&[Note { start: 0, dur: 480, pitch: 60, vel: 100, ch: 2, take: 0 }]);
        let smf = s.to_smf();
        let track = &smf[22..];
        // The tempo and time signature meta events take the first 15 bytes.
        assert_eq!(&track[15..21], &[0x00, 0xC2, 32, 0x00, 0xC9, 0]);
        assert_eq!(&track[21..25], &[0x00, 0x92, 60, 100]);
    }

    #[test]
    fn position_wraps_at_the_loop_end() {
        let mut s = Seq::new();
        s.bpm = 120.0;
        s.set_bars(1);
        s.play();
        s.advance(Duration::from_millis(2250));
        assert!((s.pos - 240.0).abs() < 1.0);
    }

    #[test]
    fn written_notes_undo_as_one_take() {
        let mut s = Seq::new();
        let n = Note { start: 0, dur: 480, pitch: 60, vel: 100, ch: 0, take: 0 };
        s.add_take(&[n, Note { start: 480, ..n }]);
        assert_eq!(s.notes.len(), 2);
        s.undo_take();
        assert!(s.notes.is_empty());
    }

    // Plays one 4-beat bar at 120 beats per minute up to beat 2, which is tick 960.
    fn playing_at_beat_two(notes: &[Note]) -> Seq {
        let mut s = Seq::new();
        s.bpm = 120.0;
        s.set_bars(1);
        s.add_take(notes);
        s.play();
        s.advance(Duration::from_millis(1000));
        s
    }

    #[test]
    fn removing_notes_stops_only_the_removed_ones() {
        let n = Note { start: 0, dur: 1900, pitch: 60, vel: 100, ch: 0, take: 0 };
        let mut s = playing_at_beat_two(&[n, Note { pitch: 64, ..n }, Note { pitch: 67, ch: 1, ..n }]);
        assert_eq!(s.sounding.len(), 3);

        assert_eq!(s.remove_notes(|n| n.pitch == 64), 1);
        assert_eq!(s.sounding, [(0, 60), (1, 67)]);
        assert_eq!(s.remove_notes(|n| n.ch == 1), 1);
        assert_eq!(s.sounding, [(0, 60)]);
        assert_eq!(s.remove_notes(|n| n.pitch == 99), 0);
        assert_eq!(s.notes.len(), 1);
        assert!(s.playing);
    }

    #[test]
    fn a_kept_note_on_the_same_key_keeps_ringing() {
        let long = Note { start: 0, dur: 1900, pitch: 60, vel: 100, ch: 0, take: 0 };
        let short = Note { start: 480, dur: 960, ..long };
        let mut s = playing_at_beat_two(&[long, short]);

        assert_eq!(s.remove_notes(|n| n.start == 480), 1);
        assert!(s.sounding.contains(&(0, 60)));
        assert_eq!(s.remove_notes(|n| n.pitch == 60), 1);
        assert!(s.sounding.is_empty());
    }

    #[test]
    fn sound_now_starts_only_the_notes_under_the_playhead() {
        let inside = Note { start: 480, dur: 960, pitch: 60, vel: 100, ch: 0, take: 0 };
        let later = Note { start: 1440, pitch: 62, ..inside };
        let ended = Note { start: 0, dur: 480, pitch: 64, ..inside };
        let mut s = playing_at_beat_two(&[]);
        s.add_take(&[inside, later, ended]);
        s.sound_now(&[inside, later, ended]);
        assert_eq!(s.sounding, [(0, 60)]);

        // The key already sounds, so a second note on it does not retrigger it.
        s.sound_now(&[inside]);
        assert_eq!(s.sounding, [(0, 60)]);

        s.stop();
        s.sound_now(&[inside]);
        assert!(s.sounding.is_empty());
    }

    #[test]
    fn remembers_only_the_instruments_that_were_set() {
        let mut s = Seq::new();
        s.set_program(1, 4);
        assert_eq!(s.programs[1], Some(4));
        assert_eq!(s.programs.iter().flatten().count(), 1);
    }

    #[test]
    fn recognises_the_mpk_and_the_soundcard() {
        assert!(is_device("MPK mini Play mk3"));
        assert!(!is_device("Microsoft GS Wavetable Synth"));
        assert!(is_soundcard("Microsoft GS Wavetable Synth"));
    }

    // Builds a Standard MIDI File from finished track bodies.
    fn smf_file(format: u16, division: u16, tracks: &[&[u8]]) -> Vec<u8> {
        let mut f = b"MThd".to_vec();
        f.extend_from_slice(&6u32.to_be_bytes());
        f.extend_from_slice(&format.to_be_bytes());
        f.extend_from_slice(&(tracks.len() as u16).to_be_bytes());
        f.extend_from_slice(&division.to_be_bytes());
        for t in tracks {
            f.extend_from_slice(b"MTrk");
            f.extend_from_slice(&(t.len() as u32).to_be_bytes());
            f.extend_from_slice(t);
        }
        f
    }

    fn sorted(notes: &[Note]) -> Vec<(u32, u32, u8, u8, u8)> {
        let mut v: Vec<_> = notes.iter().map(|n| (n.start, n.dur, n.pitch, n.vel, n.ch)).collect();
        v.sort();
        v
    }

    #[test]
    fn an_exported_loop_imports_unchanged() {
        let mut s = Seq::new();
        s.bpm = 90.0;
        s.set_bars(3);
        s.set_program(2, 32);
        let n = Note { start: 0, dur: 480, pitch: 60, vel: 100, ch: 0, take: 0 };
        s.add_take(&[n, Note { start: 1440, dur: 960, pitch: 43, vel: 80, ch: 2, ..n }]);
        s.add_take(&[Note { start: 5520, dur: 240, pitch: 64, vel: 90, ..n }]);

        let mut t = Seq::new();
        t.set_bars(8);
        t.set_program(5, 1);
        t.add_take(&[n]);
        t.play();
        let dropped = t.load_smf(parse_smf(&s.to_smf()).unwrap());

        assert_eq!(dropped, 0);
        assert!(!t.playing);
        assert_eq!(t.bars, 3);
        assert!((t.bpm - 90.0).abs() < 0.01);
        assert_eq!(t.programs, s.programs);
        assert_eq!(sorted(&t.notes), sorted(&s.notes));
        let take = t.notes[0].take;
        assert!(t.notes.iter().all(|n| n.take == take));
        t.undo_take();
        assert!(t.notes.is_empty());
    }

    #[test]
    fn imports_format_1_at_another_resolution() {
        // 120 beats per minute in the tempo track.
        let tempo: &[u8] = &[0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20, 0x00, 0xFF, 0x2F, 0x00];
        // At 96 ticks per beat: program 5 on channel 2, C4 for one beat, then E4 for two beats.
        // The second and third events use running status, and a note-on with velocity 0 ends C4.
        let notes: &[u8] = &[
            0x00, 0xC1, 5, 0x00, 0x91, 60, 100, 0x60, 60, 0, 0x00, 64, 80, 0x81, 0x40, 0x81, 64, 0,
            0x00, 0xFF, 0x2F, 0x00,
        ];
        let smf = parse_smf(&smf_file(1, 96, &[tempo, notes])).unwrap();
        assert_eq!(smf.bpm, 120.0);
        assert_eq!(smf.end, 1440);
        assert_eq!(smf.programs[1], Some(5));
        assert_eq!(sorted(&smf.notes), [(0, 480, 60, 100, 1), (480, 960, 64, 80, 1)]);

        let mut s = Seq::new();
        s.load_smf(smf);
        assert_eq!(s.bars, 1);
    }

    #[test]
    fn drops_the_notes_past_the_longest_loop() {
        let last = MAX_BARS * BEATS_PER_BAR * PPQ;
        let mut track = Vec::new();
        // Each entry is a tick and an event. The second note runs past the end of bar 64.
        let events: [(u32, [u8; 3]); 6] = [
            (0, [0x90, 60, 100]),
            (480, [0x80, 60, 0]),
            (last - 240, [0x90, 62, 100]),
            (last, [0x90, 64, 100]),
            (last + 720, [0x80, 62, 0]),
            (last + 720, [0x80, 64, 0]),
        ];
        let mut prev = 0;
        for (tick, msg) in events {
            write_vlq(&mut track, tick - prev);
            track.extend_from_slice(&msg);
            prev = tick;
        }
        track.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);

        let mut s = Seq::new();
        let dropped = s.load_smf(parse_smf(&smf_file(0, PPQ as u16, &[&track])).unwrap());
        assert_eq!(dropped, 1);
        assert_eq!(s.bars, MAX_BARS);
        assert_eq!(sorted(&s.notes), [(0, 480, 60, 100, 0), (last - 240, 240, 62, 100, 0)]);
    }

    #[test]
    fn refuses_files_it_can_not_read() {
        let eot: &[u8] = &[0x00, 0xFF, 0x2F, 0x00];
        assert!(parse_smf(b"RIFF....WAVEfmt ").is_err());
        assert!(parse_smf(&smf_file(2, 480, &[eot])).is_err());
        assert!(parse_smf(&smf_file(0, 0xE728, &[eot])).is_err());
        assert!(parse_smf(&smf_file(0, 480, &[])).is_err());
        let mut cut = smf_file(0, 480, &[&[0x00, 0x90, 60, 100, 0x00, 0xFF, 0x2F, 0x00]]);
        cut.truncate(cut.len() - 3);
        assert!(parse_smf(&cut).is_err());
    }

    #[test]
    fn import_finds_names_in_the_export_folder() {
        assert_eq!(import_path(" groove "), export_dir().join("groove.mid"));
        assert_eq!(import_path("groove.mid"), export_dir().join("groove.mid"));
        assert_eq!(import_path(r"C:\Music\song.mid"), PathBuf::from(r"C:\Music\song.mid"));
    }

    #[test]
    fn vlq_matches_the_midi_spec() {
        let mut b = Vec::new();
        write_vlq(&mut b, 0x3FFF);
        write_vlq(&mut b, 0x80);
        assert_eq!(b, [0xFF, 0x7F, 0x81, 0x00]);
    }
}
