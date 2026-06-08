#!/usr/bin/env python3
"""
Web-based Touchpad & Keyboard server for mobile devices.
Open in phone browser to control your Linux desktop, including Chinese input.
Uses raw asyncio TCP server to handle both HTTP and WebSocket on same port.
"""

import asyncio
import hashlib
import json
import struct
import signal
import socket
import subprocess
import sys
import base64
import os
import threading
import queue
from datetime import datetime

HOST = "0.0.0.0"
PORT = 8765
WS_MAGIC = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11"

def get_local_ip():
    """Get LAN IP, preferring WiFi/Ethernet over VPN/tunnel interfaces."""
    import netifaces
    # Priority: wlan/eth/enp > any non-tun > fallback
    preferred = []
    others = []
    for iface in netifaces.interfaces():
        addrs = netifaces.ifaddresses(iface).get(netifaces.AF_INET, [])
        for addr in addrs:
            ip = addr.get('addr')
            if not ip or ip == '127.0.0.1':
                continue
            if iface.startswith(('wlan', 'eth', 'enp', 'eno', 'ens')):
                preferred.append(ip)
            elif not iface.startswith(('tun', 'tap', 'wg', 'vgate', 'lo')):
                others.append(ip)
    if preferred:
        return preferred[0]
    if others:
        return others[0]
    # Fallback: connect to external IP
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.connect(("8.8.8.8", 80))
        ip = s.getsockname()[0]
        s.close()
        return ip
    except Exception:
        return "localhost"

# ─── Persistent xdotool process ──────────────────────────────

class XDo:
    """Manages a persistent `xdotool -` subprocess for low-latency commands."""
    def __init__(self):
        self._proc = None
        self._type_proc = None  # separate process for `type --file -`

    async def start(self):
        self._proc = await asyncio.create_subprocess_exec(
            "xdotool", "-",
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.DEVNULL,
            stderr=asyncio.subprocess.DEVNULL,
        )

    def _write(self, cmd: str):
        if self._proc and self._proc.stdin:
            try:
                self._proc.stdin.write((cmd + "\n").encode())
            except (BrokenPipeError, OSError):
                pass

    def mouse_move(self, dx, dy):
        self._write(f"mousemove_relative -- {int(dx)} {int(dy)}")

    def mouse_click(self, button=1):
        self._write(f"click {button}")

    def mouse_double_click(self):
        self._write("click --repeat 2 1")

    def mouse_down(self, button=1):
        self._write(f"mousedown {button}")

    def mouse_up(self, button=1):
        self._write(f"mouseup {button}")

    def mouse_scroll(self, dy):
        btn = "5" if dy > 0 else "4"  # natural: positive = page down
        for _ in range(abs(int(dy))):
            self._write(f"click {btn}")

    def send_key(self, key):
        self._write(f"key --clearmodifiers {key}")

    async def type_text(self, text):
        """Type text via clipboard paste to avoid IME conflicts on target."""
        if not text:
            return
        parts = text.split("\n")
        for i, part in enumerate(parts):
            if part:
                try:
                    # Copy text to clipboard via xclip, then paste
                    proc = await asyncio.create_subprocess_exec(
                        "xclip", "-selection", "clipboard",
                        stdin=asyncio.subprocess.PIPE,
                        stdout=asyncio.subprocess.DEVNULL,
                        stderr=asyncio.subprocess.DEVNULL,
                    )
                    await proc.communicate(input=part.encode("utf-8"))
                    # Small delay to ensure clipboard is populated
                    await asyncio.sleep(0.02)
                    self.send_key("ctrl+v")
                except Exception:
                    # Fallback: try xdotool type if xclip fails
                    try:
                        proc = await asyncio.create_subprocess_exec(
                            "xdotool", "type", "--clearmodifiers", "--file", "-",
                            stdin=asyncio.subprocess.PIPE,
                            stdout=asyncio.subprocess.DEVNULL,
                            stderr=asyncio.subprocess.DEVNULL,
                        )
                        await proc.communicate(input=part.encode("utf-8"))
                    except Exception:
                        pass
            if i < len(parts) - 1:
                self.send_key("Return")

    def close(self):
        if self._proc:
            self._proc.terminate()

xdo = XDo()
gui_event_queue = None  # Set by GUI to receive connection events
active_ws = None        # (writer, addr) — currently approved controller
pending_ws = None       # (writer, addr) — waiting new client
approval_fut = None     # asyncio.Future for approval result

# ─── HTML page ────────────────────────────────────────────────

