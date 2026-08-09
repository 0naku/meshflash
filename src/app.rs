use crate::catalog::{self, Config, Device, VersionEntry};
use crate::console::{COMMAND_REFERENCE, Console};
use crate::ports::{self, PortEntry};
use crate::worker::{self, Event, FlashJob, Source};
use eframe::egui::{
    self, Align, Align2, Button, Color32, CornerRadius, FontFamily, FontId, Frame, Layout, Margin,
    RichText, ScrollArea, Sense, Stroke, StrokeKind, TextStyle, Vec2,
};
use std::time::{Duration, Instant};

// ---------- palette ----------

// Palette from blog.meshcore.io (assets/css/main.css + layouts):
// Tailwind gray-900 #111827 background, white text, blue-500 buttons.
const ACCENT: Color32 = Color32::from_rgb(59, 130, 246); // blue-500 #3b82f6
const ON_ACCENT: Color32 = Color32::WHITE; // button text on blue
const ACCENT_DIM: Color32 = Color32::from_rgb(30, 58, 138); // blue-900 (selected fill)
const BG: Color32 = Color32::from_rgb(17, 24, 39); // gray-900 #111827
const CARD: Color32 = Color32::from_rgb(31, 41, 55); // gray-800 #1f2937
const CARD_HOVER: Color32 = Color32::from_rgb(55, 65, 81); // gray-700 #374151
const TEXT_DIM: Color32 = Color32::from_rgb(156, 163, 175); // gray-400 #9ca3af
const WARN: Color32 = Color32::from_rgb(240, 180, 60);
const ERR: Color32 = Color32::from_rgb(240, 90, 90);
const OK: Color32 = Color32::from_rgb(70, 200, 110);

const CLASS_ORDER: &[(&str, &str)] = &[
    ("ripple", "MeshCore"),
    ("meshos", "MeshCore Ultra (GUI)"),
    ("community", "Community devices"),
];

fn role_emoji(role: Option<&str>) -> &'static str {
    match role.unwrap_or("") {
        "repeater" => "📡",
        "roomServer" => "🏠",
        "companionBle" => "📱",
        "companionUsb" => "🔌",
        "meshos" | "gui" | "guiSD" => "🖥",
        _ => "📦",
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Flash,
    Console,
}

enum FlashState {
    Idle,
    Running,
    Done(String),
    Failed(String),
}

pub struct MeshFlashApp {
    tx: std::sync::mpsc::Sender<Event>,
    rx: std::sync::mpsc::Receiver<Event>,

    config: Option<Config>,
    status: String,

    tab: Tab,
    advanced: bool,

    // selection
    filter: String,
    device_index: Option<usize>,
    firmware_index: Option<usize>,
    version_name: Option<String>,
    clean_install: bool,
    nrf_touch: bool,

    // ports
    port_list: Vec<PortEntry>,
    selected_port: Option<String>,
    port_auto: bool,
    last_port_refresh: Instant,

    // flashing
    flash_state: FlashState,
    flash_log: String,
    flash_percent: u8,
    download_status: String,

    // console
    console: Option<Console>,
    console_output: String,
    console_input: String,

    // offline cache
    cache_busy: bool,
    cache_status: String,
    cache_size_bytes: u64,
    cache_cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,

    pending_ctx: Option<egui::Context>,
}

fn setup_style(ctx: &egui::Context) {
    ctx.all_styles_mut(apply_style);
}

