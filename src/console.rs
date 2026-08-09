//! Serial console for repeater and room server settings over USB.
//! Equivalent to the console of the web flasher (lib/console.js).

use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

/// Commands that the MeshCore repeater and room server CLI accepts.
/// The UI shows them as hints.
pub const COMMAND_REFERENCE: &[(&str, &str)] = &[
    ("time <epoch-secs>", "Set time"),
    ("clock", "Display current time"),
    ("ver", "Show device version"),
    ("advert", "Send advertisement packet"),
    ("reboot", "Reboot device"),
    ("erase", "Erase filesystem"),
    ("password <pw>", "Set new password"),
    ("log", "Output log"),
    ("log start", "Start packet logging"),
    ("log stop", "Stop packet logging"),
    ("log erase", "Erase packet logs"),
    ("set freq <MHz>", "Set frequency"),
    ("set tx <dBm>", "Set TX power"),
    ("set af <n>", "Set air-time factor"),
    ("set repeat {on|off}", "Set repeater mode"),
    ("set advert.interval <min>", "Set advert interval"),
    ("set name <name>", "Set advertisement name"),
    ("set lat <lat>", "Set map latitude"),
    ("set lon <lon>", "Set map longitude"),
    ("set guest.password <pw>", "Set guest password"),
    ("get freq", "Get frequency"),
    ("get tx", "Get TX power"),
    ("get af", "Get air-time factor"),
    ("get repeat", "Get repeater mode"),
    ("get advert.interval", "Get advert interval"),
    ("get name", "Get advertisement name"),
    ("get lat", "Get map latitude"),
    ("get lon", "Get map longitude"),
];

pub struct Console {
    tx: Sender<String>,
    stop: Arc<AtomicBool>,
    pub output: Receiver<String>,
}

impl Console {
    pub fn open(port_name: &str) -> Result<Self> {
        let mut port = serialport::new(port_name, 115_200)
            .timeout(Duration::from_millis(100))
            .open()
            .with_context(|| format!("opening {port_name}"))?;
        // USB-CDC firmware sends data only when DTR is set. Web Serial sets
        // DTR on open. serialport-rs does not.
        let _ = port.write_data_terminal_ready(true);
        let _ = port.write_request_to_send(false);
        let mut writer = port.try_clone().context("cloning port for writing")?;

        let (out_tx, out_rx) = std::sync::mpsc::channel::<String>();
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<String>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_reader = stop.clone();

        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            loop {
                if stop_reader.load(Ordering::Relaxed) {
                    break;
                }
                // Drain pending commands.
                while let Ok(cmd) = cmd_rx.try_recv() {
                    let _ = writer.write_all(cmd.as_bytes());
                    // MeshCore CLI expects CRLF line endings (see the web
                    // flasher's console.js sendCommand).
                    let _ = writer.write_all(b"\r\n");
                    let _ = writer.flush();
                }
                match port.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        let text = String::from_utf8_lossy(&buf[..n]).to_string();
                        if out_tx.send(text).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            tx: cmd_tx,
            stop,
            output: out_rx,
        })
    }

    pub fn send(&self, command: &str) {
        let _ = self.tx.send(command.to_string());
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