HTML_PAGE = r"""<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0, user-scalable=no, maximum-scale=1.0">
<title>Remote Touchpad</title>
<style>
*{margin:0;padding:0;box-sizing:border-box}
html,body{height:100%;width:100%;background:#1a1a2e;color:#eee;
  font-family:-apple-system,system-ui,sans-serif;touch-action:none;
  user-select:none;-webkit-user-select:none;overflow:hidden}
#app{display:flex;flex-direction:column;height:100%;padding:8px;gap:6px}
#status{text-align:center;font-size:12px;padding:4px;border-radius:8px;background:#16213e}
#status.ok{color:#4ecca3}#status.err{color:#e23e57}
#touchpad{flex:1;min-height:0;background:#16213e;border-radius:16px;position:relative;overflow:hidden}
.hint{position:absolute;top:50%;left:50%;transform:translate(-50%,-50%);
  color:#444;font-size:13px;text-align:center;pointer-events:none;line-height:1.6}
#scroll-tag{position:absolute;top:8px;right:8px;background:rgba(78,204,163,.8);
  color:#1a1a2e;padding:2px 8px;border-radius:6px;font-size:11px;font-weight:700;display:none}
.ripple{position:absolute;width:40px;height:40px;border-radius:50%;pointer-events:none;
  transform:translate(-50%,-50%);animation:rf .4s ease-out forwards}
@keyframes rf{0%{opacity:1;transform:translate(-50%,-50%) scale(.5)}
  100%{opacity:0;transform:translate(-50%,-50%) scale(2)}}

/* 隐藏的输入框：用于触发系统键盘 */
#txt{position:absolute;left:-9999px;top:-9999px;width:1px;height:1px;opacity:0}

/* 浮动键盘按钮 - 固定在顶部状态栏右侧 */
#kb-btn{position:absolute;top:2px;right:8px;width:36px;height:28px;
  border:none;border-radius:6px;background:#0f3460;font-size:16px;
  cursor:pointer;z-index:50;touch-action:manipulation;line-height:28px}
#kb-btn.active{background:#4ecca3;color:#1a1a2e}
#kb-btn:active{transform:scale(.9)}

/* 底部按钮栏 */
#bar{display:flex;gap:5px;flex-wrap:wrap}
.b{flex:1;min-width:0;padding:9px 4px;border:none;border-radius:8px;font-size:13px;
  font-weight:600;color:#eee;cursor:pointer;touch-action:manipulation;background:#533483}
.b:active{opacity:.7;transform:scale(.95)}
.bw{flex:2}
#sent{position:fixed;top:8px;left:50%;transform:translateX(-50%);background:rgba(78,204,163,.9);
  color:#1a1a2e;padding:3px 12px;border-radius:6px;font-size:12px;font-weight:600;
  opacity:0;transition:opacity .15s;pointer-events:none;z-index:99}
#sent.show{opacity:1}

/* 审批弹窗 */
#approval-overlay{position:fixed;inset:0;background:rgba(0,0,0,.7);z-index:200;
  display:none;align-items:center;justify-content:center}
#approval-overlay.show{display:flex}
#approval-modal{background:#16213e;border-radius:16px;padding:28px 24px;
  width:85%;max-width:320px;text-align:center;box-shadow:0 8px 32px rgba(0,0,0,.5)}
#approval-modal h3{font-size:17px;margin-bottom:12px;color:#fff}
#approval-modal p{font-size:14px;color:#aaa;margin-bottom:24px;line-height:1.5}
.approval-btns{display:flex;gap:12px}
.approval-btns button{flex:1;padding:12px;border:none;border-radius:10px;
  font-size:15px;font-weight:600;cursor:pointer;touch-action:manipulation}
#btn-reject{background:#e23e57;color:#fff}
#btn-accept{background:#4ecca3;color:#1a1a2e}
.approval-btns button:active{opacity:.7;transform:scale(.95)}
</style>
</head>
<body>
<div id="sent">已发送</div>
<div id="app">
  <div id="status" class="err">⏳ 连接中...</div>
  <div id="touchpad">
    <div class="hint">触控板<br><small>单指移动=光标 · 单击=点击 · 双击=双击<br>双指上下滑=滚动</small></div>
    <div id="scroll-tag">⬆⬆ 滚动</div>
  </div>
</div>

<button id="kb-btn" onclick="toggleKb()">⌨️</button>
<textarea id="txt" autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"></textarea>

<div id="approval-overlay">
  <div id="approval-modal">
    <h3>🔗 新设备请求连接</h3>
    <p id="approval-info">有新设备正在尝试接管控制权</p>
    <div class="approval-btns">
      <button id="btn-reject" onclick="approvalResp('reject')">拒绝</button>
      <button id="btn-accept" onclick="approvalResp('accept')">确认退出</button>
    </div>
  </div>
</div>

<script>
const $=id=>document.getElementById(id);
let ws,reconn,hasControl=false,hasEverControlled=false;
const st=$('status');

function connect(){
  ws=new WebSocket(`ws://${location.host}`);
  ws.onopen=()=>{st.textContent='✅ 已连接';st.className='ok';clearTimeout(reconn)};
  ws.onclose=e=>{
    hasControl=false;
    $('approval-overlay').classList.remove('show');
    if(e.code===4001){
      st.textContent='🔄 已被新设备接管';st.className='err';
      return;
    }
    if(e.code===4002){
      // 被拒绝/超时/忙线 — 不重连，显示对应状态（状态文字已在 onmessage 中设置）
      return;
    }
    // 网络异常断线 → 仅曾获得控制权的设备自动重连
    if(e.code!==1000 && e.code!==1001){
      st.textContent='❌ 已断开';st.className='err';
      if(hasEverControlled)reconn=setTimeout(connect,2000);
    }
  };
  ws.onerror=()=>ws.close();
  ws.onmessage=e=>{
    let d;try{d=JSON.parse(e.data)}catch{return}
    if(d.a==='ctrl_ok'){
      hasControl=true;hasEverControlled=true;
      st.textContent='✅ 已连接（控制中）';st.className='ok';
    } else if(d.a==='wait'){
      hasControl=false;
      if(d.reason==='timeout'){
        st.textContent='⏰ 等待超时';st.className='err';
      } else if(d.reason==='rejected'){
        st.textContent='🚫 被拒绝';st.className='err';
      } else if(d.reason==='busy'){
        st.textContent='⏳ 已有其他设备在等待';st.className='err';
      } else {
        st.textContent='⏳ 等待当前设备同意...';st.className='ok';
      }
    } else if(d.a==='approval_req'){
      $('approval-info').textContent=`${d.ip} 正在尝试接管控制权`;
      $('approval-overlay').classList.add('show');
    }
  };
}
function S(d){
  if(!hasControl && d.a!=='approval_resp')return;
  if(ws&&ws.readyState===1)ws.send(JSON.stringify(d))
}

function approvalResp(r){
  S({a:'approval_resp',r});
  $('approval-overlay').classList.remove('show');
}

// ─── Sent indicator ───────────────────────────────────
let sentTimer;
function flash(){
  const el=$('sent');el.classList.add('show');
  clearTimeout(sentTimer);sentTimer=setTimeout(()=>el.classList.remove('show'),600);
}

// ─── Keyboard toggle ─────────────────────────────────
const txt=$('txt');
const kbBtn=$('kb-btn');
let kbVisible=false;

function toggleKb(){
  if(kbVisible){
    txt.blur();
    kbVisible=false;
    kbBtn.classList.remove('active');
  } else {
    txt.focus();
    kbVisible=true;
    kbBtn.classList.add('active');
  }
}

// 键盘被系统收起时（滑动触控板等）同步按钮状态
txt.addEventListener('blur',()=>{
  clearTimeout(debounce);
  // 若输入法仍在组合中，丢弃未确认的拼音
  if(compositionActive){txt.value=lastVal;compositionActive=false}
  else{
    const v=txt.value,newText=v.slice(lastVal.length);
    if(newText){S({a:'type',t:newText});flash()}
    lastVal=v;
  }
  kbVisible=false;
  kbBtn.classList.remove('active');
});

// ─── Real-time input ─────────────────────────────────
let lastVal='',debounce,compositionActive=false;

// 追踪输入法组合状态（用于 blur 时判断）
txt.addEventListener('compositionstart',()=>{compositionActive=true});
txt.addEventListener('compositionend',()=>{compositionActive=false});

// 所有键盘输入（含退格、回车）统一走 input 事件
// 使用 e.isComposing 判断是否在输入法组合中，避免吞字
txt.addEventListener('input',(e)=>{
  if(e.isComposing)return;
  clearTimeout(debounce);
  debounce=setTimeout(()=>{
    const v=txt.value,old=lastVal;
    if(v===old)return;
    if(v.length>old.length&&v.startsWith(old)){
      // 末尾新增文字（含回车 \n）
      S({a:'type',t:v.slice(old.length)});flash();
    } else if(v.length<old.length&&old.startsWith(v)){
      // 末尾删除（退格）
      S({a:'bs',n:old.length-v.length});flash();
    } else {
      // 中间编辑（选中替换等）→ 删除旧的，输入新的
      S({a:'bs',n:old.length});
      if(v.length){S({a:'type',t:v})}
      flash();
    }
    lastVal=v;
  },30);
});

// iOS/部分浏览器 fallback：compositionend 可能不触发 input
txt.addEventListener('compositionend',()=>{
  setTimeout(()=>{
    const v=txt.value,old=lastVal;
    if(v!==old){
      if(v.length>old.length&&v.startsWith(old)){
        S({a:'type',t:v.slice(old.length)});flash();
      } else if(v.length<old.length&&old.startsWith(v)){
        S({a:'bs',n:old.length-v.length});flash();
      } else {
        S({a:'bs',n:old.length});
        if(v.length){S({a:'type',t:v})}
        flash();
      }
      lastVal=v;
    }
  },0);
});

// ─── Touchpad ─────────────────────────────────────────
const tp=$('touchpad');
let pts={},moved=false,scrolling=false,lastY=0,tStart=0;
let lastTapT=0,lastTapX=0,lastTapY=0;
let pressing=false,pressTimer=null;
let twoFingerT=0,twoFingerMoved=false;
const TH=12,SENS=6,SCR=.06;

// rAF batching: accumulate dx/dy and send once per frame
let accDx=0,accDy=0,accScr=0,mvDirty=false;
function flushMv(){
  if(mvDirty){
    if(accDx||accDy){S({a:'mv',x:accDx,y:accDy});accDx=accDy=0}
    if(accScr){S({a:'scr',y:accScr});accScr=0}
    mvDirty=false;
  }
  requestAnimationFrame(flushMv);
}
requestAnimationFrame(flushMv);

tp.addEventListener('touchstart',e=>{
  e.preventDefault();
  const now=Date.now();
  for(const t of e.changedTouches)
    pts[t.identifier]={x:t.clientX,y:t.clientY,sx:t.clientX,sy:t.clientY};
  if(e.touches.length===1){
    moved=false;scrolling=false;tStart=now;pressing=false;
    clearTimeout(pressTimer);
    pressTimer=setTimeout(()=>{
      if(!moved&&!scrolling){
        pressing=true;
        S({a:'md',b:1});
        $('scroll-tag').textContent='✊ 长按拖动中';
        $('scroll-tag').style.display='block';
      }
    },500);
  }
  if(e.touches.length===2){
    scrolling=false;clearTimeout(pressTimer);
    twoFingerT=now;twoFingerMoved=false;
    $('scroll-tag').textContent='⬆⬇ 滚动';
    $('scroll-tag').style.display='block';
    lastY=(e.touches[0].clientY+e.touches[1].clientY)/2;
  }
},{passive:false});

tp.addEventListener('touchmove',e=>{
  e.preventDefault();
  if(e.touches.length===1&&!scrolling){
    const t=e.touches[0],p=pts[t.identifier];if(!p)return;
    const dx=t.clientX-p.x,dy=t.clientY-p.y;
    if(Math.abs(t.clientX-p.sx)>TH||Math.abs(t.clientY-p.sy)>TH){
      moved=true;
      if(!pressing)clearTimeout(pressTimer);
    }
    accDx+=dx*SENS;accDy+=dy*SENS;mvDirty=true;
    p.x=t.clientX;p.y=t.clientY;
  }
  if(e.touches.length===2&&e.touches.length>=2){
    scrolling=true;twoFingerMoved=true;
    $('scroll-tag').textContent='⬆⬇ 滚动';
    const ay=(e.touches[0].clientY+e.touches[1].clientY)/2;
    const d=lastY-ay;
    if(Math.abs(d)>1.5){accScr+=Math.round(d*SCR);mvDirty=true;lastY=ay}
  }
},{passive:false});

tp.addEventListener('touchend',e=>{
  e.preventDefault();const now=Date.now();
  clearTimeout(pressTimer);

  // 长按释放 → mouse up
  if(pressing){
    pressing=false;
    S({a:'mu',b:1});
    $('scroll-tag').style.display='none';
    for(const t of e.changedTouches)delete pts[t.identifier];
    if(e.touches.length===0){scrolling=false;twoFingerT=0}
    return;
  }

  // 双指轻触 → 右键（200ms 内且无移动）
  if(e.touches.length===0&&twoFingerT&&!twoFingerMoved&&now-twoFingerT<200){
    S({a:'clk',b:3});
    const t=e.changedTouches[0];
    if(t)rip(t.clientX,t.clientY,'#533483');
    twoFingerT=0;scrolling=false;
    $('scroll-tag').style.display='none';
    for(const ct of e.changedTouches)delete pts[ct.identifier];
    return;
  }

  for(const t of e.changedTouches){
    const p=pts[t.identifier];if(!p)continue;
    if(!moved&&!scrolling&&e.touches.length===0&&now-tStart<250){
      const dt=now-lastTapT,dd=Math.hypot(t.clientX-lastTapX,t.clientY-lastTapY);
      if(dt<350&&dd<50){S({a:'dbl'});rip(t.clientX,t.clientY,'#e23e57');lastTapT=0}
      else{S({a:'clk',b:1});rip(t.clientX,t.clientY,'#4ecca3');lastTapT=now;lastTapX=t.clientX;lastTapY=t.clientY}
    }
    delete pts[t.identifier];
  }
  if(e.touches.length===0){scrolling=false;twoFingerT=0;$('scroll-tag').style.display='none'}
},{passive:false});

function rip(x,y,c){
  const r=document.createElement('div');r.className='ripple';
  const b=tp.getBoundingClientRect();
  r.style.left=(x-b.left)+'px';r.style.top=(y-b.top)+'px';
  const m=/^#(..)(..)(..)$/.exec(c);
  r.style.background=`rgba(${parseInt(m[1],16)},${parseInt(m[2],16)},${parseInt(m[3],16)},.4)`;
  tp.appendChild(r);setTimeout(()=>r.remove(),400);
}

document.body.addEventListener('touchmove',e=>e.preventDefault(),{passive:false});
connect();
</script>
</body>
</html>"""

