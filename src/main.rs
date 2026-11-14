//! shiyin-nd —— 时音引擎的无桌面（headless）命令行版。
//!
//! 复用上游 `kugou_engine` 的签名/加密/会话/传输层，不依赖 Flutter、不依赖任何后端服务，
//! 面向 NAS / 服务器做「搜索 → 解析播放地址 → 下载落盘」这一条链路。
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use serde_json::Value;

use shiyin_nd::services::{playlist, search};

mod download;
mod session;

use download::{
    dest_in, download_to, fmt_duration, resolve_url, track_from_json, Outcome, Track,
};
use session::{default_config_dir, default_music_dir, pick_str_deep, Ctx};

struct Opts {
    config: PathBuf,
    music: PathBuf,
    quality: String,
    dry_run: bool,
    json: bool,
    limit: Option<usize>,
    pick: usize,
    concurrency: usize,
    keep_hash: bool,
    name: Option<String>,
    hash: Option<String>,
    aaid: Option<String>,
    album: Option<String>,
    cookie: Option<String>,
    subdir: bool,
    positional: Vec<String>,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            config: default_config_dir(),
            music: default_music_dir(),
            quality: "flac".into(),
            dry_run: false,
            json: false,
            limit: None,
            pick: 1,
            concurrency: 3,
            keep_hash: false,
            name: None,
            hash: None,
            aaid: None,
            album: None,
            cookie: None,
            subdir: true,
            positional: Vec::new(),
        }
    }
}

