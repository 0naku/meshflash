//! ESP32 flashing with the espflash library (the Rust equivalent of esptool).
//!
//! The module does the same as the web flasher:
//!  - It writes "flash-update" bins at 0x10000.
//!  - It writes "-merged.bin" images at 0x0 after a full chip erase.

use anyhow::{Context, Result};
use espflash::connection::{Connection, Port, ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::Flasher;
use espflash::target::ProgressCallbacks;
use serialport::{SerialPortType, UsbPortInfo};
use std::time::{Duration, Instant};

pub const APP_FLASH_ADDRESS: u32 = 0x10000;

/// Flash with the loader stub. The reset after the flash must get the same
/// value, because the stub changes how the chip leaves download mode.
const USE_STUB: bool = true;

/// Product ID of the USB-Serial-JTAG peripheral of Espressif. A board with
/// this ID is connected through the native USB of the chip, not through a
/// UART bridge chip.
const USB_SERIAL_JTAG_PID: u16 = 0x1001;

struct Progress<'a> {
    callback: &'a mut dyn FnMut(u8),
    total: usize,
}

impl ProgressCallbacks for Progress<'_> {
    fn init(&mut self, _addr: u32, total: usize) {
        self.total = total;
        (self.callback)(0);
    }

    fn update(&mut self, current: usize) {
        if self.total > 0 {
            let pct = ((current as f64 / self.total as f64) * 100.0).min(100.0) as u8;
            (self.callback)(pct);
        }
    }

    fn verifying(&mut self) {}

    fn finish(&mut self, _skipped: bool) {
        (self.callback)(100);
    }
}

fn usb_info(port_name: &str) -> UsbPortInfo {
    serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .find(|p| p.port_name.eq_ignore_ascii_case(port_name))
        .and_then(|info| match info.port_type {
            SerialPortType::UsbPort(usb) => Some(usb),
            _ => None,
        })
        .unwrap_or(UsbPortInfo {
            vid: 0,
            pid: 0,
            serial_number: None,
            manufacturer: None,
            product: None,
        })
}