# ─── HTTP response helper ─────────────────────────────────────

HTML_BYTES = HTML_PAGE.encode("utf-8")
HTTP_RESPONSE = (
    "HTTP/1.1 200 OK\r\n"
    "Content-Type: text/html; charset=utf-8\r\n"
    f"Content-Length: {len(HTML_BYTES)}\r\n"
    "Connection: close\r\n\r\n"
).encode("utf-8") + HTML_BYTES

# ─── Minimal WebSocket implementation ─────────────────────────

def parse_http_request(data: bytes):
    """Parse HTTP request line and headers from raw bytes."""
    text = data.decode("utf-8", errors="replace")
    lines = text.split("\r\n")
    if not lines:
        return None, {}
    parts = lines[0].split(" ", 2)
    if len(parts) < 2:
        return None, {}
    method, path = parts[0], parts[1]
    headers = {}
    for line in lines[1:]:
        if not line:
            break
        if ":" in line:
            k, v = line.split(":", 1)
            headers[k.strip().lower()] = v.strip()
    return (method, path), headers

def ws_accept_key(key: str) -> str:
    """Compute WebSocket accept key per RFC 6455."""
    h = hashlib.sha1(key.encode() + WS_MAGIC).digest()
    return base64.b64encode(h).decode()

def ws_encode(data: str) -> bytes:
    """Encode a text message into a WebSocket frame (server → client)."""
    payload = data.encode("utf-8")
    header = bytearray()
    header.append(0x81)  # FIN + text opcode
    length = len(payload)
    if length < 126:
        header.append(length)
    elif length < 65536:
        header.append(126)
        header.extend(struct.pack(">H", length))
    else:
        header.append(127)
        header.extend(struct.pack(">Q", length))
    return bytes(header) + payload

