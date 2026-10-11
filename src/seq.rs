use midir::{MidiOutput, MidiOutputConnection};
use std::collections::HashMap;
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

    #[test]
    fn recognises_the_mpk_and_the_soundcard() {
        assert!(is_device("MPK mini Play mk3"));
        assert!(!is_device("Microsoft GS Wavetable Synth"));
        assert!(is_soundcard("Microsoft GS Wavetable Synth"));
    }

    #[test]
    fn vlq_matches_the_midi_spec() {
        let mut b = Vec::new();
        write_vlq(&mut b, 0x3FFF);
        write_vlq(&mut b, 0x80);
        assert_eq!(b, [0xFF, 0x7F, 0x81, 0x00]);
    }
}
