//! macOS IME handler via Carbon/Cocoa frameworks.
//!
//! - `get_ime_status()`: uses `TISCopyCurrentKeyboardInputSource` to read the
//!   current keyboard input source ID. If it contains pinyin/wubi/shuangpin/etc.
//!   we判定为中文模式.
//! - `toggle_ime()`: simulates CapsLock via CGEvent (default macOS IME toggle key).

use tracing::{info, warn};

use super::PlatformHandler;

// ─── Carbon FFI ──────────────────────────────────────────
#[repr(C)]
pub struct __TISInputSource;
pub type TISInputSourceRef = *const __TISInputSource;
pub type CFStringRef = *const std::ffi::c_void;
pub type CFTypeRef = *const std::ffi::c_void;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn TISCopyCurrentKeyboardInputSource() -> TISInputSourceRef;
    fn TISGetInputSourceProperty(
        inputSource: TISInputSourceRef,
        propertyKey: CFStringRef,
    ) -> CFTypeRef;
    fn CFRelease(cf: CFTypeRef);
}

// ─── CoreGraphics FFI (for CGEvent key simulation) ──────
#[repr(C)]
pub struct __CGEvent;
pub type CGEventRef = *const __CGEvent;
pub type CGEventSourceRef = *const std::ffi::c_void;
pub type CGKeyCode = u16;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        virtualKey: CGKeyCode,
        keyDown: bool,
    ) -> CGEventRef;
    fn CGEventPost(tapPoint: u32, event: CGEventRef);
}

const KCG_HID_EVENT_TAP: u32 = 0;
const K_CAPS_LOCK: CGKeyCode = 0x39;

