// ─── Connection ──────────────────────────────────────────
const $ = id => document.getElementById(id);
let ws, reconn, hasControl = false, hasEverControlled = false;
const st = $('status');

function connect() {
  ws = new WebSocket(`ws://${location.host}/ws`);
  ws.onopen = () => { st.textContent = '✅ 已连接'; st.className = 'ok'; clearTimeout(reconn) };
  ws.onclose = e => {
    hasControl = false;
    $('approval-overlay').classList.remove('show');
    if (e.code === 4001) { st.textContent = '🔄 已被新设备接管'; st.className = 'err'; return; }
    if (e.code === 4002) { return; }
    if (e.code !== 1000 && e.code !== 1001) {
      st.textContent = '❌ 已断开'; st.className = 'err';
      if (hasEverControlled) reconn = setTimeout(connect, 2000);
    }
  };
  ws.onerror = () => ws.close();
  ws.onmessage = e => {
    let d; try { d = JSON.parse(e.data) } catch { return };
    if (d.a === 'ctrl_ok') {
      hasControl = true; hasEverControlled = true;
      st.textContent = '✅ 已连接（控制中）'; st.className = 'ok';
    } else if (d.a === 'wait') {
      hasControl = false;
      if (d.reason === 'timeout') { st.textContent = '⏰ 等待超时'; st.className = 'err'; }
      else if (d.reason === 'rejected') { st.textContent = '🚫 被拒绝'; st.className = 'err'; }
      else if (d.reason === 'busy') { st.textContent = '⏳ 已有其他设备在等待'; st.className = 'err'; }
      else { st.textContent = '⏳ 等待当前设备同意...'; st.className = 'ok'; }
    } else if (d.a === 'approval_req') {
      $('approval-info').textContent = `${d.ip} 正在尝试接管控制权`;
      $('approval-overlay').classList.add('show');
    }
  };
}

function S(d) {
  if (!hasControl && d.a !== 'approval_resp') return;
  if (ws && ws.readyState === 1) ws.send(JSON.stringify(d));
}

function approvalResp(r) {
  S({ a: 'approval_resp', r });
  $('approval-overlay').classList.remove('show');
}

// ─── Sent indicator ─────────────────────────────────────
let sentTimer;
function flash() {
  const el = $('sent'); el.classList.add('show');
  clearTimeout(sentTimer); sentTimer = setTimeout(() => el.classList.remove('show'), 600);
}

// ─── Keyboard toggle ────────────────────────────────────
const txt = $('txt');
const kbBtn = $('kb-btn');
let kbVisible = false;

function toggleKb() {
  if (kbVisible) { txt.blur(); kbVisible = false; kbBtn.classList.remove('active'); }
  else { txt.focus(); kbVisible = true; kbBtn.classList.add('active'); }
}

txt.addEventListener('blur', () => {
  clearTimeout(debounce);
  if (compositionActive) { txt.value = lastVal; compositionActive = false; }
  else {
    const v = txt.value, newText = v.slice(lastVal.length);
    if (newText) { S({ a: 'type', t: newText }); flash(); }
    lastVal = v;
  }
  kbVisible = false; kbBtn.classList.remove('active');
});

// ─── Real-time input ────────────────────────────────────
let lastVal = '', debounce, compositionActive = false;

txt.addEventListener('compositionstart', () => { compositionActive = true });
txt.addEventListener('compositionend', () => { compositionActive = false });

txt.addEventListener('input', e => {
  if (e.isComposing) return;
  clearTimeout(debounce);
  debounce = setTimeout(() => {
    const v = txt.value, old = lastVal;
    if (v === old) return;
    if (v.length > old.length && v.startsWith(old)) {
      S({ a: 'type', t: v.slice(old.length) }); flash();
    } else if (v.length < old.length && old.startsWith(v)) {
      S({ a: 'bs', n: old.length - v.length }); flash();
    } else {
      S({ a: 'bs', n: old.length });
      if (v.length) { S({ a: 'type', t: v }); }
      flash();
    }
    lastVal = v;
  }, 30);
});

