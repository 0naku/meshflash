//! nRF52 legacy serial DFU (Adafruit bootloader), ported byte-for-byte from
//! the MeshCore web flasher's lib/dfu.js (itself adapted from
//! adafruit-nrfutil's dfu_transport_serial.py).
//!
//! Protocol: HCI packets over SLIP framing at 115200 baud. Each packet gets
//! a 3-bit sequence number and a CRC16. The device answers each packet with
//! an ACK. The firmware comes as a zip file that contains manifest.json and
//! the application .bin and .dat files.

use anyhow::{Context, Result, anyhow, bail};
use serialport::SerialPort;
use std::io::{Cursor, Read, Write};
use std::time::{Duration, Instant};

const DFU_TOUCH_BAUD: u32 = 1200;
const SERIAL_PORT_OPEN_WAIT_MS: u64 = 100;
const TOUCH_RESET_WAIT_MS: u64 = 1500;

const READ_TIMEOUT_MS: u64 = 5000; // dfu.js: DEFAULT_SERIAL_PORT_TIMEOUT (1s) * 5
const FLASH_PAGE_SIZE: usize = 4096;
const FLASH_PAGE_ERASE_TIME_S: f64 = 0.0897;
const FLASH_WORD_WRITE_TIME_S: f64 = 0.000100;
const FLASH_PAGE_WRITE_TIME_S: f64 = (FLASH_PAGE_SIZE as f64 / 4.0) * FLASH_WORD_WRITE_TIME_S;
const DFU_PACKET_MAX_SIZE: usize = 512;

const DATA_INTEGRITY_CHECK_PRESENT: u8 = 1;
const RELIABLE_PACKET: u8 = 1;
const HCI_PACKET_TYPE: u8 = 14;

const DFU_INIT_PACKET: u32 = 1;
const DFU_START_PACKET: u32 = 3;
const DFU_DATA_PACKET: u32 = 4;
const DFU_STOP_DATA_PACKET: u32 = 5;
const DFU_ERASE_PAGE: u32 = 6;

const DFU_UPDATE_MODE_APP: u32 = 4;

pub fn crc16(data: &[u8], start: u16) -> u16 {
    let mut crc: u32 = start as u32;
    for &byte in data {
        crc = ((crc >> 8) & 0x00FF) | ((crc << 8) & 0xFF00);
        crc ^= byte as u32;
        crc ^= (crc & 0x00FF) >> 4;
        crc ^= (crc << 8) << 4;
        crc ^= ((crc & 0x00FF) << 4) << 1;
    }
    (crc & 0xFFFF) as u16
}

fn slip_header(seq: u8, dip: u8, rp: u8, pkt_type: u8, pkt_len: usize) -> [u8; 4] {
    let len = pkt_len as u32;
    let b0 = seq | (((seq + 1) % 8) << 3) | (dip << 6) | (rp << 7);
    let b1 = pkt_type | (((len & 0x000F) << 4) as u8);
    let b2 = ((len & 0x0FF0) >> 4) as u8;
    let b3 = (0u32.wrapping_sub(b0 as u32 + b1 as u32 + b2 as u32) & 0xFF) as u8;
    [b0, b1, b2, b3]
}

fn slip_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 8);
    for &byte in data {
        match byte {
            0xC0 => out.extend_from_slice(&[0xDB, 0xDC]),
            0xDB => out.extend_from_slice(&[0xDB, 0xDD]),
            other => out.push(other),
        }
    }
    out
}

fn slip_decode(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        match data[i] {
            0xDB => {
                i += 1;
                match data.get(i) {
                    Some(0xDC) => out.push(0xC0),
                    Some(0xDD) => out.push(0xDB),
                    Some(other) => bail!("invalid SLIP escape: DB {:02x}", other),
                    None => bail!("invalid SLIP escape: incomplete"),
                }
            }
            0xC0 => {} // frame delimiters are skipped
            other => out.push(other),
        }
        i += 1;
    }
    Ok(out)
}

/// Build one HCI packet: header + payload + CRC16, SLIP-escaped, C0-delimited.
fn hci_packet(seq: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(payload.len() + 6);
    frame.extend_from_slice(&slip_header(
        seq,
        DATA_INTEGRITY_CHECK_PRESENT,
        RELIABLE_PACKET,
        HCI_PACKET_TYPE,
        payload.len(),
    ));
    frame.extend_from_slice(payload);
    let crc = crc16(&frame, 0xFFFF);
    frame.push((crc & 0xFF) as u8);
    frame.push((crc >> 8) as u8);

    let mut out = Vec::with_capacity(frame.len() + 10);
    out.push(0xC0);
    out.extend_from_slice(&slip_encode(&frame));
    out.push(0xC0);
    out
}

/// Manifest inside a DFU zip package.
#[derive(serde::Deserialize)]
struct ManifestRoot {
    manifest: Manifest,
}

#[derive(serde::Deserialize)]
struct Manifest {
    application: ManifestApp,
}

#[derive(serde::Deserialize)]
struct ManifestApp {
    bin_file: String,
    dat_file: String,
}