/// Map common key name strings to macOS virtual key codes (CGKeyCode).
fn key_name_to_cgkeycode(name: &str) -> Option<CGKeyCode> {
    let lower = name.trim().to_lowercase();
    match lower.as_str() {
        "shift" | "lshift" => Some(0x38),      // kVK_Shift
        "rshift" => Some(0x3C),                 // kVK_RightShift
        "ctrl" | "lctrl" | "control" => Some(0x3B), // kVK_Control
        "rctrl" => Some(0x3E),                  // kVK_RightControl
        "alt" | "option" | "lalt" => Some(0x3A), // kVK_Option
        "roption" | "ralt" => Some(0x3D),       // kVK_RightOption
        "cmd" | "command" | "meta" => Some(0x37), // kVK_Command
        "space" => Some(0x31),                  // kVK_Space
        "capslock" | "caps_lock" => Some(0x39), // kVK_CapsLock
        "tab" => Some(0x30),
        "enter" | "return" => Some(0x24),
        "escape" | "esc" => Some(0x35),
        "backspace" => Some(0x33),
        "delete" | "del" => Some(0x75),
        "insert" | "ins" => Some(0x72),         // kVK_Help
        "home" => Some(0x73),
        "end" => Some(0x77),
        "pageup" | "page_up" => Some(0x74),
        "pagedown" | "page_down" => Some(0x79),
        "up" => Some(0x7E),
        "down" => Some(0x7D),
        "left" => Some(0x7B),
        "right" => Some(0x7C),
        "f1" => Some(0x7A), "f2" => Some(0x78), "f3" => Some(0x63),
        "f4" => Some(0x76), "f5" => Some(0x60), "f6" => Some(0x61),
        "f7" => Some(0x62), "f8" => Some(0x64), "f9" => Some(0x65),
        "f10" => Some(0x6D), "f11" => Some(0x67), "f12" => Some(0x6F),
        // Single letters a-z
        c if c.len() == 1 => {
            let ch = c.chars().next().unwrap();
            match ch {
                'a' => Some(0x00), 'b' => Some(0x0B), 'c' => Some(0x08),
                'd' => Some(0x02), 'e' => Some(0x0E), 'f' => Some(0x03),
                'g' => Some(0x05), 'h' => Some(0x04), 'i' => Some(0x22),
                'j' => Some(0x26), 'k' => Some(0x28), 'l' => Some(0x25),
                'm' => Some(0x2E), 'n' => Some(0x2D), 'o' => Some(0x1F),
                'p' => Some(0x23), 'q' => Some(0x0C), 'r' => Some(0x0F),
                's' => Some(0x01), 't' => Some(0x11), 'u' => Some(0x20),
                'v' => Some(0x09), 'w' => Some(0x0D), 'x' => Some(0x07),
                'y' => Some(0x10), 'z' => Some(0x06),
                ' ' => Some(0x31),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Simulate a key combo via CGEvent. Keys is a "+" separated string like "ctrl+space".
fn send_cgkey_combo(keys: &str) {
    let parts: Vec<&str> = keys.split('+').map(|s| s.trim()).collect();
    let mut keycodes: Vec<CGKeyCode> = Vec::new();
    for part in &parts {
        if let Some(kc) = key_name_to_cgkeycode(part) {
            keycodes.push(kc);
        } else {
            warn!("Unknown key name '{}' in combo '{}', skipping", part, keys);
        }
    }
    if keycodes.is_empty() {
        warn!("No valid keys in combo '{}'", keys);
        return;
    }

    unsafe {
        // Key down in order
        for &kc in &keycodes {
            let ev = CGEventCreateKeyboardEvent(std::ptr::null(), kc, true);
            if !ev.is_null() {
                CGEventPost(KCG_HID_EVENT_TAP, ev);
                CFRelease(ev as CFTypeRef);
            }
        }
        // Key up in reverse order
        for &kc in keycodes.iter().rev() {
            let ev = CGEventCreateKeyboardEvent(std::ptr::null(), kc, false);
            if !ev.is_null() {
                CGEventPost(KCG_HID_EVENT_TAP, ev);
                CFRelease(ev as CFTypeRef);
            }
        }
    }
    info!("CGEvent: simulated key combo '{}'", keys);
}

// kTISPropertyInputSourceID — we need the actual CFString pointer.
// Rather than linking Carbon's constant, we create it at runtime.
extern "C" {
    fn CFStringCreateWithCString(
        alloc: *const std::ffi::c_void,
        cStr: *const std::ffi::c_char,
        encoding: u32,
    ) -> CFStringRef;
}

const K_CF_STRING_ENCODING_UTF8: u32 = 0x08000100;

/// Helper to get a CFString property from a TISInputSource.
unsafe fn get_tis_string_property(
    source: TISInputSourceRef,
    property_name: &str,
) -> Option<String> {
    let c_name = std::ffi::CString::new(property_name).ok()?;
    let cf_key = CFStringCreateWithCString(
        std::ptr::null(),
        c_name.as_ptr(),
        K_CF_STRING_ENCODING_UTF8,
    );
    if cf_key.is_null() {
        return None;
    }
    let value = TISGetInputSourceProperty(source, cf_key);
    // We don't release cf_key since it's a constant-like usage

    if value.is_null() {
        return None;
    }

    // The value is a CFStringRef — convert to Rust String
    // Use CFStringGetCString via a small helper
    cf_string_to_rust(value as CFStringRef)
}

/// Convert a CFStringRef to a Rust String.
unsafe fn cf_string_to_rust(cf_str: CFStringRef) -> Option<String> {
    extern "C" {
        fn CFStringGetLength(theString: CFStringRef) -> i64;
        fn CFStringGetCString(
            theString: CFStringRef,
            buffer: *mut std::ffi::c_char,
            bufferSize: i64,
            encoding: u32,
        ) -> bool;
    }

    if cf_str.is_null() {
        return None;
    }

    let len = CFStringGetLength(cf_str);
    if len <= 0 {
        return None;
    }

    // Allocate buffer (len * 4 for UTF-8 worst case + 1 for null)
    let buf_len = (len * 4 + 1) as usize;
    let mut buf = vec![0u8; buf_len];
    let ok = CFStringGetCString(
        cf_str,
        buf.as_mut_ptr() as *mut std::ffi::c_char,
        buf_len as i64,
        K_CF_STRING_ENCODING_UTF8,
    );
    if !ok {
        return None;
    }

    // Find null terminator
    let null_pos = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8(buf[..null_pos].to_vec()).ok()
}

pub struct MacosHandler;

impl PlatformHandler for MacosHandler {
    fn get_ime_status(&self) -> String {
        unsafe {
            let source = TISCopyCurrentKeyboardInputSource();
            if source.is_null() {
                warn!("TISCopyCurrentKeyboardInputSource returned null, defaulting EN");
                return "EN".to_string();
            }

            let source_id = get_tis_string_property(
                source,
                "TISPropertyInputSourceID",
            );
            CFRelease(source as CFTypeRef);

            match source_id {
                Some(id) => {
                    let id_lower = id.to_lowercase();
                    info!("macOS input source: {}", id);
                    // Check if this is a CJK input method
                    if id_lower.contains("pinyin")
                        || id_lower.contains("wubi")
                        || id_lower.contains("shuangpin")
                        || id_lower.contains("cangjie")
                        || id_lower.contains("zhuyin")
                        || id_lower.contains("rime")
                        || id_lower.contains("hangul")
                        || id_lower.contains("kana")
                        || id_lower.contains("simplified")
                        || id_lower.contains("tradition")
                    {
                        "ZH".to_string()
                    } else {
                        "EN".to_string()
                    }
                }
                None => {
                    warn!("Could not read input source ID, defaulting EN");
                    "EN".to_string()
                }
            }
        }
    }

    fn toggle_ime(&self, custom_keys: Option<&str>) {
        if let Some(keys) = custom_keys {
            send_cgkey_combo(keys);
            return;
        }

        // Default on macOS: simulate CapsLock press+release
        unsafe {
            let key_down = CGEventCreateKeyboardEvent(
                std::ptr::null(),
                K_CAPS_LOCK,
                true,
            );
            if !key_down.is_null() {
                CGEventPost(KCG_HID_EVENT_TAP, key_down);
                CFRelease(key_down as CFTypeRef);
            }

            let key_up = CGEventCreateKeyboardEvent(
                std::ptr::null(),
                K_CAPS_LOCK,
                false,
            );
            if !key_up.is_null() {
                CGEventPost(KCG_HID_EVENT_TAP, key_up);
                CFRelease(key_up as CFTypeRef);
            }
        }
        info!("macOS IME toggle: simulated CapsLock");
    }
}