txt.addEventListener('compositionend', () => {
  setTimeout(() => {
    const v = txt.value, old = lastVal;
    if (v !== old) {
      if (v.length > old.length && v.startsWith(old)) {
        S({ a: 'type', t: v.slice(old.length) }); flash();
      } else if (v.length < old.length && old.startsWith(v)) {
        S({ a: 'bs', n: old.length - v.length }); flash();
      } else {
        S({ a: 'bs', n: old.length });
        if (v.length) { S({ a: 'type', t: v }); }
        flash();
      }
      lastVal = v;
    }
  }, 0);
});

// ─── Touchpad ────────────────────────────────────────────
const tp = $('touchpad');
let pts = {}, moved = false, scrolling = false, lastY = 0, tStart = 0;
let lastTapT = 0, lastTapX = 0, lastTapY = 0;
let pressing = false, pressTimer = null;
let twoFingerT = 0, twoFingerMoved = false;
const TH = 12, SENS = 6, SCR = .06;

let accDx = 0, accDy = 0, accScr = 0, mvDirty = false;
function flushMv() {
  if (mvDirty) {
    if (accDx || accDy) { S({ a: 'mv', x: accDx, y: accDy }); accDx = accDy = 0; }
    if (accScr) { S({ a: 'scr', y: accScr }); accScr = 0; }
    mvDirty = false;
  }
  requestAnimationFrame(flushMv);
}
requestAnimationFrame(flushMv);

tp.addEventListener('touchstart', e => {
  e.preventDefault();
  const now = Date.now();
  for (const t of e.changedTouches)
    pts[t.identifier] = { x: t.clientX, y: t.clientY, sx: t.clientX, sy: t.clientY };
  if (e.touches.length === 1) {
    moved = false; scrolling = false; tStart = now; pressing = false;
    clearTimeout(pressTimer);
    pressTimer = setTimeout(() => {
      if (!moved && !scrolling) {
        pressing = true; S({ a: 'md', b: 1 });
        $('scroll-tag').textContent = '✊ 长按拖动中';
        $('scroll-tag').style.display = 'block';
      }
    }, 500);
  }
  if (e.touches.length === 2) {
    scrolling = false; clearTimeout(pressTimer);
    twoFingerT = now; twoFingerMoved = false;
    $('scroll-tag').textContent = '⬆⬇ 滚动';
    $('scroll-tag').style.display = 'block';
    lastY = (e.touches[0].clientY + e.touches[1].clientY) / 2;
  }
}, { passive: false });

tp.addEventListener('touchmove', e => {
  e.preventDefault();
  if (e.touches.length === 1 && !scrolling) {
    const t = e.touches[0], p = pts[t.identifier]; if (!p) return;
    const dx = t.clientX - p.x, dy = t.clientY - p.y;
    if (Math.abs(t.clientX - p.sx) > TH || Math.abs(t.clientY - p.sy) > TH) {
      moved = true; if (!pressing) clearTimeout(pressTimer);
    }
    accDx += dx * SENS; accDy += dy * SENS; mvDirty = true;
    p.x = t.clientX; p.y = t.clientY;
  }
  if (e.touches.length >= 2) {
    scrolling = true; twoFingerMoved = true;
    $('scroll-tag').textContent = '⬆⬇ 滚动';
    const ay = (e.touches[0].clientY + e.touches[1].clientY) / 2;
    const d = lastY - ay;
    if (Math.abs(d) > 1.5) { accScr += Math.round(d * SCR); mvDirty = true; lastY = ay; }
  }
}, { passive: false });

