# shiyin-nd · 酷狗音乐下载器（无服务器 / NAS 版）

把 [bamboostrip/shiyin-music](https://github.com/bamboostrip/shiyin-music) 的**核心①**剥离出来做成的命令行 + 网页工具：

**原本依赖远程服务器的酷狗接口逻辑（签名、加密、会话、传输）已经用 Rust 重写并内嵌进二进制，运行时不需要任何后端服务器、不需要 GUI、不需要浏览器。**

> **本项目由豆包（Doubao）构建。**
> 从读上游源码、拆模块、写 CLI、编译打包、写网页界面、写文档，到最后的部署与验证，全部由豆包（Doubao AI 助手）完成，人只负责提需求、提供账号和点「确认」。技术栈与完整开发过程见 [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md)。

- 单文件静态二进制（x86_64，musl，约 2.7 MB，无 glibc 依赖）
- 一个 Alpine 容器即可跑，镜像约 55 MB（含 Python 3 跑网页）
- 支持扫码登录，支持无损 FLAC 与自动降级
- 支持单曲 / 关键词 / 整张歌单批量下载，多任务并发
- 自带网页界面：搜索、下载、进度、曲库浏览，免 SSH
- 网页带账号密码登录，可自助改账号

> 上游项目自述「仅供学习交流，请勿商业用途」。本仓库是同一份引擎代码的个人使用形态，**请勿传播、勿商用**，自行遵守酷狗音乐用户协议与当地法律。

---

## 1. 和上游的关系

| | 上游 shiyin-music | 本仓库 shiyin-nd |
|---|---|---|
| 形态 | Flutter 桌面 GUI | 命令行 + 网页（可容器化） |
| 依赖 | GTK3 + libmpv + appindicator + libnotify + ALSA + D-Bus | 无（静态二进制；网页版只多一个 python3） |
| 架构 | 仅 x86_64（Linux/Windows/macOS/Android） | x86_64（Linux，musl 静态） |
| 后端服务器 | 不需要（引擎本来就是本地 FFI） | 不需要 |
| 用途 | 交互式听歌、管理歌单 | 往 NAS 曲库批量落盘 |
| 引擎代码 | `rust/src/kugou/*` | 原样移植，未改动（见 [NOTICE.md](NOTICE.md)） |

引擎部分（`src/kugou/` 下的 api_response / config / crypto / models / request / session / session_store / signer / transport 十个模块）来自上游，签名算法（Default / V5 / Web / OfficialAndroid 四套 salt）、加解密、会话持久化全部保持原样；本仓库做的是**把调用入口从 Dart FFI 换成 Rust CLI**，再补上搜索、下载、歌单、网页这几层。

技术上能这么干的前提是：上游 `rust/` 本来就是一个独立的 `kugou_engine` crate（`crate-type = ["cdylib", "staticlib"]`），与 Flutter 完全解耦；`services/` 里的 search/song/playlist/login/lyric 只依赖 `crate::error` 和 `crate::kugou`，自包含，可以直接当库用。

---

## 2. 架构：为什么不需要服务器

```
┌────────────────────────────┐        ┌──────────────────────┐
│  shiyin-nd (Rust 静态二进制) │        │  酷狗开放接口         │
│                            │        │  complexsearch        │
│  src/kugou/  签名·加密·会话 │──────▶│  trackercdn (/v5/url) │
│  src/services/ 搜索·歌曲    │  HTTPS │  pubsongs (歌单)      │
│  src/session.rs 登录态      │        │  login-user (扫码)    │
│  src/download.rs 流式落盘    │        └──────────────────────┘
└───────────▲────────────────┘
            │ 子进程调用（localhost）
┌───────────┴────────────────┐
│  web.py (Python 标准库)     │◀──── 浏览器 http://<host>:8090
│  登录 · 搜索 · 下载 · 曲库   │
└────────────────────────────┘
```

- 签名、加密、设备指纹、会话全部在本地算，**没有任何中转服务器**；
- 网页服务只是给二进制套一层 HTTP 界面，和 CLI 共用同一份会话文件；
- 二进制是 static-pie、stripped，拷到任何 x86_64 Linux 上直接跑。

---

## 3. 版本

| 版本 | 变化 |
|---|---|
| 0.1.0 | 从上游剥出引擎，独立 crate 编译通过，CLI 骨架（login/status/search/url/download） |
| 0.1.1 | 静态 musl 编译修复、二维码终端渲染修复、歌单并发下载、实测无损落盘 |
| 0.2.0 | 新增网页界面 `web.py`：搜索、下载任务进度、曲库浏览、网页扫码登录；曲库目录可配置 |
| 0.3.0 | 网页加账号密码登录（初始 `admin`/`admin`）、HttpOnly 会话 Cookie、账户自助修改 |

版本号规则：`X.Y.Z`，X 只在推倒重来时进位，Y 在界面变化时进位并重置 Z，Z 每次改动都进位。

---

## 4. 快速开始

### 4.1 Docker（推荐）

```bash
git clone <this-repo> shiyin-nd
cd shiyin-nd

# 改 docker-compose.yml：把 /path/to/your/music 换成你的音乐目录
docker compose up -d --build
```

打开 `http://<你的主机IP>:8090`，用初始账号 `admin` / `admin` 登录，**进去第一件事是点右上角「账户」把用户名和密码改掉**。

只用命令行也行：

```bash
docker compose run --rm cli search "晴天" -n 10
docker compose run --rm cli download "晴天" --pick 1 -q flac -o /music
```

### 4.2 本地编译（不用 Docker）

需要 Rust 1.80+：

```bash
cargo build --release
# 静态版（推荐，可跨 glibc 版本搬来搬去）
rustup target add x86_64-unknown-linux-musl
CC_x86_64_unknown_linux_musl=musl-gcc cargo build --release --target x86_64-unknown-linux-musl
```

产物在 `target/release/shiyin-nd`（静态版在 `target/x86_64-unknown-linux-musl/release/shiyin-nd`）。

> **不要**用 `.cargo/config.toml` 把 linker 覆盖成系统的 `musl-gcc`。Rust 内置 musl std 自带的链接方式（rust-lld）才匹配，覆盖后产物启动即段错误。这个坑详见 [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md#51-静态编译段错误)。

---

## 5. 第一次使用：登录

酷狗匿名调 `/v5/url` 会返回：

```json
{"errcode":20028,"error":"本次请求需要验证","status":0}
```

20000~20100 这一段是「登录态失效」，所以**必须先登录**才能拿到播放地址。

本项目走扫码登录（上游 `services/login.rs` 只提供短信验证码、扫码、token 刷新三种，没有密码登录；App 的密码登录要 RSA 加密 `p` 字段 + 设备指纹 + 图片验证码，自动化不现实）。

```bash
./shiyin-nd login          # 终端里会画出二维码，手机酷狗 App 扫码
./shiyin-nd status         # 看 userid / vip_type / token 前缀
```

网页版界面里点「扫码登录」效果一样，二维码直接显示在页面上。

**关于「无损是否必须 VIP」**：实测并非如此。自己的账号 `vip_type=0`（无会员标识），仍能解析并下载相当一部分曲目的真无损（16 bit / stereo / 44.1 kHz 的真 FLAC）。受限的是新歌、独家、付费专辑，这类会按 lossless → high → standard 降级。

---

## 6. 网页界面

| 区域 | 功能 |
|---|---|
| 顶部状态 | 酷狗登录态、userid、VIP 标识；未登录点「扫码登录」出二维码 |
| 右上角 | 当前账号、「账户」（改用户名/密码）、「退出」 |
| 搜索 | 歌名/歌手 + 条数 + 音质（FLAC / 320 / 128） |
| 结果表 | 封面、歌名、专辑、时长；曲库里已有的标灰并禁用下载 |
| 下载任务 | 实时进度、已下载体积、完成文件名；最多 3 个并发 |
| 曲库 | 列出音乐目录下的文件与子目录、总量统计 |

接口（全部需要登录，除 `/api/auth` 和 `/api/login`）：

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/` | 未登录返回登录页，已登录返回主页面 |
| GET | `/api/auth` | 当前登录用户名 |
| POST | `/api/login` | `{username, password}`，成功下发 `sy_session` Cookie |
| POST | `/api/logout` | 退出并清 Cookie |
| POST | `/api/account` | `{username, new_password, current_password}` 改账号 |
| GET | `/api/status` | 酷狗登录态 |
| GET | `/api/search?q=&n=` | 搜索，返回 `items[]` |
| POST | `/api/download` | `{item, quality}` 起一个下载任务 |
| GET | `/api/tasks` | 任务列表与进度 |
| GET | `/api/library` | 曲库列表 |
| GET | `/api/login/qr` | 未登录时返回二维码 base64 |

账号存在 `config/web_auth.json`：PBKDF2-HMAC-SHA256（12 万轮）+ 每账号独立盐，权限 600，不存明文。忘了密码就把这个文件移走再重启容器，会自动重新生成 `admin`/`admin`。会话在内存中保持 7 天，**容器重启后需要重新登录一次**（账号密码不会丢）。

---

## 7. 命令行

```bash
shiyin-nd login                                    # 扫码登录
shiyin-nd status                                   # 登录态
shiyin-nd session set --cookie "..."               # 直接注入 Cookie
shiyin-nd search "晴天" -n 10                      # 搜索
shiyin-nd url --hash <HASH> --aaid <MIXSONGID>     # 只解析播放地址
shiyin-nd download "晴天" --pick 1 -q flac -o /music
shiyin-nd download --hash <HASH> --aaid <ID> --name "歌名" -q flac -o /music
shiyin-nd playlist <歌单ID> -q flac --concurrency 4 -o /music/<歌单名>
shiyin-nd env                                      # 打印当前配置路径
```

常用参数：

| 参数 | 说明 |
|---|---|
| `-q, --quality` | `flac` / `320` / `128`，默认 flac，取不到会降级 |
| `-n, --num` | 搜索条数 |
| `--pick` | 搜索结果里取第几条（1 开始） |
| `--hash` `--aaid` `--album` `--name` | 精确指定一首歌（aaid 即搜索结果的 `MixSongID`） |
| `-o, --out` | 输出目录 |
| `--concurrency` | 歌单并发数 |
| `--no-subdir` | 歌单不建子目录 |
| `--keep-hash` | 文件名保留 hash 后缀（重名时用） |
| `--json` | 机读输出（网页版就靠它） |
| `--dry-run` | 只打印不下载 |

下载行为：先写 `.part` 再 rename（避免半截文件进媒体库）；已存在则跳过；每 4 MB 打一行进度；对返回 HTML 或 errcode 的内容做检测，避免把错误页当音频存下来；文件名清掉非法字符并截断到 120 字符。

---

## 8. 目录与配置

```
shiyin-nd/
├── Cargo.toml / Cargo.lock
├── src/
│   ├── main.rs          # CLI 入口与参数解析（手写，不依赖 clap）
│   ├── session.rs       # 登录、二维码终端渲染、配置读写
│   ├── download.rs      # 流式下载、进度、落盘
│   ├── kugou/           # 引擎（来自上游，见 NOTICE）
│   ├── services/        # search / song / playlist / lyric / login
│   └── error.rs
├── web.py               # 网页服务（Python 标准库，无第三方依赖）
├── Dockerfile           # 网页版镜像（多阶段）
├── Dockerfile.src       # 只构建 CLI 的镜像
├── docker-compose.yml
├── config/              # 运行时生成：登录态、网页账号
└── docs/
```

环境变量：

| 变量 | 默认 | 作用 |
|---|---|---|
| `SHIYIN_CONFIG_DIR` | `~/.shiyin-nd` | 会话与配置目录 |
| `SHIYIN_MUSIC_DIR` | 当前目录 | 默认输出目录 |
| `SHIYIN_BIN` | `/usr/local/bin/shiyin-nd` | 网页服务调用的二进制 |
| `WEB_PORT` | `8090` | 网页端口 |

---

## 9. 常见问题

**Q：匿名调用直接报 20028？**
必须先登录，见第 5 节。

**Q：下载下来是 128k 而不是无损？**
这首没有开放无损（新歌/独家/付费），程序已按 lossless→high→standard 降级。`file` 命令或 `ffprobe` 一看就知道实际是什么。

**Q：怎么确认是真无损？**
```bash
file "周杰伦 - 晴天.flac"
# FLAC audio bitstream data, 16 bit, stereo, 44.1 kHz, 11895828 samples
```
或 `ffprobe -show_streams` 看 `bits_per_raw_sample` / `sample_rate`。文件大小也能反推：44.1kHz/16bit/stereo 约 10 MB/分钟。

**Q：容器写出来的文件在 NAS 上删不掉 / 媒体库扫不到？**
容器默认用 root 写，属主不对。compose 里加 `user: "你的UID:你的GID"`。

**Q：网页登录后过一阵又要登录？**
会话存在内存里，容器重启或重建就清空。账号密码不受影响。

**Q：NAS 上装不了上游 GUI？**
上游 Linux 产物只有 x86_64，且依赖 GTK3 + libmpv + appindicator + libnotify + ALSA + D-Bus，无 Docker 镜像、无服务端模式、无 ARM 构建。这正是本仓库存在的理由。

---

## 10. 合规与免责

- 本项目**仅供个人学习与自用**，请勿传播、请勿商用。
- 酷狗接口、签名常量、协议参数来自上游开源仓库的公开实现；本项目不包含任何私有密钥。
- 下载内容请自行确保有权获取，遵守酷狗音乐用户协议与所在地法律。
- 仓库内**不含任何真实账号、Cookie、Token、密码、内网地址**，`config/` 已在 `.gitignore` 里排除。

## 11. 许可

本仓库新增代码（CLI、下载逻辑、网页服务、文档）采用 MIT。
引擎模块 `src/kugou/` 来自上游仓库，上游未附许可证声明，详见 [NOTICE.md](NOTICE.md)。