def ws_decode(data: bytes):
    """Decode one WebSocket frame (client → server). Returns (message, remaining_bytes) or (None, data) if incomplete."""
    if len(data) < 2:
        return None, data
    byte0, byte1 = data[0], data[1]
    opcode = byte0 & 0x0F
    masked = bool(byte1 & 0x80)
    length = byte1 & 0x7F
    offset = 2
    if length == 126:
        if len(data) < 4:
            return None, data
        length = struct.unpack(">H", data[2:4])[0]
        offset = 4
    elif length == 127:
        if len(data) < 10:
            return None, data
        length = struct.unpack(">Q", data[2:10])[0]
        offset = 10
    mask_len = 4 if masked else 0
    total = offset + mask_len + length
    if len(data) < total:
        return None, data
    if masked:
        mask_key = data[offset:offset + 4]
        payload = bytearray(data[offset + 4:total])
        for i in range(len(payload)):
            payload[i] ^= mask_key[i % 4]
    else:
        payload = data[offset + 0:total]
    if opcode == 0x1:  # text
        return payload.decode("utf-8", errors="replace"), data[total:]
    elif opcode == 0x8:  # close
        return "__close__", data[total:]
    elif opcode == 0x9:  # ping → send pong
        return "__ping__", data[total:]
    elif opcode == 0xA:  # pong
        return None, data[total:]
    return None, data[total:]

