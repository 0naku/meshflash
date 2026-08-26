//! ESP32 flashing with the espflash library, the Rust equivalent of esptool.
//!
//! This module writes the same images as the web flasher:
//!  - A "flash-update" bin goes to 0x10000.
//!  - A "-merged.bin" image goes to 0x0, after a full erase of the chip.

use anyhow::{Context, Result};
use espflash::connection::{Connection, Port, ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::Flasher;
use espflash::target::ProgressCallbacks;
use serialport::{SerialPortType, UsbPortInfo};
use std::time::{Duration, Instant};

pub const APP_FLASH_ADDRESS: u32 = 0x10000;

/// Flash with the loader stub. The reset after the flash must use the same
/// value, because the stub changes how the chip leaves download mode.
const USE_STUB: bool = true;

/// USB vendor ID of Espressif. A port with this vendor ID comes from the chip
/// itself, not from a UART bridge chip on the board. This port leaves the USB
/// bus at every reset. It then returns as a new USB device.
const ESPRESSIF_VID: u16 = 0x303A;

/// Product ID of the USB-Serial-JTAG peripheral. The ROM shows this product ID
/// while the chip is in download mode. A board that runs its firmware shows a
/// different product ID on the same native USB. A Heltec V4 with MeshCore shows
/// 0x0002. Thus the product ID gives the mode of the chip, not the wiring of
/// the board.
const USB_SERIAL_JTAG_PID: u16 = 0x1001;

/// How long to wait for a board to return after a reset.
///
/// A chip that re-enumerates needs time for the USB reset. Then the operating
/// system needs more time to make the device node. Firmware that starts its own
/// USB stack adds its full boot to this wait. A Heltec V4 with MeshCore needed
/// 17 seconds to return on Linux. With a short deadline, MeshFlash stops before
/// the board returns, and the flash fails.
const PORT_RETURN_TIMEOUT: Duration = Duration::from_secs(30);

/// A board shows in the port list a short time before MeshFlash can open it.
const OPEN_TRIES: u32 = 10;

/// The port comes from the chip itself, so it disappears at every reset.
fn is_native_usb(info: &UsbPortInfo) -> bool {
    info.vid == ESPRESSIF_VID
}

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

/// The port for the next try, after the board resets.
///
/// A board with a native USB interface (ESP32-S3, ESP32-C3) leaves the USB bus
/// when it resets. It returns as a new USB device. Windows gives it a new port
/// name. Linux frequently gives it the old name again. This function waits for
/// this full cycle and gives the name that the board has now.
///
/// A test of the name alone is not enough. Immediately after the reset, the
/// operating system still lists the old device node. A test that asks only "is
/// `previous` present?" gets the answer yes, and gives back a dead port. This
/// function thus waits for the board to leave the bus first. Then it accepts
/// the board again.
///
/// `serial` is the USB serial number read before the reset. The chip keeps this
/// number when it re-enumerates. The product ID changes between the firmware
/// (0x0002 on a Heltec V4) and the ROM download mode (0x1001), but the serial
/// number does not. A match on the serial number finds the correct board, even
/// when a second ESP32 board is connected.
fn port_after_reset(
    previous: &str,
    serial: Option<&str>,
    native_usb: bool,
    log: &mut dyn FnMut(String),
) -> String {
    // A board behind a UART bridge chip keeps its port through the reset. The
    // bridge never leaves the bus. A wait for a disconnect that cannot occur
    // only delays the next try.
    if !native_usb {
        std::thread::sleep(Duration::from_millis(500));
        return previous.to_string();
    }

    let deadline = Instant::now() + PORT_RETURN_TIMEOUT;
    let mut left_the_bus = false;
    while Instant::now() < deadline {
        let entries = crate::ports::list_ports();

        // The serial number names the same board, even under a new port name.
        if let Some(want) = serial
            && let Some(found) = entries.iter().find(|p| p.serial.as_deref() == Some(want))
            && (left_the_bus || found.name != previous)
        {
            return found.name.clone();
        }

        if entries
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case(previous))
        {
            // The board returned under the old name. This is valid only
            // after it left the bus.
            if left_the_bus {
                return previous.to_string();
            }
        } else if !left_the_bus {
            left_the_bus = true;
            log("The board left the USB bus. MeshFlash waits for it to return…".to_string());
        }

        // No serial number matched. The board reports no serial number, or the
        // ROM reports a different one in download mode than the firmware did.
        // The chip family is the fallback, and only when one ESP32 port is
        // present. With two boards, MeshFlash must not guess which one it uses.
        if left_the_bus
            && let [only] = crate::ports::family_candidates(&entries, "esp32").as_slice()
        {
            return only.name.clone();
        }

        std::thread::sleep(Duration::from_millis(250));
    }

    log(format!(
        "The board did not return within {} s.",
        PORT_RETURN_TIMEOUT.as_secs()
    ));
    previous.to_string()
}

