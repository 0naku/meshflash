//! Background jobs (catalog refresh, download + flash) with progress events
//! delivered to the UI thread over a channel.

use crate::catalog::{self, Config, Release};
use crate::{dfu, esp, net, ports};
use anyhow::bail;
use std::collections::HashSet;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

pub enum Event {
    Log(String),
    DownloadProgress(u64, u64),
    FlashProgress(u8),
    CatalogReady(Box<Config>),
    Done(String),
    Failed(String),
    CacheProgress {
        done: usize,
        total: usize,
        name: String,
    },
    CacheDone(String),
}

#[derive(Clone)]
pub enum Source {
    Url(String),
    File(std::path::PathBuf),
}

#[derive(Clone)]
pub struct FlashJob {
    pub port: String,
    pub source: Source,
    pub device_type: String, // "esp32" | "nrf52"
    pub erase_all: bool,     // esp32: wipe; nrf52: erase pages before update
    pub esp_address: u32,
    pub nrf_touch: bool, // perform 1200-baud touch before DFU
}

pub fn spawn_catalog_refresh(tx: Sender<Event>, ctx: eframe::egui::Context) {
    std::thread::spawn(move || {
        let agent = net::agent();

        let config_text =
            net::fetch_string(&agent, &format!("{}/config.json", catalog::FLASHER_ORIGIN));
        let releases_text =
            net::fetch_string(&agent, &format!("{}/releases", catalog::FLASHER_ORIGIN));

        let (mut config, releases, live): (Config, Vec<Release>, bool) =
            match (&config_text, &releases_text) {
                (Ok(c), Ok(r)) => {
                    match (
                        serde_json::from_str::<Config>(c),
                        serde_json::from_str::<Vec<Release>>(r),
                    ) {
                        (Ok(config), Ok(releases)) => (config, releases, true),
                        _ => {
                            let (c, r) = catalog::load_bundled().expect("bundled catalog");
                            (c, r, false)
                        }
                    }
                }
                _ => {
                    let (c, r) = catalog::load_bundled().expect("bundled catalog");
                    (c, r, false)
                }
            };

        catalog::merge_releases(&mut config, &releases);

        let _ = tx.send(Event::Log(if live {
            format!("Catalog: live ({} devices)", config.device.len())
        } else {
            format!("Catalog: offline copy ({} devices)", config.device.len())
        }));
        let _ = tx.send(Event::CatalogReady(Box::new(config)));
        ctx.request_repaint();
    });
}

pub fn spawn_flash(job: FlashJob, tx: Sender<Event>, ctx: eframe::egui::Context) {
    std::thread::spawn(move || {
        let result = run_flash(&job, &tx, &ctx);
        match result {
            Ok(msg) => {
                let _ = tx.send(Event::Done(msg));
            }
            Err(e) => {
                let _ = tx.send(Event::Failed(format!("{e:#}")));
            }
        }
        ctx.request_repaint();
    });
}

fn run_flash(
    job: &FlashJob,
    tx: &Sender<Event>,
    ctx: &eframe::egui::Context,
) -> anyhow::Result<String> {
    let log = |msg: String| {
        let _ = tx.send(Event::Log(msg));
        ctx.request_repaint();
    };

    let data = match &job.source {
        // A file in the cache is used directly. Then the flash needs no
        // network, and the log does not report a download that did not happen.
        Source::Url(url) if net::cached_file(url).is_some() => {
            let path = net::cached_file(url).expect("checked above");
            let data = std::fs::read(&path)?;
            log(format!(
                "From cache: {} ({} bytes)",
                net::cache_name(url).unwrap_or(url),
                data.len()
            ));
            data
        }
        Source::Url(url) => {
            let agent = net::agent();
            log(format!("Download: {url}"));
            let mut last_pct = 0u64;
            let data = net::download_cached(&agent, url, &mut |done, total| {
                if let Some(pct) = (done * 100).checked_div(total)
                    && pct != last_pct
                {
                    last_pct = pct;
                    let _ = tx.send(Event::DownloadProgress(done, total));
                    ctx.request_repaint();
                }
                true
            })?;
            log(format!("Download complete: {} bytes", data.len()));
            data
        }
        Source::File(path) => {
            log(format!("Load file: {}", path.display()));
            std::fs::read(path)?
        }
    };

    if job.device_type == "esp32" {
        let port = require_port(&job.port, &job.device_type, &log)?;
        log("ESP32 flash started.".to_string());
        esp::flash_bin(
            &port,
            &data,
            if job.erase_all { 0x0 } else { job.esp_address },
            job.erase_all,
            &mut |m| log(m),
            &mut |p| {
                let _ = tx.send(Event::FlashProgress(p));
                ctx.request_repaint();
            },
        )?;
        Ok("The ESP32 flash is complete. The device restarts now.".to_string())
    } else {
        let package = dfu::read_package(&data)?;
        let mut port = require_port(&job.port, &job.device_type, &log)?;
        if job.nrf_touch {
            let before = port_names();
            log("1200-baud touch to start DFU mode…".to_string());
            dfu::force_dfu_mode(&port)?;
            port = resolve_port_after_touch(&port, &before);
            log(format!("The DFU uses port {port}."));
        }
        log("nRF52 serial DFU started.".to_string());
        let mut dfu = dfu::Dfu::open(&port)?;
        dfu.update(&package, job.erase_all, |p| {
            let _ = tx.send(Event::FlashProgress(p));
            ctx.request_repaint();
        })?;
        Ok("The nRF52 DFU is complete. The device restarts with the new firmware.".to_string())
    }
}

