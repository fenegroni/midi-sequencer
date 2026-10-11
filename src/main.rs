#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod mcp;
mod seq;

use eframe::egui;
use midir::{Ignore, MidiInput, MidiInputConnection};
use seq::{Note, Seq, BEATS_PER_BAR, MAX_BARS, MAX_BPM, MIN_BPM, PPQ};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct App {
    seq: Arc<Mutex<Seq>>,
    in_ports: Vec<String>,
    out_ports: Vec<String>,
    in_sel: Option<String>,
    in_conn: Option<MidiInputConnection<()>>,
    last_scan: Instant,
    message: String,
    mcp_status: String,
}

impl App {
    fn new() -> Self {
        let seq = Arc::new(Mutex::new(Seq::new()));
        seq::spawn_engine(seq.clone());
        let mcp_status = match mcp::spawn(seq.clone()) {
            Ok(()) => format!("MCP server: http://{}/mcp", mcp::ADDR),
            Err(e) => format!("MCP server failed to start: {e}"),
        };
        let mut app = App {
            seq,
            in_ports: Vec::new(),
            out_ports: Vec::new(),
            in_sel: None,
            in_conn: None,
            last_scan: Instant::now(),
            message: String::new(),
            mcp_status,
        };
        app.scan();
        app
    }

    // Lists the MIDI ports and connects to the MPK as soon as it shows up.
    fn scan(&mut self) {
        self.last_scan = Instant::now();
        self.in_ports = MidiInput::new("MidiSequencer scan")
            .map(|m| m.ports().iter().filter_map(|p| m.port_name(p).ok()).collect())
            .unwrap_or_default();
        self.out_ports = seq::out_ports();

        if self.in_sel.as_ref().is_some_and(|n| !self.in_ports.contains(n)) {
            self.in_conn = None;
            self.in_sel = None;
            self.seq.lock().unwrap().in_name = None;
        }
        // The output lives in Seq because the MCP server can change it too.
        let out_name = self.seq.lock().unwrap().out_name.clone();
        let out_gone = out_name.as_ref().is_some_and(|n| !self.out_ports.contains(n));
        if out_gone {
            self.seq.lock().unwrap().set_out(None, None);
        }
        if self.in_sel.is_none() {
            if let Some(name) = find_device(&self.in_ports) {
                self.connect_in(name);
            }
        }
        if out_name.is_none() || out_gone {
            if let Some(name) = find_device(&self.out_ports) {
                self.connect_out(name);
            }
        }
    }

    fn connect_in(&mut self, name: String) {
        self.in_conn = None;
        let seq = self.seq.clone();
        let result = (|| {
            let mut midi = MidiInput::new("MidiSequencer in").map_err(|e| e.to_string())?;
            midi.ignore(Ignore::All);
            let port = midi
                .ports()
                .into_iter()
                .find(|p| midi.port_name(p).ok().as_ref() == Some(&name))
                .ok_or("The port is gone.".to_string())?;
            midi.connect(&port, "in", move |_, msg, _| seq.lock().unwrap().on_midi_in(msg), ())
                .map_err(|e| e.to_string())
        })();
        match result {
            Ok(conn) => {
                self.in_conn = Some(conn);
                self.in_sel = Some(name.clone());
                self.seq.lock().unwrap().in_name = Some(name);
                self.message.clear();
            }
            Err(e) => {
                self.in_sel = None;
                self.message = format!("MIDI input '{name}' failed: {e}");
            }
        }
    }

    fn connect_out(&mut self, name: String) {
        let result = self.seq.lock().unwrap().connect_out(&name);
        match result {
            Ok(()) => self.message.clear(),
            Err(e) => self.message = format!("MIDI output '{name}' failed: {e}"),
        }
    }

    fn export(&mut self) {
        self.message = match self.seq.lock().unwrap().export(None) {
            Ok(path) => format!("Exported {}", path.display()),
            Err(e) => format!("Export failed: {e}"),
        };
    }
}

fn find_device(ports: &[String]) -> Option<String> {
    ports.iter().find(|n| seq::is_device(n)).cloned()
}

fn port_label(name: &str) -> &str {
    if seq::is_soundcard(name) { "PC soundcard (GS Wavetable)" } else { name }
}

fn port_combo(ui: &mut egui::Ui, id: &str, ports: &[String], current: &Option<String>) -> Option<String> {
    let mut picked = None;
    egui::ComboBox::from_id_salt(id)
        .width(260.0)
        .selected_text(current.as_deref().map_or("(not connected)", port_label))
        .show_ui(ui, |ui| {
            for name in ports {
                if ui.selectable_label(current.as_ref() == Some(name), port_label(name)).clicked() {
                    picked = Some(name.clone());
                }
            }
        });
    picked
}

