use crate::ports::{self, Ports};
use crate::seq::{self, note_name, Note, Seq, BEATS_PER_BAR, MAX_BARS, MAX_BPM, MIN_BPM, PPQ};
use serde_json::{json, Value};
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response, Server};

pub const ADDR: &str = "127.0.0.1:7391";
const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

// Serves MCP over streamable HTTP on localhost. Every reply is plain JSON.
pub fn spawn(seq: Arc<Mutex<Seq>>, ports: Arc<Mutex<Ports>>) -> Result<(), String> {
    let server = Server::http(ADDR).map_err(|e| e.to_string())?;
    thread::spawn(move || {
        for req in server.incoming_requests() {
            handle(req, &seq, &ports);
        }
    });
    Ok(())
}

fn handle(mut req: Request, seq: &Arc<Mutex<Seq>>, ports: &Arc<Mutex<Ports>>) {
    // A browser page on another origin must not be able to drive the sequencer.
    let foreign_origin = req.headers().iter().any(|h| {
        h.field.equiv("Origin") && {
            let v = h.value.as_str();
            !(v.starts_with("http://127.0.0.1") || v.starts_with("http://localhost"))
        }
    });
    if foreign_origin {
        let _ = req.respond(Response::empty(403));
        return;
    }
    if req.url().split('?').next() != Some("/mcp") {
        let _ = req.respond(Response::empty(404));
        return;
    }
    if *req.method() != Method::Post {
        let _ = req.respond(Response::empty(405));
        return;
    }

    let mut body = String::new();
    if req.as_reader().take(1 << 20).read_to_string(&mut body).is_err() {
        let _ = req.respond(Response::empty(400));
        return;
    }
    let reply = match serde_json::from_str::<Value>(&body) {
        Ok(Value::Array(batch)) => {
            let out: Vec<Value> = batch.iter().filter_map(|m| rpc(m, seq, ports)).collect();
            (!out.is_empty()).then(|| Value::Array(out))
        }
        Ok(msg) => rpc(&msg, seq, ports),
        Err(_) => Some(json!({"jsonrpc": "2.0", "id": null,
            "error": {"code": -32700, "message": "Parse error"}})),
    };
    let _ = match reply {
        Some(v) => {
            let header = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
            req.respond(Response::from_string(v.to_string()).with_header(header))
        }
        None => req.respond(Response::empty(202)),
    };
}

