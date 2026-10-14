use crate::seq::{self, Seq};
use midir::{Ignore, MidiInput, MidiInputConnection};
use std::sync::{Arc, Mutex};

// Owns the MIDI input connection and the port lists. The window, the headless loop and the
// MCP server share it, so it never sits inside the Seq lock: the input callback takes that
// lock, and closing the input while holding it could deadlock.
pub struct Ports {
    seq: Arc<Mutex<Seq>>,
    pub in_ports: Vec<String>,
    pub out_ports: Vec<String>,
    pub in_sel: Option<String>,
    in_conn: Option<MidiInputConnection<()>>,
    pub message: String,
}

impl Ports {
    pub fn new(seq: Arc<Mutex<Seq>>) -> Self {
        Ports {
            seq,
            in_ports: Vec::new(),
            out_ports: Vec::new(),
            in_sel: None,
            in_conn: None,
            message: String::new(),
        }
    }

    // Lists the MIDI ports and connects to the MPK as soon as it shows up.
    pub fn scan(&mut self) {
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
                let _ = self.connect_in(name);
            }
        }
        if out_name.is_none() || out_gone {
            if let Some(name) = find_device(&self.out_ports) {
                let _ = self.connect_out(name);
            }
        }
    }

    pub fn connect_in(&mut self, name: String) -> Result<(), String> {
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
                Ok(())
            }
            Err(e) => {
                self.in_sel = None;
                self.seq.lock().unwrap().in_name = None;
                self.message = format!("MIDI input '{name}' failed: {e}");
                Err(self.message.clone())
            }
        }
    }

    pub fn connect_out(&mut self, name: String) -> Result<(), String> {
        let result = self.seq.lock().unwrap().connect_out(&name);
        match result {
            Ok(()) => {
                self.message.clear();
                Ok(())
            }
            Err(e) => {
                self.message = format!("MIDI output '{name}' failed: {e}");
                Err(self.message.clone())
            }
        }
    }
}

fn find_device(ports: &[String]) -> Option<String> {
    ports.iter().find(|n| seq::is_device(n)).cloned()
}

pub fn open_export_folder() {
    let dir = seq::export_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::process::Command::new("explorer").arg(dir).spawn();
}
