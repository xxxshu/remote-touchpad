//! Platform abstraction for IME status reading and toggle.
//!
//! Uses conditional compilation to select the correct backend at build time.
//! Each platform implements `PlatformHandler` with OS-specific IME APIs.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "macos")]
mod macos;

/// Unified interface for platform-specific IME operations.
pub trait PlatformHandler: Send + Sync {
    /// Read the current IME state from the OS.
    /// Returns `"ZH"` if a CJK input method is active, `"EN"` otherwise.
    fn get_ime_status(&self) -> String;

    /// Simulate a physical key press to toggle the IME.
    /// If `custom_keys` is provided (e.g. `"ctrl+space"` or `"shift"`),
    /// use that combo instead of the platform default.
    fn toggle_ime(&self, custom_keys: Option<&str>);
}

/// Create the platform-specific handler.
pub fn get_platform() -> Box<dyn PlatformHandler> {
    #[cfg(target_os = "linux")]
    { Box::new(linux::LinuxHandler) }

    #[cfg(target_os = "windows")]
    { Box::new(windows::WindowsHandler) }

    #[cfg(target_os = "macos")]
    { Box::new(macos::MacosHandler) }

    // Fallback for unsupported platforms (should not happen in practice)
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    { Box::new(FallbackHandler) }
}

/// Fallback handler for unsupported platforms — always returns EN, no-op toggle.
#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
struct FallbackHandler;

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
impl PlatformHandler for FallbackHandler {
    fn get_ime_status(&self) -> String { "EN".to_string() }
    fn toggle_ime(&self, _custom_keys: Option<&str>) {}
}
