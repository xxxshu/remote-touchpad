//! Linux IME handler via fcitx-remote / ibus.
//!
//! - `get_ime_status()`: calls `fcitx-remote` — exit code 1 = inactive (EN),
//!   2 = active (ZH). Falls back to `ibus engine` query.
//! - `toggle_ime()`: calls `fcitx-remote -t` or uses xdotool for custom keys.

use tracing::{info, warn};

use super::PlatformHandler;

pub struct LinuxHandler;

impl PlatformHandler for LinuxHandler {
    fn get_ime_status(&self) -> String {
        // Try fcitx-remote first (works for both fcitx4 and fcitx5)
        if let Some(status) = try_fcitx_remote() {
            return status;
        }
        // Fallback: try ibus
        if let Some(status) = try_ibus() {
            return status;
        }
        // Fallback: assume EN if no IME daemon detected
        warn!("No IME daemon detected (fcitx/ibus), defaulting to EN");
        "EN".to_string()
    }

    fn toggle_ime(&self, custom_keys: Option<&str>) {
        if let Some(keys) = custom_keys {
            // Use xdotool to simulate custom key combo
            let status = std::process::Command::new("xdotool")
                .args(["key", keys])
                .status();
            match status {
                Ok(s) if s.success() => info!("IME toggle via custom keys: {}", keys),
                Ok(_) => warn!("xdotool key '{}' returned non-zero", keys),
                Err(e) => warn!("xdotool key '{}' failed: {}", keys, e),
            }
        } else {
            // Default: use fcitx-remote -t to toggle
            let status = std::process::Command::new("fcitx-remote")
                .args(["-t"])
                .status();
            match status {
                Ok(s) if s.success() => info!("IME toggled via fcitx-remote -t"),
                Ok(_) => {
                    // fcitx-remote not available, try xdotool with Shift as fallback
                    warn!("fcitx-remote -t failed, trying xdotool Shift");
                    let _ = std::process::Command::new("xdotool")
                        .args(["key", "Shift_L"])
                        .status();
                }
                Err(e) => {
                    warn!("fcitx-remote not found: {}, trying xdotool Shift", e);
                    let _ = std::process::Command::new("xdotool")
                        .args(["key", "Shift_L"])
                        .status();
                }
            }
        }
    }
}

/// Try `fcitx-remote` to get IME status.
/// Returns Some("ZH") if active, Some("EN") if inactive, None if fcitx not available.
fn try_fcitx_remote() -> Option<String> {
    let output = std::process::Command::new("fcitx-remote")
        .output()
        .ok()?;

    // fcitx-remote returns:
    //   0  = fcitx not running or connection failed
    //   1  = inactive (English mode)
    //   2  = active (Chinese mode)
    //   >= 3 also means active with different sub-states
    match output.status.code() {
        Some(0) => {
            // fcitx not running
            None
        }
        Some(1) => {
            info!("fcitx-remote: IME inactive (EN)");
            Some("EN".to_string())
        }
        Some(code) if code >= 2 => {
            info!("fcitx-remote: IME active (ZH), code={}", code);
            Some("ZH".to_string())
        }
        _ => None,
    }
}

/// Try `ibus engine` to determine if a CJK input method is active.
fn try_ibus() -> Option<String> {
    let output = std::process::Command::new("ibus")
        .args(["engine"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let engine = String::from_utf8_lossy(&output.stdout).trim().to_lowercase();
    info!("ibus engine: {}", engine);

    // Common CJK engine names
    if engine.contains("pinyin")
        || engine.contains("wubi")
        || engine.contains("shuangpin")
        || engine.contains("cangjie")
        || engine.contains("zhuyin")
        || engine.contains("rime")
        || engine.contains("chinese")
        || engine.contains("hangul")
        || engine.contains("kana")
    {
        Some("ZH".to_string())
    } else {
        Some("EN".to_string())
    }
}
