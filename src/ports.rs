//! Serial port enumeration with USB metadata for friendlier picking.

use serialport::{SerialPortInfo, SerialPortType};

#[derive(Clone, Debug, PartialEq)]
pub struct PortEntry {
    pub name: String,
    pub label: String,
    pub is_usb: bool,
    pub vid: u16,
    pub pid: u16,
}

pub fn list_ports() -> Vec<PortEntry> {
    let mut entries: Vec<PortEntry> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .map(describe)
        .collect();
    // USB devices first (the radios), then everything else.
    entries.sort_by(|a, b| b.is_usb.cmp(&a.is_usb).then(a.name.cmp(&b.name)));
    entries
}

/// USB vendor IDs commonly seen on each chip family's boards.
const ESP32_VIDS: &[u16] = &[
    0x303A, // Espressif (native USB / USB-Serial-JTAG)
    0x10C4, // Silicon Labs CP210x
    0x1A86, // WCH CH340/CH9102
    0x0403, // FTDI
];
const NRF52_VIDS: &[u16] = &[
    0x239A, // Adafruit
    0x2886, // Seeed
    0x1915, // Nordic
];

/// USB ports that belong to the chip family of `device_type`. A radio keeps
/// its vendor ID when it re-enumerates. This function therefore finds the same
/// radio again after it took a new port name.
pub fn family_candidates(entries: &[PortEntry], device_type: &str) -> Vec<PortEntry> {
    let vids: &[u16] = match device_type {
        "esp32" => ESP32_VIDS,
        "nrf52" => NRF52_VIDS,
        _ => &[],
    };
    entries
        .iter()
        .filter(|p| p.is_usb && vids.contains(&p.vid))
        .cloned()
        .collect()
}

/// Best-guess port for a device type. The UI uses it to pre-select the
/// port. Then most users do not think about COM numbers.
pub fn auto_pick(entries: &[PortEntry], device_type: &str) -> Option<String> {
    let vids: &[u16] = match device_type {
        "esp32" => ESP32_VIDS,
        "nrf52" => NRF52_VIDS,
        _ => &[],
    };
    entries
        .iter()
        .find(|p| p.is_usb && vids.contains(&p.vid))
        .or_else(|| entries.iter().find(|p| p.is_usb))
        .map(|p| p.name.clone())
}

fn describe(info: SerialPortInfo) -> PortEntry {
    match &info.port_type {
        SerialPortType::UsbPort(usb) => {
            let mut desc = usb
                .product
                .clone()
                .unwrap_or_else(|| "USB serial".to_string());
            if let Some(mfr) = &usb.manufacturer
                && !desc.to_lowercase().contains(&mfr.to_lowercase())
            {
                desc = format!("{mfr} {desc}");
            }
            PortEntry {
                label: format!("{} — {}", info.port_name, desc),
                name: info.port_name,
                is_usb: true,
                vid: usb.vid,
                pid: usb.pid,
            }
        }
        _ => PortEntry {
            label: info.port_name.clone(),
            name: info.port_name,
            is_usb: false,
            vid: 0,
            pid: 0,
        },
    }
}