tp.addEventListener('touchend', e => {
  e.preventDefault(); const now = Date.now();
  clearTimeout(pressTimer);

  if (pressing) {
    pressing = false; S({ a: 'mu', b: 1 });
    $('scroll-tag').style.display = 'none';
    for (const t of e.changedTouches) delete pts[t.identifier];
    if (e.touches.length === 0) { scrolling = false; twoFingerT = 0; }
    return;
  }

  if (e.touches.length === 0 && twoFingerT && !twoFingerMoved && now - twoFingerT < 200) {
    S({ a: 'clk', b: 3 });
    const t = e.changedTouches[0];
    if (t) rip(t.clientX, t.clientY, '#533483');
    twoFingerT = 0; scrolling = false;
    $('scroll-tag').style.display = 'none';
    for (const ct of e.changedTouches) delete pts[ct.identifier];
    return;
  }

  for (const t of e.changedTouches) {
    const p = pts[t.identifier]; if (!p) continue;
    if (!moved && !scrolling && e.touches.length === 0 && now - tStart < 250) {
      const dt = now - lastTapT, dd = Math.hypot(t.clientX - lastTapX, t.clientY - lastTapY);
      if (dt < 350 && dd < 50) { S({ a: 'dbl' }); rip(t.clientX, t.clientY, '#e23e57'); lastTapT = 0; }
      else { S({ a: 'clk', b: 1 }); rip(t.clientX, t.clientY, '#4ecca3'); lastTapT = now; lastTapX = t.clientX; lastTapY = t.clientY; }
    }
    delete pts[t.identifier];
  }
  if (e.touches.length === 0) { scrolling = false; twoFingerT = 0; $('scroll-tag').style.display = 'none'; }
}, { passive: false });

function rip(x, y, c) {
  const r = document.createElement('div'); r.className = 'ripple';
  const b = tp.getBoundingClientRect();
  r.style.left = (x - b.left) + 'px'; r.style.top = (y - b.top) + 'px';
  const m = /^#(..)(..)(..)$/.exec(c);
  r.style.background = `rgba(${parseInt(m[1], 16)},${parseInt(m[2], 16)},${parseInt(m[3], 16)},.4)`;
  tp.appendChild(r); setTimeout(() => r.remove(), 400);
}

document.body.addEventListener('touchmove', e => e.preventDefault(), { passive: false });

// ─── Function Keys Drawer ───────────────────────────────
const fkBtn = $('fk-btn');
const fkDrawer = $('fk-drawer');
const fkOverlay = $('fk-overlay');
let drawerOpen = false;

// Modifier lock state
const modState = { ctrl: false, shift: false, alt: false };

function toggleDrawer() {
  drawerOpen = !drawerOpen;
  fkDrawer.classList.toggle('open', drawerOpen);
  fkOverlay.classList.toggle('show', drawerOpen);
  fkBtn.classList.toggle('active', drawerOpen);
}

// Build modifier prefix from locked state
function getModPrefix() {
  let prefix = '';
  if (modState.ctrl) prefix += 'ctrl+';
  if (modState.shift) prefix += 'shift+';
  if (modState.alt) prefix += 'alt+';
  return prefix;
}

// Send a key with optional locked modifiers prepended
function sendKey(key) {
  const fullKey = getModPrefix() + key;
  S({ a: 'key', k: fullKey });
  flash();
}

// Single keys
document.querySelectorAll('.fk-btn[data-key]:not(.combo)').forEach(btn => {
  btn.addEventListener('click', () => {
    const key = btn.dataset.key;
    sendKey(key);
  });
});

// Modifier lock buttons
document.querySelectorAll('.fk-btn.modifier').forEach(btn => {
  btn.addEventListener('click', () => {
    const mod = btn.dataset.mod;
    modState[mod] = !modState[mod];
    btn.classList.toggle('active-mod', modState[mod]);
  });
});

// Combo keys (ignore locked modifiers, send the combo as-is)
document.querySelectorAll('.fk-btn.combo').forEach(btn => {
  btn.addEventListener('click', () => {
    const key = btn.dataset.key;
    S({ a: 'key', k: key });
    flash();
  });
});

// Prevent touch events inside drawer from reaching touchpad
fkDrawer.addEventListener('touchstart', e => e.stopPropagation(), { passive: true });
fkDrawer.addEventListener('touchmove', e => e.stopPropagation(), { passive: true });
fkDrawer.addEventListener('touchend', e => e.stopPropagation(), { passive: true });

// ─── Init ────────────────────────────────────────────────
connect();