fn parse_args(raw: &[String]) -> Result<(String, Opts)> {
    let mut o = Opts::default();
    let mut cmd: Option<String> = None;
    let mut sub: Option<String> = None;
    let mut it = raw.iter().peekable();

    while let Some(arg) = it.next() {
        let (key, inline) = match arg.split_once('=') {
            Some((k, v)) if k.starts_with("--") => (k.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let take = |it: &mut std::iter::Peekable<std::slice::Iter<String>>| -> Result<String> {
            if let Some(v) = inline.clone() {
                return Ok(v);
            }
            it.next()
                .map(|s| s.to_string())
                .ok_or_else(|| anyhow!("参数 {key} 缺少取值"))
        };
        match key.as_str() {
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "-c" | "--config" => o.config = PathBuf::from(take(&mut it)?),
            "-o" | "--out" => o.music = PathBuf::from(take(&mut it)?),
            "-q" | "--quality" => o.quality = take(&mut it)?,
            "-n" | "--num" | "--limit" => o.limit = Some(take(&mut it)?.parse()?),
            "--pick" => o.pick = take(&mut it)?.parse()?,
            "--concurrency" => o.concurrency = take(&mut it)?.parse::<usize>()?.max(1),
            "--hash" => o.hash = Some(take(&mut it)?),
            "--aaid" => o.aaid = Some(take(&mut it)?),
            "--album" => o.album = Some(take(&mut it)?),
            "--name" => o.name = Some(take(&mut it)?),
            "--cookie" => o.cookie = Some(take(&mut it)?),
            "--userid" => {
                let v = take(&mut it)?;
                o.cookie = Some(format!("{};userid={v}", o.cookie.unwrap_or_default()));
            }
            "--token" => {
                let v = take(&mut it)?;
                o.cookie = Some(format!("{};token={v}", o.cookie.unwrap_or_default()));
            }
            "--vip-type" => {
                let v = take(&mut it)?;
                o.cookie = Some(format!("{};vip_type={v}", o.cookie.unwrap_or_default()));
            }
            "--vip-token" => {
                let v = take(&mut it)?;
                o.cookie = Some(format!("{};vip_token={v}", o.cookie.unwrap_or_default()));
            }
            "--t1" => {
                let v = take(&mut it)?;
                o.cookie = Some(format!("{};t1={v}", o.cookie.unwrap_or_default()));
            }
            "--json" => o.json = true,
            "--dry-run" => o.dry_run = true,
            "--keep-hash" => o.keep_hash = true,
            "--no-subdir" => o.subdir = false,
            s if s.starts_with('-') && !s.starts_with("--") => {
                bail!("未知短参数 {s}，用 --help 查看用法")
            }
            s if s.starts_with("--") => bail!("未知参数 {s}，用 --help 查看用法"),
            _ => {
                if cmd.is_none() {
                    cmd = Some(arg.clone());
                } else if sub.is_none() {
                    sub = Some(arg.clone());
                } else {
                    o.positional.push(arg.clone());
                }
            }
        }
    }

    let cmd = cmd.ok_or_else(|| anyhow!("缺少命令，用 --help 查看用法"))?;
    if let Some(s) = sub {
        o.positional.insert(0, s);
    }
    Ok((cmd, o))
}

fn print_help() {
    println!(
        r#"shiyin-nd —— 时音引擎的 headless 版（NAS / 服务器用）

用法: shiyin-nd <命令> [参数]

命令:
  login                     扫码登录（终端显示二维码，最长等 600 秒）
  status                    查看登录态与配置路径
  session set <cookie>      用浏览器 / 客户端 Cookie 直接注入登录态
                            格式: "userid=...; token=...; vip_type=1; vip_token=...; t1=..."
  session clear             清空登录态与设备指纹
  search <关键词>           搜索曲目（默认 20 条）
  url <关键词> | --hash H   只解析播放地址，不下载
  download <关键词>         搜索并下载第 --pick 条（默认第 1 条）
  download --hash H         直接按 hash 下载（可配 --aaid / --album / --name）
  playlist <歌单ID>         整单批量下载（global_collection_id）
  env                       自检：目录、登录态、网络、音质链

通用参数:
  -c, --config <dir>        配置目录（默认 /config，否则 ./config）
  -o, --out <dir>           曲库目录（默认 /music，否则 ./music）
  -q, --quality <档位>      flac（默认）/ 320 / 128，失败自动降级
  -n, --num <N>             搜索条数 / 歌单最多下载条数
      --pick <N>            下载搜索结果里的第 N 条
      --concurrency <N>     歌单并发下载数（默认 3）
      --keep-hash           文件名追加 hash 前 8 位，避免同名冲突
      --no-subdir           歌单下载时不按歌单名建子目录
      --dry-run             只解析地址，不落盘
      --json                以 JSON 输出（便于脚本消费）

示例:
  shiyin-nd login
  shiyin-nd search "周杰伦 晴天" -n 5
  shiyin-nd download "晴天" --pick 1 -q flac
  shiyin-nd playlist 3F2A... --concurrency 4 -o /music/周杰伦
  shiyin-nd session set "userid=12345; token=abc; vip_type=1; vip_token=def; t1=ghi"
"#
    );
}

#[tokio::main]
async fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() {
        print_help();
        return;
    }
    let (cmd, opts) = match parse_args(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("参数错误：{e}");
            std::process::exit(2);
        }
    };
    let code = match run(&cmd, opts).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("错误：{e}");
            1
        }
    };
    std::process::exit(code);
}

async fn run(cmd: &str, opts: Opts) -> Result<()> {
    match cmd {
        "env" => cmd_env(&opts).await,
        "login" => {
            let mut ctx = Ctx::load(opts.config.clone())?;
            session::qr_login(&mut ctx).await
        }
        "status" => cmd_status(&opts),
        "session" => cmd_session(&opts),
        "search" => cmd_search(&opts).await,
        "url" => cmd_url(&opts).await,
        "download" => cmd_download(&opts).await,
        "playlist" => cmd_playlist(&opts).await,
        other => bail!("未知命令 {other}，用 --help 查看用法"),
    }
}

fn cmd_status(opts: &Opts) -> Result<()> {
    let ctx = Ctx::load(opts.config.clone())?;
    println!("配置目录 : {}", opts.config.display());
    println!("曲库目录 : {}", opts.music.display());
    if ctx.logged_in() {
        println!("登录态   : 已登录");
        println!("userid   : {}", ctx.session.userid);
        println!("vip_type : {}", ctx.session.vip_type);
        println!("token    : {}", mask(&ctx.session.token));
        println!("dfid     : {}", mask(&ctx.session.dfid));
    } else {
        println!("登录态   : 未登录（下载需要登录态，否则 /v5/url 返回 20028）");
    }
    Ok(())
}

