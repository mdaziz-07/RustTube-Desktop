use anyhow::{Context, Result, anyhow};
use parking_lot::Mutex;
use serde_json::Value;
use std::io::Read;
use std::time::Duration;
use ureq::config::{ConfigBuilder, IpFamily};
use ureq::typestate::AgentScope;

pub const BROWSER_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

pub(crate) struct NetworkAgent(Mutex<Option<CachedAgent>>);

struct CachedAgent {
    ipv4_only: bool,
    proxy: String,
    agent: ureq::Agent,
}

impl NetworkAgent {
    pub(crate) const fn new() -> Self {
        Self(Mutex::new(None))
    }

    pub(crate) fn get(
        &self,
        settings: &crate::store::Settings,
        configure: impl FnOnce(ConfigBuilder<AgentScope>) -> ConfigBuilder<AgentScope>,
    ) -> ureq::Agent {
        let mut cached = self.0.lock();
        if let Some(current) = cached.as_ref().filter(|current| {
            current.ipv4_only == settings.ipv4_only && current.proxy == settings.proxy
        }) {
            return current.agent.clone();
        }

        let family = if settings.ipv4_only {
            IpFamily::Ipv4Only
        } else {
            IpFamily::Any
        };
        let mut builder = configure(ureq::Agent::config_builder()).ip_family(family);
        if !settings.proxy.trim().is_empty() {
            if let Ok(proxy) = ureq::Proxy::new(&settings.proxy) {
                builder = builder.proxy(Some(proxy));
            }
        }
        let agent = builder.build().new_agent();
        *cached = Some(CachedAgent {
            ipv4_only: settings.ipv4_only,
            proxy: settings.proxy.clone(),
            agent: agent.clone(),
        });
        agent
    }
}

fn agent() -> ureq::Agent {
    static AGENT: NetworkAgent = NetworkAgent::new();
    AGENT.get(&crate::store::settings_snapshot(), |builder| {
        builder
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .user_agent(BROWSER_UA)
    })
}

fn download_agent() -> ureq::Agent {
    static AGENT: NetworkAgent = NetworkAgent::new();
    AGENT.get(&crate::store::settings_snapshot(), |builder| {
        builder
            .timeout_global(Some(Duration::from_secs(300)))
            .http_status_as_error(false)
            .user_agent(BROWSER_UA)
    })
}

#[cfg(test)]
mod tests {
    use super::{IpFamily, NetworkAgent};
    use crate::store::Settings;

    #[test]
    fn changes_address_family_after_the_first_request() {
        let cache = NetworkAgent::new();
        let mut settings = Settings {
            ipv4_only: true,
            ..Settings::default()
        };
        let initial = cache.get(&settings, |builder| builder);
        assert_eq!(initial.config().ip_family(), IpFamily::Ipv4Only);

        settings.ipv4_only = false;
        let updated = cache.get(&settings, |builder| builder);
        assert_eq!(updated.config().ip_family(), IpFamily::Any);
        assert_eq!(initial.config().ip_family(), IpFamily::Ipv4Only);

        settings.ipv4_only = true;
        let restored = cache.get(&settings, |builder| builder);
        assert_eq!(restored.config().ip_family(), IpFamily::Ipv4Only);
    }
}

pub fn get_bytes(url: &str, headers: &[(&str, &str)]) -> Result<Vec<u8>> {
    let mut req = agent().get(url);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut resp = req.call().with_context(|| format!("GET {url}"))?;
    let status = resp.status().as_u16();
    let mut buf = Vec::new();
    resp.body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .reader()
        .read_to_end(&mut buf)?;
    if status >= 400 {
        return Err(anyhow!("HTTP {status} for {url}"));
    }
    Ok(buf)
}

pub fn get_text(url: &str) -> Result<String> {
    Ok(String::from_utf8_lossy(&get_bytes(url, &[])?).into_owned())
}

pub fn get_json(url: &str) -> Result<Value> {
    let b = get_bytes(url, &[("Accept", "application/json")])?;
    serde_json::from_slice(&b).context("invalid json")
}

/// Returns (status, json)
pub fn get_json_status(url: &str) -> Result<(u16, Value)> {
    let mut resp = agent().get(url).header("Accept", "application/json").call()?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
}

