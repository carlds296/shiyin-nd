//! 会话上下文：配置目录、HTTP 客户端、登录态持久化、扫码登录。
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use reqwest::Client;
use serde_json::Value;

use shiyin_nd::kugou::{session::KgSession, session_store::FileSessionStore};
use shiyin_nd::services::login;

pub struct Ctx {
    pub client: Client,
    pub session: KgSession,
    pub store: FileSessionStore,
    pub config_dir: PathBuf,
}

impl Ctx {
    pub fn load(config_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&config_dir)
            .with_context(|| format!("无法创建配置目录 {}", config_dir.display()))?;
        let store = FileSessionStore::new(config_dir.to_string_lossy().as_ref());
        let mut session = store.load().unwrap_or_default();
        session.normalize();
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(90))
            .user_agent("Mozilla/5.0 (Linux; Android 15) shiyin-nd/0.1")
            .build()
            .context("构建 HTTP 客户端失败")?;
        Ok(Self {
            client,
            session,
            store,
            config_dir,
        })
    }

    pub fn save(&self) -> Result<()> {
        self.store.save(&self.session);
        Ok(())
    }

    pub fn logged_in(&self) -> bool {
        self.session.is_logged_in()
    }

    /// 把登录响应里的凭证写进会话并落盘。
    pub fn apply_login(&mut self, resp: &Value) -> Result<()> {
        let userid = pick_str(resp, &["userid", "user_id"]);
        let token = pick_str(resp, &["token", "token"]);
        if userid.is_empty() || userid == "0" || token.is_empty() {
            bail!("登录响应缺少 userid / token");
        }
        let vip_type = {
            let s = pick_str(resp, &["vip_type", "vipType"]);
            if s.is_empty() { "0".to_string() } else { s }
        };
        let vip_token = pick_str(resp, &["vip_token", "vipToken"]);
        let t1 = pick_str(resp, &["t1"]);
        self.session
            .update_auth(&userid, &token, &vip_type, &vip_token, &t1);
        self.save()
    }
}

/// 取字符串字段，兼容数字 / 字符串两种上游形态。
pub fn pick_str(v: &Value, keys: &[&str]) -> String {
    for k in keys {
        if let Some(x) = v.get(*k) {
            if let Some(s) = x.as_str() {
                if !s.is_empty() {
                    return s.to_string();
                }
            }
            if let Some(n) = x.as_i64() {
                return n.to_string();
            }
            if let Some(n) = x.as_u64() {
                return n.to_string();
            }
        }
    }
    String::new()
}

/// 递归在 data/ 子层里找字段（登录响应有时把凭证包在 data 里）。
pub fn pick_str_deep(v: &Value, keys: &[&str]) -> String {
    let direct = pick_str(v, keys);
    if !direct.is_empty() {
        return direct;
    }
    if let Some(inner) = v.get("data") {
        let s = pick_str(inner, keys);
        if !s.is_empty() {
            return s;
        }
        if let Some(arr) = inner.as_array() {
            for item in arr {
                let s = pick_str(item, keys);
                if !s.is_empty() {
                    return s;
                }
            }
        }
    }
    String::new()
}