fn rpc(msg: &Value, seq: &Arc<Mutex<Seq>>, ports: &Arc<Mutex<Ports>>) -> Option<Value> {
    // A message without an id is a notification and gets no reply.
    let id = msg.get("id")?.clone();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match msg.get("method").and_then(Value::as_str).unwrap_or("") {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
            let version = if PROTOCOL_VERSIONS.contains(&asked) { asked } else { PROTOCOL_VERSIONS[0] };
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "midi-sequencer", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Controls a MIDI loop recorder. The user plays the notes on a MIDI keyboard. These tools control the transport, read the recorded loop, and export it."
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);
            let (text, is_error) = match call_tool(name, &args, seq, ports) {
                Ok(v) => (v.to_string(), false),
                Err(e) => (e, true),
            };
            json!({"content": [{"type": "text", "text": text}], "isError": is_error})
        }
        _ => {
            return Some(json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": "Method not found"}}))
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn tools() -> Value {
    let none = json!({"type": "object", "properties": {}});
    json!([
        {"name": "get_status", "inputSchema": none,
         "description": "Returns the transport state, tempo, loop length, playhead position, note count, and MIDI ports."},
        {"name": "play", "inputSchema": none,
         "description": "Starts loop playback from the first bar."},
        {"name": "stop", "inputSchema": none,
         "description": "Stops playback and recording and rewinds to the first bar."},
        {"name": "set_recording",
         "description": "Turns overdub recording on or off. Turning it on also starts playback.",
         "inputSchema": {"type": "object", "required": ["enabled"],
            "properties": {"enabled": {"type": "boolean"}}}},
        {"name": "set_tempo",
         "description": "Sets the tempo in beats per minute.",
         "inputSchema": {"type": "object", "required": ["bpm"],
            "properties": {"bpm": {"type": "number", "minimum": MIN_BPM, "maximum": MAX_BPM}}}},
        {"name": "set_loop_length",
         "description": "Sets the loop length in 4/4 bars. Notes beyond a shorter loop are kept but stay silent.",
         "inputSchema": {"type": "object", "required": ["bars"],
            "properties": {"bars": {"type": "integer", "minimum": 1, "maximum": MAX_BARS}}}},
        {"name": "set_midi_out",
         "description": "Switches the MIDI output. Use a name from midi_out_ports in the status. 'Microsoft GS Wavetable Synth' plays through the PC soundcard. MIDI thru turns on for every output except the MPK.",
         "inputSchema": {"type": "object", "required": ["port"],
            "properties": {"port": {"type": "string"}}}},
        {"name": "add_notes",
         "description": "Writes notes into the loop as one new take, so 'Undo last take' removes them together. Positions and durations are in beats, and start_beat 0 is the loop start. A new note first sounds when the playhead reaches its start, unless sound_now is true.",
         "inputSchema": {"type": "object", "required": ["notes"],
            "properties": {
                "sound_now": {"type": "boolean", "description": "When true and the loop is playing, notes whose span already holds the playhead start at once. The default is false."},
                "notes": {"type": "array", "items": {"type": "object",
                "required": ["start_beat", "duration_beats", "pitch"],
                "properties": {
                    "start_beat": {"type": "number", "minimum": 0},
                    "duration_beats": {"type": "number", "exclusiveMinimum": 0},
                    "pitch": {"type": "integer", "minimum": 0, "maximum": 127},
                    "velocity": {"type": "integer", "minimum": 1, "maximum": 127, "description": "The default is 100."},
                    "channel": {"type": "integer", "minimum": 1, "maximum": 16, "description": "The default is 1. Channel 10 plays drums."}}}}}}},
        {"name": "remove_notes",
         "description": "Removes every note that matches all the given filters. Give at least one of channel, pitch and start_beat. Only removed notes that sound now are stopped, so every other note keeps ringing. undo_take can not bring removed notes back. Returns the status and the removed note count.",
         "inputSchema": {"type": "object",
            "properties": {
                "channel": {"type": "integer", "minimum": 1, "maximum": 16},
                "pitch": {"type": "integer", "minimum": 0, "maximum": 127},
                "start_beat": {"type": "number", "minimum": 0, "description": "The start in beats, as get_notes shows it."}}}},
        {"name": "set_instrument",
         "description": "Selects a General MIDI instrument for one channel, numbered 1 to 128 as in the GM list. Examples are 1 Acoustic Grand Piano, 5 Electric Piano 1, 33 Acoustic Bass, 34 Electric Bass (finger), 49 String Ensemble 1, and 89 Pad 1 (new age). On channel 10 the number picks a drum kit. The MPK plays on channel 1, so put written parts on another channel to give them their own sound.",
         "inputSchema": {"type": "object", "required": ["channel", "program"],
            "properties": {
                "channel": {"type": "integer", "minimum": 1, "maximum": 16},
                "program": {"type": "integer", "minimum": 1, "maximum": 128}}}},
        {"name": "get_notes", "inputSchema": none,
         "description": "Returns every recorded note in the loop. Positions and durations are in beats, and start_beat 0 is the loop start. Channel 10 holds the drum pads."},
        {"name": "export_midi",
         "description": "Exports the loop as a .mid file into the export folder and returns the full path.",
         "inputSchema": {"type": "object",
            "properties": {"filename": {"type": "string",
                "description": "Optional file name without a folder. The default is a timestamped name."}}}},
        {"name": "import_midi",
         "description": format!("Loads a format 0 or 1 .mid file and replaces the loop with its notes as one take. It stops playback and takes the tempo, the loop length in whole 4/4 bars, and the instruments from the file. Notes that start after bar {MAX_BARS} are dropped. Returns the path, the note count, and the dropped note count."),
         "inputSchema": {"type": "object", "required": ["file"],
            "properties": {"file": {"type": "string",
                "description": "A file name in the export folder, or an absolute path. A name without an extension gets .mid."}}}},
        {"name": "set_midi_in",
         "description": "Switches the MIDI input. Use a name from midi_in_ports in the status.",
         "inputSchema": {"type": "object", "required": ["port"],
            "properties": {"port": {"type": "string"}}}},
        {"name": "set_metronome",
         "description": "Turns the metronome on or off. It plays a side stick on the drum channel on every beat.",
         "inputSchema": {"type": "object", "required": ["enabled"],
            "properties": {"enabled": {"type": "boolean"}}}},
        {"name": "set_midi_thru",
         "description": "Turns MIDI thru on or off. Thru forwards what the user plays to the output.",
         "inputSchema": {"type": "object", "required": ["enabled"],
            "properties": {"enabled": {"type": "boolean"}}}},
        {"name": "undo_take", "inputSchema": none,
         "description": "Removes the notes of the latest take, which is one recording pass or one add_notes call."},
        {"name": "clear", "inputSchema": none,
         "description": "Removes every note from the loop."},
        {"name": "open_export_folder", "inputSchema": none,
         "description": "Opens the export folder in Windows Explorer on the user's screen."},
        {"name": "quit", "inputSchema": none,
         "description": "Closes the app. Notes that were not exported are lost."}
    ])
}

// Tools that need the ports run before the Seq lock is taken, because Ports takes that lock
// itself. Every other tool runs under the Seq lock. Both answer with the status.
// import_midi reads its file before it takes the lock, so the engine never waits on the disk.
fn call_tool(name: &str, args: &Value, seq: &Arc<Mutex<Seq>>, ports: &Arc<Mutex<Ports>>) -> Result<Value, String> {
    match name {
        "import_midi" => {
            let file = args.get("file").and_then(Value::as_str).ok_or("The argument 'file' must be a string.")?;
            let (path, smf) = seq::read_smf(file)?;
            let mut s = seq.lock().unwrap();
            let dropped = s.load_smf(smf);
            return Ok(json!({"path": path.to_string_lossy(), "notes": s.notes.len(),
                "dropped_notes": dropped, "bpm": s.bpm, "bars": s.bars}));
        }
        "set_midi_in" => {
            let port = args.get("port").and_then(Value::as_str).ok_or("The argument 'port' must be a string.")?;
            let mut p = ports.lock().unwrap();
            if !p.in_ports.iter().any(|n| n == port) {
                p.scan();
            }
            if !p.in_ports.iter().any(|n| n == port) {
                return Err(format!("There is no MIDI input named '{port}'. The inputs are {:?}.", p.in_ports));
            }
            p.connect_in(port.to_string())?;
        }
        "open_export_folder" => ports::open_export_folder(),
        "quit" => {
            // The reply goes out first, then the process ends.
            thread::spawn(|| {
                thread::sleep(Duration::from_millis(300));
                std::process::exit(0);
            });
        }
        _ => {
            let lists = PortLists::of(ports);
            let mut s = seq.lock().unwrap();
            return call(name, args, &mut s, &lists).map(|v| v.unwrap_or_else(|| status(&s, &lists)));
        }
    }
    let lists = PortLists::of(ports);
    Ok(status(&seq.lock().unwrap(), &lists))
}

// The port lists as the last scan found them, so that polling the status stays cheap.
struct PortLists {
    in_ports: Vec<String>,
    out_ports: Vec<String>,
    message: String,
}

impl PortLists {
    fn of(ports: &Arc<Mutex<Ports>>) -> Self {
        let p = ports.lock().unwrap();
        PortLists { in_ports: p.in_ports.clone(), out_ports: p.out_ports.clone(), message: p.message.clone() }
    }
}

// Answers Some(value) for a tool with its own result, and None for one that answers with the
// status.
fn call(name: &str, args: &Value, s: &mut Seq, lists: &PortLists) -> Result<Option<Value>, String> {
    match name {
        "get_status" => {}
        "play" => s.play(),
        "stop" => s.stop(),
        "set_recording" => {
            let on = args.get("enabled").and_then(Value::as_bool).ok_or("The argument 'enabled' must be a boolean.")?;
            s.set_recording(on);
        }
        "set_tempo" => {
            let bpm = args.get("bpm").and_then(Value::as_f64).ok_or("The argument 'bpm' must be a number.")?;
            s.set_bpm(bpm as f32);
        }
        "set_loop_length" => {
            let bars = args.get("bars").and_then(Value::as_u64).ok_or("The argument 'bars' must be a positive integer.")?;
            s.set_bars(bars.min(MAX_BARS as u64) as u32);
        }
        "set_midi_out" => {
            let port = args.get("port").and_then(Value::as_str).ok_or("The argument 'port' must be a string.")?;
            let ports = seq::out_ports();
            if !ports.iter().any(|p| p == port) {
                return Err(format!("There is no MIDI output named '{port}'. The outputs are {ports:?}."));
            }
            s.connect_out(port)?;
        }
        "add_notes" => {
            let list = args.get("notes").and_then(Value::as_array).ok_or("The argument 'notes' must be an array.")?;
            let parsed = list.iter().map(|n| parse_note(n, s.len_ticks())).collect::<Result<Vec<_>, _>>()?;
            s.add_take(&parsed);
            if args.get("sound_now").and_then(Value::as_bool).unwrap_or(false) {
                s.sound_now(&parsed);
            }
        }
        "remove_notes" => {
            let ch = whole(args, "channel", 1, 16)?.map(|c| c as u8 - 1);
            let pitch = whole(args, "pitch", 0, 127)?.map(|p| p as u8);
            let start = match args.get("start_beat") {
                None | Some(Value::Null) => None,
                Some(v) => {
                    let b = v.as_f64().filter(|b| *b >= 0.0).ok_or("The argument 'start_beat' must be a number of 0 or more.")?;
                    Some((b * PPQ as f64).round() as u32)
                }
            };
            if ch.is_none() && pitch.is_none() && start.is_none() {
                return Err("Give at least one of 'channel', 'pitch' and 'start_beat'.".into());
            }
            let removed = s.remove_notes(|n| {
                ch.map_or(true, |c| n.ch == c) && pitch.map_or(true, |p| n.pitch == p) && start.map_or(true, |t| n.start == t)
            });
            let mut v = status(s, lists);
            v["removed"] = json!(removed);
            return Ok(Some(v));
        }
        "set_instrument" => {
            let int = |key: &str, max: u64| args.get(key).and_then(Value::as_u64).filter(|v| (1..=max).contains(v));
            let ch = int("channel", 16).ok_or("The argument 'channel' must be an integer from 1 to 16.")?;
            let program = int("program", 128).ok_or("The argument 'program' must be an integer from 1 to 128.")?;
            s.set_program(ch as u8 - 1, program as u8 - 1);
        }
        "set_metronome" => s.metronome = flag(args)?,
        "set_midi_thru" => s.thru = flag(args)?,
        "undo_take" => s.undo_take(),
        "clear" => s.clear(),
        "get_notes" => return Ok(Some(notes(s))),
        "export_midi" => {
            let path = s.export(args.get("filename").and_then(Value::as_str))?;
            return Ok(Some(json!({"path": path.to_string_lossy(), "notes": s.notes.len()})));
        }
        _ => return Err(format!("Unknown tool: {name}")),
    }
    Ok(None)
}

fn parse_note(n: &Value, len: u32) -> Result<Note, String> {
    let num = |key: &str| n.get(key).and_then(Value::as_f64);
    let start = num("start_beat").filter(|b| *b >= 0.0).ok_or("Every note needs a 'start_beat' of 0 or more.")?;
    let dur = num("duration_beats").filter(|b| *b > 0.0).ok_or("Every note needs a positive 'duration_beats'.")?;
    let pitch = num("pitch").filter(|p| (0.0..=127.0).contains(p)).ok_or("Every note needs a 'pitch' from 0 to 127.")?;
    let vel = num("velocity").unwrap_or(100.0).clamp(1.0, 127.0);
    let ch = num("channel").unwrap_or(1.0).clamp(1.0, 16.0);
    let start = (start * PPQ as f64).round() as u32;
    if start >= len {
        return Err(format!("A note starts at beat {}, but the loop has only {} beats.", start / PPQ, len / PPQ));
    }
    let dur = ((dur * PPQ as f64).round() as u32).clamp(1, len - 1);
    Ok(Note { start, dur, pitch: pitch as u8, vel: vel as u8, ch: ch as u8 - 1, take: 0 })
}

// Reads an optional whole number. A value that is given must lie in min..=max.
fn whole(args: &Value, key: &str, min: u64, max: u64) -> Result<Option<u64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .filter(|x| (min..=max).contains(x))
            .map(Some)
            .ok_or(format!("The argument '{key}' must be an integer from {min} to {max}.")),
    }
}