/// Reset an ESP32-S3 that is connected through the native USB of the chip.
///
/// espflash cannot do this reset. Its watchdog strategy first calls
/// `can_rtc_wdt_reset`. That test needs GPIO0 low and the force-download-boot
/// bit clear. A board that entered download mode over USB does not always meet
/// both conditions. espflash then resets nothing and reports success. This
/// function does the reset itself.
///
/// The register values go to the log. They give the reason why a board starts
/// in download mode, or stays in it. No other message gives this information.
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
        .context("cannot read GPIO_STRAP")?;
    let option1 = connection
        .read_reg(OPTION1)
        .context("cannot read OPTION1")?;
    log(format!(
        "Boot registers: strap={strap:#010x} option1={option1:#010x} force_download={}",
        option1 & FORCE_DOWNLOAD_BOOT
    ));

    // While this bit is set, the chip starts in download mode again after
    // every reset. MeshFlash must clear the bit. If it does not, the board
    // never starts the new firmware.
    if option1 & FORCE_DOWNLOAD_BOOT != 0 {
        log("MeshFlash clears the force-download-boot bit…".to_string());
        connection
            .write_reg(OPTION1, 0, Some(FORCE_DOWNLOAD_BOOT))
            .context("cannot clear the force-download-boot bit")?;
    }

    chip.rtc_wdt_reset(connection)
        .context("the RTC watchdog reset failed")?;
    log("The RTC watchdog reset ran.".to_string());
    Ok(())
}

/// Open a serial port, with more tries while the driver starts.
///
/// Another try helps only while the operating system prepares the port. A
/// refusal by the permission system is a fixed condition. MeshFlash reports it
/// immediately, with the reason. On Linux the device node belongs to a group
/// that the user is frequently not a member of, and more tries do not help.
fn open_port(name: &str, log: &mut dyn FnMut(String)) -> Result<Port> {
    let mut attempt = 0;
    loop {
        match serialport::new(name, 115_200)
            .timeout(Duration::from_secs(3))
            .open_native()
        {
            Ok(port) => return Ok(port),
            Err(e) if is_permission_denied(&e) => {
                return Err(e).context(permission_help(name));
            }
            Err(e) if attempt + 1 < OPEN_TRIES => {
                attempt += 1;
                log(format!("The port is not ready ({e}). New try in 0.3 s…"));
                std::thread::sleep(Duration::from_millis(300));
            }
            Err(e) => return Err(e).with_context(|| format!("cannot open {name}")),
        }
    }
}

fn is_permission_denied(e: &serialport::Error) -> bool {
    matches!(
        e.kind(),
        serialport::ErrorKind::Io(std::io::ErrorKind::PermissionDenied)
    )
}

#[cfg(target_os = "linux")]
fn permission_help(name: &str) -> String {
    format!(
        "no permission to open {name}. Serial devices belong to the \"uucp\" group on \
         Arch-based systems, and to the \"dialout\" group on Debian-based systems. Add \
         your user to that group with `sudo usermod -aG uucp $USER`. Then log out and \
         log in again"
    )
}

#[cfg(not(target_os = "linux"))]
fn permission_help(name: &str) -> String {
    format!("no permission to open {name}. Close each program that holds the port")
}

