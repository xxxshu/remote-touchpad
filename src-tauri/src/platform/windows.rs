//! Windows IME handler via Imm32 API.
//!
//! - `get_ime_status()`: uses `GetForegroundWindow` + `ImmGetContext` +
//!   `ImmGetConversionStatus` + `IME_CMODE_NATIVE` to precisely detect Chinese mode.
//! - `toggle_ime()`: simulates LShift via keybd_event (default), or custom keys via SendInput.

use tracing::{info, warn};

use super::PlatformHandler;

// ─── Windows FFI declarations ─────────────────────────────
type HIMC = isize;
type HWND = isize;

#[link(name = "user32")]
extern "system" {
    fn GetForegroundWindow() -> HWND;
    fn keybd_event(bVk: u8, bScan: u8, dwFlags: u32, dwExtraInfo: usize);
    fn SendInput(cInputs: u32, pInputs: *const INPUT, cbSize: i32) -> u32;
    fn MapVirtualKeyW(uCode: u32, uMapType: u32) -> u32;
}

#[link(name = "imm32")]
extern "system" {
    fn ImmGetContext(hWnd: HWND) -> HIMC;
    fn ImmReleaseContext(hWnd: HWND, hIMC: HIMC) -> i32;
    fn ImmGetOpenStatus(hIMC: HIMC) -> i32;
    fn ImmGetConversionStatus(hIMC: HIMC, lpfdwConversion: *mut u32, lpfdwSentence: *mut u32) -> i32;
}

// ─── SendInput FFI types ─────────────────────────────────
const INPUT_KEYBOARD: u32 = 1;
const KEYEVENTF_KEYUP: u32 = 0x0002;
const KEYEVENTF_EXTENDEDKEY: u32 = 0x0001;
const IME_CMODE_NATIVE: u32 = 0x0001;

// Extended keys that need KEYEVENTF_EXTENDEDKEY flag
const EXTENDED_KEYS: &[u8] = &[0x25, 0x26, 0x27, 0x28, // arrow keys
    0x24, 0x23, 0x2D, 0x2E, // Home, End, Insert, Delete
    0x5B, 0x5C, 0x5D,       // LWin, RWin, Apps
    0xA0, 0xA1, 0xA2, 0xA3, // LShift, RShift, LCtrl, RCtrl
    0xA4, 0xA5,             // LAlt, RAlt
    0x6A, 0x6B, 0x6F,       // Multiply, Add, Divide
    0x90,                    // NumLock
];

#[repr(C)]
#[derive(Clone, Copy)]
struct KEYBDINPUT {
    wVk: u16,
    wScan: u16,
    dwFlags: u32,
    time: u32,
    dwExtraInfo: usize,
}

