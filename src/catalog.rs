//! Device/firmware catalog: the flasher's config.json model plus the
//! /releases feed, merged the same way flasher.js does it.

use anyhow::{Context, Result};
use regex::Regex;
use serde::Deserialize;
use serde_json::Map;
use std::collections::BTreeMap;

pub const FLASHER_ORIGIN: &str = "https://flasher.meshcore.io";
pub const GITHUB_RELEASES_BASE: &str = "https://github.com/meshcore-dev/MeshCore";

pub const BUNDLED_CONFIG: &str = include_str!("../assets/config.json");
pub const BUNDLED_RELEASES: &str = include_str!("../assets/releases.json");

#[derive(Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(rename = "staticPath")]
    pub static_path: String,
    #[serde(default)]
    pub role: BTreeMap<String, Role>,
    #[serde(default)]
    pub notice: BTreeMap<String, String>,
    pub device: Vec<Device>,
}

// Some fields mirror the JSON schema for completeness. The UI does not use
// all of them yet.
#[allow(dead_code)]
#[derive(Deserialize, Clone, Debug, Default)]
pub struct Role {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(rename = "subTitle", default)]
    pub sub_title: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
}

#[allow(dead_code)]
#[derive(Deserialize, Clone, Debug)]
pub struct Device {
    #[serde(default)]
    pub maker: String,
    #[serde(default)]
    pub class: String,
    pub name: String,
    #[serde(rename = "type")]
    pub device_type: String, // "esp32" | "nrf52" | "noflash"
    /// nRF flash-eraser DFU package (zip) served from staticPath.
    #[serde(default)]
    pub erase: Option<String>,
    #[serde(default)]
    pub bootloader: Option<String>,
    #[serde(default)]
    pub firmware: Vec<Firmware>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Firmware {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(rename = "subTitle", default)]
    pub sub_title: Option<String>,
    #[serde(default)]
    pub notice: Option<String>,
    #[serde(default)]
    pub github: Option<GithubDef>,
    /// Version name -> entry. The insertion order is the display order
    /// (newest first).
    #[serde(default)]
    pub version: Map<String, serde_json::Value>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct GithubDef {
    #[serde(rename = "type")]
    pub release_type: String,
    /// fileType ("flash-update", "flash-wipe", "download") -> filename regex
    #[serde(default)]
    pub files: Map<String, serde_json::Value>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct VersionEntry {
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub files: Vec<FileEntry>,
}

#[allow(dead_code)]
#[derive(Deserialize, Clone, Debug)]
pub struct FileEntry {
    #[serde(rename = "type")]
    pub file_type: String,
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Release {
    pub version: String,
    #[serde(rename = "type")]
    pub release_type: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub files: Vec<ReleaseFile>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct ReleaseFile {
    pub name: String,
    pub url: String,
}

impl Firmware {
    pub fn parsed_versions(&self) -> Vec<(String, VersionEntry)> {
        self.version
            .iter()
            .filter_map(|(name, value)| {
                serde_json::from_value::<VersionEntry>(value.clone())
                    .ok()
                    .map(|entry| (name.clone(), entry))
            })
            .filter(|(_, entry)| !entry.files.is_empty())
            .collect()
    }

    pub fn has_data(&self) -> bool {
        !self.parsed_versions().is_empty()
    }
}

/// Port of getGithubReleases() and addGithubFiles() from flasher.js. The
/// function fills the version map of each github-backed firmware from the
/// releases feed. Then it removes devices that have no flashable firmware.
pub fn merge_releases(config: &mut Config, releases: &[Release]) {
    for device in &mut config.device {
        for firmware in &mut device.firmware {
            let Some(gdef) = &firmware.github else {
                continue;
            };
            if gdef.files.is_empty() {
                continue;
            }

            let mut versions: Map<String, serde_json::Value> = Map::new();
            for (file_type, regex_value) in &gdef.files {
                let Some(pattern) = regex_value.as_str() else {
                    continue;
                };
                let Ok(re) = Regex::new(pattern) else {
                    continue;
                };

                for release in releases {
                    if release.release_type != gdef.release_type {
                        continue;
                    }
                    let entry = versions.entry(release.version.clone()).or_insert_with(
                        || serde_json::json!({ "notes": release.notes, "files": [] }),
                    );
                    let files = entry
                        .get_mut("files")
                        .and_then(|f| f.as_array_mut())
                        .expect("files array");
                    for file in &release.files {
                        if re.is_match(&file.name) {
                            files.push(serde_json::json!({
                                "type": file_type,
                                "name": file.url,
                                "title": file.name,
                            }));
                        }
                    }
                }
            }

            versions.retain(|_, v| {
                v.get("files")
                    .and_then(|f| f.as_array())
                    .map(|a| !a.is_empty())
                    .unwrap_or(false)
            });
            firmware.version = versions;
        }
    }

    config
        .device
        .retain(|device| device.firmware.iter().any(|f| f.has_data()));
}

pub fn load_bundled() -> Result<(Config, Vec<Release>)> {
    let config: Config =
        serde_json::from_str(BUNDLED_CONFIG).context("parsing bundled config.json")?;
    let releases: Vec<Release> =
        serde_json::from_str(BUNDLED_RELEASES).context("parsing bundled releases.json")?;
    Ok((config, releases))
}

/// Resolve a catalog file name to a downloadable URL.
///
/// - absolute http(s) URLs pass through
/// - "/releases/download/..." paths are GitHub release assets (fetch straight
///   from GitHub rather than the flasher's proxy)
/// - other absolute paths are served by the flasher site
/// - bare names live under the flasher's staticPath (/firmware)
pub fn resolve_file_url(static_path: &str, name: &str) -> String {
    if name.starts_with("http://") || name.starts_with("https://") {
        name.to_string()
    } else if name.starts_with("/releases/download/") {
        format!("{GITHUB_RELEASES_BASE}{name}")
    } else if name.starts_with('/') {
        format!("{FLASHER_ORIGIN}{name}")
    } else {
        format!("{FLASHER_ORIGIN}{static_path}/{name}")
    }
}

/// Display title for a firmware entry. The role definition supplies the
/// values that the entry does not set.
pub fn firmware_title(config: &Config, firmware: &Firmware) -> String {
    let role = firmware.role.as_deref().and_then(|r| config.role.get(r));
    let title = firmware
        .title
        .clone()
        .or_else(|| role.and_then(|r| r.title.clone()))
        .unwrap_or_else(|| "Firmware".into());
    let sub = firmware
        .sub_title
        .clone()
        .or_else(|| role.and_then(|r| r.sub_title.clone()));
    match sub {
        Some(sub) if !sub.is_empty() => format!("{title} — {sub}"),
        _ => title,
    }
}
