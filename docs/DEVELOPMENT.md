# 开发记录 · 这个项目是怎么做出来的

> 一句话：**全部由豆包（Doubao）完成** —— 需求由人提出，读代码、拆模块、写实现、编译、部署、验证、写文档由豆包执行。

---

## 1. 起因与可行性判断

最初的诉求是「上游 `bamboostrip/shiyin-music` 能不能装在家用 NAS 上」。结论是不能：

- 上游只有桌面 GUI 形态，Linux 产物仅 x86_64；
- 运行时依赖 GTK3 + libmpv + libayatana-appindicator3 + libnotify4 + ALSA + D-Bus；
- 没有 Docker 镜像、没有服务端模式、没有 ARM 构建。

被逐一排除的方案：NAS 直接跑 GUI、Docker + Xvfb 远程桌面、VMM 装 Ubuntu 桌面、自行交叉编译 ARM64。

真正的突破口是架构本身：Flutter UI → Rust 引擎 FFI → **引擎直接连酷狗**，本来就没有后端服务器。所以只要把引擎单独拿出来，就不需要「去掉服务器」这件事——服务器从来不存在，需要去掉的只是 GUI。

## 2. 模块剖析

- `rust/` 是一个独立 crate `kugou_engine`，`crate-type = ["cdylib", "staticlib"]`，与 Flutter 通过 flutter_rust_bridge 交互，但自身不依赖 Flutter。
- `rust/src/kugou/` 十个模块承载全部协议逻辑：签名（`SignatureType::Default / V5 / Web / OfficialAndroid` 四套 salt）、MD5/AES/RSA 加解密、会话与设备指纹、HTTP 传输。
- `rust/src/services/` 里 search / song / playlist / login / lyric 只依赖 `crate::error` + `crate::kugou`，天然自包含。

关键接口：

| 用途 | 接口 | 签名类型 |
|---|---|---|
| 搜索 | `GET /v3/search/song`（complexsearch.kugou.com） | Default |
| 播放地址 | `GET /v5/url`（trackercdn.kugou.com） | V5 |
| 歌单信息 | `POST /v3/get_list_info`（pubsongs.kugou.com） | — |
| 歌单曲目 | `GET /pubsongs/v2/get_other_list_file_nofilt` | — |
| 扫码登录 | `GET /v2/qrcode`（login-user.kugou.com，appid 1001） | — |
| 轮询扫码 | `GET /v2/get_userinfo_qrcode` | — |
| 刷新 token | `POST /v5/login_by_token` | — |

播放地址参数：`hash` / `quality` / `album_id` / `album_audio_id` / `IsFreePart`，音质取值 `128` / `320` / `flac`，`album_audio_id` 对应搜索结果的 `MixSongID`（回退 `Audioid` / `SongID`）。

**最关键的一个卡点**：匿名调 `/v5/url` 返回

```json
{"errcode":20028,"error":"本次请求需要验证","status":0}
```

上游 `is_session_invalid_response` 把 `20000..20100` 判为登录态失效 —— 匿名拿不到地址，**必须登录**。这一条决定了整个工具必须有登录环节。

## 3. 组装独立工程

新建工程目录，只拷必要文件：

```
src/kugou/*.rs        十个模块
src/error.rs
src/services/{search,song,playlist,lyric,login}.rs
src/lib.rs            自写，只 mod 声明 + pub use
src/services/mod.rs   自写
Cargo.toml            去掉 flutter_rust_bridge，只留协议所需的库
```

第一次 `cargo build --release` 耗时 2 分 14 秒、零错误 —— 70 KB 引擎代码脱离 Flutter 和 FRB 可以独立编译。这是整个方案成立的实证。

随后写探针验证：匿名搜索「周杰伦」total=480；`get_qr_key` 返回 qrcode/status；`/v5/url` 三档 quality 全部 20028（印证必须登录）。

## 4. CLI 设计

手写参数解析（不引入 clap，减少依赖、控制体积），命令集：

`login` / `status` / `session set|clear` / `search` / `url` / `download` / `playlist` / `env`

要点：

- 歌单下载走 `playlist_info` + `playlist_tracks` 分页（100 条/页，最多 50 页），`Semaphore` + `JoinSet` 控制并发，默认建子目录；
- 下载流式写 `.part` → rename，跳过已存在，4 MB 打一次进度；
- 对返回内容做 HTML / errcode 检测，避免把错误页存成音频；
- 文件名清理非法字符并截断到 120 字符。

## 5. 踩过的坑

### 5.1 静态编译段错误

musl 静态版最初连 `--help` 都段错误（exit 139）。根因是 `.cargo/config.toml` 把 linker 覆盖成了 Ubuntu 的 `musl-gcc`，与 Rust 内置 musl std 的链接方式不匹配。

**解决**：删掉 `.cargo/config.toml`（不覆盖 linker，用 rust-lld），只保留 `CC_x86_64_unknown_linux_musl=musl-gcc`。重编得到 static-pie、2.7 MB，`ldd` 显示 statically linked。

### 5.2 二维码画不出来

酷狗返回的是 1-bit 调色板 PNG（384×384 / 423×423），而且**黑底白模块**（反相）。