fn cmd_session(opts: &Opts) -> Result<()> {
    let action = opts
        .positional
        .first()
        .map(|s| s.as_str())
        .unwrap_or("show");
    match action {
        "set" => {
            let mut ctx = Ctx::load(opts.config.clone())?;
            let raw = opts
                .positional
                .get(1)
                .cloned()
                .or_else(|| opts.cookie.clone())
                .ok_or_else(|| {
                    anyhow!("用法：shiyin-nd session set \"userid=...; token=...; vip_type=...\"")
                })?;
            session::apply_cookie(&mut ctx, &raw)?;
            println!("已写入登录态：userid={}  vip_type={}", ctx.session.userid, ctx.session.vip_type);
            Ok(())
        }
        "clear" => {
            let mut ctx = Ctx::load(opts.config.clone())?;
            let p = opts.config.join("kg_session.json");
            if p.exists() {
                std::fs::remove_file(&p).ok();
            }
            ctx.session.logout();
            println!("已清空登录态：{}", p.display());
            Ok(())
        }
        _ => cmd_status(opts),
    }
}

async fn cmd_env(opts: &Opts) -> Result<()> {
    println!("=== 运行时自检 ===");
    cmd_status(opts)?;
    let ctx = Ctx::load(opts.config.clone())?;
    print!("网络/签名链 : ");
    match search::search_raw(&ctx.client, &ctx.session, "晴天", 1, 1, "song").await {
        Ok(v) => {
            let total = v.get("total").and_then(|t| t.as_i64()).unwrap_or(-1);
            println!("正常（搜索 total={total}）");
        }
        Err(e) => println!("失败：{e}"),
    }
    print!("播放地址链 : ");
    let t = Track {
        hash: "B3A52A7A958BF0AED0EBFBA2E9A818B7".into(),
        album_id: "966846".into(),
        album_audio_id: "32100650".into(),
        artist: "周杰伦".into(),
        title: "晴天".into(),
        duration: 269,
        size_hint: 0,
    };
    match resolve_url(&ctx.client, &ctx.session, &t, "128").await {
        Ok(r) => println!("可用（{} → {}", r.quality, short_url(&r.url)),
        Err(e) => println!("当前不可用：{e}"),
    }
    Ok(())
}

async fn cmd_search(opts: &Opts) -> Result<()> {
    let kw = opts
        .positional
        .first()
        .ok_or_else(|| anyhow!("用法：shiyin-nd search <关键词>"))?
        .clone();
    let ctx = Ctx::load(opts.config.clone())?;
    let n = opts.limit.unwrap_or(20) as i64;
    let v = search::search_raw(&ctx.client, &ctx.session, &kw, 1, n, "song").await?;
    if opts.json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let lists = v
        .get("lists")
        .and_then(|l| l.as_array())
        .cloned()
        .unwrap_or_default();
    let total = v.get("total").and_then(|t| t.as_i64()).unwrap_or(-1);
    println!("「{}」共 {total} 条，显示 {}：", kw, lists.len());
    for (i, item) in lists.iter().enumerate() {
        let t = track_from_json(item);
        println!(
            "{:>3}) {}  [{}]  hash={}  aaid={}",
            i + 1,
            t.label(),
            fmt_duration(t.duration),
            if t.hash.is_empty() { "无" } else { &t.hash[..8.min(t.hash.len())] },
            if t.album_audio_id.is_empty() { "无" } else { &t.album_audio_id }
        );
    }
    println!("\n下载第 N 条：shiyin-nd download \"{kw}\" --pick N");
    Ok(())
}

async fn cmd_url(opts: &Opts) -> Result<()> {
    let ctx = Ctx::load(opts.config.clone())?;
    let t = build_track(opts).await?;
    let r = resolve_url(&ctx.client, &ctx.session, &t, &opts.quality).await?;
    println!("{}", r.url);
    eprintln!("（音质 {}，实际请求 {}）", r.quality, opts.quality);
    Ok(())
}

