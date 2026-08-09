//! HTTP fetch + firmware download cache.

use anyhow::{Context, Result, bail};
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

pub fn agent() -> ureq::Agent {
    // native-tls uses the certificate store of the operating system. Then a
    // company proxy with its own root certificate still works.
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .build();
    let config = ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_connect(Some(Duration::from_secs(15)))
        .user_agent(concat!("meshflash/", env!("CARGO_PKG_VERSION")))
        .max_redirects(8)
        .build();
    ureq::Agent::new_with_config(config)
}

pub fn fetch_string(agent: &ureq::Agent, url: &str) -> Result<String> {
    let mut response = agent
        .get(url)
        .call()
        .with_context(|| format!("GET {url}"))?;
    Ok(response.body_mut().read_to_string()?)
}

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("MeshFlash")
        .join("firmware")
}

/// File name that the cache uses for `url`.
pub fn cache_name(url: &str) -> Option<&str> {
    url.rsplit('/').next().filter(|s| !s.is_empty())
}

/// Path of the cached file for `url`, when the file is there and not empty.
/// The caller can then read it without a network connection.
pub fn cached_file(url: &str) -> Option<PathBuf> {
    let path = cache_dir().join(cache_name(url)?);
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() && meta.len() > 0 => Some(path),
        _ => None,
    }
}

pub fn cache_size() -> u64 {
    std::fs::read_dir(cache_dir())
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

pub fn clear_cache() -> std::io::Result<()> {
    let dir = cache_dir();
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

/// Download `url` into the local cache and return the file bytes. If the
/// file is already in the cache, the function returns it without a download.
/// `progress` receives (downloaded, total_or_0). When `progress` returns
/// `false`, the download stops.
pub fn download_cached(
    agent: &ureq::Agent,
    url: &str,
    progress: &mut impl FnMut(u64, u64) -> bool,
) -> Result<Vec<u8>> {
    let filename = url
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .context("cannot derive filename from url")?;
    let dir = cache_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(filename);

    if path.is_file() {
        let data = std::fs::read(&path)?;
        if !data.is_empty() {
            let _ = progress(data.len() as u64, data.len() as u64);
            return Ok(data);
        }
    }

    let mut response = agent
        .get(url)
        .call()
        .with_context(|| format!("GET {url}"))?;
    if response.status().as_u16() != 200 {
        bail!("HTTP {} for {url}", response.status());
    }
    let total: u64 = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let mut reader = response.body_mut().as_reader();
    let mut data: Vec<u8> = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        if !progress(data.len() as u64, total) {
            bail!("download canceled");
        }
    }

    if data.is_empty() {
        bail!("downloaded empty file from {url}");
    }

    // The cache write is best effort. The flash uses the data in memory.
    let _ = std::fs::write(&path, &data);
    Ok(data)
}