fn draw_roll(ui: &mut egui::Ui, notes: &[Note], len: u32, pos: f64, playing: bool) {
    let size = egui::vec2(ui.available_width(), 150.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    let x_of = |tick: f64| rect.left() + rect.width() * (tick / len as f64) as f32;

    for beat in 0..len / PPQ {
        let shade = if beat % BEATS_PER_BAR == 0 { 110 } else { 55 };
        let x = x_of((beat * PPQ) as f64);
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(shade)),
        );
    }

    let visible: Vec<&Note> = notes.iter().filter(|n| n.start < len).collect();
    let lo = visible.iter().map(|n| n.pitch).min().unwrap_or(48) as f32 - 1.0;
    let hi = (visible.iter().map(|n| n.pitch).max().unwrap_or(72) as f32 + 1.0).max(lo + 12.0);
    let row = rect.height() / (hi - lo + 1.0);
    for n in visible {
        let color = if n.ch == 9 {
            egui::Color32::from_rgb(240, 160, 60)
        } else {
            egui::Color32::from_rgb(90, 170, 250)
        };
        let y = rect.bottom() - (n.pitch as f32 - lo + 1.0) * row;
        let bar = |from: u32, to: u32| {
            let (x0, x1) = (x_of(from as f64), x_of(to as f64).max(x_of(from as f64) + 2.0));
            let shape = egui::Rect::from_min_max(egui::pos2(x0, y), egui::pos2(x1, y + row.max(2.0)));
            painter.rect_filled(shape, 1.0, color);
        };
        let end = n.start + n.dur;
        bar(n.start, end.min(len));
        if end > len {
            bar(0, end - len);
        }
    }

    if playing {
        let x = x_of(pos);
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(2.0_f32, egui::Color32::WHITE),
        );
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.last_scan.elapsed() > Duration::from_secs(2) {
            self.scan();
        }

        // The UI works on a snapshot so that drawing never stalls the MIDI clock.
        let (mut bpm, mut bars, mut metronome, mut thru, playing, recording, pos, len, notes, out_name) = {
            let s = self.seq.lock().unwrap();
            (s.bpm, s.bars, s.metronome, s.thru, s.playing, s.recording, s.pos, s.len_ticks(), s.notes.clone(),
             s.out_name.clone())
        };
        let mut thru_clicked = false;

        let keys = !ctx.wants_keyboard_input();
        let space = keys && ctx.input(|i| i.key_pressed(egui::Key::Space));
        let key_r = keys && ctx.input(|i| i.key_pressed(egui::Key::R));

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::Grid::new("ports").num_columns(2).show(ui, |ui| {
                ui.label("MIDI in");
                if let Some(name) = port_combo(ui, "in", &self.in_ports, &self.in_sel) {
                    self.connect_in(name);
                }
                ui.end_row();
                ui.label("MIDI out");
                if let Some(name) = port_combo(ui, "out", &self.out_ports, &out_name) {
                    self.connect_out(name);
                }
                ui.end_row();
            });
            ui.separator();

            ui.horizontal(|ui| {
                let play_label = if playing { "■ Stop" } else { "▶ Play" };
                if ui.add_sized([90.0, 32.0], egui::Button::new(play_label)).clicked() || space {
                    let mut s = self.seq.lock().unwrap();
                    if playing { s.stop() } else { s.play() }
                }
                let rec_text = egui::RichText::new("● Record").color(if recording {
                    egui::Color32::WHITE
                } else {
                    egui::Color32::from_rgb(230, 80, 80)
                });
                let mut rec_button = egui::Button::new(rec_text);
                if recording {
                    rec_button = rec_button.fill(egui::Color32::from_rgb(190, 40, 40));
                }
                if ui.add_sized([90.0, 32.0], rec_button).clicked() || key_r {
                    self.seq.lock().unwrap().set_recording(!recording);
                }
                ui.label("BPM");
                ui.add(egui::DragValue::new(&mut bpm).range(MIN_BPM..=MAX_BPM).speed(0.5));
                ui.label("Bars");
                ui.add(egui::DragValue::new(&mut bars).range(1..=MAX_BARS).speed(0.1));
            });

            ui.horizontal(|ui| {
                ui.checkbox(&mut metronome, "Metronome");
                thru_clicked = ui
                    .checkbox(&mut thru, "MIDI thru")
                    .on_hover_text("Forwards what you play to the output. It turns on by itself for every output except the MPK.")
                    .changed();
                let beat = (pos / PPQ as f64) as u32;
                ui.label(format!(
                    "Bar {}  Beat {}   {} notes",
                    beat / BEATS_PER_BAR + 1,
                    beat % BEATS_PER_BAR + 1,
                    notes.len()
                ));
            });

            draw_roll(ui, &notes, len, pos, playing);

            ui.horizontal(|ui| {
                if ui.button("Undo last take").clicked() {
                    self.seq.lock().unwrap().undo_take();
                }
                if ui.button("Clear").clicked() {
                    self.seq.lock().unwrap().clear();
                }
                if ui.button("Export .mid").clicked() {
                    self.export();
                }
                if ui.button("Open export folder").clicked() {
                    let dir = seq::export_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = std::process::Command::new("explorer").arg(dir).spawn();
                }
            });

            ui.separator();
            ui.small("Space starts and stops. R toggles recording.");
            ui.small(format!("{}   v{}", self.mcp_status, env!("CARGO_PKG_VERSION")));
            if !self.message.is_empty() {
                ui.label(&self.message);
            }
        });

        {
            let mut s = self.seq.lock().unwrap();
            if bpm != s.bpm {
                s.set_bpm(bpm);
            }
            if bars != s.bars {
                s.set_bars(bars);
            }
            s.metronome = metronome;
            // A new output sets thru during this frame, so only a click may overwrite it.
            if thru_clicked {
                s.thru = thru;
            }
        }

        ctx.request_repaint_after(Duration::from_millis(if playing { 16 } else { 250 }));
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([560.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native("MIDI Sequencer", options, Box::new(|_cc| Ok(Box::new(App::new()))))
}