fn port_names() -> HashSet<String> {
    serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.port_name)
        .collect()
}

/// Make sure that the flash starts on a port that is present.
///
/// On Windows, USB re-enumeration changes COM numbers. The port that the user
/// selected can therefore be gone at the moment of the flash. This function
/// waits a short time for the device, then it accepts a new port name when
/// exactly one port of the correct chip family is present. With two or more
/// candidates it stops, because it cannot know which radio the user wants.
fn require_port(name: &str, device_type: &str, log: &dyn Fn(String)) -> anyhow::Result<String> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let entries = ports::list_ports();
        if entries.iter().any(|p| p.name.eq_ignore_ascii_case(name)) {
            return Ok(name.to_string());
        }

        let candidates = ports::family_candidates(&entries, device_type);
        if candidates.len() == 1 {
            let found = candidates[0].name.clone();
            log(format!(
                "Port {name} is not present. MeshFlash uses {found} instead ({}).",
                candidates[0].label
            ));
            return Ok(found);
        }

        if Instant::now() >= deadline {
            let mut available: Vec<String> = entries.into_iter().map(|p| p.name).collect();
            available.sort();
            bail!(
                "Port {name} is not present, and MeshFlash cannot select a new port \
                 without a risk. A device can get a new port number after a reset. \
                 Available ports: {}. Select the port again.",
                if available.is_empty() {
                    "none".to_string()
                } else {
                    available.join(", ")
                }
            )
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// After the 1200-baud touch, the device restarts in its bootloader. The
/// bootloader can get a different COM number. This function prefers the
/// original name. If the original name is gone, it takes the first new port.
fn resolve_port_after_touch(original: &str, before: &HashSet<String>) -> String {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let now = port_names();
        if now.iter().any(|p| p.eq_ignore_ascii_case(original)) {
            return original.to_string();
        }
        if let Some(new_port) = now.iter().find(|p| !before.contains(*p)) {
            return new_port.clone();
        }
        if Instant::now() > deadline {
            return original.to_string(); // let the open fail with a clear error
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

pub fn channel() -> (Sender<Event>, Receiver<Event>) {
    std::sync::mpsc::channel()
}

/// Download the latest version of each firmware and the eraser packages into
/// the local cache. Then a flash is possible without an internet connection.
/// Set `cancel` to stop the downloads, also in the middle of a file.
pub fn spawn_predownload(
    config: Config,
    tx: Sender<Event>,
    ctx: eframe::egui::Context,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering;
    std::thread::spawn(move || {
        let agent = net::agent();

        // Collect unique URLs: latest version per firmware + eraser packages.
        let mut seen = HashSet::new();
        let mut urls: Vec<String> = Vec::new();
        let mut push = |url: String| {
            if seen.insert(url.clone()) {
                urls.push(url);
            }
        };
        for device in &config.device {
            if let Some(erase) = &device.erase {
                push(catalog::resolve_file_url(&config.static_path, erase));
            }
            for firmware in &device.firmware {
                if let Some((_, entry)) = firmware.parsed_versions().first() {
                    for file in &entry.files {
                        push(catalog::resolve_file_url(&config.static_path, &file.name));
                    }
                }
            }
        }

        let total = urls.len();
        let mut failed = 0usize;
        let mut done = 0usize;
        for (i, url) in urls.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let name = url.rsplit('/').next().unwrap_or(url).to_string();
            let _ = tx.send(Event::CacheProgress {
                done: i,
                total,
                name,
            });
            ctx.request_repaint();
            match net::download_cached(&agent, url, &mut |_, _| !cancel.load(Ordering::Relaxed)) {
                Ok(_) => done += 1,
                Err(_) if cancel.load(Ordering::Relaxed) => break,
                Err(_) => failed += 1,
            }
        }

        let msg = if cancel.load(Ordering::Relaxed) {
            format!("Download canceled — {done} of {total} files are in the cache.")
        } else if failed == 0 {
            format!("All {total} firmware files are in the cache for offline use.")
        } else {
            format!("{done} of {total} files are in the cache. {failed} downloads failed.")
        };
        let _ = tx.send(Event::CacheDone(msg));
        ctx.request_repaint();
    });
}