/// The port to use for the next try, after the board reset.
///
/// A board with a native USB interface (ESP32-S3 and ESP32-C3) disconnects
/// from USB when it enters download mode. It comes back some hundred
/// milliseconds later, and Windows can give it a different COM number. This
/// function waits for the board and gives the name that is present now.
///
/// With two or more ESP32 ports it keeps `previous`, because MeshFlash must
/// not guess between two radios.
fn port_after_reset(previous: &str) -> String {
    // Give the USB stack time to report the disconnect. Without this pause the
    // board can still be listed under its old name for a moment.
    std::thread::sleep(Duration::from_millis(500));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let entries = crate::ports::list_ports();
        if let [only] = crate::ports::family_candidates(&entries, "esp32").as_slice() {
            return only.name.clone();
        }
        if entries
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case(previous))
            || Instant::now() >= deadline
        {
            return previous.to_string();
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Reset an ESP32-S3 that is connected through the native USB of the chip.
///
/// espflash cannot do this reset. Its watchdog strategy first asks
/// `can_rtc_wdt_reset`, and that check wants GPIO0 low and the
/// force-download-boot bit clear. A board that entered download mode over USB
/// does not always meet both conditions, and espflash then resets nothing and
/// reports success. This function does the reset itself.
///
/// The register values go to the log. They tell why a board starts or stays in
/// download mode, and no other message gives that information.
fn reset_esp32s3_native_usb(
    connection: &mut Connection,
    chip: espflash::target::Chip,
    log: &mut dyn FnMut(String),
) -> Result<()> {
    // ESP32-S3 register map. The same addresses that espflash uses.
    const GPIO_STRAP: u32 = 0x6000_4038;
    const OPTION1: u32 = 0x6000_812C;
    const FORCE_DOWNLOAD_BOOT: u32 = 0x1;

    let strap = connection
        .read_reg(GPIO_STRAP)
        .context("reading GPIO_STRAP")?;
    let option1 = connection.read_reg(OPTION1).context("reading OPTION1")?;
    log(format!(
        "Boot registers: strap={strap:#010x} option1={option1:#010x} force_download={}",
        option1 & FORCE_DOWNLOAD_BOOT
    ));

    // While this bit is set, the chip starts in download mode again after
    // every reset. Clear it, or the board never reaches the new firmware.
    if option1 & FORCE_DOWNLOAD_BOOT != 0 {
        log("Clearing the force-download-boot bit…".to_string());
        connection
            .write_reg(OPTION1, 0, Some(FORCE_DOWNLOAD_BOOT))
            .context("clearing force-download-boot")?;
    }

    chip.rtc_wdt_reset(connection)
        .context("RTC watchdog reset")?;
    log("The RTC watchdog reset ran.".to_string());
    Ok(())
}

/// Open a serial port, with short retries while the driver settles.
fn open_port(name: &str, log: &mut dyn FnMut(String)) -> Result<Port> {
    let mut attempt = 0;
    loop {
        match serialport::new(name, 115_200)
            .timeout(Duration::from_secs(3))
            .open_native()
        {
            Ok(port) => return Ok(port),
            Err(e) if attempt < 4 => {
                attempt += 1;
                log(format!("The port is not ready ({e}). New try in 0.3 s…"));
                std::thread::sleep(Duration::from_millis(300));
            }
            Err(e) => return Err(e).with_context(|| format!("opening {name}")),
        }
    }
}

/// Flash `data` at `address`, optionally erasing the whole chip first.
pub fn flash_bin(
    port_name: &str,
    data: &[u8],
    address: u32,
    erase_all: bool,
    log: &mut dyn FnMut(String),
    progress: &mut dyn FnMut(u8),
) -> Result<()> {
    log(format!("Connection to {port_name} started…"));

    // A board with a native USB interface leaves the bus when it enters
    // download mode. The open port then dies in the middle of the connect, and
    // the board comes back under a different COM number. One retry on the new
    // port replaces the "click Flash again" that the user had to do.
    const CONNECT_TRIES: u32 = 3;
    let mut current = port_name.to_string();
    let mut tries_left = CONNECT_TRIES;

    let mut flasher = loop {
        // An open failure gets the same recovery as a connect failure: the
        // board may simply be sitting on a different port now.
        let serial = match open_port(&current, log) {
            Ok(serial) => serial,
            Err(err) => {
                tries_left -= 1;
                if tries_left == 0 {
                    return Err(err);
                }
                let next = port_after_reset(&current);
                log(format!("Port {current} did not open. New try on {next}…"));
                current = next;
                continue;
            }
        };
        // A chip on its native USB does not leave download mode when the reset
        // uses only the control lines. For those boards espflash must use the
        // RTC watchdog. The strategy is fixed here, before the connect, so it
        // comes from the USB product ID and not from the chip type.
        let info = usb_info(&current);
        let after = if info.pid == USB_SERIAL_JTAG_PID {
            ResetAfterOperation::WatchdogReset
        } else {
            ResetAfterOperation::HardReset
        };
        let connection = Connection::new(
            serial,
            info,
            after,
            ResetBeforeOperation::DefaultReset,
            115_200,
        );

        match Flasher::connect(
            connection,
            USE_STUB,
            false, // verify
            false, // skip
            None,  // autodetect chip
            Some(115_200),
        ) {
            Ok(flasher) => break flasher,
            Err(err) => {
                tries_left -= 1;
                if tries_left == 0 {
                    return Err(err).context(
                        "cannot connect to the ESP32 bootloader — make sure that the device \
                         is connected and that no other program uses the port",
                    );
                }
                log(format!(
                    "The bootloader did not answer on {current}. The board can change its \
                     port when it enters download mode. Waiting for it…"
                ));
                let next = port_after_reset(&current);
                if next == current {
                    log(format!("New try on {current}…"));
                } else {
                    log(format!("The board is now on {next}. New try…"));
                    current = next;
                }
            }
        }
    };

    if current != port_name {
        log(format!("The flash uses port {current}."));
    }

    let chip = flasher.chip();
    log(format!("Detected chip: {chip}"));

    if erase_all {
        log("Full flash erase (this can take some minutes)…".to_string());
        flasher.erase_flash().context("flash erase failed")?;
    }

    log(format!(
        "Flash write: {} bytes at {address:#x}…",
        data.len()
    ));
    let mut prog = Progress {
        callback: progress,
        total: 0,
    };
    flasher
        .write_bin_to_flash(address, data, &mut prog)
        .context("flash write failed")?;

    log("Device reset…".to_string());
    let native_usb = usb_info(&current).pid == USB_SERIAL_JTAG_PID;
    let mut reset_done = false;

    if native_usb && chip == espflash::target::Chip::Esp32s3 {
        match reset_esp32s3_native_usb(flasher.connection(), chip, log) {
            Ok(()) => reset_done = true,
            Err(err) => log(format!("The watchdog reset failed ({err:#}).")),
        }
    }

    // Boards with a UART bridge, and any chip without the routine above, use
    // the reset of espflash over the control lines.
    if !reset_done && let Err(err) = flasher.connection().reset_after(USE_STUB, chip) {
        log(format!(
            "The reset after the flash failed ({err}). Press the reset button on the device."
        ));
    }
    Ok(())
}