#[repr(C)]
union INPUT_UNION {
    ki: std::mem::ManuallyDrop<KEYBDINPUT>,
    _pad: [u8; 24],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct INPUT {
    type_: u32,
    union_: INPUT_UNION,
}

/// Map common key name strings to Windows virtual key codes.
fn key_name_to_vk(name: &str) -> Option<u8> {
    let lower = name.trim().to_lowercase();
    match lower.as_str() {
        "shift" | "lshift" => Some(0xA0),
        "rshift" => Some(0xA1),
        "ctrl" | "lctrl" | "control" => Some(0xA2),
        "rctrl" => Some(0xA3),
        "alt" | "lalt" => Some(0xA4),
        "ralt" => Some(0xA5),
        "space" => Some(0x20),
        "capslock" | "caps_lock" => Some(0x14),
        "tab" => Some(0x09),
        "enter" | "return" => Some(0x0D),
        "escape" | "esc" => Some(0x1B),
        "backspace" => Some(0x08),
        "delete" | "del" => Some(0x2E),
        "insert" | "ins" => Some(0x2D),
        "home" => Some(0x24),
        "end" => Some(0x23),
        "pageup" | "page_up" => Some(0x21),
        "pagedown" | "page_down" => Some(0x22),
        "up" => Some(0x26),
        "down" => Some(0x28),
        "left" => Some(0x25),
        "right" => Some(0x27),
        "f1" => Some(0x70), "f2" => Some(0x71), "f3" => Some(0x72),
        "f4" => Some(0x73), "f5" => Some(0x74), "f6" => Some(0x75),
        "f7" => Some(0x76), "f8" => Some(0x77), "f9" => Some(0x78),
        "f10" => Some(0x79), "f11" => Some(0x7A), "f12" => Some(0x7B),
        // Single letters
        c if c.len() == 1 => {
            let ch = c.chars().next().unwrap();
            if ch.is_ascii_alphanumeric() || ch == ' ' {
                Some(ch.to_ascii_uppercase() as u8)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Build a SendInput KEYDOWN event for a given VK code.
fn make_key_input(vk: u8, key_up: bool) -> INPUT {
    let scan = unsafe { MapVirtualKeyW(vk as u32, 0) };
    let mut flags = if key_up { KEYEVENTF_KEYUP } else { 0 };
    if EXTENDED_KEYS.contains(&vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        type_: INPUT_KEYBOARD,
        union_: INPUT_UNION {
            ki: std::mem::ManuallyDrop::new(KEYBDINPUT {
                wVk: vk as u16,
                wScan: scan as u16,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            }),
        },
    }
}

/// Simulate a key combo via SendInput. Keys is a "+" separated string like "ctrl+space".
fn send_key_combo(keys: &str) {
    let parts: Vec<&str> = keys.split('+').map(|s| s.trim()).collect();
    let mut vks: Vec<u8> = Vec::new();
    for part in &parts {
        if let Some(vk) = key_name_to_vk(part) {
            vks.push(vk);
        } else {
            warn!("Unknown key name '{}' in combo '{}', skipping", part, keys);
        }
    }
    if vks.is_empty() {
        warn!("No valid keys in combo '{}'", keys);
        return;
    }

    // Build events: all key-down in order, then all key-up in reverse order
    let mut events: Vec<INPUT> = Vec::new();
    for &vk in &vks {
        events.push(make_key_input(vk, false));
    }
    for &vk in vks.iter().rev() {
        events.push(make_key_input(vk, true));
    }

    unsafe {
        SendInput(
            events.len() as u32,
            events.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        );
    }
    info!("SendInput: simulated key combo '{}'", keys);
}

pub struct WindowsHandler;

impl PlatformHandler for WindowsHandler {
    fn get_ime_status(&self) -> String {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd == 0 {
                warn!("GetForegroundWindow returned null, defaulting EN");
                return "EN".to_string();
            }
            let himc = ImmGetContext(hwnd);
            if himc == 0 {
                warn!("ImmGetContext returned null, defaulting EN");
                return "EN".to_string();
            }

            // Check if IME is open first
            let is_open = ImmGetOpenStatus(himc);
            if is_open == 0 {
                ImmReleaseContext(hwnd, himc);
                info!("Windows IME status: closed (EN)");
                return "EN".to_string();
            }

            // IME is open — check conversion mode for native (Chinese) mode
            let mut conversion: u32 = 0;
            let mut _sentence: u32 = 0;
            let ok = ImmGetConversionStatus(himc, &mut conversion, &mut _sentence);
            ImmReleaseContext(hwnd, himc);

            if ok != 0 && (conversion & IME_CMODE_NATIVE) != 0 {
                info!("Windows IME status: native mode (ZH), conversion=0x{:X}", conversion);
                "ZH".to_string()
            } else {
                info!("Windows IME status: non-native mode (EN), conversion=0x{:X}", conversion);
                "EN".to_string()
            }
        }
    }

    fn toggle_ime(&self, custom_keys: Option<&str>) {
        if let Some(keys) = custom_keys {
            send_key_combo(keys);
        } else {
            // Default: simulate LShift press+release
            // Most Chinese IMEs on Windows use Shift to toggle
            unsafe {
                keybd_event(0xA0, 0, 0, 0);             // LShift down
                keybd_event(0xA0, 0, KEYEVENTF_KEYUP, 0); // LShift up
            }
            info!("Windows IME toggle: simulated LShift");
        }
    }
}