def ws_pong(payload: bytes = b"") -> bytes:
    """Build a pong frame."""
    header = bytearray([0x8A, len(payload)])
    return bytes(header) + payload

def ws_close(code: int = 0) -> bytes:
    """Build a close frame with optional status code."""
    if code:
        payload = struct.pack(">H", code)
        header = bytearray([0x88, len(payload)])
        frame = bytes(header) + payload
        print(f"[ws_close] code={code} frame={frame.hex()}", flush=True)
        return frame
    return b"\x88\x80" + os.urandom(4)

# ─── Connection handler ───────────────────────────────────────

async def handle_connection(reader: asyncio.StreamReader, writer: asyncio.StreamWriter):
    """Handle a TCP connection: either serve HTTP or upgrade to WebSocket."""
    try:
        # Read initial data with timeout
        try:
            data = await asyncio.wait_for(reader.read(4096), timeout=5)
        except asyncio.TimeoutError:
            writer.close()
            return

        if not data:
            writer.close()
            return

        (method, path), headers = parse_http_request(data)

        # WebSocket upgrade?
        upgrade = headers.get("upgrade", "").lower()
        if upgrade == "websocket":
            await handle_websocket(reader, writer, headers)
            return

        # Regular HTTP → serve HTML page
        writer.write(HTTP_RESPONSE)
        await writer.drain()
        writer.close()
        await writer.wait_closed()

    except (ConnectionResetError, BrokenPipeError, OSError):
        pass
    finally:
        try:
            writer.close()
        except Exception:
            pass

