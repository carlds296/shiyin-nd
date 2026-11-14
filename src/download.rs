//! 下载引擎：音质降级解析、流式落盘、命名、断点续传。
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use reqwest::Client;
use serde_json::Value;
use tokio::io::AsyncWriteExt;

use shiyin_nd::kugou::session::KgSession;
use shiyin_nd::services::song;

use crate::session::{pick_str, pick_str_deep, truncate};

#[derive(Debug, Clone)]
pub struct Track {
    pub hash: String,
    pub album_id: String,
    pub album_audio_id: String,
    pub artist: String,
    pub title: String,
    pub duration: i64,
    pub size_hint: u64,
}

pub fn track_from_json(v: &Value) -> Track {
    let hash = pick_str(v, &["FileHash", "hash", "hash_flac", "hash_320"]);
    let title = {
        let t = pick_str(v, &["OriSongName", "SongName", "FileName", "songname", "name"]);
        if t.is_empty() { "未知曲目".to_string() } else { t }
    };
    let artist = pick_str(v, &["SingerName", "singername", "singer_name", "author_name"]);
    let album_id = pick_str(v, &["AlbumID", "album_id"]);
    let album_audio_id = pick_str(
        v,
        &[
            "MixSongID",
            "mixsongid",
            "Audioid",
            "audio_id",
            "SongID",
            "album_audio_id",
        ],
    );
    let duration = v
        .get("Duration")
        .and_then(|d| d.as_i64())
        .or_else(|| v.get("timelen").and_then(|d| d.as_i64()))
        .unwrap_or(0);
    let size_hint = v
        .get("FileSize")
        .and_then(|d| d.as_u64())
        .unwrap_or(0);
    Track {
        hash,
        album_id,
        album_audio_id,
        artist,
        title,
        duration,
        size_hint,
    }
}

impl Track {
    /// 用于日志与文件名的简短描述。
    pub fn label(&self) -> String {
        if self.artist.is_empty() {
            self.title.clone()
        } else {
            format!("{} - {}", self.artist, self.title)
        }
    }

    pub fn is_valid(&self) -> bool {
        !self.hash.is_empty()
    }
}

pub fn quality_chain(want: &str) -> Vec<&'static str> {
    match want {
        "flac" => vec!["flac", "320", "128"],
        "320" => vec!["320", "128"],
        _ => vec!["128"],
    }
}

pub fn ext_for_quality(q: &str) -> &'static str {
    if q == "flac" { "flac" } else { "mp3" }
}

pub struct Resolved {
    pub url: String,
    /// 实际拿到的音质（可能低于请求音质）
    pub quality: String,
}

/// 按降级链解析播放地址。风控 20028 直接判定为「需要登录」，不再无意义重试。
pub async fn resolve_url(
    client: &Client,
    session: &KgSession,
    t: &Track,
    want: &str,
) -> Result<Resolved> {
    if t.hash.is_empty() {
        bail!("缺少 FileHash，无法解析播放地址");
    }
    let mut last_reason = String::new();
    for q in quality_chain(want) {
        match song::get_play_url(
            client,
            session,
            &t.hash,
            Some(q),
            Some(&t.album_id),
            Some(&t.album_audio_id),
            false,
        )
        .await
        {
            Ok(v) => {
                if let Some(urls) = v.get("url").and_then(|u| u.as_array()) {
                    if let Some(u) = urls.first().and_then(|x| x.as_str()) {
                        if !u.is_empty() {
                            return Ok(Resolved {
                                url: u.to_string(),
                                quality: q.to_string(),
                            });
                        }
                    }
                }
                let code = pick_str_deep(&v, &["errcode", "error_code", "err_code"]);
                let msg = pick_str_deep(&v, &["error", "msg", "err"]);
                if code == "20028" || code == "20017" {
                    bail!(
                        "需要登录/会员：{}（{}）。先执行 `shiyin-nd login` 或 `session set --cookie`",
                        if msg.is_empty() { "本次请求需要验证".into() } else { msg },
                        code
                    );
                }
                let priv_status = v.get("priv_status").and_then(|p| p.as_i64()).unwrap_or(0);
                last_reason = if priv_status == 10 {
                    format!("音质 {q} 需要购买专辑")
                } else if priv_status == 1 {
                    format!("音质 {q} 需要 VIP")
                } else {
                    format!(
                        "音质 {q} 无返回地址：{}",
                        truncate(&v.to_string(), 160)
                    )
                };
            }
            Err(e) => last_reason = format!("音质 {q} 请求失败：{e}"),
        }
        println!("    · {last_reason}");
    }
    Err(anyhow!("没有可用音质：{last_reason}"))
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Downloaded(u64),
    Skipped,
}