pub struct DfuPackage {
    pub app_bin: Vec<u8>,
    pub init_packet: Vec<u8>,
}

pub fn read_package(zip_bytes: &[u8]) -> Result<DfuPackage> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(zip_bytes)).context("opening DFU zip package")?;

    let manifest: ManifestRoot = {
        let mut file = archive
            .by_name("manifest.json")
            .context("manifest.json not found in DFU package")?;
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        serde_json::from_str(&text).context("parsing manifest.json")?
    };

    let read_entry =
        |archive: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str| -> Result<Vec<u8>> {
            let mut file = archive
                .by_name(name)
                .with_context(|| format!("{name} not found in DFU package"))?;
            let mut data = Vec::new();
            file.read_to_end(&mut data)?;
            Ok(data)
        };

    let app_bin = read_entry(&mut archive, &manifest.manifest.application.bin_file)?;
    let init_packet = read_entry(&mut archive, &manifest.manifest.application.dat_file)?;
    Ok(DfuPackage {
        app_bin,
        init_packet,
    })
}

/// 1200-baud touch: open and close the port at 1200 baud. This puts the
/// device in DFU mode.
pub fn force_dfu_mode(port_name: &str) -> Result<()> {
    let port = serialport::new(port_name, DFU_TOUCH_BAUD)
        .timeout(Duration::from_millis(500))
        .open()
        .with_context(|| format!("opening {port_name} at 1200 baud"))?;
    std::thread::sleep(Duration::from_millis(SERIAL_PORT_OPEN_WAIT_MS));
    drop(port);
    std::thread::sleep(Duration::from_millis(TOUCH_RESET_WAIT_MS));
    Ok(())
}

pub struct Dfu {
    port: Box<dyn SerialPort>,
    sequence: u8,
    last_ack: i16,
}