async def handle_websocket(reader: asyncio.StreamReader, writer: asyncio.StreamWriter, headers: dict):
    """WebSocket handler with approval flow: new clients need current controller's consent."""
    global active_ws, pending_ws, approval_fut
    ws_key = headers.get("sec-websocket-key", "")
    if not ws_key:
        writer.close()
        return

    # Send handshake response
    accept = ws_accept_key(ws_key)
    response = (
        "HTTP/1.1 101 Switching Protocols\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Accept: {accept}\r\n\r\n"
    ).encode("utf-8")
    writer.write(response)
    await writer.drain()

    addr = writer.get_extra_info("peername", ("?", 0))
    now = datetime.now().strftime("%H:%M:%S")
    print(f"[ws] Client connected: {addr[0]}:{addr[1]}", flush=True)

    # ── No active controller → take control immediately ──
    if active_ws is None:
        active_ws = (writer, addr)
        writer.write(ws_encode(json.dumps({"a": "ctrl_ok"})))
        await writer.drain()
        print(f"[ws] {addr[0]}:{addr[1]} is now controller (no previous)", flush=True)
        if gui_event_queue is not None:
            gui_event_queue.put(f"✅ {addr[0]}:{addr[1]}  ({now})")
    else:
        # ── Active controller exists → need approval ──
        # Reject if another client is already waiting
        if pending_ws is not None:
            writer.write(ws_encode(json.dumps({"a": "wait", "reason": "busy"})))
            await writer.drain()
            writer.write(ws_close(4002))
            await writer.drain()
            writer.close()
            return

        pending_ws = (writer, addr)
        approval_fut = asyncio.get_running_loop().create_future()

        # Notify current controller
        active_writer, _ = active_ws
        active_writer.write(ws_encode(json.dumps({"a": "approval_req", "ip": addr[0]})))
        await active_writer.drain()

        # Notify new client they are waiting
        writer.write(ws_encode(json.dumps({"a": "wait"})))
        await writer.drain()

        if gui_event_queue is not None:
            gui_event_queue.put(f"⏳ {addr[0]}:{addr[1]}  ({now}) 等待审批")
        print(f"[ws] {addr[0]}:{addr[1]} waiting for approval", flush=True)

        # Wait for approval (30s timeout)
        try:
            result = await asyncio.wait_for(approval_fut, timeout=30)
        except asyncio.TimeoutError:
            result = "timeout"

        if result == "accept":
            # Disconnect old controller with kick code
            old_writer, old_addr = active_ws
            print(f"[approval] Accepted: kicking {old_addr[0]}:{old_addr[1]}", flush=True)
            try:
                old_writer.write(ws_close(4001))
                await old_writer.drain()
                old_writer.close()
            except Exception as e:
                print(f"[approval] Error closing old: {e}", flush=True)
            if gui_event_queue is not None:
                gui_event_queue.put(f"🔄 {old_addr[0]}:{old_addr[1]}  已退出")

            # Promote pending to active
            active_ws = (writer, addr)
            pending_ws = None
            approval_fut = None
            print(f"[approval] Promoting {addr[0]}:{addr[1]} to controller", flush=True)
            writer.write(ws_encode(json.dumps({"a": "ctrl_ok"})))
            await writer.drain()
            print(f"[approval] ctrl_ok sent to {addr[0]}:{addr[1]}", flush=True)
            if gui_event_queue is not None:
                gui_event_queue.put(f"✅ {addr[0]}:{addr[1]}  ({now}) 已接管控制")
            print(f"[ws] {addr[0]}:{addr[1]} approved, now controller", flush=True)
        elif result == "timeout":
            writer.write(ws_encode(json.dumps({"a": "wait", "reason": "timeout"})))
            await writer.drain()
            writer.write(ws_close(4002))
            await writer.drain()
            pending_ws = None
            approval_fut = None
            writer.close()
            if gui_event_queue is not None:
                gui_event_queue.put(f"⏰ {addr[0]}:{addr[1]}  等待超时")
            return
        else:  # reject
            writer.write(ws_encode(json.dumps({"a": "wait", "reason": "rejected"})))
            await writer.drain()
            writer.write(ws_close(4002))
            await writer.drain()
            pending_ws = None
            approval_fut = None
            writer.close()
            if gui_event_queue is not None:
                gui_event_queue.put(f"🚫 {addr[0]}:{addr[1]}  被拒绝")
            return

    # ── Active controller message loop ──
    buf = b""
    try:
        while True:
            chunk = await reader.read(65536)
            if not chunk:
                break
            buf += chunk
            while buf:
                msg, buf = ws_decode(buf)
                if msg is None:
                    break
                if msg == "__close__":
                    writer.write(ws_close())
                    await writer.drain()
                    return
                if msg == "__ping__":
                    writer.write(ws_pong())
                    await writer.drain()
                    continue
                try:
                    data = json.loads(msg)
                except json.JSONDecodeError:
                    continue
                act = data.get("a")
                print(f"[ws] Received: {act} from {addr[0]}:{addr[1]}", flush=True)
                # Handle approval response from current controller
                if act == "approval_resp":
                    print(f"[ws] Processing approval_resp: {data.get('r')}", flush=True)
                    if approval_fut and not approval_fut.done():
                        approval_fut.set_result(data.get("r", "reject"))
                    continue
                # Ignore commands while approval dialog is open
                if approval_fut and not approval_fut.done():
                    continue
                if act == "mv":
                    xdo.mouse_move(data["x"], data["y"])
                elif act == "clk":
                    xdo.mouse_click(data.get("b", 1))
                elif act == "dbl":
                    xdo.mouse_double_click()
                elif act == "md":
                    xdo.mouse_down(data.get("b", 1))
                elif act == "mu":
                    xdo.mouse_up(data.get("b", 1))
                elif act == "scr":
                    xdo.mouse_scroll(data.get("y", 0))
                elif act == "type":
                    await xdo.type_text(data.get("t", ""))
                elif act == "key":
                    xdo.send_key(data.get("k", ""))
                elif act == "bs":
                    n = data.get("n", 1)
                    xdo._write(f"key --clearmodifiers --repeat {n} BackSpace")

    except (ConnectionResetError, BrokenPipeError, OSError):
        pass
    finally:
        # Clear state if this connection is the active/pending one
        if active_ws is not None and active_ws[0] is writer:
            active_ws = None
        if pending_ws is not None and pending_ws[0] is writer:
            pending_ws = None
        # Resolve pending approval as timeout if active controller disconnects
        if approval_fut and not approval_fut.done():
            approval_fut.set_result("timeout")
        print(f"[ws] Client disconnected: {addr[0]}:{addr[1]}", flush=True)
        if gui_event_queue is not None:
            gui_event_queue.put(f"❌ {addr[0]}:{addr[1]}  已断开")
        try:
            writer.close()
        except Exception:
            pass