/// 流式下载到 `dest`：先写 `.part`，完成后原子改名；已存在且大小吻合则跳过。
pub async fn download_to(
    client: &Client,
    url: &str,
    dest: &Path,
    expect_bytes: Option<u64>,
) -> Result<Outcome> {
    if let Ok(meta) = std::fs::metadata(dest) {
        let len = meta.len();
        if expect_bytes.map(|e| e == 0 || len == e).unwrap_or(true) && len > 0 {
            return Ok(Outcome::Skipped);
        }
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建目录失败：{}", parent.display()))?;
    }
    let part = dest.with_extension(format!(
        "{}.part",
        dest.extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_else(|| "bin".into())
    ));

    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("请求音频地址失败"))?;
    let status = resp.status();
    if !status.is_success() {
        bail!("音频服务器返回 HTTP {status}");
    }
    let total = resp.content_length().or(expect_bytes);
    let mut resp = resp;
    let mut file = tokio::fs::File::create(&part)
        .await
        .with_context(|| format!("写入失败：{}", part.display()))?;
    let mut written: u64 = 0;
    let mut last_tick: u64 = 0;
    while let Some(chunk) = resp.chunk().await.context("读取音频流中断")? {
        file.write_all(&chunk).await.context("落盘失败")?;
        written += chunk.len() as u64;
        if written - last_tick >= 4 * 1024 * 1024 {
            last_tick = written;
            match total {
                Some(t) if t > 0 => print!(
                    "\r    {:.1}% ({:.1}/{:.1} MB)",
                    written as f64 / t as f64 * 100.0,
                    written as f64 / 1048576.0,
                    t as f64 / 1048576.0
                ),
                _ => print!("\r    {:.1} MB", written as f64 / 1048576.0),
            }
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
    }
    file.flush().await.ok();
    drop(file);
    if written == 0 {
        let _ = tokio::fs::remove_file(&part).await;
        bail!("下载得到 0 字节");
    }
    if written > 2 * 1024 && looks_like_error_page(&part).await {
        let _ = tokio::fs::remove_file(&part).await;
        bail!("返回内容不是音频（疑似被拦截或链接过期）");
    }
    match tokio::fs::rename(&part, dest).await {
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            let _ = tokio::fs::remove_file(dest).await;
            tokio::fs::rename(&part, dest).await?;
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(anyhow!("改名失败：{e}"));
        }
    }
    println!("\r    {:.1} MB 完成", written as f64 / 1048576.0);
    Ok(Outcome::Downloaded(written))
}

async fn looks_like_error_page(path: &Path) -> bool {
    use tokio::io::AsyncReadExt;
    let mut f = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut head = [0u8; 512];
    let n = f.read(&mut head).await.unwrap_or(0);
    if n == 0 {
        return true;
    }
    let sample = String::from_utf8_lossy(&head[..n]).to_lowercase();
    sample.contains("<html") || sample.contains("<!doctype html") || sample.contains("\"errcode\"")
}

/// 生成安全文件名：`歌手 - 歌名.ext`，清理非法字符与超长名。
pub fn safe_file_name(t: &Track, ext: &str) -> String {
    let mut base = if t.artist.trim().is_empty() {
        t.title.clone()
    } else {
        format!("{} - {}", t.artist.trim(), t.title.trim())
    };
    base = sanitize(&base);
    if base.chars().count() > 120 {
        base = base.chars().take(120).collect::<String>().trim().to_string();
    }
    if base.is_empty() {
        base = format!("track-{}", &t.hash[..8.min(t.hash.len())]);
    }
    format!("{base}.{ext}")
}

fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let bad = matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
            || c.is_control();
        if bad {
            out.push('_');
        } else {
            out.push(c);
        }
    }
    while out.contains("  ") {
        out = out.replace("  ", " ");
    }
    out.trim().trim_matches('.').to_string()
}

pub fn dest_in(dir: &Path, t: &Track, quality: &str, keep_hash: bool) -> PathBuf {
    let mut name = safe_file_name(t, ext_for_quality(quality));
    if keep_hash {
        let h = &t.hash[..8.min(t.hash.len())];
        let ext = ext_for_quality(quality);
        name = format!(
            "{}.{}.{}",
            safe_file_name(t, "").trim_end_matches('.').to_string(),
            h,
            ext
        );
        name = name.replace("..", ".");
    }
    dir.join(name)
}

pub fn fmt_duration(secs: i64) -> String {
    if secs <= 0 {
        return "--:--".into();
    }
    format!("{:02}:{:02}", secs / 60, secs % 60)
}
