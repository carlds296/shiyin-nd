#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""shiyin-nd 网页界面 0.3.0

账号密码登录 + 会话 Cookie + 账户自助修改。
纯标准库，调用同镜像内的 shiyin-nd 二进制。
"""

import os
import re
import json
import time
import uuid
import base64
import hashlib
import secrets
import threading
import subprocess
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

BIN = os.environ.get("SHIYIN_BIN", "/usr/local/bin/shiyin-nd")
MUSIC = os.environ.get("SHIYIN_MUSIC_DIR", "/music")
CONFIG = os.environ.get("SHIYIN_CONFIG_DIR", "/config")
PORT = int(os.environ.get("WEB_PORT", "8090"))
QR_PATH = os.path.join(CONFIG, "login-qrcode.png")
AUTH_PATH = os.path.join(CONFIG, "web_auth.json")

ENV = dict(os.environ)
ENV["SHIYIN_CONFIG_DIR"] = CONFIG
ENV["SHIYIN_MUSIC_DIR"] = MUSIC

TASKS = {}
TASKS_LOCK = threading.Lock()
LOGIN_LOCK = threading.Lock()
LOGIN_STATE = {"proc": None, "started": 0.0}
SEM = threading.Semaphore(3)

SESSIONS = {}
SESS_LOCK = threading.Lock()
SESSION_TTL = 7 * 86400
COOKIE_NAME = "sy_session"

PCT_RE = re.compile(r"(\d+(?:\.\d+)?)%\s*\(([^)]+)\)")
DONE_RE = re.compile(r"完成：(.+?)（(.+?)）")


# ------------------------------------------------------------------ 账户

def pw_hash(password, salt):
    return hashlib.pbkdf2_hmac("sha256", password.encode("utf-8"),
                               salt.encode("utf-8"), 120000).hex()


def save_auth(data):
    tmp = AUTH_PATH + ".tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        json.dump(data, fh, ensure_ascii=False, indent=2)
    os.replace(tmp, AUTH_PATH)
    try:
        os.chmod(AUTH_PATH, 0o600)
    except OSError:
        pass


def load_auth():
    try:
        with open(AUTH_PATH, "r", encoding="utf-8") as fh:
            data = json.load(fh)
        if data.get("username") and data.get("hash") and data.get("salt"):
            return data
    except Exception:  # noqa: BLE001
        pass
    data = {"username": "admin", "salt": secrets.token_hex(16),
            "created": time.strftime("%Y-%m-%d %H:%M:%S")}
    data["hash"] = pw_hash("admin", data["salt"])
    save_auth(data)
    return data


def verify_login(username, password):
    data = load_auth()
    if not username or not password:
        return False
    ok_user = secrets.compare_digest(username, data["username"])
    ok_pw = secrets.compare_digest(pw_hash(password, data["salt"]), data["hash"])
    return ok_user and ok_pw


def issue_session(username):
    token = secrets.token_urlsafe(32)
    with SESS_LOCK:
        SESSIONS[token] = {"user": username, "ts": time.time()}
    return token


def drop_session(token):
    with SESS_LOCK:
        SESSIONS.pop(token, None)


def session_user(token):
    if not token:
        return None
    with SESS_LOCK:
        item = SESSIONS.get(token)
        if not item:
            return None
        if time.time() - item["ts"] > SESSION_TTL:
            SESSIONS.pop(token, None)
            return None
        item["ts"] = time.time()
        return item["user"]


# ------------------------------------------------------------------ 引擎调用

def run(args, timeout=60):
    try:
        p = subprocess.run([BIN] + args, capture_output=True, text=True,
                           timeout=timeout, env=ENV)
        return p.returncode, (p.stdout or "") + (p.stderr or "")
    except subprocess.TimeoutExpired:
        return 124, "超时"
    except Exception as exc:  # noqa: BLE001
        return 1, str(exc)


def safe_name(name):
    bad = '<>:"/\\|?*'
    return "".join("_" if c in bad else c for c in name).strip()[:120]


def get_status():
    rc, out = run(["status"], timeout=20)
    logged = "已登录" in out and "未登录" not in out
    info = {"logged_in": logged, "raw": out.strip()}
    for key in ("userid", "vip_type"):
        m = re.search(r"%s\s*:\s*(\S+)" % key, out)
        if m:
            info[key] = m.group(1)
    return info


def get_search(query, limit):
    rc, out = run(["search", query, "-n", str(limit), "--json"], timeout=45)
    if rc != 0:
        return {"ok": False, "error": out.strip()[:300]}
    start = out.find("{")
    if start < 0:
        return {"ok": False, "error": "返回内容不是 JSON"}
    try:
        data = json.loads(out[start:])
    except json.JSONDecodeError as exc:
        return {"ok": False, "error": "解析失败：%s" % exc}
    items = []
    for row in (data.get("lists") or []):
        name = row.get("FileName") or ""
        if not name:
            continue
        items.append({
            "name": name,
            "album": row.get("AlbumName") or "",
            "duration": row.get("Duration") or 0,
            "hash": row.get("FileHash") or "",
            "album_id": row.get("AlbumID") or "",
            "mix_id": str(row.get("MixSongID") or row.get("Audioid") or row.get("SongID") or ""),
            "cover": row.get("Image") or "",
            "exists": os.path.exists(os.path.join(MUSIC, safe_name(name) + ".flac")),
        })
    return {"ok": True, "items": items}


def start_download(item, quality):
    tid = uuid.uuid4().hex[:8]
    task = {"id": tid, "title": item.get("name") or "未命名",
            "quality": quality, "status": "排队中", "percent": 0.0,
            "size": "", "file": "", "error": "", "elapsed": 0,
            "started": time.time()}
    with TASKS_LOCK:
        TASKS[tid] = task
    threading.Thread(target=download_worker, args=(task, item, quality), daemon=True).start()
    return tid


def download_worker(task, item, quality):
    with SEM:
        if not item.get("hash"):
            task["status"] = "失败"
            task["error"] = "缺少 hash"
            return
        args = ["download", "--hash", item["hash"]]
        if item.get("mix_id"):
            args += ["--aaid", item["mix_id"]]
        if item.get("album_id"):
            args += ["--album", item["album_id"]]
        args += ["--name", item.get("name") or "audio", "-q", quality, "-o", MUSIC]
        task["status"] = "下载中"
        try:
            proc = subprocess.Popen([BIN] + args, stdout=subprocess.PIPE,
                                    stderr=subprocess.STDOUT, text=True,
                                    bufsize=1, env=ENV)
        except Exception as exc:  # noqa: BLE001
            task["status"] = "失败"
            task["error"] = str(exc)
            return
        tail = []
        for line in proc.stdout:
            for pct, size in PCT_RE.findall(line):
                try:
                    task["percent"] = float(pct)
                except ValueError:
                    pass
                task["size"] = size
            m = DONE_RE.search(line)
            if m:
                task["file"] = m.group(1)
                task["size"] = m.group(2)
                task["percent"] = 100.0
            tail.append(line.strip())
            if len(tail) > 12:
                tail.pop(0)
        rc = proc.wait()
        task["elapsed"] = round(time.time() - task["started"], 1)
        if rc == 0 and (task["file"] or task["percent"] >= 100):
            task["status"] = "完成"
            task["percent"] = 100.0
            if not task["file"]:
                task["file"] = os.path.join(MUSIC, safe_name(item.get("name") or "") + ".flac")
        else:
            task["status"] = "失败"
            task["error"] = "；".join([t for t in tail if t][-3:])[:300]


def ensure_login_png():
    with LOGIN_LOCK:
        proc = LOGIN_STATE["proc"]
        alive = proc is not None and proc.poll() is None
        if (not alive) or time.time() - LOGIN_STATE["started"] > 600:
            if os.path.exists(QR_PATH):
                try:
                    os.remove(QR_PATH)
                except OSError:
                    pass
            LOGIN_STATE["proc"] = subprocess.Popen(
                [BIN, "login"], stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL, env=ENV)
            LOGIN_STATE["started"] = time.time()
    for _ in range(40):
        if os.path.exists(QR_PATH) and os.path.getsize(QR_PATH) > 200:
            with open(QR_PATH, "rb") as fh:
                return base64.b64encode(fh.read()).decode()
        time.sleep(0.5)
    return None


def list_library(limit=400):
    if not os.path.isdir(MUSIC):
        return {"ok": False, "error": "曲库目录不存在：%s" % MUSIC}
    items, total, total_size = [], 0, 0
    try:
        with os.scandir(MUSIC) as it:
            for entry in it:
                if entry.name.startswith("."):
                    continue
                try:
                    st = entry.stat()
                except OSError:
                    continue
                if entry.is_file(follow_symlinks=False):
                    if entry.name.lower().endswith((".flac", ".mp3", ".m4a", ".wav", ".ogg", ".ape")):
                        total += 1
                        total_size += st.st_size
                        if len(items) < limit:
                            items.append({"name": entry.name, "size": st.st_size,
                                          "mtime": time.strftime("%Y-%m-%d %H:%M", time.localtime(st.st_mtime)),
                                          "dir": False})
                elif entry.is_dir(follow_symlinks=False):
                    items.append({"name": entry.name, "size": 0,
                                  "mtime": time.strftime("%Y-%m-%d", time.localtime(st.st_mtime)),
                                  "dir": True})
    except OSError as exc:
        return {"ok": False, "error": str(exc)}
    items.sort(key=lambda x: (not x["dir"], x["mtime"]), reverse=True)
    return {"ok": True, "dir": MUSIC, "count": total, "size": total_size, "items": items[:limit]}


# ------------------------------------------------------------------ 页面

BASE_CSS = r"""
*{box-sizing:border-box;margin:0;padding:0}
body{font-family:-apple-system,"Segoe UI","PingFang SC","Microsoft YaHei",sans-serif;
background:#0f1115;color:#e6e8eb;padding:18px;max-width:1100px;margin:0 auto}
h1{font-size:19px;font-weight:600;margin-bottom:14px}
h1 em{color:#4ec9b0;font-style:normal;font-size:13px;margin-left:8px;font-weight:400}
.card{background:#181b21;border:1px solid #262a33;border-radius:12px;padding:15px;margin-bottom:13px}
h2{font-size:14px;font-weight:600;margin-bottom:10px;color:#c8ccd2}
.row{display:flex;gap:9px;align-items:center;flex-wrap:wrap}
input[type=text],input[type=password],select{background:#0f1115;border:1px solid #2d323d;color:#e6e8eb;
border-radius:8px;padding:9px 11px;font-size:14px;outline:none}
input[type=text]{flex:1;min-width:170px}
input:focus,select:focus{border-color:#4ec9b0}
button{background:#4ec9b0;color:#0b1410;border:0;border-radius:8px;padding:9px 16px;
font-size:14px;font-weight:600;cursor:pointer}
button:hover{filter:brightness(1.12)}
button:disabled{opacity:.4;cursor:not-allowed}
button.ghost{background:#252a33;color:#c8ccd2;font-weight:400}
button.tiny{padding:5px 11px;font-size:12px}
.muted{color:#8b929e;font-size:13px}
.pill{display:inline-block;padding:3px 10px;border-radius:999px;font-size:12px;background:#1f242c;color:#9aa1ad}
.pill.ok{background:#12352a;color:#4ec9b0}
.pill.bad{background:#3a1e1e;color:#e07b7b}
.pill.run{background:#1e2f3a;color:#6ec6e0}
table{width:100%;border-collapse:collapse;font-size:13.5px}
th,td{padding:8px 7px;text-align:left;border-bottom:1px solid #23262f;vertical-align:middle}
th{color:#8b929e;font-weight:500;font-size:11.5px;letter-spacing:.5px}
tr:hover td{background:#1b1f27}
.cover{width:32px;height:32px;border-radius:5px;object-fit:cover;background:#242832}
.sub{color:#8b929e;font-size:11.5px;display:block;margin-top:2px}
.bar{height:5px;background:#242832;border-radius:3px;overflow:hidden;margin-top:5px}
.bar>i{display:block;height:100%;background:#4ec9b0;width:0;transition:width .3s}
.qr{max-width:250px;border-radius:8px;display:block;margin:12px auto}
.hide{display:none}
.err{color:#e07b7b;font-size:13px;margin-top:8px}
.ok-msg{color:#4ec9b0;font-size:13px;margin-top:8px}
.foot{color:#5c636e;font-size:12px;text-align:center;margin-top:16px;line-height:1.6}
.gate{max-width:380px;margin:12vh auto 0}
.gate h1{text-align:center;margin-bottom:18px}
.gate input{width:100%;margin-bottom:10px}
.gate button{width:100%}
"""

LOGIN_PAGE = r"""<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>登录 · 时音 NAS</title>
<style>__CSS__</style>
</head>
<body>
<div class="gate">
  <h1>时音 · NAS 音乐下载<em>0.3.0</em></h1>
  <div class="card">
    <h2>登录</h2>
    <input type="text" id="u" placeholder="用户名" autocomplete="username">
    <input type="password" id="p" placeholder="密码" autocomplete="current-password">
    <button id="btnLogin">登录</button>
    <div id="err" class="err"></div>
    <div class="muted" style="margin-top:12px;line-height:1.6">
      初始账号 admin / 密码 admin<br>
      登录后可在右上角「账户」里改掉。
    </div>
  </div>
  <div class="foot">个人自用 · 请勿传播或商用</div>
</div>
<script>
const $ = id => document.getElementById(id);
async function login(){
  const u = $('u').value.trim(), p = $('p').value;
  if(!u || !p){ $('err').textContent = '请填写用户名和密码'; return; }
  $('btnLogin').disabled = true;
  const r = await fetch('/api/login', {
    method:'POST', headers:{'Content-Type':'application/json'},
    body: JSON.stringify({username:u, password:p})
  });
  const j = await r.json().catch(()=>({ok:false,error:'响应异常'}));
  $('btnLogin').disabled = false;
  if(j.ok){ location.reload(); }
  else { $('err').textContent = j.error || '用户名或密码不对'; }
}
$('btnLogin').onclick = login;
$('p').onkeydown = e => { if(e.key === 'Enter') login(); };
$('u').onkeydown = e => { if(e.key === 'Enter') $('p').focus(); };
</script>
</body>
</html>
"""

PAGE = r"""<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>时音 · NAS 音乐下载</title>
<style>__CSS__</style>
</head>
<body>
<h1>时音 · NAS 音乐下载<em>shiyin-nd 0.3.0</em></h1>

<div class="card">
  <div class="row">
    <span id="stPill" class="pill">检查中…</span>
    <span id="stInfo" class="muted"></span>
    <button class="ghost tiny" id="btnLogin">扫码登录</button>
    <button class="ghost tiny" onclick="loadLib()">刷新曲库</button>
    <span style="flex:1"></span>
    <span class="muted" id="whoAmI"></span>
    <button class="ghost tiny" id="btnAccount">账户</button>
    <button class="ghost tiny" id="btnLogout">退出</button>
  </div>
  <div id="qrBox" class="hide" style="text-align:center">
    <img id="qrImg" class="qr" alt="二维码">
    <div class="muted">用酷狗 App 扫码，扫完这页会自动更新</div>
  </div>
</div>

<div class="card hide" id="accCard">
  <h2>账户设置</h2>
  <div class="row" style="margin-bottom:8px">
    <span class="muted" style="width:88px">当前用户</span>
    <input type="text" id="accCur" disabled>
  </div>
  <div class="row" style="margin-bottom:8px">
    <span class="muted" style="width:88px">新用户名</span>
    <input type="text" id="accUser" placeholder="留空则不改">
  </div>
  <div class="row" style="margin-bottom:8px">
    <span class="muted" style="width:88px">新密码</span>
    <input type="password" id="accPass" placeholder="至少 4 位，留空则不改">
  </div>
  <div class="row" style="margin-bottom:8px">
    <span class="muted" style="width:88px">确认密码</span>
    <input type="password" id="accPass2" placeholder="再输一次新密码">
  </div>
  <div class="row" style="margin-bottom:8px">
    <span class="muted" style="width:88px">当前密码</span>
    <input type="password" id="accOld" placeholder="验证身份，必填">
  </div>
  <div class="row">
    <button id="btnSaveAcc">保存</button>
    <button class="ghost" id="btnCancelAcc">取消</button>
  </div>
  <div id="accMsg" class="ok-msg"></div>
</div>

<div class="card">
  <h2>搜索</h2>
  <div class="row">
    <input type="text" id="q" placeholder="歌名 / 歌手，例如 晴天">
    <select id="n">
      <option value="10">10 条</option>
      <option value="20" selected>20 条</option>
      <option value="40">40 条</option>
    </select>
    <select id="quality">
      <option value="flac" selected>无损 FLAC</option>
      <option value="320">320 kbps</option>
      <option value="128">128 kbps</option>
    </select>
    <button id="btnSearch">搜索</button>
  </div>
  <div id="searchHint" class="muted" style="margin-top:8px"></div>
  <div id="results"></div>
</div>

<div class="card">
  <h2>下载任务</h2>
  <div id="tasks"><div class="muted">暂无任务</div></div>
</div>

<div class="card">
  <h2>曲库 <span id="libInfo" class="muted"></span></h2>
  <div id="lib"><div class="muted">加载中…</div></div>
</div>

<div class="foot">个人自用 · 请勿传播或商用 · 遵守酷狗音乐用户协议</div>

<script>
const $ = id => document.getElementById(id);
let curItems = [];

function fmtDur(s){ if(!s) return ''; const m=Math.floor(s/60), r=s%60; return m+':'+String(r).padStart(2,'0'); }
function fmtSize(b){ if(!b) return ''; if(b>1073741824) return (b/1073741824).toFixed(2)+' GB';
  if(b>1048576) return (b/1048576).toFixed(1)+' MB'; if(b>1024) return (b/1024).toFixed(0)+' KB'; return b+' B'; }
function esc(s){ return String(s||'').replace(/[<>&"]/g, c => ({'<':'&lt;','>':'&gt;','&':'&amp;','"':'&quot;'}[c])); }

async function api(path, opts){
  const r = await fetch(path, opts);
  if(r.status === 401){ location.reload(); return {ok:false,error:'未登录'}; }
  const t = await r.text();
  try { return JSON.parse(t); } catch(e){ return {ok:false, error:t.slice(0,200)}; }
}

async function loadAuth(){
  const a = await api('/api/auth');
  if(a.ok && a.username){ $('whoAmI').textContent = a.username; $('accCur').value = a.username; }
}
async function logout(){ await api('/api/logout', {method:'POST'}); location.reload(); }
$('btnLogout').onclick = logout;

$('btnAccount').onclick = () => $('accCard').classList.toggle('hide');
$('btnCancelAcc').onclick = () => { $('accCard').classList.add('hide'); $('accMsg').textContent=''; };

async function saveAccount(){
  const user = $('accUser').value.trim();
  const pass = $('accPass').value;
  const pass2 = $('accPass2').value;
  const old = $('accOld').value;
  const msg = $('accMsg');
  if(!old){ msg.className='err'; msg.textContent='请填写当前密码'; return; }
  if(pass && pass.length < 4){ msg.className='err'; msg.textContent='新密码至少 4 位'; return; }
  if(pass && pass !== pass2){ msg.className='err'; msg.textContent='两次输入的新密码不一致'; return; }
  if(!user && !pass){ msg.className='err'; msg.textContent='用户名和新密码至少要改一个'; return; }
  const r = await api('/api/account', {
    method:'POST', headers:{'Content-Type':'application/json'},
    body: JSON.stringify({username:user, new_password:pass, current_password:old})
  });
  if(r.ok){
    msg.className='ok-msg'; msg.textContent='已保存：' + (r.message || '');
    $('accUser').value=''; $('accPass').value=''; $('accPass2').value=''; $('accOld').value='';
    if(r.username){ $('whoAmI').textContent = r.username; $('accCur').value = r.username; }
    setTimeout(()=>$('accCard').classList.add('hide'), 2600);
  } else {
    msg.className='err'; msg.textContent = r.error || '保存失败';
  }
}
$('btnSaveAcc').onclick = saveAccount;

async function loadStatus(){
  const s = await api('/api/status');
  if(s.logged_in){
    $('stPill').className='pill ok'; $('stPill').textContent='酷狗已登录';
    $('stInfo').textContent = 'userid ' + (s.userid||'-') + ' · VIP ' + (s.vip_type||'0');
    $('qrBox').classList.add('hide');
  } else {
    $('stPill').className='pill bad'; $('stPill').textContent='酷狗未登录';
    $('stInfo').textContent = '需要扫码登录后才能拿无损地址';
  }
}

async function doLogin(){
  $('btnLogin').disabled = true;
  const r = await api('/api/login/qr');
  $('btnLogin').disabled = false;
  if(r.ok && r.png){
    $('qrImg').src = 'data:image/png;base64,' + r.png;
    $('qrBox').classList.remove('hide');
    setTimeout(doLogin.poll, 2000);
  } else if(r.logged_in){ loadStatus(); }
  else { alert('二维码生成失败：' + (r.error || '未知原因')); }
}
doLogin.poll = async () => {
  const s = await api('/api/status');
  if(s.logged_in){ loadStatus(); $('qrBox').classList.add('hide'); }
  else setTimeout(doLogin.poll, 2000);
};
$('btnLogin').onclick = doLogin;

async function search(){
  const q = $('q').value.trim();
  if(!q){ $('q').focus(); return; }
  $('btnSearch').disabled = true;
  $('searchHint').textContent = '搜索中…';
  const r = await api('/api/search?q=' + encodeURIComponent(q) + '&n=' + $('n').value);
  $('btnSearch').disabled = false;
  if(!r.ok){ $('searchHint').textContent = '搜索失败：' + (r.error||''); $('results').innerHTML=''; return; }
  curItems = r.items;
  $('searchHint').textContent = '共 ' + r.items.length + ' 条结果';
  let html = '<table><thead><tr><th></th><th>歌曲</th><th>专辑</th><th>时长</th><th></th></tr></thead><tbody>';
  r.items.forEach((it, i) => {
    html += '<tr><td>' + (it.cover ? '<img class="cover" src="' + it.cover + '" onerror="this.style.visibility=\'hidden\'">' : '') + '</td>';
    html += '<td><div>' + esc(it.name) + '</div><span class="sub">' + (it.exists ? '曲库已有' : 'hash ' + it.hash.slice(0,8)) + '</span></td>';
    html += '<td class="muted">' + esc(it.album) + '</td><td class="muted">' + fmtDur(it.duration) + '</td>';
    html += '<td style="text-align:right"><button class="tiny" ' + (it.exists ? 'disabled' : '') + ' onclick="dl(' + i + ')">' + (it.exists ? '已存在' : '下载') + '</button></td></tr>';
  });
  html += '</tbody></table>';
  $('results').innerHTML = html;
}
$('btnSearch').onclick = search;
$('q').onkeydown = e => { if(e.key === 'Enter') search(); };

async function dl(i){
  const it = curItems[i];
  const r = await api('/api/download', { method:'POST', headers:{'Content-Type':'application/json'},
    body: JSON.stringify({item: it, quality: $('quality').value}) });
  if(!r.ok) alert('启动失败：' + (r.error||''));
  pollTasks();
}

async function pollTasks(){
  const r = await api('/api/tasks');
  if(!r.ok || !r.tasks.length) return;
  let html = '<table><thead><tr><th>歌曲</th><th>音质</th><th>状态</th><th>进度</th></tr></thead><tbody>';
  r.tasks.forEach(t => {
    let cls='pill run', label=t.status;
    if(t.status==='完成') cls='pill ok'; else if(t.status==='失败') cls='pill bad';
    html += '<tr><td><div>' + esc(t.title) + '</div>' + (t.file ? '<span class="sub">' + esc(t.file.split('/').pop()) + '</span>' : '') + '</td>';
    html += '<td class="muted">' + esc(t.quality) + '</td><td><span class="' + cls + '">' + label + '</span></td>';
    html += '<td style="min-width:180px"><div class="muted" style="font-size:12px">' + (t.percent? t.percent.toFixed(1)+'%' : '') + ' ' + (t.size||'') + (t.error? '<br>'+esc(t.error) : '') + '</div><div class="bar"><i style="width:'+t.percent+'%"></i></div></td></tr>';
  });
  html += '</tbody></table>';
  $('tasks').innerHTML = html;
}

async function loadLib(){
  $('lib').innerHTML = '<div class="muted">读取中…</div>';
  const r = await api('/api/library');
  if(!r.ok){ $('lib').innerHTML = '<div class="muted">' + esc(r.error||'读取失败') + '</div>'; return; }
  $('libInfo').textContent = '· ' + r.dir + ' · ' + r.count + ' 首 / ' + fmtSize(r.size);
  let html = '<table><thead><tr><th>名称</th><th>大小</th><th>时间</th></tr></thead><tbody>';
  r.items.slice(0,120).forEach(it => {
    html += '<tr><td>' + (it.dir ? '📁 ' : '🎵 ') + esc(it.name) + '</td><td class="muted">' + (it.dir?'目录':fmtSize(it.size)) + '</td><td class="muted">' + it.mtime + '</td></tr>';
  });
  html += '</tbody></table>';
  if(r.items.length > 120) html += '<div class="muted" style="margin-top:8px">仅显示前 120 项</div>';
  $('lib').innerHTML = html;
}

loadAuth(); loadStatus(); loadLib();
setInterval(pollTasks, 1500);
setInterval(() => { if(!$('qrBox').classList.contains('hide')) return; loadStatus(); }, 15000);
</script>
</body>
</html>
"""


class Handler(BaseHTTPRequestHandler):
    server_version = "shiyin-web/0.3.0"

    def log_message(self, fmt, *args):
        pass

    def _cookie(self, name):
        raw = self.headers.get("Cookie") or ""
        for part in raw.split(";"):
            part = part.strip()
            if "=" in part:
                k, v = part.split("=", 1)
                if k == name:
                    return v
        return None

    def _user(self):
        return session_user(self._cookie(COOKIE_NAME))

    def _send(self, code, body, ctype="application/json; charset=utf-8", headers=None):
        if isinstance(body, (dict, list)):
            body = json.dumps(body, ensure_ascii=False).encode("utf-8")
        elif isinstance(body, str):
            body = body.encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.end_headers()
        try:
            self.wfile.write(body)
        except BrokenPipeError:
            pass

    def _json_body(self):
        length = int(self.headers.get("Content-Length") or 0)
        if not length:
            return {}
        try:
            return json.loads(self.rfile.read(length).decode("utf-8"))
        except Exception:  # noqa: BLE001
            return None

    def do_GET(self):
        path = self.path.split("?")[0]
        try:
            if path == "/":
                if self._user():
                    return self._send(200, PAGE.replace("__CSS__", BASE_CSS), "text/html; charset=utf-8")
                return self._send(200, LOGIN_PAGE.replace("__CSS__", BASE_CSS), "text/html; charset=utf-8")
            if path == "/api/auth":
                user = self._user()
                return self._send(200, {"ok": True, "logged_in": bool(user), "username": user or ""})
            if not self._user():
                return self._send(401, {"ok": False, "error": "请先登录"})
            if path == "/api/status":
                return self._send(200, get_status())
            if path == "/api/account":
                return self._send(200, {"ok": True, "username": load_auth()["username"]})
            if path == "/api/search":
                qs = parse_qs(self.path)
                q = qs.get("q", [""])[0].strip()
                raw_n = qs.get("n", ["20"])[0]
                if not q:
                    return self._send(400, {"ok": False, "error": "缺少参数 q"})
                try:
                    n = max(1, min(100, int(raw_n)))
                except ValueError:
                    n = 20
                return self._send(200, get_search(q, n))
            if path == "/api/tasks":
                with TASKS_LOCK:
                    tasks = sorted(TASKS.values(), key=lambda x: x["started"], reverse=True)[:30]
                return self._send(200, {"ok": True, "tasks": tasks})
            if path == "/api/library":
                return self._send(200, list_library())
            if path == "/api/login/qr":
                st = get_status()
                if st["logged_in"]:
                    return self._send(200, {"ok": True, "logged_in": True})
                png = ensure_login_png()
                if png:
                    return self._send(200, {"ok": True, "logged_in": False, "png": png})
                return self._send(200, {"ok": False, "error": "二维码生成超时"})
            return self._send(404, {"ok": False, "error": "not found"})
        except Exception as exc:  # noqa: BLE001
            return self._send(500, {"ok": False, "error": str(exc)})

    def do_POST(self):
        path = self.path.split("?")[0]
        try:
            payload = self._json_body()
            if payload is None:
                return self._send(400, {"ok": False, "error": "请求体不是 JSON"})

            if path == "/api/login":
                username = (payload.get("username") or "").strip()
                password = payload.get("password") or ""
                if not verify_login(username, password):
                    return self._send(401, {"ok": False, "error": "用户名或密码不对"})
                token = issue_session(username)
                cookie = "%s=%s; Path=/; HttpOnly; SameSite=Lax; Max-Age=%d" % (
                    COOKIE_NAME, token, SESSION_TTL)
                return self._send(200, {"ok": True, "username": username},
                                  headers={"Set-Cookie": cookie})

            if not self._user():
                return self._send(401, {"ok": False, "error": "请先登录"})

            if path == "/api/logout":
                drop_session(self._cookie(COOKIE_NAME))
                cookie = "%s=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0" % COOKIE_NAME
                return self._send(200, {"ok": True}, headers={"Set-Cookie": cookie})

            if path == "/api/account":
                current = payload.get("current_password") or ""
                new_user = (payload.get("username") or "").strip()
                new_pass = payload.get("new_password") or ""
                data = load_auth()
                if not secrets.compare_digest(pw_hash(current, data["salt"]), data["hash"]):
                    return self._send(403, {"ok": False, "error": "当前密码不对"})
                if new_user and len(new_user) > 32:
                    return self._send(400, {"ok": False, "error": "用户名最长 32 个字符"})
                if new_pass and len(new_pass) < 4:
                    return self._send(400, {"ok": False, "error": "新密码至少 4 位"})
                if not new_user and not new_pass:
                    return self._send(400, {"ok": False, "error": "用户名和新密码至少要改一个"})
                if new_pass:
                    data["salt"] = secrets.token_hex(16)
                    data["hash"] = pw_hash(new_pass, data["salt"])
                if new_user:
                    data["username"] = new_user
                data["updated"] = time.strftime("%Y-%m-%d %H:%M:%S")
                save_auth(data)
                parts = []
                if new_user:
                    parts.append("用户名已改为 %s" % new_user)
                if new_pass:
                    parts.append("密码已更新")
                return self._send(200, {"ok": True, "username": data["username"],
                                        "message": "，".join(parts)})

            if path == "/api/download":
                item = payload.get("item") or {}
                if not item.get("hash"):
                    return self._send(400, {"ok": False, "error": "缺少 hash"})
                quality = payload.get("quality") or "flac"
                if quality not in ("flac", "320", "128"):
                    quality = "flac"
                return self._send(200, {"ok": True, "id": start_download(item, quality)})

            return self._send(404, {"ok": False, "error": "not found"})
        except Exception as exc:  # noqa: BLE001
            return self._send(500, {"ok": False, "error": str(exc)})


def parse_qs(path):
    import urllib.parse
    return urllib.parse.parse_qs(urllib.parse.urlparse(path).query)


if __name__ == "__main__":
    os.makedirs(MUSIC, exist_ok=True)
    data = load_auth()
    print("shiyin-web 0.3.0 on 0.0.0.0:%d  music=%s  user=%s" % (PORT, MUSIC, data["username"]), flush=True)
    ThreadingHTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