# ─── Main ─────────────────────────────────────────────────────

async def main(stop_event=None):
    result = subprocess.run(["which", "xdotool"], capture_output=True)
    if result.returncode != 0:
        print("❌ xdotool not found! Install: apt install xdotool")
        sys.exit(1)

    # Check for xclip (used for clipboard paste, more reliable for CJK)
    result_clip = subprocess.run(["which", "xclip"], capture_output=True)
    if result_clip.returncode != 0:
        print("⚠️  xclip not found! Chinese input may not work correctly.")
        print("   Install with: apt install xclip")

    await xdo.start()

    local_ip = get_local_ip()

    print(f"""
╔══════════════════════════════════════════╗
║      🖱️  Remote Touchpad Server          ║
╠══════════════════════════════════════════╣
║                                          ║
║  Open on your phone:                     ║
║                                          ║
║  👉  http://{local_ip}:{PORT}  👈        ║
║                                          ║
╚══════════════════════════════════════════╝
""")

    server = await asyncio.start_server(
        handle_connection, HOST, PORT,
    )

    print("✅ Running! Press Ctrl+C to stop.\n")

    if stop_event is None:
        stop_event = asyncio.Event()
        loop = asyncio.get_running_loop()
        def on_signal():
            print("\n🛑 Shutting...")
            stop_event.set()
        for sig in (signal.SIGINT, signal.SIGTERM):
            try:
                loop.add_signal_handler(sig, on_signal)
            except (NotImplementedError, OSError):
                pass

    async with server:
        await stop_event.wait()

    xdo.close()
    if not hasattr(stop_event, '_gui_mode'):
        print("👋 Stopped.")

    return server

