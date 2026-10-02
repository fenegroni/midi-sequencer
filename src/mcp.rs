use crate::seq::{note_name, Seq, BEATS_PER_BAR, MAX_BARS, MAX_BPM, MIN_BPM, PPQ};
use serde_json::{json, Value};
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread;
use tiny_http::{Header, Method, Request, Response, Server};

pub const ADDR: &str = "127.0.0.1:7391";
const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

// Serves MCP over streamable HTTP on localhost. Every reply is plain JSON.
pub fn spawn(seq: Arc<Mutex<Seq>>) -> Result<(), String> {
    let server = Server::http(ADDR).map_err(|e| e.to_string())?;
    thread::spawn(move || {
        for req in server.incoming_requests() {
            handle(req, &seq);
        }
    });
    Ok(())
}

fn handle(mut req: Request, seq: &Arc<Mutex<Seq>>) {
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
            let out: Vec<Value> = batch.iter().filter_map(|m| rpc(m, seq)).collect();
            (!out.is_empty()).then(|| Value::Array(out))
        }
        Ok(msg) => rpc(&msg, seq),
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

fn rpc(msg: &Value, seq: &Arc<Mutex<Seq>>) -> Option<Value> {
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
            let (text, is_error) = match call(name, &args, &mut seq.lock().unwrap()) {
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
        {"name": "get_notes", "inputSchema": none,
         "description": "Returns every recorded note in the loop. Positions and durations are in beats, and start_beat 0 is the loop start. Channel 10 holds the drum pads."},
        {"name": "export_midi",
         "description": "Exports the loop as a .mid file into the export folder and returns the full path.",
         "inputSchema": {"type": "object",
            "properties": {"filename": {"type": "string",
                "description": "Optional file name without a folder. The default is a timestamped name."}}}}
    ])
}

fn call(name: &str, args: &Value, s: &mut Seq) -> Result<Value, String> {
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
        "get_notes" => return Ok(notes(s)),
        "export_midi" => {
            let path = s.export(args.get("filename").and_then(Value::as_str))?;
            return Ok(json!({"path": path.to_string_lossy(), "notes": s.notes.len()}));
        }
        _ => return Err(format!("Unknown tool: {name}")),
    }
    Ok(status(s))
}

fn status(s: &Seq) -> Value {
    let beat = (s.pos / PPQ as f64) as u32;
    json!({
        "state": if s.recording { "recording" } else if s.playing { "playing" } else { "stopped" },
        "bpm": s.bpm,
        "bars": s.bars,
        "beats_per_bar": BEATS_PER_BAR,
        "position": {"bar": beat / BEATS_PER_BAR + 1, "beat": beat % BEATS_PER_BAR + 1},
        "note_count": s.notes.len(),
        "metronome": s.metronome,
        "midi_in": s.in_name,
        "midi_out": s.out_name,
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
