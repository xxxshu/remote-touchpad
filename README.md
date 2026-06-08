# Remote Touchpad

> 同一局域网下，手机浏览器控制电脑的触控板 & 键盘工具。

---

## 需求总览

### Bug 修复（已完成）

| 问题 | 根因 | 方案 |
|------|------|------|
| 中文输入吞字 | `compositionstart/end` + `input` 事件竞态 | 改用 `e.isComposing` 判断，移除手动 `composing` 标志 |
| 中文输入错字 | `xdotool type` 被目标端 IME 二次处理 | 改用 `xclip` + `Ctrl+V` 剪贴板粘贴 |
| 输入延迟大 | debounce 80ms + 每次 spawn 新 xdotool 进程 | debounce 降至 30ms + 剪贴板粘贴 |

### 功能拓展

| 功能 | 状态 | 说明 |
|------|------|------|
| 扫码连接 | ✅ 完成 | Tauri GUI 显示二维码，手机扫码跳转浏览器 |
| 被控端 GUI | ✅ 完成 | 用 Tauri v2 替代 tkinter（当前在 Linux 有内存问题，Windows 应正常） |
| 前后端分离 | ✅ 完成 | `frontend/` 手机端页面，`src-tauri/` Rust 后端 + 桌面 GUI |
| 功能键抽屉 | ✅ 完成 | 侧边抽屉：ESC/Tab/方向键/锁定修饰键/组合键 |
| 连接审批 | ✅ 完成 | 新设备需当前控制器同意，拒绝后不重连 |

---

## 项目结构

```
remote-touchpad/
├── server.py                    # 旧版 Python 实现（完整可用，作为参考）
├── start.sh                     # 旧版启动脚本
├── requirements.txt             # Python 依赖
│
├── frontend/                    # 手机端 Web 前端（扫码后打开的页面）
│   ├── index.html               #   结构 + 功能键抽屉 + 审批弹窗
│   ├── style.css                #   样式（深色主题 + 抽屉动画）
│   └── app.js                   #   触控板手势 + 键盘 + 功能键 + 审批逻辑
│
├── src-tauri/                   # Rust 后端 + Tauri 桌面 GUI
│   ├── Cargo.toml               #   依赖: tokio, axum, tokio-tungstenite, tauri
│   ├── tauri.conf.json           #   Tauri 配置
│   ├── build.rs
│   ├── capabilities/
│   ├── icons/
│   ├── ui/
│   │   └── index.html           #   Tauri 窗口管理界面（端口/QR/设备列表）
│   └── src/
│       ├── main.rs              #   Tauri 入口
│       ├── lib.rs               #   Tauri commands (start/stop/status)
│       ├── server.rs            #   WebSocket + HTTP 服务器 (axum)
│       ├── input.rs             #   xdotool 输入模拟
│       └── protocol.rs          #   消息协议定义
│
├── build.spec                   # PyInstaller 打包配置（旧版 Python）
└── README.md                    # 本文件
```

---

## 当前进度

### ✅ 已完成

1. **旧版 Python 全功能可用**（`server.py`）
   - 中文输入修复（剪贴板粘贴）
   - 延迟优化
   - 连接审批流程
   - tkinter GUI + 二维码
   - 单设备控制 + 拒绝不重连

2. **Rust 后端已编译通过**
   - `src-tauri/target/debug/remote-touchpad` 二进制已生成（227MB debug）
   - tokio + axum + tokio-tungstenite WebSocket 服务器
   - xdotool 子进程输入模拟
   - 审批流程（active_ws / pending_ws + oneshot channel）

3. **前端已分离**
   - `frontend/` 包含完整的手机端页面
   - 功能键侧边抽屉 UI 已实现

4. **Tauri 桌面框架已搭建**
   - 窗口可以弹出
   - 管理界面 HTML 已编写（端口/QR/设备列表）

---

### ⚠️ 待解决（Windows 上继续）

#### 1. Tauri 窗口管理界面未正常加载

**问题**：Linux (aarch64 proot) 环境下内存不足（10GB），Rust 编译 Tauri 导致 OOM，Trae IDE 被杀。窗口弹出但内容可能为空。

**Windows 上应该没有此问题**（内存通常更充裕，WebView2 是原生的）。

**需要验证**：
```bash
cd src-tauri
cargo tauri dev
```
检查 Tauri 窗口是否正确显示 `src-tauri/ui/index.html` 的管理界面。

#### 2. Tauri 窗口管理界面 JS 需要调试

`src-tauri/ui/index.html` 中使用了 `window.__TAURI__.core.invoke()` 调用 Rust 命令。需要确认：
- `invoke('start_server_cmd', { port })` 是否正确启动服务器
- `invoke('get_status')` 是否返回状态和事件
- 二维码 SVG 是否正确渲染