pub fn post_json(url: &str, body: &Value, headers: &[(&str, &str)]) -> Result<Value> {
    let mut req = agent().post(url).header("Content-Type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut resp = req
        .send(serde_json::to_vec(body)?)
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status().as_u16();
    let text = resp
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_string()?;
    if status >= 400 {
        return Err(anyhow!("HTTP {status}: {}", text.chars().take(300).collect::<String>()));
    }
    serde_json::from_str(&text).context("invalid json")
}

/// POST returning the status code and JSON body even for error statuses.
pub fn post_json_status(url: &str, body: &Value, headers: &[(&str, &str)]) -> Result<(u16, Value)> {
    let mut req = agent().post(url).header("Content-Type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut resp = req.send(serde_json::to_vec(body)?).with_context(|| format!("POST {url}"))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
}

/// Streaming download to a writer with progress callback (downloaded, total).
pub fn download_to(
    url: &str,
    out: &mut dyn std::io::Write,
    mut progress: impl FnMut(u64, Option<u64>) -> bool,
) -> Result<()> {
    let mut resp = download_agent().get(url).call()?;
    if resp.status().as_u16() >= 400 {
        return Err(anyhow!("HTTP {}", resp.status()));
    }
    let total = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let mut reader = resp.body_mut().with_config().limit(u64::MAX).reader();
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        done += n as u64;
        if !progress(done, total) {
            return Err(anyhow!("cancelled"));
        }
    }
    Ok(())
}

fn write_all_at(file: &std::fs::File, mut offset: u64, mut data: &[u8]) -> std::io::Result<()> {
    while !data.is_empty() {
        #[cfg(windows)]
        let n = std::os::windows::fs::FileExt::seek_write(file, data, offset)?;
        #[cfg(unix)]
        let n = std::os::unix::fs::FileExt::write_at(file, data, offset)?;
        #[cfg(not(any(windows, unix)))]
        let n = {
            let _ = (&file, &offset);
            return Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "positional write unsupported"));
        };
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "write zero"));
        }
        offset += n as u64;
        data = &data[n..];
    }
    Ok(())
}

fn probe_content_length(url: &str, headers: &[(&str, &str)]) -> Option<u64> {
    let mut req = download_agent().get(url).header("Range", "bytes=0-0");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = req.call().ok()?;
    if resp.status().as_u16() == 206 {
        if let Some(cr) = resp.headers().get("content-range").and_then(|v| v.to_str().ok()) {
            if let Some(total) = cr.rsplit('/').next().and_then(|s| s.trim().parse::<u64>().ok()) {
                return Some(total);
            }
        }
    }
    if resp.status().as_u16() < 400 {
        return resp
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .filter(|&n| n > 1);
    }
    None
}