if __name__ == "__main__":
    auto_start = "--auto" in sys.argv or "--cli" not in sys.argv
    cli_mode = "--cli" in sys.argv

    if cli_mode:
        # CLI mode: skip GUI entirely
        asyncio.run(main())
    else:
        # ─── GUI Mode ──────────────────────────────────────────────
        try:
            import tkinter as tk
            from tkinter import scrolledtext
            import io
        except ImportError:
            print("ℹ️  tkinter not available, running in CLI mode.")
            asyncio.run(main())
            sys.exit(0)

        try:
            import segno
            HAS_QR = True
        except ImportError:
            HAS_QR = False
        try:
            from PIL import Image, ImageTk
            HAS_PIL = True
        except ImportError:
            HAS_PIL = False

        class TouchpadGUI:
            """Tkinter GUI for the Remote Touchpad server."""

            def __init__(self):
                self.root = tk.Tk()
                self.root.title("Remote Touchpad")
                self.root.geometry("380x520")
                self.root.resizable(False, False)

                self.server_running = False
                self.server_loop = None
                self.server_thread = None
                self.server_stop = None
                self.event_queue = queue.Queue()

                self._build_ui()

            def _build_ui(self):
                root = self.root

                # Title
                tk.Label(root, text="🖱️ Remote Touchpad",
                         font=("Arial", 16, "bold")).pack(pady=(12, 6))

                # Port config row
                port_frame = tk.Frame(root)
                port_frame.pack(pady=4)
                tk.Label(port_frame, text="端口:", font=("Arial", 11)).pack(side=tk.LEFT)
                self.port_var = tk.StringVar(value=str(PORT))
                self.port_entry = tk.Entry(port_frame, textvariable=self.port_var,
                                           width=8, font=("Arial", 11))
                self.port_entry.pack(side=tk.LEFT, padx=(4, 12))

                self.toggle_btn = tk.Button(port_frame, text="▶ 启动服务器",
                                            font=("Arial", 11, "bold"),
                                            width=14, command=self._toggle_server)
                self.toggle_btn.pack(side=tk.LEFT)

                # Connection info
                self.info_var = tk.StringVar(value="")
                self.info_label = tk.Label(root, textvariable=self.info_var,
                                           font=("Arial", 11), fg="#4ecca3",
                                           cursor="hand2")
                self.info_label.pack(pady=(8, 2))
                self.info_label.bind("<Button-1>", self._copy_url)

                # QR code
                self.qr_label = tk.Label(root, text="")
                self.qr_label.pack(pady=4)

                # Hint
                self.hint_var = tk.StringVar(value="")
                tk.Label(root, textvariable=self.hint_var,
                         font=("Arial", 9), fg="#888").pack()

                # Connected devices
                tk.Label(root, text="已连接设备:", font=("Arial", 11, "bold"),
                         anchor="w").pack(fill=tk.X, padx=16, pady=(12, 2))

                self.clients_text = scrolledtext.ScrolledText(
                    root, height=8, font=("Consolas", 10),
                    state=tk.DISABLED, bg="#1a1a2e", fg="#eee",
                    insertbackground="#eee", relief=tk.FLAT, bd=4
                )
                self.clients_text.pack(fill=tk.BOTH, expand=True, padx=12, pady=(0, 12))

            def _get_local_ip(self):
                return get_local_ip()

            def _generate_qr(self, url):
                """Generate QR code image for tkinter display."""
                if not HAS_QR or not HAS_PIL:
                    return None
                try:
                    qr = segno.make(url)
                    buf = io.BytesIO()
                    qr.save(buf, kind="png", scale=6, border=2,
                            dark="#4ecca3", light="#1a1a2e")
                    buf.seek(0)
                    img = Image.open(buf)
                    return ImageTk.PhotoImage(img)
                except Exception:
                    return None

            def _toggle_server(self):
                if not self.server_running:
                    self._start_server()
                else:
                    self._stop_server()

            def _start_server(self):
                global PORT, gui_event_queue
                try:
                    port = int(self.port_var.get())
                    if not (1 <= port <= 65535):
                        raise ValueError
                except ValueError:
                    self.info_var.set("❌ 端口无效")
                    return

                PORT = port
                gui_event_queue = self.event_queue
                self.server_stop = asyncio.Event()
                self.server_stop._gui_mode = True

                self.server_thread = threading.Thread(
                    target=self._run_server, daemon=True
                )
                self.server_thread.start()

                self.server_running = True
                self.toggle_btn.config(text="⏹ 停止服务器", bg="#e23e57", fg="white")
                self.port_entry.config(state=tk.DISABLED)

                local_ip = self._get_local_ip()
                url = f"http://{local_ip}:{PORT}"
                self.info_var.set(f"📱 {url}")
                self.hint_var.set("手机扫描下方二维码或输入地址连接")

                qr_img = self._generate_qr(url)
                if qr_img:
                    self._qr_img = qr_img  # prevent GC
                    self.qr_label.config(image=qr_img)
                else:
                    self.qr_label.config(text="(安装 segno 和 Pillow 以显示二维码)")

                # Start polling for connection events
                self.root.after(200, self._poll_events)

            def _run_server(self):
                self.server_loop = asyncio.new_event_loop()
                asyncio.set_event_loop(self.server_loop)
                try:
                    self.server_loop.run_until_complete(
                        main(self.server_stop)
                    )
                except Exception:
                    pass
                finally:
                    try:
                        self.server_loop.close()
                    except Exception:
                        pass

            def _stop_server(self):
                global gui_event_queue
                if self.server_stop:
                    self.server_stop.set()
                if self.server_loop and self.server_loop.is_running():
                    self.server_loop.call_soon_threadsafe(self.server_loop.stop)
                if self.server_thread:
                    self.server_thread.join(timeout=3)
                gui_event_queue = None
                self.server_running = False
                self.toggle_btn.config(text="▶ 启动服务器",
                                       bg=self.root.cget("bg"), fg="black")
                self.port_entry.config(state=tk.NORMAL)
                self.info_var.set("服务器已停止")
                self.hint_var.set("")
                self.qr_label.config(image="", text="")
                self._qr_img = None

            def _poll_events(self):
                """Poll connection events from the server thread."""
                count = 0
                while not self.event_queue.empty():
                    try:
                        msg = self.event_queue.get_nowait()
                        self.clients_text.config(state=tk.NORMAL)
                        self.clients_text.insert(tk.END, msg + "\n")
                        self.clients_text.see(tk.END)
                        self.clients_text.config(state=tk.DISABLED)
                        count += 1
                    except queue.Empty:
                        break
                if count > 0:
                    print(f"[gui] Polled {count} event(s)", flush=True)
                if self.server_running:
                    self.root.after(300, self._poll_events)

            def _copy_url(self, event=None):
                url = self.info_var.get().replace("📱 ", "")
                if url and "http" in url:
                    self.root.clipboard_clear()
                    self.root.clipboard_append(url)

            def run(self):
                self.root.protocol("WM_DELETE_WINDOW", self._on_close)
                self.root.mainloop()

            def _on_close(self):
                if self.server_running:
                    self._stop_server()
                self.root.destroy()

        gui = TouchpadGUI()
        if auto_start:
            gui.root.after(500, gui._toggle_server)
        gui.run()
