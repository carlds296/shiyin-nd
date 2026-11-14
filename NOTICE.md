# NOTICE · 来源与许可说明

## 1. 上游代码

本仓库 `src/kugou/` 目录下的以下文件来自开源仓库：

- 上游：<https://github.com/bamboostrip/shiyin-music>
- 路径：上游 `rust/src/kugou/`
- 文件：`api_response.rs`、`config.rs`、`crypto.rs`、`models.rs`、`mod.rs`、`request.rs`、`session.rs`、`session_store.rs`、`signer.rs`、`transport.rs`

这些文件为本仓库**原样移植**（未修改算法与常量），作用是承载酷狗接口的签名、加密、设备指纹、会话持久化与 HTTP 传输。

`src/services/` 下的 `search.rs`、`song.rs`、`playlist.rs`、`lyric.rs`、`login.rs` 同样来自上游同名模块，本仓库仅调整了 `mod.rs` 的引用关系使之脱离 Flutter/FRB 编译。

## 2. 许可状态

- 上游仓库根目录**没有 LICENSE / COPYING / NOTICE 文件**（截至本项目开发时核实过仓库根目录清单）。
- 上游 README 中自述：「仅供学习交流，请勿商业用途」。
- 因此：**上游代码不附带任何明示的开源许可**，其著作权归原作者所有。本仓库不对这部分代码主张任何权利，也不对其授予任何许可。
- 本仓库新增部分（`src/main.rs`、`src/session.rs`、`src/download.rs`、`src/lib.rs`、`src/error.rs`、`src/services/mod.rs`、`web.py`、Docker 相关文件、全部文档）采用 MIT 许可。

## 3. 用途声明

本仓库是个人技术研究与自用存档：

- 不用于商业用途；
- 不提供任何在线服务、不托管任何音频内容；
- 不包含任何真实账号、Cookie、Token、密码、内网 IP、主机名等个人信息（`config/` 已被 `.gitignore` 排除）；
- 仓库中出现的接口地址、APP_ID、salt 等常量，均为上游公开实现中已有的内容，不属于私有密钥。

## 4. 权利人异议处理

如果原作者或任何权利人认为本仓库的某部分内容不宜公开，请开 issue 说明，维护者会立即移除对应内容或整个仓库。

## 5. 第三方依赖

本项目在构建期使用的开源库列在 `Cargo.toml`（Rust 侧）与 `web.py` 头部注释（Python 侧仅用标准库）。它们各自遵循自己的许可。
