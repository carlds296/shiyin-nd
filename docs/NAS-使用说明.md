# NAS 部署说明（通用版）

> 本文给出一个已经验证过的部署路径：Docker + 单容器 + 静态二进制 + 网页界面。
> 文中所有路径、IP、账号都用占位符表示，替换成你自己的即可。

---

## 一、前置条件

| 项 | 要求 |
|---|---|
| 架构 | x86_64（Intel / AMD）。ARM 机型不支持（没有 ARM 构建） |
| 系统 | 任意能跑 Docker 的 Linux 发行版（已验证 Debian 12 + Docker 24 + compose v2） |
| 权限 | 能执行 `docker`（多数 NAS 需要 sudo） |
| 端口 | 一个空闲 TCP 端口，本文用 `8090` |
| 磁盘 | 按曲库大小留，无损约 10 MB/分钟 |

检查端口是否被占用：

```bash
ss -ltnp 2>/dev/null | grep ':8090' || echo "8090 空闲"
```

## 二、部署步骤

### 2.1 取代码

```bash
cd /path/to/docker           # 换成你的应用目录
git clone <this-repo> shiyin-nd
cd shiyin-nd
```

### 2.2 改 compose

```yaml
volumes:
  - /path/to/music:/music    # 改这里：你的音乐目录
  - ./config:/config
```

如果容器以非 root 运行，加上你的 UID/GID，避免写出来的文件属主不对：

```yaml
user: "1000:10"              # 你的 UID:GID
```

查自己的 UID/GID：`id`

### 2.3 构建并启动

```bash
docker compose up -d --build
docker compose ps
docker compose logs --tail 20 shiyin-web
```

日志里出现下面这行就说明起来了：

```
shiyin-web 0.3.0 on 0.0.0.0:8090  music=/music  user=admin
```

### 2.4 首次登录网页

浏览器打开 `http://<NAS_IP>:8090`，用 `admin` / `admin` 登录，然后：

1. 点右上角「账户」；
2. 填新用户名 + 新密码 + 确认密码 + 当前密码（`admin`）；
3. 保存，右上角用户名立刻变成新的。

**别把 admin/admin 留着。**

### 2.5 登录酷狗

网页顶部点「扫码登录」，用手机酷狗 App 扫页面上的二维码。扫码成功后状态变成「酷狗已登录」，并显示 userid 与 VIP 标识。

命令行方式等价：

```bash
docker compose run --rm cli login     # 终端里画出二维码
docker compose run --rm cli status
```

登录态写在 `config/kg_session.json`，容器重建不会丢。

### 2.6 试下载

网页搜索框输入歌名，选「无损 FLAC」，点对应行的「下载」，任务区会实时走进度。落盘后到 `曲库` 区确认文件在。

命令行：

```bash
docker compose run --rm cli download "晴天" --pick 1 -q flac -o /music
```

校验是不是真无损：

```bash
file "/path/to/music/歌名.flac"
# FLAC audio stream data, 16 bit, stereo, 44100 Hz
```

---

## 三、日常运维

```bash
cd /path/to/shiyin-nd

docker compose ps                          # 状态
docker compose logs --tail 50 shiyin-web   # 日志
docker compose restart shiyin-web          # 重启
docker compose down                        # 停止
docker compose up -d                       # 启动
docker compose up -d --build               # 改过代码后重建
```

`restart: unless-stopped` 已配好，NAS 重启后容器会自动拉起。

### 3.1 忘记网页密码

```bash
cd /path/to/shiyin-nd
mv config/web_auth.json config/web_auth.json.bak
docker compose restart shiyin-web
```

重启后会自动重新生成 `admin` / `admin`。确认新账号可用后，把 `.bak` 删掉。

### 3.2 换端口

compose 里 `ports` 改成 `"<新端口>:8090"`，`environment` 里 `WEB_PORT` 保持 `8090` 不用动（容器内不变）。

### 3.3 定时追更歌单

NAS 的 cron / 计划任务里加一条：

```bash
30 3 * * * cd /path/to/shiyin-nd && docker compose run --rm cli \
  playlist <歌单ID> -q flac --concurrency 3 >> cron.log 2>&1
```

已存在的文件会自动跳过，不会重复下载。

---

## 四、常见问题

**网页打不开？**
先看容器是否 Up、端口是否映射成功，再确认防火墙/安全组是否放行该端口。

**点下载没反应 / 任务一直排队？**
同时最多 3 个下载任务，排队是正常的；看 `docker compose logs` 里有没有 errcode。

**下载下来的不是无损？**
该曲目未开放无损，程序按 lossless → high → standard 降级。判断方法见 2.6。

**曲库列表是空的？**
容器内 `/music` 没挂上，或挂载路径写错。`docker compose exec shiyin-web ls /music` 验证。

**容器重启后要重新登录网页？**
会话在内存里，属正常行为；账号密码不受影响。

**容器写出的文件在 NAS 上删不掉？**
compose 加 `user: "<你的UID>:<你的GID>"`。

---

## 五、卸载

```bash
docker compose down
docker image rm shiyin-nd:0.3.0
rm -rf /path/to/shiyin-nd
```

曲库目录不受影响，需要的话自行清理。

---

## 六、合规提示

个人自用，请勿传播、勿商用，遵守酷狗音乐用户协议与所在地法律。上游项目自述「仅供学习交流、请勿商业用途」。