impl Dfu {
    pub fn open(port_name: &str) -> Result<Self> {
        // A port is not always ready immediately after re-enumeration.
        // Retry for a short time.
        let mut last_err = None;
        for _ in 0..6 {
            match serialport::new(port_name, 115_200)
                .timeout(Duration::from_millis(READ_TIMEOUT_MS))
                .open()
            {
                Ok(port) => {
                    return Ok(Self {
                        port,
                        sequence: 0,
                        last_ack: -1,
                    });
                }
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
        Err(last_err.unwrap()).with_context(|| format!("opening {port_name} at 115200 baud"))
    }

    /// Flash a DFU zip package. `progress` receives 0..=100.
    pub fn update(
        &mut self,
        package: &DfuPackage,
        erase_before_update: bool,
        mut progress: impl FnMut(u8),
    ) -> Result<()> {
        self.sequence = 0;
        self.last_ack = -1;

        let app_size = package.app_bin.len();

        if erase_before_update {
            self.erase_flash(app_size)?;
        }

        self.send_start_dfu(DFU_UPDATE_MODE_APP, 0, 0, app_size)?;
        self.send_init_packet(&package.init_packet)?;
        self.send_firmware(&package.app_bin, &mut progress)?;
        Ok(())
    }

    fn next_seq(&mut self) -> u8 {
        // dfu.js increments before use, so the first packet carries seq 1.
        self.sequence = (self.sequence + 1) % 8;
        self.sequence
    }

    fn send_packet(&mut self, payload: &[u8]) -> Result<()> {
        let seq = self.next_seq();
        let pkt = hci_packet(seq, payload);
        self.port.write_all(&pkt)?;
        self.port.flush()?;
        self.get_ack()?;
        Ok(())
    }

    fn get_ack(&mut self) -> Result<u8> {
        let mut buffer: Vec<u8> = Vec::new();
        let mut c0_count = 0;
        let deadline = Instant::now() + Duration::from_millis(READ_TIMEOUT_MS);
        let mut byte = [0u8; 64];

        while c0_count < 2 {
            if Instant::now() > deadline {
                self.sequence = 0;
                bail!("no DFU ACK before the timeout");
            }
            match self.port.read(&mut byte) {
                Ok(0) => {}
                Ok(n) => {
                    for &b in &byte[..n] {
                        buffer.push(b);
                        if b == 0xC0 {
                            c0_count += 1;
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    self.sequence = 0;
                    bail!("no DFU ACK before the timeout");
                }
                Err(e) => {
                    self.sequence = 0;
                    return Err(e).context("reading DFU ACK");
                }
            }
        }

        let first = buffer
            .iter()
            .position(|&b| b == 0xC0)
            .ok_or_else(|| anyhow!("incomplete ACK"))?;
        let second = buffer[first + 1..]
            .iter()
            .position(|&b| b == 0xC0)
            .map(|p| p + first + 1)
            .ok_or_else(|| anyhow!("incomplete ACK"))?;
        let decoded = slip_decode(&buffer[first + 1..second])?;
        if decoded.len() < 2 {
            bail!("incomplete ACK");
        }

        let ack = (decoded[0] >> 3) & 0x07;
        if self.last_ack != -1 && ack as i16 != (self.last_ack + 1) % 8 {
            self.sequence = 0;
            bail!(
                "invalid ACK sequence: expected {}, got {}",
                (self.last_ack + 1) % 8,
                ack
            );
        }
        self.last_ack = ack as i16;
        Ok(ack)
    }

    fn send_start_dfu(
        &mut self,
        mode: u32,
        softdevice_size: usize,
        bootloader_size: usize,
        app_size: usize,
    ) -> Result<()> {
        let mut frame = Vec::with_capacity(20);
        frame.extend_from_slice(&DFU_START_PACKET.to_le_bytes());
        frame.extend_from_slice(&mode.to_le_bytes());
        frame.extend_from_slice(&(softdevice_size as u32).to_le_bytes());
        frame.extend_from_slice(&(bootloader_size as u32).to_le_bytes());
        frame.extend_from_slice(&(app_size as u32).to_le_bytes());
        self.send_packet(&frame)?;

        let total = softdevice_size + bootloader_size + app_size;
        let erase_wait_s = f64::max(
            0.5,
            ((total / FLASH_PAGE_SIZE) as f64 + 1.0) * FLASH_PAGE_ERASE_TIME_S,
        );
        std::thread::sleep(Duration::from_secs_f64(erase_wait_s));
        Ok(())
    }

    fn send_init_packet(&mut self, init_packet: &[u8]) -> Result<()> {
        let mut frame = Vec::with_capacity(init_packet.len() + 6);
        frame.extend_from_slice(&DFU_INIT_PACKET.to_le_bytes());
        frame.extend_from_slice(init_packet);
        frame.extend_from_slice(&0u16.to_le_bytes()); // padding
        self.send_packet(&frame)
    }

    fn send_erase_page(&mut self, page_address: u32) -> Result<()> {
        let mut frame = Vec::with_capacity(8);
        frame.extend_from_slice(&DFU_ERASE_PAGE.to_le_bytes());
        frame.extend_from_slice(&page_address.to_le_bytes());
        self.send_packet(&frame)?;
        std::thread::sleep(Duration::from_secs_f64(FLASH_PAGE_ERASE_TIME_S));
        Ok(())
    }

    fn erase_flash(&mut self, app_size: usize) -> Result<()> {
        let num_pages = app_size.div_ceil(FLASH_PAGE_SIZE);
        for i in 0..num_pages {
            self.send_erase_page((i * FLASH_PAGE_SIZE) as u32)?;
        }
        Ok(())
    }

    fn send_firmware(&mut self, firmware: &[u8], progress: &mut impl FnMut(u8)) -> Result<()> {
        let total = firmware.len();
        let mut sent = 0usize;

        std::thread::sleep(Duration::from_secs_f64(FLASH_PAGE_WRITE_TIME_S));

        for (index, chunk) in firmware.chunks(DFU_PACKET_MAX_SIZE).enumerate() {
            let mut frame = Vec::with_capacity(chunk.len() + 4);
            frame.extend_from_slice(&DFU_DATA_PACKET.to_le_bytes());
            frame.extend_from_slice(chunk);
            self.send_packet(&frame)?;

            sent += chunk.len();
            progress(((sent as f64 / total as f64) * 100.0).min(100.0) as u8);

            // One flash page is 8 packets. Give the bootloader time to write it.
            if (index + 1) % 8 == 0 {
                std::thread::sleep(Duration::from_secs_f64(FLASH_PAGE_WRITE_TIME_S));
            }
        }

        std::thread::sleep(Duration::from_secs_f64(FLASH_PAGE_WRITE_TIME_S));
        self.send_packet(&DFU_STOP_DATA_PACKET.to_le_bytes())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_matches_reference() {
        // Reference values computed with the JS implementation from dfu.js.
        assert_eq!(crc16(b"", 0xFFFF), 0xFFFF);
        assert_eq!(crc16(b"123456789", 0xFFFF), 0x29B1); // CRC-16/CCITT-FALSE check value
        assert_eq!(crc16(&[0x00], 0xFFFF), 0xE1F0);
        assert_eq!(crc16(&[0xFF, 0xFF, 0xFF, 0xFF], 0xFFFF), 0x1D0F);
    }

    #[test]
    fn slip_roundtrip() {
        let data = vec![0x01, 0xC0, 0x02, 0xDB, 0x03];
        let encoded = slip_encode(&data);
        assert_eq!(encoded, vec![0x01, 0xDB, 0xDC, 0x02, 0xDB, 0xDD, 0x03]);
        assert_eq!(slip_decode(&encoded).unwrap(), data);
    }

    #[test]
    fn hci_header_matches_js() {
        // slipPartsToFourBytes(1, 1, 1, 14, 16) from dfu.js
        let h = slip_header(1, 1, 1, 14, 16);
        assert_eq!(h[0], 1 | (2 << 3) | (1 << 6) | (1 << 7));
        assert_eq!(h[1], 14 | ((16 & 0x0F) << 4) as u8);
        assert_eq!(h[2], (16u32 >> 4) as u8);
        assert_eq!(
            h[3],
            (0u32.wrapping_sub(h[0] as u32 + h[1] as u32 + h[2] as u32) & 0xFF) as u8
        );
    }
}