/// High-speed multi-connection Range chunked download to a file on disk.
/// Splits large streams into 2 MB chunks downloaded in parallel (up to 8 connections),
/// falling back cleanly to a single stream if Range isn't supported or file is small.
pub fn download_chunked_to(
    url: &str,
    path: &std::path::Path,
    known_len: Option<u64>,
    headers: &[(&str, &str)],
    cancel: &std::sync::atomic::AtomicBool,
    progress: impl Fn(u64, Option<u64>) + Send + Sync,
) -> Result<u64> {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    const CHUNK_SIZE: u64 = 2 * 1024 * 1024;
    const WORKERS: usize = 8;

    let total = known_len.filter(|&n| n > 0).or_else(|| probe_content_length(url, headers));
    if let Some(total) = total.filter(|&n| n >= CHUNK_SIZE) {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        file.set_len(total)?;
        let mut ranges = Vec::new();
        let mut start = 0u64;
        while start < total {
            let end = (start + CHUNK_SIZE - 1).min(total - 1);
            ranges.push((start, end));
            start = end + 1;
        }
        let next_idx = AtomicUsize::new(0);
        let downloaded = AtomicU64::new(0);
        let first_err: Mutex<Option<anyhow::Error>> = Mutex::new(None);
        let n_workers = WORKERS.min(ranges.len());
        progress(0, Some(total));
        std::thread::scope(|scope| {
            for _ in 0..n_workers {
                scope.spawn(|| {
                    let mut buf = vec![0u8; 64 * 1024];
                    loop {
                        if cancel.load(Ordering::Relaxed) || first_err.lock().is_some() {
                            break;
                        }
                        let idx = next_idx.fetch_add(1, Ordering::Relaxed);
                        let Some(&(r_start, r_end)) = ranges.get(idx) else { break };
                        let mut attempt = 0;
                        let chunk_res: Result<()> = loop {
                            if cancel.load(Ordering::Relaxed) {
                                break Err(anyhow!("cancelled"));
                            }
                            let res = (|| -> Result<()> {
                                let mut req = download_agent()
                                    .get(url)
                                    .header("Range", &format!("bytes={r_start}-{r_end}"));
                                for (k, v) in headers {
                                    req = req.header(*k, *v);
                                }
                                let mut resp = req.call()?;
                                let status = resp.status().as_u16();
                                if status >= 400 {
                                    return Err(anyhow!("HTTP {status}"));
                                }
                                if status != 206 {
                                    return Err(anyhow!("range unsupported (HTTP {status})"));
                                }
                                let mut reader = resp.body_mut().with_config().limit(u64::MAX).reader();
                                let mut pos = r_start;
                                let expected = r_end - r_start + 1;
                                let mut got = 0u64;
                                while got < expected {
                                    if cancel.load(Ordering::Relaxed) {
                                        return Err(anyhow!("cancelled"));
                                    }
                                    let max_read = ((expected - got) as usize).min(buf.len());
                                    let n = reader.read(&mut buf[..max_read])?;
                                    if n == 0 {
                                        break;
                                    }
                                    write_all_at(&file, pos, &buf[..n])?;
                                    pos += n as u64;
                                    got += n as u64;
                                    let cur = downloaded.fetch_add(n as u64, Ordering::Relaxed) + n as u64;
                                    progress(cur.min(total), Some(total));
                                }
                                if got < expected {
                                    return Err(anyhow!("incomplete chunk ({got}/{expected})"));
                                }
                                Ok(())
                            })();
                            match res {
                                Ok(()) => break Ok(()),
                                Err(_) if cancel.load(Ordering::Relaxed) => break Err(anyhow!("cancelled")),
                                Err(e) if e.to_string().contains("range unsupported") => break Err(e),
                                Err(_) if attempt < 2 => {
                                    attempt += 1;
                                    std::thread::sleep(Duration::from_millis(150 * attempt as u64));
                                }
                                Err(e) => break Err(e),
                            }
                        };
                        if let Err(e) = chunk_res {
                            let mut err_slot = first_err.lock();
                            if err_slot.is_none() {
                                *err_slot = Some(e);
                            }
                            break;
                        }
                    }
                });
            }
        });
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!("cancelled"));
        }
        if let Some(err) = first_err.into_inner() {
            if !err.to_string().contains("range unsupported") {
                return Err(err);
            }
        } else {
            return Ok(total);
        }
    }

    // Fallback single-connection streaming download (unknown length, < 2 MB, or server rejected Range)
    let mut req = download_agent().get(url);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut resp = req.call()?;
    if resp.status().as_u16() >= 400 {
        return Err(anyhow!("HTTP {}", resp.status()));
    }
    let cl = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .or(total);
    let mut file = std::fs::File::create(path)?;
    let mut reader = resp.body_mut().with_config().limit(u64::MAX).reader();
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    use std::io::Write;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!("cancelled"));
        }
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        done += n as u64;
        progress(done, cl);
    }
    Ok(done)
}