fn flag(args: &Value) -> Result<bool, String> {
    args.get("enabled").and_then(Value::as_bool).ok_or("The argument 'enabled' must be a boolean.".into())
}

fn status(s: &Seq, p: &PortLists) -> Value {
    let beat = (s.pos / PPQ as f64) as u32;
    json!({
        "state": if s.recording { "recording" } else if s.playing { "playing" } else { "stopped" },
        "bpm": s.bpm,
        "bars": s.bars,
        "beats_per_bar": BEATS_PER_BAR,
        "position": {"bar": beat / BEATS_PER_BAR + 1, "beat": beat % BEATS_PER_BAR + 1,
            "beats": (s.pos / PPQ as f64 * 1000.0).round() / 1000.0},
        "note_count": s.notes.len(),
        "metronome": s.metronome,
        "midi_thru": s.thru,
        "midi_in": s.in_name,
        "midi_out": s.out_name,
        "midi_in_ports": p.in_ports,
        "midi_out_ports": p.out_ports,
        "instruments": s.programs.iter().enumerate()
            .filter_map(|(ch, p)| p.map(|p| ((ch + 1).to_string(), json!(p + 1))))
            .collect::<serde_json::Map<_, _>>(),
        "port_message": (!p.message.is_empty()).then_some(&p.message),
    })
}

fn notes(s: &Seq) -> Value {
    let beats = |ticks: u32| (ticks as f64 / PPQ as f64 * 1000.0).round() / 1000.0;
    let mut sorted = s.notes.clone();
    sorted.sort_by_key(|n| (n.start, n.pitch));
    let list: Vec<Value> = sorted
        .iter()
        .map(|n| json!({
            "start_beat": beats(n.start),
            "duration_beats": beats(n.dur),
            "pitch": n.pitch,
            "name": note_name(n.pitch),
            "velocity": n.vel,
            "channel": n.ch + 1,
            "take": n.take,
        }))
        .collect();
    json!({"bpm": s.bpm, "bars": s.bars, "beats_per_bar": BEATS_PER_BAR, "notes": list})
}