async fn cmd_download(opts: &Opts) -> Result<()> {
    let ctx = Arc::new(Ctx::load(opts.config.clone())?);
    if !ctx.logged_in() {
        eprintln!("提示：当前未登录。酷狗对 /v5/url 有风控（errcode 20028），匿名通常拿不到地址。");
        eprintln!("      先跑 `shiyin-nd login` 扫码，或 `session set` 注入 Cookie。\n");
    }
    let t = build_track(opts).await?;
    let dir = opts.music.clone();
    let label = t.label();
    println!("目标：{label}");

    let r = resolve_url(&ctx.client, &ctx.session, &t, &opts.quality).await?;
    let dest = dest_in(&dir, &t, &r.quality, opts.keep_hash);
    println!("地址：{}（音质 {}）", short_url(&r.url), r.quality);
    if opts.dry_run {
        println!("[dry-run] 将保存到 {}", dest.display());
        return Ok(());
    }
    match download_to(&ctx.client, &r.url, &dest, Some(t.size_hint)).await? {
        Outcome::Skipped => println!("已存在，跳过：{}", dest.display()),
        Outcome::Downloaded(n) => {
            println!("完成：{}（{:.1} MB）", dest.display(), n as f64 / 1048576.0)
        }
    }
    Ok(())
}

async fn cmd_playlist(opts: &Opts) -> Result<()> {
    let id = opts
        .positional
        .first()
        .ok_or_else(|| anyhow!("用法：shiyin-nd playlist <global_collection_id>"))?
        .clone();
    let ctx = Arc::new(Ctx::load(opts.config.clone())?);

    let info = playlist::playlist_info(&ctx.client, &ctx.session, &id)
        .await
        .unwrap_or(Value::Null);
    let name = pick_str_deep(&info, &["name", "list_name", "global_collection_name"]);
    println!(
        "歌单：{}（{}）",
        if name.is_empty() { &id } else { &name },
        id
    );

    let mut tracks: Vec<Track> = Vec::new();
    let mut page = 0i64;
    let pagesize = 100i64;
    loop {
        let v = playlist::playlist_tracks(&ctx.client, &ctx.session, &id, page * pagesize, pagesize)
            .await?;
        let items = extract_list(&v);
        if items.is_empty() {
            break;
        }
        let got = items.len();
        for item in items {
            let t = track_from_json(&item);
            if t.is_valid() {
                tracks.push(t);
            }
        }
        if got < pagesize as usize {
            break;
        }
        page += 1;
        if page > 50 {
            break;
        }
    }
    if let Some(lim) = opts.limit {
        tracks.truncate(lim);
    }
    if tracks.is_empty() {
        bail!("没取到可下载曲目（歌单可能为空或全部下架）");
    }
    println!("可下载 {} 首，音质 {}，并发 {}\n", tracks.len(), opts.quality, opts.concurrency);

    let out_root = if opts.subdir {
        let safe = if name.is_empty() { id.clone() } else { name.clone() };
        opts.music.join(sanitize_dir(&safe))
    } else {
        opts.music.clone()
    };

    let sem = Arc::new(tokio::sync::Semaphore::new(opts.concurrency));
    let mut set = tokio::task::JoinSet::new();
    for (idx, t) in tracks.into_iter().enumerate() {
        let ctx = ctx.clone();
        let sem = sem.clone();
        let dir = out_root.clone();
        let quality = opts.quality.clone();
        let dry = opts.dry_run;
        let keep = opts.keep_hash;
        set.spawn(async move {
            let _permit = sem.acquire().await.ok();
            let label = t.label();
            let r = match resolve_url(&ctx.client, &ctx.session, &t, &quality).await {
                Ok(r) => r,
                Err(e) => return (idx, label, Err(e), None),
            };
            let dest = dest_in(&dir, &t, &r.quality, keep);
            if dry {
                return (idx, label, Ok(dest.display().to_string()), None);
            }
            match download_to(&ctx.client, &r.url, &dest, Some(t.size_hint)).await {
                Ok(Outcome::Skipped) => (idx, label, Ok(dest.display().to_string()), Some(true)),
                Ok(Outcome::Downloaded(_)) => (idx, label, Ok(dest.display().to_string()), Some(false)),
                Err(e) => (idx, label, Err(e), None),
            }
        });
    }

    let mut ok = 0usize;
    let mut skip = 0usize;
    let mut fail = 0usize;
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((idx, label, res, skipped)) => match res {
                Ok(path) => {
                    if let Some(true) = skipped {
                        skip += 1;
                        println!("[{:>3}] 已存在  {label}", idx + 1);
                    } else {
                        ok += 1;
                        println!("[{:>3}] 完成    {label}", idx + 1);
                    }
                    let _ = path;
                }
                Err(e) => {
                    fail += 1;
                    println!("[{:>3}] 失败    {label} :: {e}", idx + 1);
                }
            },
            Err(e) => {
                fail += 1;
                println!("任务异常：{e}");
            }
        }
    }
    println!("\n汇总：成功 {ok} · 跳过 {skip} · 失败 {fail} → {}", out_root.display());
    Ok(())
}