fn apply_style(style: &mut egui::Style) {
    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(24.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(15.0, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(15.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(12.5, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(13.5, FontFamily::Monospace),
        ),
    ]
    .into();

    style.spacing.item_spacing = Vec2::new(10.0, 8.0);
    style.spacing.button_padding = Vec2::new(14.0, 7.0);

    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = BG;
    v.extreme_bg_color = Color32::from_rgb(11, 16, 28);
    v.selection.bg_fill = ACCENT_DIM;
    v.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    v.hyperlink_color = ACCENT;

    let round = CornerRadius::same(8);
    v.widgets.inactive.corner_radius = round;
    v.widgets.hovered.corner_radius = round;
    v.widgets.active.corner_radius = round;
    v.widgets.open.corner_radius = round;
    v.widgets.noninteractive.corner_radius = round;
    v.widgets.inactive.bg_fill = CARD;
    v.widgets.inactive.weak_bg_fill = CARD;
    v.widgets.hovered.bg_fill = CARD_HOVER;
    v.widgets.hovered.weak_bg_fill = CARD_HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    v.widgets.active.bg_fill = CARD_HOVER;
    v.widgets.active.weak_bg_fill = CARD_HOVER;

    style.visuals = v;
}

impl MeshFlashApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_style(&cc.egui_ctx);
        let (tx, rx) = worker::channel();
        worker::spawn_catalog_refresh(tx.clone(), cc.egui_ctx.clone());
        Self {
            tx,
            rx,
            config: None,
            status: "Loading catalog…".into(),
            tab: Tab::Flash,
            advanced: false,
            filter: String::new(),
            device_index: None,
            firmware_index: None,
            version_name: None,
            clean_install: false,
            nrf_touch: true,
            port_list: ports::list_ports(),
            selected_port: None,
            port_auto: true,
            last_port_refresh: Instant::now(),
            flash_state: FlashState::Idle,
            flash_log: String::new(),
            flash_percent: 0,
            download_status: String::new(),
            console: None,
            console_output: String::new(),
            console_input: String::new(),
            cache_busy: false,
            cache_status: String::new(),
            cache_size_bytes: crate::net::cache_size(),
            cache_cancel: Default::default(),
            pending_ctx: None,
        }
    }

    // ---------- data plumbing ----------

    fn pump_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Log(line) => {
                    if self.config.is_none() {
                        self.status = line.clone();
                    }
                    self.flash_log.push_str(&line);
                    self.flash_log.push('\n');
                }
                Event::DownloadProgress(done, total) => {
                    self.download_status = format!(
                        "Downloading firmware… {:.1} / {:.1} MB",
                        done as f64 / 1e6,
                        total as f64 / 1e6
                    );
                }
                Event::FlashProgress(pct) => {
                    self.download_status.clear();
                    self.flash_percent = pct;
                }
                Event::CatalogReady(config) => {
                    self.config = Some(*config);
                }
                Event::Done(msg) => {
                    self.flash_state = FlashState::Done(msg);
                    self.flash_percent = 100;
                }
                Event::Failed(msg) => {
                    self.flash_state = FlashState::Failed(msg);
                }
                Event::CacheProgress { done, total, name } => {
                    self.cache_busy = true;
                    self.cache_status = format!("{}/{total}  {name}", done + 1);
                    self.cache_size_bytes = crate::net::cache_size();
                }
                Event::CacheDone(msg) => {
                    self.cache_busy = false;
                    self.cache_status = msg;
                    self.cache_size_bytes = crate::net::cache_size();
                }
            }
        }

        if let Some(console) = &self.console {
            while let Ok(chunk) = console.output.try_recv() {
                self.console_output.push_str(&chunk);
                let len = self.console_output.len();
                if len > 200_000 {
                    self.console_output = self.console_output.split_off(len - 100_000);
                }
            }
        }
    }

    fn refresh_ports(&mut self) {
        let fresh = ports::list_ports();
        if fresh != self.port_list {
            self.port_list = fresh;
        }
        let selection_alive = self
            .selected_port
            .as_ref()
            .map(|s| self.port_list.iter().any(|p| &p.name == s))
            .unwrap_or(false);
        if (self.port_auto || !selection_alive)
            && self.device_index.is_some()
            && let Some(device) = self.selected_device()
        {
            let device_type = device.device_type.clone();
            if let Some(pick) = ports::auto_pick(&self.port_list, &device_type)
                && (!selection_alive || self.port_auto)
            {
                self.selected_port = Some(pick);
                self.port_auto = true;
            }
        }
    }

    fn selected_device(&self) -> Option<&Device> {
        self.config.as_ref()?.device.get(self.device_index?)
    }

    fn selected_version(&self) -> Option<(String, VersionEntry)> {
        let device = self.selected_device()?;
        let firmware = device.firmware.get(self.firmware_index?)?;
        let name = self.version_name.clone()?;
        firmware
            .parsed_versions()
            .into_iter()
            .find(|(n, _)| *n == name)
    }

    fn start_flash_job(&mut self, job: FlashJob) {
        self.flash_state = FlashState::Running;
        self.flash_percent = 0;
        self.flash_log.clear();
        self.download_status.clear();
        if let Some(ctx) = self.pending_ctx.clone() {
            worker::spawn_flash(job, self.tx.clone(), ctx);
        }
    }

    // ---------- shared widgets ----------

    /// Left-aligned clickable card: bold title, dim subtitle underneath.
    fn card_button(ui: &mut egui::Ui, width: f32, title: &str, subtitle: &str) -> egui::Response {
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 56.0), Sense::click());
        if ui.is_rect_visible(rect) {
            let hovered = response.hovered();
            let bg = if hovered { CARD_HOVER } else { CARD };
            let stroke = if hovered {
                Stroke::new(1.0_f32, ACCENT)
            } else {
                Stroke::new(1.0_f32, Color32::from_rgb(55, 65, 81))
            };
            let painter = ui.painter().with_clip_rect(rect);
            painter.rect(rect, CornerRadius::same(8), bg, stroke, StrokeKind::Inside);
            painter.text(
                rect.left_top() + Vec2::new(12.0, 10.0),
                Align2::LEFT_TOP,
                title,
                FontId::new(15.0, FontFamily::Proportional),
                Color32::from_rgb(230, 233, 238),
            );
            painter.text(
                rect.left_bottom() + Vec2::new(12.0, -8.0),
                Align2::LEFT_BOTTOM,
                subtitle,
                FontId::new(12.0, FontFamily::Proportional),
                TEXT_DIM,
            );
        }
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    }

    fn steps_header(ui: &mut egui::Ui, current: u8) {
        ui.horizontal(|ui| {
            for (i, label) in ["Device", "Firmware", "Flash"].iter().enumerate() {
                let n = i as u8 + 1;
                let active = n == current;
                let done = n < current;
                let (badge, color) = if done {
                    ("✔".to_string(), OK)
                } else {
                    (n.to_string(), if active { ACCENT } else { TEXT_DIM })
                };
                ui.label(
                    RichText::new(format!("{badge}  {label}"))
                        .color(if active { Color32::WHITE } else { color })
                        .strong(),
                );
                if n < 3 {
                    ui.label(RichText::new("›").color(TEXT_DIM));
                }
            }
        });
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(6.0);
    }

    fn port_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Port:");
            let selected_label = match self.selected_port.as_ref() {
                None => "— no port found —".into(),
                Some(name) => self
                    .port_list
                    .iter()
                    .find(|p| &p.name == name)
                    .map(|p| p.label.clone())
                    .unwrap_or_else(|| format!("{name} (disconnected)")),
            };
            let before = self.selected_port.clone();
            egui::ComboBox::from_id_salt("port_combo")
                .width(340.0)
                .selected_text(selected_label)
                .show_ui(ui, |ui| {
                    for port in &self.port_list {
                        ui.selectable_value(
                            &mut self.selected_port,
                            Some(port.name.clone()),
                            &port.label,
                        );
                    }
                });
            if self.selected_port != before {
                self.port_auto = false; // manual choice wins from here on
            }
            if self.port_auto && self.selected_port.is_some() {
                ui.label(RichText::new("auto-detected").color(TEXT_DIM).small());
            }
        });
    }

    // ---------- screens ----------

    fn ui_devices(&mut self, ui: &mut egui::Ui) {
        Self::steps_header(ui, 1);
        ui.label(RichText::new("Select the device that you want to flash").heading());
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("🔍").color(TEXT_DIM));
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Search devices…")
                    .desired_width(280.0),
            );
        });

        let filter = self.filter.to_lowercase();
        let filtering = !filter.is_empty();
        let mut clicked: Option<usize> = None;

        // Collect owned display data. Then the scroll closure does not hold
        // a borrow of self.config while it changes other state.
        type Group = (
            &'static str,
            &'static str,
            Vec<(usize, String, &'static str)>,
        );
        let groups: Vec<Group> = {
            let Some(config) = &self.config else { return };
            CLASS_ORDER
                .iter()
                .filter_map(|(class, class_title)| {
                    let devices: Vec<(usize, String, &'static str)> = config
                        .device
                        .iter()
                        .enumerate()
                        .filter(|(_, d)| {
                            d.class == *class
                                && (filter.is_empty() || d.name.to_lowercase().contains(&filter))
                        })
                        .map(|(i, d)| {
                            let chip = match d.device_type.as_str() {
                                "esp32" => "ESP32",
                                "nrf52" => "nRF52",
                                _ => "info only",
                            };
                            (i, d.name.clone(), chip)
                        })
                        .collect();
                    if devices.is_empty() {
                        None
                    } else {
                        Some((*class, *class_title, devices))
                    }
                })
                .collect()
        };

        ScrollArea::vertical().id_salt("devices").show(ui, |ui| {
            for (class, class_title, devices) in &groups {
                ui.add_space(6.0);
                let header = RichText::new(format!("{}  ({})", class_title, devices.len()))
                    .color(ACCENT)
                    .strong()
                    .size(16.0);
                egui::CollapsingHeader::new(header)
                    .id_salt(*class)
                    // During a search, keep all groups open. Then all
                    // matches show.
                    .open(if filtering { Some(true) } else { None })
                    .default_open(true)
                    .show(ui, |ui| {
                        let available = ui.available_width();
                        let cols = ((available / 280.0).floor() as usize).max(1);
                        let card_w = (available - (cols as f32 - 1.0) * 10.0) / cols as f32 - 4.0;
                        ui.horizontal_wrapped(|ui| {
                            for (index, name, chip) in devices {
                                if Self::card_button(ui, card_w, name, chip).clicked() {
                                    clicked = Some(*index);
                                }
                            }
                        });
                    });
            }
            ui.add_space(12.0);
        });

        if let Some(index) = clicked {
            self.device_index = Some(index);
            self.firmware_index = None;
            self.version_name = None;
            self.clean_install = false;
            self.port_auto = true;
            self.refresh_ports();
        }
    }

    /// Sticky bottom bar on the device screen: offline cache manager and
    /// (in advanced mode) custom-file flashing.
    fn ui_cache_bar(&mut self, ui: &mut egui::Ui) {
        let mut custom_file_clicked = false;
        // horizontal_centered aligns each item to the vertical center of the
        // tallest item (the padded buttons). Then labels do not sit high.
        ui.horizontal_centered(|ui| {
            ui.label(RichText::new("💾 Offline cache:").strong());
            ui.label(
                RichText::new(format!("{:.1} MB", self.cache_size_bytes as f64 / 1e6))
                    .color(TEXT_DIM),
            );
            if self.cache_busy {
                ui.add(egui::Spinner::new().size(14.0));
                let cancelling = self.cache_cancel.load(std::sync::atomic::Ordering::Relaxed);
                if cancelling {
                    ui.label(RichText::new("Cancel in progress…").color(WARN).small());
                } else if ui.button("✖ Cancel").clicked() {
                    self.cache_cancel
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                }
            } else {
                if ui
                    .button("⬇ Download all firmware")
                    .on_hover_text(
                        "Downloads the latest version of each firmware in the catalog. \
                             Then you can flash without an internet connection.",
                    )
                    .clicked()
                {
                    self.cache_busy = true;
                    self.cache_status = "Starting…".into();
                    self.cache_cancel = Default::default(); // a new, cleared cancel flag
                    if let Some(ctx) = self.pending_ctx.clone()
                        && let Some(cfg) = self.config.clone()
                    {
                        worker::spawn_predownload(
                            cfg,
                            self.tx.clone(),
                            ctx,
                            self.cache_cancel.clone(),
                        );
                    }
                }
                if self.cache_size_bytes > 0 && ui.button("🗑 Clear").clicked() {
                    let _ = crate::net::clear_cache();
                    self.cache_size_bytes = 0;
                    self.cache_status.clear();
                }
            }

            // Right side: the custom-file button stays at the edge. The
            // status text truncates with "…" in the space between.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.advanced
                    && ui
                        .button("📄 Flash custom file…")
                        .on_hover_text(".bin = ESP32, .zip = nRF52 DFU package")
                        .clicked()
                {
                    custom_file_clicked = true;
                }
                if !self.cache_status.is_empty() {
                    let color = if self.cache_busy { TEXT_DIM } else { OK };
                    ui.add(
                        egui::Label::new(RichText::new(&self.cache_status).color(color).small())
                            .truncate(),
                    )
                    .on_hover_text(&self.cache_status);
                }
            });
        });

        if custom_file_clicked {
            self.custom_file_flow();
        }
    }

    fn custom_file_flow(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Firmware", &["bin", "zip"])
            .pick_file()
        else {
            return;
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let is_bin = name.ends_with(".bin");
        let merged = name.contains("merged");
        let device_type = if is_bin { "esp32" } else { "nrf52" };

        self.refresh_ports();
        let port = self
            .selected_port
            .clone()
            .or_else(|| ports::auto_pick(&self.port_list, device_type));
        let Some(port) = port else {
            self.flash_state =
                FlashState::Failed("No serial port found — plug the device in.".into());
            return;
        };
        self.start_flash_job(FlashJob {
            port,
            source: Source::File(path),
            device_type: device_type.into(),
            erase_all: is_bin && merged,
            esp_address: if merged {
                0
            } else {
                crate::esp::APP_FLASH_ADDRESS
            },
            nrf_touch: self.nrf_touch,
        });
    }

    fn ui_firmware(&mut self, ui: &mut egui::Ui) {
        let Some(config) = self.config.clone() else {
            return;
        };
        let Some(device) = self.selected_device().cloned() else {
            return;
        };

        Self::steps_header(ui, 2);
        ui.horizontal(|ui| {
            ui.label(RichText::new(&device.name).heading());
            ui.label(RichText::new(format!("({})", device.device_type)).color(TEXT_DIM));
            if ui.small_button("change device").clicked() {
                self.device_index = None;
                self.firmware_index = None;
                self.version_name = None;
            }
        });
        ui.add_space(6.0);

        if device.device_type == "noflash" {
            ui.label("MeshFlash cannot flash this device over USB.");
            return;
        }

        ui.label(RichText::new("Select the function for this device").strong());
        ui.horizontal_wrapped(|ui| {
            for (i, firmware) in device.firmware.iter().enumerate() {
                if !firmware.has_data() {
                    continue;
                }
                let title = format!(
                    "{} {}",
                    role_emoji(firmware.role.as_deref()),
                    catalog::firmware_title(&config, firmware)
                );
                let selected = self.firmware_index == Some(i);
                let mut button = Button::new(RichText::new(title).size(15.0));
                if selected {
                    button = button.fill(ACCENT_DIM).stroke(Stroke::new(1.0_f32, ACCENT));
                }
                if ui.add(button).clicked() {
                    self.firmware_index = Some(i);
                    self.version_name = firmware
                        .parsed_versions()
                        .first()
                        .map(|(name, _)| name.clone());
                }
            }
        });

        let Some(fw_index) = self.firmware_index else {
            ui.add_space(6.0);
            ui.label(RichText::new("Select a function above to continue.").color(TEXT_DIM));
            return;
        };
        let firmware = &device.firmware[fw_index];
        let versions = firmware.parsed_versions();

        ui.add_space(8.0);

        // Version: latest by default, dropdown only in advanced mode.
        ui.horizontal(|ui| {
            ui.label("Version:");
            if self.advanced {
                let current = self.version_name.clone().unwrap_or_default();
                egui::ComboBox::from_id_salt("version_combo")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for (name, _) in &versions {
                            ui.selectable_value(&mut self.version_name, Some(name.clone()), name);
                        }
                    });
            } else {
                let latest = versions.first().map(|(n, _)| n.clone()).unwrap_or_default();
                self.version_name = Some(latest.clone());
                ui.label(RichText::new(format!("{latest}  (latest)")).strong());
            }
        });

        // Notice
        if let Some(notice_key) = &firmware.notice {
            let notice = config
                .notice
                .get(notice_key)
                .cloned()
                .unwrap_or_else(|| notice_key.clone());
            let notice = strip_html(&notice);
            if !notice.is_empty() {
                ui.add_space(2.0);
                Frame::new()
                    .fill(Color32::from_rgb(45, 38, 20))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.label(RichText::new(format!("⚠ {notice}")).color(WARN));
                    });
            }
        }

        let Some((_, entry)) = self.selected_version() else {
            return;
        };

        if let Some(notes) = &entry.notes {
            egui::CollapsingHeader::new("Changes in this version")
                .default_open(false)
                .show(ui, |ui| {
                    ScrollArea::vertical()
                        .id_salt("notes")
                        .max_height(130.0)
                        .show(ui, |ui| {
                            ui.label(RichText::new(strip_html(notes)).color(TEXT_DIM));
                        });
                });
        }

        ui.add_space(8.0);

        // Install mode, in plain language.
        let has_wipe = entry.files.iter().any(|f| f.file_type == "flash-wipe");
        let has_update = entry.files.iter().any(|f| f.file_type == "flash-update");
        if device.device_type == "esp32" {
            if has_wipe && has_update {
                ui.label(RichText::new("Install type:").strong());
                ui.radio_value(
                    &mut self.clean_install,
                    false,
                    "Update — keep the settings and the node identity",
                );
                ui.radio_value(
                    &mut self.clean_install,
                    true,
                    "First install / factory reset — erase all data",
                );
            } else if has_wipe {
                self.clean_install = true;
                ui.label(
                    RichText::new("⚠ This image erases all flash memory (first-install image).")
                        .color(WARN),
                );
            } else {
                self.clean_install = false;
            }
        }

        // Advanced extras
        if self.advanced {
            egui::CollapsingHeader::new("Advanced")
                .default_open(true)
                .show(ui, |ui| {
                    if device.device_type == "nrf52" {
                        ui.checkbox(
                            &mut self.nrf_touch,
                            "Enter DFU mode automatically (1200-baud touch)",
                        );
                        ui.label(
                            RichText::new(
                                "If the device is already in DFU mode, clear this checkbox.",
                            )
                            .color(TEXT_DIM)
                            .small(),
                        );
                        if let Some(erase_pkg) = &device.erase {
                            let can =
                                self.selected_port.is_some() && !matches!(self.flash_state, FlashState::Running);
                            if ui
                                .add_enabled(can, Button::new("🧹 Erase device (flash eraser package)"))
                                .clicked()
                            {
                                let url = catalog::resolve_file_url(&config.static_path, erase_pkg);
                                let port = self.selected_port.clone().unwrap();
                                let nrf_touch = self.nrf_touch;
                                self.start_flash_job(FlashJob {
                                    port,
                                    source: Source::Url(url),
                                    device_type: "nrf52".into(),
                                    erase_all: false,
                                    esp_address: 0,
                                    nrf_touch,
                                });
                            }
                        }
                    } else {
                        ui.label(
                            RichText::new("An update writes at 0x10000. A full wipe writes the merged image at 0x0.")
                                .color(TEXT_DIM),
                        );
                    }
                });
        }

        ui.add_space(10.0);
        self.port_row(ui);
        ui.add_space(10.0);

        // The big flash button.
        let ready = self.selected_port.is_some();
        ui.horizontal(|ui| {
            let button = Button::new(
                RichText::new("⚡  Flash")
                    .size(18.0)
                    .strong()
                    .color(ON_ACCENT),
            )
            .fill(ACCENT)
            .min_size(Vec2::new(200.0, 44.0));
            if ui.add_enabled(ready, button).clicked() {
                let file = if device.device_type == "esp32" {
                    let wanted = if self.clean_install {
                        "flash-wipe"
                    } else {
                        "flash-update"
                    };
                    entry
                        .files
                        .iter()
                        .find(|f| f.file_type == wanted)
                        .or_else(|| {
                            entry
                                .files
                                .iter()
                                .find(|f| f.file_type.starts_with("flash"))
                        })
                } else {
                    entry
                        .files
                        .iter()
                        .find(|f| f.file_type.starts_with("flash"))
                };
                if let Some(file) = file {
                    let url = catalog::resolve_file_url(&config.static_path, &file.name);
                    let job = FlashJob {
                        port: self.selected_port.clone().unwrap(),
                        source: Source::Url(url),
                        device_type: device.device_type.clone(),
                        erase_all: self.clean_install && device.device_type == "esp32",
                        esp_address: crate::esp::APP_FLASH_ADDRESS,
                        nrf_touch: self.nrf_touch,
                    };
                    self.start_flash_job(job);
                } else {
                    self.flash_state =
                        FlashState::Failed("No flashable file in this version.".into());
                }
            }
            if !ready {
                ui.label(
                    RichText::new(
                        "Connect the device with USB. The port then shows here automatically.",
                    )
                    .color(TEXT_DIM),
                );
            }
        });
    }

    fn ui_flashing(&mut self, ui: &mut egui::Ui) {
        Self::steps_header(ui, 3);

        let device_name = self
            .selected_device()
            .map(|d| d.name.clone())
            .unwrap_or_else(|| "Custom firmware".into());

        match &self.flash_state {
            FlashState::Running => {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(format!("Flashing {device_name}…")).heading());
                    ui.add_space(6.0);
                    ui.label(RichText::new("Do not disconnect the USB cable.").color(WARN));
                    ui.add_space(14.0);
                    if !self.download_status.is_empty() {
                        ui.label(&self.download_status);
                        ui.add(egui::Spinner::new().size(22.0));
                    } else {
                        ui.add(
                            egui::ProgressBar::new(self.flash_percent as f32 / 100.0)
                                .desired_width(420.0)
                                .show_percentage(),
                        );
                    }
                });
            }
            FlashState::Done(msg) => {
                let msg = msg.clone();
                ui.add_space(24.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("✔").color(OK).size(52.0));
                    ui.label(RichText::new("Done!").heading());
                    ui.add_space(4.0);
                    ui.label(&msg);
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "If the device is a repeater or a room server, open the Console \
                             tab. There you can set the name, the frequency, and the location.",
                        )
                        .color(TEXT_DIM),
                    );
                    ui.add_space(14.0);
                    if ui
                        .add(Button::new(
                            RichText::new("Flash another device").size(15.0),
                        ))
                        .clicked()
                    {
                        self.flash_state = FlashState::Idle;
                        self.device_index = None;
                        self.firmware_index = None;
                        self.version_name = None;
                    }
                    if ui.small_button("Same device, different firmware").clicked() {
                        self.flash_state = FlashState::Idle;
                        self.firmware_index = None;
                        self.version_name = None;
                    }
                });
            }
            FlashState::Failed(msg) => {
                let msg = msg.clone();
                ui.add_space(24.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("✘").color(ERR).size(52.0));
                    ui.label(RichText::new("The flash failed").heading());
                    ui.add_space(4.0);
                    ui.label(RichText::new(&msg).color(ERR));
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.add_space(ui.available_width() / 2.0 - 120.0);
                            if ui.button("← Back").clicked() {
                                self.flash_state = FlashState::Idle;
                            }
                            if ui
                                .add(
                                    Button::new(RichText::new("Retry").color(ON_ACCENT))
                                        .fill(ACCENT),
                                )
                                .clicked()
                            {
                                self.flash_state = FlashState::Idle;
                                // user retries from the firmware screen with fresh ports
                                self.refresh_ports();
                            }
                        });
                    });
                });
            }
            FlashState::Idle => {}
        }

        if !self.flash_log.is_empty() {
            ui.add_space(16.0);
            egui::CollapsingHeader::new("Log")
                .default_open(matches!(self.flash_state, FlashState::Failed(_)))
                .show(ui, |ui| {
                    Frame::new()
                        .fill(Color32::from_rgb(12, 13, 17))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            ScrollArea::vertical()
                                .id_salt("flash_log")
                                .max_height(180.0)
                                .stick_to_bottom(true)
                                .show(ui, |ui| {
                                    ui.monospace(&self.flash_log);
                                });
                        });
                });
        }
    }

    fn ui_console(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Serial console").heading());
        ui.label(
            RichText::new(
                "The console talks to repeater and room server firmware over USB serial \
                 (115200 baud). After you flash a device, use the console to set the name, \
                 the frequency, and the location.",
            )
            .color(TEXT_DIM),
        );
        ui.add_space(6.0);
        self.port_row(ui);
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            if self.console.is_none() {
                let can_open = self.selected_port.is_some();
                let button =
                    Button::new(RichText::new("Open console").color(ON_ACCENT)).fill(ACCENT);
                if ui.add_enabled(can_open, button).clicked() {
                    match Console::open(self.selected_port.as_ref().unwrap()) {
                        Ok(console) => {
                            self.console = Some(console);
                            self.console_output.clear();
                        }
                        Err(e) => {
                            self.console_output = format!("Failed to open: {e:#}\n");
                        }
                    }
                }
            } else if ui.button("Close console").clicked() {
                self.console = None;
            }
        });

        ui.add_space(6.0);
        // The input row is laid out bottom-up first. Then the layout cannot
        // push it off the screen. The two columns fill the remaining space.
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.horizontal(|ui| {
                let edit = egui::TextEdit::singleline(&mut self.console_input)
                    .hint_text("type a command (for example: get name)")
                    .desired_width(ui.available_width() - 90.0)
                    .font(TextStyle::Monospace);
                let response = ui.add(edit);
                let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Send").clicked() || enter) && !self.console_input.is_empty() {
                    if let Some(console) = &self.console {
                        console.send(&self.console_input);
                        self.console_output
                            .push_str(&format!("> {}\n", self.console_input));
                    }
                    self.console_input.clear();
                    response.request_focus();
                }
            });
            ui.add_space(4.0);

            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                let body = ui.available_height();
                ui.columns(2, |columns| {
                    Frame::new()
                        .fill(Color32::from_rgb(11, 16, 28))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::same(10))
                        .show(&mut columns[0], |ui| {
                            ScrollArea::vertical()
                                .id_salt("console_out")
                                .max_height(body - 24.0)
                                .min_scrolled_height((body - 24.0).max(80.0))
                                .stick_to_bottom(true)
                                .show(ui, |ui| {
                                    ui.monospace(if self.console_output.is_empty() {
                                        "— the console output shows here —"
                                    } else {
                                        &self.console_output
                                    });
                                });
                        });

                    columns[1]
                        .label(RichText::new("Command list (click a command to use it)").strong());
                    ScrollArea::vertical()
                        .id_salt("console_ref")
                        .max_height(body - 40.0)
                        .show(&mut columns[1], |ui| {
                            for (cmd, help) in COMMAND_REFERENCE {
                                if ui
                                    .selectable_label(
                                        false,
                                        RichText::new(format!("{cmd}  —  {help}")).small(),
                                    )
                                    .clicked()
                                {
                                    self.console_input = cmd
                                        .split('<')
                                        .next()
                                        .unwrap_or(cmd)
                                        .split('{')
                                        .next()
                                        .unwrap_or(cmd)
                                        .trim_end()
                                        .to_string();
                                }
                            }
                        });
                });
            });
        });
    }
}