/// Downloads ordered HLS media segments across up to 8 parallel connections and concatenates
/// them sequentially into `out_path`, reporting `(bytes_downloaded, segments_completed)`.
pub fn download_hls_segments_to(
    urls: &[String],
    out_path: &std::path::Path,
    headers: &[(&str, &str)],
    cancel: &std::sync::atomic::AtomicBool,
    progress: impl Fn(u64, usize) + Send + Sync,
) -> Result<u64> {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    if urls.is_empty() {
        return Err(anyhow!("no HLS segments"));
    }
    struct SegDirGuard(std::path::PathBuf);
    impl Drop for SegDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let seg_dir = out_path.with_file_name(format!(
        ".{}.segs",
        out_path.file_name().and_then(|n| n.to_str()).unwrap_or("hls")
    ));
    let _ = std::fs::remove_dir_all(&seg_dir);
    std::fs::create_dir_all(&seg_dir)?;
    let _guard = SegDirGuard(seg_dir.clone());

    let total_segs = urls.len();
    let next = AtomicUsize::new(0);
    let bytes_done = AtomicU64::new(0);
    let segs_done = AtomicUsize::new(0);
    let err: std::sync::Mutex<Option<anyhow::Error>> = std::sync::Mutex::new(None);
    let workers = total_segs.min(8);

    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                if cancel.load(Ordering::Relaxed) || err.lock().unwrap().is_some() {
                    break;
                }
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= total_segs {
                    break;
                }
                let seg_path = seg_dir.join(format!("seg_{i:06}.part"));
                let mut seg_err: Option<anyhow::Error> = None;
                for attempt in 0..3u64 {
                    if cancel.load(Ordering::Relaxed) {
                        seg_err = Some(anyhow!("cancelled"));
                        break;
                    }
                    let mut wrote_attempt = 0u64;
                    let attempt_res = (|| -> Result<()> {
                        let mut req = download_agent().get(&urls[i]);
                        for (k, v) in headers {
                            req = req.header(*k, *v);
                        }
                        let mut resp = req.call()?;
                        if resp.status().as_u16() >= 400 {
                            return Err(anyhow!("HTTP {}", resp.status()));
                        }
                        let file = std::fs::File::create(&seg_path)?;
                        let mut writer = std::io::BufWriter::with_capacity(64 * 1024, file);
                        let mut reader = resp.body_mut().with_config().limit(256 * 1024 * 1024).reader();
                        let mut buf = [0u8; 64 * 1024];
                        loop {
                            if cancel.load(Ordering::Relaxed) {
                                return Err(anyhow!("cancelled"));
                            }
                            let n = reader.read(&mut buf)?;
                            if n == 0 {
                                break;
                            }
                            writer.write_all(&buf[..n])?;
                            wrote_attempt += n as u64;
                            let cur_bytes = bytes_done.fetch_add(n as u64, Ordering::Relaxed) + n as u64;
                            progress(cur_bytes, segs_done.load(Ordering::Relaxed));
                        }
                        writer.flush()?;
                        Ok(())
                    })();
                    match attempt_res {
                        Ok(()) => {
                            let cur_segs = segs_done.fetch_add(1, Ordering::Relaxed) + 1;
                            progress(bytes_done.load(Ordering::Relaxed), cur_segs);
                            seg_err = None;
                            break;
                        }
                        Err(e) => {
                            if wrote_attempt > 0 {
                                bytes_done.fetch_sub(wrote_attempt, Ordering::Relaxed);
                            }
                            let _ = std::fs::remove_file(&seg_path);
                            let cancelled = cancel.load(Ordering::Relaxed) || e.to_string() == "cancelled";
                            seg_err = Some(e);
                            if cancelled || attempt == 2 {
                                break;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(150 * (attempt + 1)));
                        }
                    }
                }
                if let Some(e) = seg_err {
                    let mut g = err.lock().unwrap();
                    if g.is_none() {
                        *g = Some(e);
                    }
                    break;
                }
            });
        }
    });

    if cancel.load(Ordering::Relaxed) {
        return Err(anyhow!("cancelled"));
    }
    if let Some(e) = err.into_inner().unwrap() {
        return Err(e);
    }

    let out_file = std::fs::File::create(out_path)?;
    let mut out_writer = std::io::BufWriter::with_capacity(256 * 1024, out_file);
    let mut total_written = 0u64;
    for i in 0..total_segs {
        if cancel.load(Ordering::Relaxed) {
            drop(out_writer);
            let _ = std::fs::remove_file(out_path);
            return Err(anyhow!("cancelled"));
        }
        let seg_path = seg_dir.join(format!("seg_{i:06}.part"));
        let mut src = std::fs::File::open(&seg_path)?;
        total_written += std::io::copy(&mut src, &mut out_writer)?;
        drop(src);
        let _ = std::fs::remove_file(&seg_path);
    }
    out_writer.flush()?;
    Ok(total_written)
}

pub fn urlencode(s: &str) -> String {
    let mut o = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

/// Decodes %XX escapes and '+' (query strings).
pub fn urldecode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