/// 统一从各种响应形态里挖出曲目数组。
fn extract_list(v: &Value) -> Vec<Value> {
    let candidates = [
        vec!["lists"],
        vec!["data", "lists"],
        vec!["data", "list"],
        vec!["data", "songs"],
        vec!["data", "info"],
        vec!["list"],
        vec!["songs"],
    ];
    for path in candidates {
        let mut cur = v;
        let mut ok = true;
        for seg in &path {
            match cur.get(*seg) {
                Some(next) => cur = next,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            if let Some(arr) = cur.as_array() {
                if !arr.is_empty() {
                    return arr.clone();
                }
            }
        }
    }
    Vec::new()
}

async fn build_track(opts: &Opts) -> Result<Track> {
    if let Some(h) = &opts.hash {
        let title = opts.name.clone().unwrap_or_else(|| format!("track-{}", &h[..8.min(h.len())]));
        return Ok(Track {
            hash: h.to_uppercase(),
            album_id: opts.album.clone().unwrap_or_else(|| "0".into()),
            album_audio_id: opts.aaid.clone().unwrap_or_else(|| "0".into()),
            artist: String::new(),
            title,
            duration: 0,
            size_hint: 0,
        });
    }
    let kw = opts
        .positional
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("需要关键词或 --hash"))?;
    let ctx = Ctx::load(opts.config.clone())?;
    let v = search::search_raw(&ctx.client, &ctx.session, &kw, 1, 10, "song").await?;
    let lists = v
        .get("lists")
        .and_then(|l| l.as_array())
        .cloned()
        .unwrap_or_default();
    if lists.is_empty() {
        bail!("搜索「{kw}」无结果");
    }
    let idx = opts.pick.saturating_sub(1);
    let item = lists
        .get(idx)
        .ok_or_else(|| anyhow!("--pick {} 超出结果数（共 {} 条）", opts.pick, lists.len()))?;
    let t = track_from_json(item);
    if !t.is_valid() {
        bail!("「{}」缺少 FileHash，换一条或换关键词", t.label());
    }
    Ok(t)
}

fn short_url(u: &str) -> String {
    if let Ok(parsed) = url::Url::parse(u) {
        let host = parsed.host_str().unwrap_or("?");
        let file = parsed
            .path_segments()
            .and_then(|s| s.last())
            .unwrap_or("");
        return format!("{host}/…/{}", file.chars().take(28).collect::<String>());
    }
    u.chars().take(60).collect()
}

fn mask(s: &str) -> String {
    if s.is_empty() {
        return "(空)".into();
    }
    let n = s.chars().count();
    if n <= 8 {
        return format!("{}***", s.chars().take(2).collect::<String>());
    }
    format!(
        "{}…{}",
        s.chars().take(4).collect::<String>(),
        s.chars().skip(n - 4).collect::<String>()
    )
}

fn sanitize_dir(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let t = cleaned.trim().trim_matches('.').to_string();
    if t.is_empty() {
        "playlist".into()
    } else {
        t.chars().take(80).collect()
    }
}