解决思路：`set_transformations(EXPAND)` → 求非白包围盒 → 取中心扫描线上最短同色段作为 module 边长 → 用四角 + 四边中点亮度探测是否反相（≥5 个暗点判定 inverted，整体取反）→ 按模块网格中心采样 → 在黑白背景上绘制；非 tty 环境直接跳过。

### 5.3 密码登录不可行

`services/login.rs` 只有短信验证码、扫码、token 刷新三种。酷狗 App 的账号密码登录需要 RSA 加密 `p` 字段 + 设备指纹 + 图片验证码，自动化不现实，**改走扫码**。等待窗口从 120 秒放宽到 600 秒重编译。

### 5.4 「FLAC 必须 VIP」的判断是错的

最初以为要 VIP 才能拿无损。实测账号 `vip_type=0`、`vip_token` 为空，仍然成功解析并下载：

| 曲目 | 大小 | 校验 |
|---|---|---|
| 周杰伦 - 晴天 | 30.2 MB | 11895828 samples / 16 bit / stereo / 44.1 kHz |
| 周杰伦 - 青花瓷 | 25.0 MB | 10555268 samples / 16 bit / stereo / 44.1 kHz |
| 于秋璇 - 高山流水 | 25.2 MB | 16 bit / stereo / 44.1 kHz |

据此修正：酷狗对相当一部分曲目开放无损，受限的是新歌/独家/付费专辑，会走 lossless→high→standard 降级链（只换 quality，不换 hash）。

### 5.5 部署环节的实操坑

- **SFTP 被 chroot**：部分 NAS 的 SFTP 根目录列表有内容，但实际卷路径（如 `/volumeX`、`/home`）全部 `FileNotFoundError`，无法直接 put。改成「让目标机自己 `curl` 下载安装包」+「base64 单向写小文件」。
- **`pkill -f` 会自杀**：`pkill -f "shiyin-nd login"` 把执行命令的 shell 一起杀掉（exit 143）。改成 `ps -eo pid,comm | awk '$2=="shiyin-nd"{print $1}' | xargs -r kill`。
- **SSH 后台任务挂住**：在非交互 session 里 `nohup ... &` 会让 channel 挂到超时，改成先起后台、再单独查询。
- **容器以非 root 运行**：`web.py` 报 `[Errno 13] Permission denied`，Dockerfile 里补 `chmod 755 /app /app/web.py`。

## 6. 网页层

0.2.0：单文件 `web.py`，纯 Python 标准库 `ThreadingHTTPServer`。前端深色简洁风，青绿点缀。接口把 CLI 的 `--json` 输出解析成结构化结果，进度用正则从 stdout 抓百分比与体积。

0.3.0：加账号体系。

- `PBKDF2-HMAC-SHA256`（120000 轮）+ 每账号独立盐，存 `config/web_auth.json`（chmod 600）；
- 会话表在内存，`sy_session` Cookie（HttpOnly / SameSite=Lax，7 天）；
- 未登录只能看到登录页，除 `/api/auth`、`/api/login` 外全部 401；
- 登录后可自助改用户名/密码（当前密码校验，用户名 ≤32 字符、新密码 ≥4 位）；
- 去掉了早期的 `WEB_TOKEN` 参数旁路，不留后门。

验证方式：接口逐项断言（未登录 401、错误密码 401、正确登录 200、改密后旧凭据失效新凭据可用、再改回），再用真实浏览器走一遍登录→主界面→账户面板→搜索。

## 7. 用到的东西（技术栈与依赖）

**语言与工具链**

| 项目 | 用途 |
|---|---|
| Rust 1.80+ / cargo | 引擎与 CLI |
| `x86_64-unknown-linux-musl` target + `musl-gcc` | 静态编译 |
| Python 3（仅标准库） | 网页服务，无 pip 依赖 |
| Docker / docker compose | 打包与部署 |
| alpine:3.20 | 运行时基础镜像（约 5 MB） |
| rust:1-bookworm | 构建阶段镜像 |

**Rust 依赖**（详见 `Cargo.toml`）：tokio、reqwest（rustls）、rustls、serde/serde_json、md-5、digest、hex、flate2、base64、rand、aes、cipher、rsa、num-bigint、chrono、uuid、thiserror、tracing、url、png。

**外部服务**：酷狗音乐公开接口（见第 2 节表格），仅走 HTTPS，无中转服务器。

**AI 参与**：全部代码、Docker 配置、部署脚本、文档由**豆包（Doubao）** 生成与会话式迭代。人的角色是提出需求、确认方案、提供测试账号、扫码授权、以及在自己的 NAS 上点确认。

## 8. 时间线

1. 判断上游不能直接装在 NAS；
2. 识别「核心①」= 引擎协议层，本身无服务器；
3. 组装独立 crate，一次编译通过；
4. 探针确认必须登录（20028）；
5. 写 CLI，解决静态链接、二维码反相；
6. 扫码登录，实测三首真无损，修正 VIP 判断，发布 0.1.1；
7. 打包 Docker，部署到家用 NAS，实测落盘；
8. 加网页界面 0.2.0，曲库迁移，网页内实测下载 25 MB FLAC；
9. 加账号体系 0.3.0，接口 + 浏览器双重验证；
10. 脱敏开源（本仓库）。