/// 扫码登录：拉二维码 → 终端渲染 + 落盘 PNG → 轮询确认。
pub async fn qr_login(ctx: &mut Ctx) -> Result<()> {
    let resp = login::get_qr_key(&ctx.client, &ctx.session)
        .await
        .context("获取二维码失败")?;
    let key = crate::session::pick_str_deep(&resp, &["qrcode", "qrcode_key", "key"]);
    if key.is_empty() {
        bail!("二维码响应缺少 key：{}", truncate(&resp.to_string(), 300));
    }
    let img_b64 = crate::session::pick_str_deep(&resp, &["qrcode_img"]);
    let png_bytes = if img_b64.is_empty() {
        None
    } else {
        let raw = img_b64
            .split_once("base64,")
            .map(|(_, b)| b)
            .unwrap_or(img_b64.as_str());
        base64_decode(raw).ok()
    };

    if let Some(bytes) = png_bytes {
        let p = ctx.config_dir.join("login-qrcode.png");
        std::fs::write(&p, &bytes).ok();
        match render_qr_ansi(&bytes) {
            Ok(art) => {
                println!("{art}");
                println!(
                    "\n用酷狗 App 扫描上方二维码（终端显示不全时可直接打开图片：{}）",
                    p.display()
                );
            }
            Err(e) => println!(
                "二维码图片已保存：{}，请打开后用酷狗 App 扫描（终端渲染跳过：{e}）",
                p.display()
            ),
        }
    }

    println!("等待扫码确认（最长 600 秒）…");
    let deadline = std::time::Instant::now() + Duration::from_secs(600);
    let mut last_tip = String::new();
    while std::time::Instant::now() < deadline {
        let st = login::check_qr_status(&ctx.client, "", &ctx.session, &key).await;
        match st {
            Ok(v) => {
                let token = pick_str_deep(&v, &["token"]);
                let userid = pick_str_deep(&v, &["userid"]);
                if !token.is_empty() && !userid.is_empty() && userid != "0" {
                    ctx.apply_login(&v)?;
                    println!("\n登录成功：userid={userid}  vip_type={}", pick_str_deep(&v, &["vip_type"]));
                    return Ok(());
                }
                let code = pick_str_deep(&v, &["status", "error_code", "err_code"]);
                let tip = format!("code={code} msg={}", pick_str_deep(&v, &["error", "msg"]));
                if tip != last_tip {
                    println!("  {tip}");
                    last_tip = tip;
                }
            }
            Err(e) => println!("  轮询出错（继续）：{e}"),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    bail!("扫码登录超时，请重试")
}

fn base64_decode(s: &str) -> Result<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|e| anyhow!("base64 解码失败: {e}"))
}