#### 3. HTTP 服务器前端路径

`lib.rs` 中 `start_server_cmd` 查找 `frontend/` 目录的逻辑：
- `resource_dir/frontend/` → 打包后使用
- `exe_dir/../frontend/` → cargo run 时使用
- `cwd/../frontend/` → 备选

Windows 上需要验证路径是否正确。

#### 4. 审批流程在 Rust 中的实现需要端到端测试

`server.rs` 中的审批流程：
- 新设备连接 → 发送 `wait` → 通知当前控制器
- 当前控制器发送 `approval_resp` → 前端 `approval_req` 弹窗
- 需要完整测试：两台手机（或一台手机 + 电脑浏览器）

#### 5. 功能键抽屉需要真机测试

`frontend/app.js` 中的功能键：
- 单键：ESC/Tab/方向键 → `S({a:'key', k:'Escape'})`
- 修饰键锁定：Ctrl/Shift/Alt 点击切换状态
- 组合键：Ctrl+C/V/X/Z/A/S、Ctrl+/、Shift+Tab
- 需要验证锁定修饰键 + 单键的组合是否正确

#### 6. 打包发布

```bash
# Windows 打包
cd src-tauri
cargo tauri build
```

生成的安装包在 `src-tauri/target/release/bundle/`。

---

## 技术架构

```
┌─────────────────┐     HTTP/WS      ┌──────────────────┐
│   手机浏览器     │ ◄──────────────► │  Rust HTTP 服务器  │
│  (frontend/)    │    :8765 端口     │  (server.rs)     │
│                 │                   │                  │
│  触控板手势      │   JSON messages   │  axum + tokio    │
│  功能键抽屉      │ ──────────────►  │  xdotool 子进程   │
│  审批弹窗        │                   │  审批状态机       │
└─────────────────┘                   └──────────────────┘
                                              ▲
                                              │ Tauri commands
                                              │
                                      ┌───────┴────────┐
                                      │  Tauri 窗口     │
                                      │  (ui/index.html)│
                                      │                 │
                                      │  端口配置        │
                                      │  二维码显示      │
                                      │  设备列表        │
                                      └─────────────────┘
```

### WebSocket 消息协议

**客户端 → 服务器**：
| 动作 `a` | 字段 | 说明 |
|----------|------|------|
| `mv` | `x`, `y` | 鼠标移动（rAF 批处理） |
| `clk` | `b` (1/3) | 单击 (1=左, 3=右) |
| `dbl` | — | 双击 |
| `md` | `b` | 鼠标按下（长按拖动） |
| `mu` | `b` | 鼠标释放 |
| `scr` | `y` | 滚动 |
| `type` | `t` | 输入文字 |
| `key` | `k` | 发送按键（如 `ctrl+c`） |
| `bs` | `n` | 退格 N 次 |
| `approval_resp` | `r` | 审批响应 (`accept`/`reject`) |

**服务器 → 客户端**：
| 动作 `a` | 字段 | 说明 |
|----------|------|------|
| `ctrl_ok` | — | 获得控制权 |
| `wait` | `reason?` | 等待审批 (timeout/rejected/busy) |
| `approval_req` | `ip` | 新设备请求连接 |

**WebSocket 关闭码**：
| 代码 | 含义 |
|------|------|
| 4001 | 被新设备接管（不重连） |
| 4002 | 被拒绝/超时/忙线（不重连） |
| 其他 | 网络异常（曾有控制权则自动重连） |

---

## 运行方式

### 方式一：新版 Tauri（推荐）

```bash
# 安装 Rust: https://rustup.rs/
# 安装 Tauri 依赖: https://tauri.app/start/prerequisites/

cd src-tauri
cargo tauri dev          # 开发模式
cargo tauri build        # 打包发布
```

### 方式二：旧版 Python（Linux）

```bash
pip install segno Pillow netifaces
apt install xdotool xclip python3-tk

python3 server.py        # GUI 模式
python3 server.py --cli  # CLI 模式
python3 server.py --auto # GUI 自动启动服务器
```

---

## 环境要求

| 组件 | 说明 |
|------|------|
| Rust | rustc 1.96+ (https://rustup.rs/) |
| Tauri | v2 (cargo install tauri-cli) |
| 系统 | Windows 10/11 (WebView2 内置) / Linux (webkit2gtk-4.1) |
| 被控端 | xdotool (Linux) / 无需额外工具 (Windows，需改用 enigo) |

> **注意**：`input.rs` 当前使用 xdotool 子进程，仅支持 Linux。
> Windows 上需改用 [enigo](https://crates.io/crates/enigo) crate 进行输入模拟。