/// Flash `data` at `address`. If `erase_all` is true, erase the full chip
/// first.
pub fn flash_bin(
    port_name: &str,
    data: &[u8],
    address: u32,
    erase_all: bool,
    log: &mut dyn FnMut(String),
    progress: &mut dyn FnMut(u8),
) -> Result<()> {
    log(format!("Connection to {port_name} started…"));

    // A board with a native USB interface leaves the bus when it resets into
    // download mode. The open port then dies while it connects. The board
    // returns as a new USB device. A new try on the port that the board
    // returned to replaces the manual "click Flash again".
    const CONNECT_TRIES: u32 = 3;
    let mut current = port_name.to_string();
    let mut tries_left = CONNECT_TRIES;

    let mut flasher = loop {
        // Read the identity of the port before the connection starts. The
        // connection makes the board disappear, and then there is nothing to
        // read.
        let info = usb_info(&current);
        let native_usb = is_native_usb(&info);
        let serial = info.serial_number.clone();

        // A failure to open gets the same recovery as a failure to connect.
        // The board can be on a different port.
        let port = match open_port(&current, log) {
            Ok(port) => port,
            Err(err) => {
                tries_left -= 1;
                if tries_left == 0 {
                    return Err(err);
                }
                let next = port_after_reset(&current, serial.as_deref(), native_usb, log);
                log(format!("Port {current} did not open. New try on {next}…"));
                current = next;
                continue;
            }
        };
        // A chip on its native USB does not leave download mode when the reset
        // uses only the control lines. Those boards need the RTC watchdog of
        // espflash. MeshFlash sets the strategy here, before the connection
        // starts, so the strategy comes from the wiring and not from the chip
        // type. The test is the vendor ID, not the product ID. A board that
        // runs its firmware reports a product ID of its own. Only the ROM in
        // download mode reports the USB-Serial-JTAG product ID.
        let after = if native_usb {
            ResetAfterOperation::WatchdogReset
        } else {
            ResetAfterOperation::HardReset
        };
        let connection = Connection::new(
            port,
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
                        "cannot connect to the ESP32 bootloader. Make sure that the board \
                         is connected. Make sure that no other program uses the port",
                    );
                }
                log(format!(
                    "The bootloader did not answer on {current}. The board can change its \
                     port when it enters download mode. MeshFlash waits for it…"
                ));
                let next = port_after_reset(&current, serial.as_deref(), native_usb, log);
                if next == current {
                    log(format!("New try on {current}…"));
                } else {
                    log(format!("The board is now on {next}. New try…"));
                    current = next;
                }

                // A board that returns with the product ID of its firmware
                // started that firmware, not the bootloader. More tries do not
                // correct this. The user must start download mode by hand.
                let back = usb_info(&current);
                if is_native_usb(&back) && back.pid != USB_SERIAL_JTAG_PID {
                    log(
                        "The board started its firmware, not the bootloader. Hold the BOOT \
                         button. Press RESET. Release BOOT. Then flash again."
                            .to_string(),
                    );
                }
            }
        }
    };

    if current != port_name {
        log(format!("The flash uses port {current}."));
    }

    let chip = flasher.chip();
    log(format!("Chip found: {chip}"));

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

    log("Board reset…".to_string());
    let native_usb = is_native_usb(&usb_info(&current));
    let mut reset_done = false;

    if native_usb && chip == espflash::target::Chip::Esp32s3 {
        match reset_esp32s3_native_usb(flasher.connection(), chip, log) {
            Ok(()) => reset_done = true,
            Err(err) => log(format!("The watchdog reset failed ({err:#}).")),
        }
    }

    // Boards with a UART bridge use the reset of espflash over the control
    // lines. A chip without the routine for the ESP32-S3 uses it too.
    if !reset_done && let Err(err) = flasher.connection().reset_after(USE_STUB, chip) {
        log(format!(
            "The reset after the flash failed ({err}). Press the RESET button on the board."
        ));
    }
    Ok(())
}