/// 把 PNG 二维码渲染成终端字符画。
///
/// 酷狗返回的是 1bit 调色板 PNG（384x384），这里先 EXPAND 展开成 RGB，
/// 再自动探测模块边长（取扫描线上最短同色段），按模块网格采样输出，
/// 避免直接缩放造成的模块糊边。用黑白背景色绘制，保证扫码器可识别。
fn render_qr_ansi(png_bytes: &[u8]) -> Result<String> {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        bail!("输出不是交互终端");
    }
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().context("PNG 解析失败")?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).context("PNG 解码失败")?;
    let w = info.width as usize;
    let h = info.height as usize;
    let ch = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => bail!("调色板未展开"),
    };

    let lum = |x: usize, y: usize| -> u16 {
        let x = x.min(w.saturating_sub(1));
        let y = y.min(h.saturating_sub(1));
        let idx = (y * w + x) * ch;
        if ch >= 3 {
            let r = buf[idx] as u32;
            let g = buf[idx + 1] as u32;
            let b = buf[idx + 2] as u32;
            ((r * 299 + g * 587 + b * 114) / 1000) as u16
        } else {
            buf[idx] as u16
        }
    };
    let dark = |x: usize, y: usize| lum(x, y) < 128;

    // 1) 求非白区域包围盒（二维码真实内容区）
    let (mut min_x, mut max_x, mut min_y, mut max_y) = (w, 0usize, h, 0usize);
    let mut found = false;
    for y in 0..h {
        for x in 0..w {
            if dark(x, y) {
                found = true;
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }
        }
    }
    if !found {
        bail!("二维码图片全白");
    }

    // 2) 取穿过中心的一条扫描线，最短同色段即模块边长
    let scan_y = (min_y + max_y) / 2;
    let mut module = usize::MAX;
    let mut prev = dark(min_x, scan_y);
    let mut run = 1usize;
    for x in (min_x + 1)..=max_x {
        let d = dark(x, scan_y);
        if d == prev {
            run += 1;
        } else {
            module = module.min(run);
            prev = d;
            run = 1;
        }
    }
    module = module.min(run);
    if module == usize::MAX || module < 2 {
        module = ((max_x - min_x + 1) as f64 / 33.0).round().max(2.0) as usize;
    }

    let span_x = max_x - min_x + 1;
    let span_y = max_y - min_y + 1;
    let cols = ((span_x as f64) / module as f64).round().max(1.0) as usize;
    let rows = ((span_y as f64) / module as f64).round().max(1.0) as usize;
    if cols > 200 || rows > 200 {
        bail!("模块探测异常（{cols}x{rows}）");
    }

    // 3) 反相检测：若四角 / 四边中点是暗色，说明这张图是「黑底白模块」，
    //    渲染时整体取反，保证终端里呈现的是标准「白底黑块」。
    let mid_x = min_x + span_x / 2;
    let mid_y = min_y + span_y / 2;
    let probes = [
        lum(min_x, min_y),
        lum(max_x, min_y),
        lum(min_x, max_y),
        lum(max_x, max_y),
        lum(mid_x, min_y),
        lum(mid_x, max_y),
        lum(min_x, mid_y),
        lum(max_x, mid_y),
    ];
    let dark_probes = probes.iter().filter(|&&l| l < 128).count();
    let inverted = dark_probes >= 5;

    // 4) 按模块中心采样，黑白背景绘制（每模块 2 字符宽，颜色变化才写转义）
    let mut out = String::with_capacity(rows * cols * 4);
    out.push('\n');
    for r in 0..rows {
        let sy = min_y + r * module + module / 2;
        let mut cur: Option<bool> = None;
        for c in 0..cols {
            let sx = min_x + c * module + module / 2;
            let mut d = dark(sx, sy);
            if inverted {
                d = !d;
            }
            if cur != Some(d) {
                out.push_str(if d { "\x1b[40m" } else { "\x1b[47m" });
                cur = Some(d);
            }
            out.push_str("  ");
        }
        out.push_str("\x1b[0m\n");
    }
    Ok(out)
}

pub fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// 解析用户在 shell 里粘贴的其它客户端配置文本（key=value 或 cookie 串）。
pub fn apply_cookie(ctx: &mut Ctx, cookie: &str) -> Result<()> {
    let mut userid = String::new();
    let mut token = String::new();
    let mut vip_type = "0".to_string();
    let mut vip_token = String::new();
    let mut t1 = String::new();
    let mut dfid = String::new();

    for part in cookie.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (k, v) = match part.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        let k = k.trim();
        let v = v.trim();
        match k {
            "userid" | "user_id" => userid = v.to_string(),
            "token" => token = v.to_string(),
            "vip_type" => vip_type = v.to_string(),
            "vip_token" => vip_token = v.to_string(),
            "t1" => t1 = v.to_string(),
            "dfid" => dfid = v.to_string(),
            _ => {}
        }
    }

    if userid.is_empty() || token.is_empty() {
        bail!("cookie 里没解析出 userid / token，请检查格式：userid=...; token=...; vip_type=...; vip_token=...");
    }
    if !dfid.is_empty() && dfid != "-" {
        ctx.session.dfid = dfid;
    }
    ctx.session
        .update_auth(&userid, &token, &vip_type, &vip_token, &t1);
    ctx.save()?;
    Ok(())
}

pub fn default_config_dir() -> PathBuf {
    if let Ok(p) = std::env::var("SHIYIN_CONFIG_DIR") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    let container = Path::new("/config");
    if container.exists() {
        return container.to_path_buf();
    }
    PathBuf::from("config")
}

pub fn default_music_dir() -> PathBuf {
    if let Ok(p) = std::env::var("SHIYIN_MUSIC_DIR") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    let container = Path::new("/music");
    if container.exists() {
        return container.to_path_buf();
    }
    PathBuf::from("music")
}