fn strip_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .trim()
        .to_string()
}

impl eframe::App for MeshFlashApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.pending_ctx = Some(ctx.clone());
        self.pump_events();

        if self.last_port_refresh.elapsed() > Duration::from_secs(2) {
            self.last_port_refresh = Instant::now();
            self.refresh_ports();
        }
        // periodic wake-ups for port refresh / console output
        ctx.request_repaint_after(Duration::from_millis(
            if self.console.is_some() || matches!(self.flash_state, FlashState::Running) {
                100
            } else {
                1000
            },
        ));

        // Sticky bottom bar with the offline cache manager, only on the
        // device-selection screen.
        let on_device_screen = matches!(self.tab, Tab::Flash)
            && self.config.is_some()
            && matches!(self.flash_state, FlashState::Idle)
            && self.device_index.is_none();
        if on_device_screen {
            egui::Panel::bottom("cache_bar")
                .frame(
                    Frame::new()
                        .fill(CARD)
                        .inner_margin(Margin::symmetric(14, 8)),
                )
                .show(ui, |ui| self.ui_cache_bar(ui));
        }

        egui::Panel::top("top")
            .frame(
                Frame::new()
                    .fill(CARD)
                    .inner_margin(Margin::symmetric(14, 10)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("⚡ MeshFlash")
                            .strong()
                            .size(19.0)
                            .color(ACCENT),
                    );
                    ui.add_space(12.0);
                    ui.selectable_value(&mut self.tab, Tab::Flash, "Flash");
                    ui.selectable_value(&mut self.tab, Tab::Console, "Console");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.toggle_value(&mut self.advanced, "⚙ Advanced")
                            .on_hover_text(
                                "Shows these extra controls:\n\
                                 • Version: pick an older release, not only the latest\n\
                                 • Flash custom file: flash a .bin or .zip from your disk\n\
                                 • nRF52 only: the DFU mode checkbox and the flash eraser",
                            );
                        ui.label(RichText::new(&self.status).color(TEXT_DIM).small());
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(Frame::new().fill(BG).inner_margin(Margin::same(16)))
            .show(ui, |ui| match self.tab {
                Tab::Flash => {
                    if self.config.is_none() {
                        ui.centered_and_justified(|ui| {
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new().size(22.0));
                                ui.label("Loading device catalog…");
                            });
                        });
                    } else if !matches!(self.flash_state, FlashState::Idle) {
                        self.ui_flashing(ui);
                    } else if self.device_index.is_none() {
                        self.ui_devices(ui);
                    } else {
                        self.ui_firmware(ui);
                    }
                }
                Tab::Console => self.ui_console(ui),
            });
    }
}
