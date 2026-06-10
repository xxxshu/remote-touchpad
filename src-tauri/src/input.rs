use enigo::{
    Button,
    Coordinate::Rel,
    Direction::{Click, Press, Release},
    Enigo, Keyboard, Mouse, Settings,
};
use std::sync::Mutex;
use anyhow::Result;
use tracing::info;

/// Cross-platform input simulator using enigo (works on Windows, macOS, Linux).
pub struct InputSimulator {
    enigo: Mutex<Option<Enigo>>,
}

impl InputSimulator {
    pub async fn new() -> Result<Self> {
        let enigo = tokio::task::spawn_blocking(|| {
            Enigo::new(&Settings::default())
        }).await
            .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {}", e))?
            .map_err(|e| anyhow::anyhow!("Failed to init enigo: {}", e))?;
        info!("Enigo input simulator initialized");
        Ok(Self {
            enigo: Mutex::new(Some(enigo)),
        })
    }

    /// Create without initializing enigo (will try lazy init on first use)
    pub fn new_lazy() -> Self {
        Self {
            enigo: Mutex::new(None),
        }
    }

    fn get_enigo(&self) -> Result<std::sync::MutexGuard<'_, Option<Enigo>>> {
        let guard = self.enigo.lock().unwrap();
        if guard.is_none() {
            drop(guard);
            let mut guard = self.enigo.lock().unwrap();
            if guard.is_none() {
                let enigo = Enigo::new(&Settings::default())
                    .map_err(|e| anyhow::anyhow!("Lazy enigo init failed: {}", e))?;
                *guard = Some(enigo);
                info!("Enigo input simulator initialized (lazy)");
            }
            Ok(guard)
        } else {
            Ok(guard)
        }
    }

    pub async fn mouse_move(&self, dx: f64, dy: f64) -> Result<()> {
        let mut guard = self.get_enigo()?;
        let e = guard.as_mut().unwrap();
        e.move_mouse(dx as i32, dy as i32, Rel)
            .map_err(|e| anyhow::anyhow!("mouse_move: {}", e))
    }

    pub async fn mouse_click(&self, button: u8) -> Result<()> {
        let btn = map_button(button);
        let mut guard = self.get_enigo()?;
        let e = guard.as_mut().unwrap();
        e.button(btn, Click)
            .map_err(|e| anyhow::anyhow!("mouse_click: {}", e))
    }

    pub async fn mouse_double_click(&self) -> Result<()> {
        let mut guard = self.get_enigo()?;
        let e = guard.as_mut().unwrap();
        e.button(Button::Left, Click)
            .map_err(|e| anyhow::anyhow!("dbl_click 1: {}", e))?;
        e.button(Button::Left, Click)
            .map_err(|e| anyhow::anyhow!("dbl_click 2: {}", e))
    }

    pub async fn mouse_down(&self, button: u8) -> Result<()> {
        let btn = map_button(button);
        let mut guard = self.get_enigo()?;
        let e = guard.as_mut().unwrap();
        e.button(btn, Press)
            .map_err(|e| anyhow::anyhow!("mouse_down: {}", e))
    }

    pub async fn mouse_up(&self, button: u8) -> Result<()> {
        let btn = map_button(button);
        let mut guard = self.get_enigo()?;
        let e = guard.as_mut().unwrap();
        e.button(btn, Release)
            .map_err(|e| anyhow::anyhow!("mouse_up: {}", e))
    }

    pub async fn mouse_scroll(&self, dx: f64, dy: f64) -> Result<()> {
        let ix = dx.round() as i32;
        let iy = dy.round() as i32;
        let mut guard = self.get_enigo()?;
        let e = guard.as_mut().unwrap();
        if iy != 0 {
            e.scroll(iy, enigo::Axis::Vertical)
                .map_err(|e| anyhow::anyhow!("scroll vertical: {}", e))?;
        }
        if ix != 0 {
            e.scroll(ix, enigo::Axis::Horizontal)
                .map_err(|e| anyhow::anyhow!("scroll horizontal: {}", e))?;
        }
        Ok(())
    }

    pub async fn mouse_zoom(&self, amount: f64) -> Result<()> {
        // Negate: positive amount (spread fingers = zoom in) → Ctrl+scroll-up (negative)
        let scroll_amount = -(amount * 10.0).round() as i32;
        if scroll_amount == 0 { return Ok(()); }
        let mut guard = self.get_enigo()?;
        let e = guard.as_mut().unwrap();
        // Ctrl+Scroll = zoom on most platforms (browsers, Office, etc.)
        e.key(enigo::Key::Control, enigo::Direction::Press)
            .map_err(|e| anyhow::anyhow!("zoom ctrl press: {}", e))?;
        e.scroll(scroll_amount, enigo::Axis::Vertical)
            .map_err(|e| anyhow::anyhow!("zoom scroll: {}", e))?;
        e.key(enigo::Key::Control, enigo::Direction::Release)
            .map_err(|e| anyhow::anyhow!("zoom ctrl release: {}", e))?;
        Ok(())
    }

    /// Send a key combo like "ctrl+c", "Escape", "shift+Tab".
    pub async fn send_key(&self, key: &str) -> Result<()> {
        let (modifiers, main_key) = parse_key_string(key);
        let mut guard = self.get_enigo()?;
        let e = guard.as_mut().unwrap();

        // Press modifiers
        for m in &modifiers {
            e.key(*m, Press)
                .map_err(|e| anyhow::anyhow!("mod press: {}", e))?;
        }

        // Press+release main key
        e.key(main_key, Click)
            .map_err(|e| anyhow::anyhow!("key click: {}", e))?;

        // Release modifiers in reverse order
        for m in modifiers.iter().rev() {
            e.key(*m, Release)
                .map_err(|e| anyhow::anyhow!("mod release: {}", e))?;
        }

        Ok(())
    }

    /// Press a key (keydown + keyup separately, not Click).
    /// This allows IMEs to intercept the key events for composition (e.g. Chinese pinyin).
    pub async fn press_key(&self, key: &str) -> Result<()> {
        let (modifiers, main_key) = parse_key_string(key);
        let mut guard = self.get_enigo()?;
        let e = guard.as_mut().unwrap();

        for m in &modifiers {
            e.key(*m, Press)
                .map_err(|e| anyhow::anyhow!("mod press: {}", e))?;
        }
        // Use Press then Release instead of Click — IME can intercept each phase
        e.key(main_key, Press)
            .map_err(|e| anyhow::anyhow!("key down: {}", e))?;
        e.key(main_key, Release)
            .map_err(|e| anyhow::anyhow!("key up: {}", e))?;
        for m in modifiers.iter().rev() {
            e.key(*m, Release)
                .map_err(|e| anyhow::anyhow!("mod release: {}", e))?;
        }
        Ok(())
    }

    /// Send a key combo using xdotool (Linux only).
    /// Converts frontend key names to xdotool keysym format.
    /// e.g. "Space" → "space", "," → "comma", "shift+!" → "shift+1"
    #[cfg(target_os = "linux")]
    pub async fn send_combo_xdotool(&self, key: &str) -> Result<()> {
        let xdotool_key = to_xdotool_key(key);
        tokio::task::spawn_blocking(move || {
            std::process::Command::new("xdotool")
                .args(["key", &xdotool_key])
                .status()
                .map_err(|e| anyhow::anyhow!("xdotool key: {}", e))
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking: {}", e))??;
        Ok(())
    }

    /// Type text — uses xdotool type for ASCII, enigo text() for CJK.
    pub async fn type_text(&self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        for (i, line) in text.split('\n').enumerate() {
            if !line.is_empty() {
                self.type_line(line).await?;
            }
            if i < text.split('\n').count() - 1 {
                self.send_key("Return").await?;
            }
        }
        Ok(())
    }

    /// Type a single line of text.
    async fn type_line(&self, text: &str) -> Result<()> {
        let is_ascii_only = text.is_ascii();

        // On Linux: prefer xdotool for ASCII, enigo text() for CJK
        #[cfg(target_os = "linux")]
        {
            if is_ascii_only {
                let text = text.to_string();
                let ok = tokio::task::spawn_blocking(move || {
                    std::process::Command::new("xdotool")
                        .args(["type", "--clearmodifiers", &text])
                        .status()
                        .map_or(false, |s| s.success())
                })
                .await
                .unwrap_or(false);
                if ok {
                    return Ok(());
                }
            }
        }

        // For CJK / all non-Linux: use enigo text() directly
        let mut guard = self.get_enigo()?;
        let e = guard.as_mut().unwrap();
        e.text(text)
            .map_err(|e| anyhow::anyhow!("text: {}", e))?;
        Ok(())
    }

    /// Copy text to clipboard then simulate Ctrl+V.
    async fn clipboard_paste(&self, text: &str) -> Result<()> {
        {
            let mut ctx = arboard::Clipboard::new()
                .map_err(|e| anyhow::anyhow!("clipboard: {}", e))?;
            ctx.set_text(text.to_string())
                .map_err(|e| anyhow::anyhow!("clipboard set: {}", e))?;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        #[cfg(target_os = "linux")]
        {
            let ok = tokio::task::spawn_blocking(|| {
                std::process::Command::new("xdotool")
                    .args(["key", "ctrl+v"])
                    .status()
                    .map_or(false, |s| s.success())
            })
            .await
            .unwrap_or(false);
            if ok {
                return Ok(());
            }
            return Err(anyhow::anyhow!("xdotool not available"));
        }

        #[cfg(not(target_os = "linux"))]
        {
            let mut guard = self.get_enigo()?;
            let e = guard.as_mut().unwrap();
            e.key(enigo::Key::Control, Press)
                .map_err(|e| anyhow::anyhow!("ctrl press: {}", e))?;
            e.key(enigo::Key::Unicode('v'), Click)
                .map_err(|e| anyhow::anyhow!("v click: {}", e))?;
            e.key(enigo::Key::Control, Release)
                .map_err(|e| anyhow::anyhow!("ctrl release: {}", e))?;
            Ok(())
        }
    }

    pub async fn close(&mut self) {
        // enigo doesn't need explicit cleanup
    }
}

/// Map button number (1=left, 2=middle, 3=right) to enigo Button.
fn map_button(b: u8) -> Button {
    match b {
        1 => Button::Left,
        2 => Button::Middle,
        3 => Button::Right,
        _ => Button::Left,
    }
}

/// Parse a key string like "ctrl+shift+Tab" into (modifier_keys, main_key).
fn parse_key_string(key: &str) -> (Vec<enigo::Key>, enigo::Key) {
    let parts: Vec<&str> = key.split('+').map(|s| s.trim()).collect();
    let mut modifiers = Vec::new();
    let mut main_key_str = "";

    for (i, part) in parts.iter().enumerate() {
        if i == parts.len() - 1 {
            // Last part is the main key
            main_key_str = part;
        } else {
            // Modifier
            if let Some(m) = map_modifier(part) {
                modifiers.push(m);
            }
        }
    }

    let main_key = map_key_name(main_key_str);
    (modifiers, main_key)
}

/// Map modifier name to enigo Key.
fn map_modifier(name: &str) -> Option<enigo::Key> {
    match name.to_lowercase().as_str() {
        "ctrl" | "control" => Some(enigo::Key::Control),
        "shift" => Some(enigo::Key::Shift),
        "alt" | "option" => Some(enigo::Key::Alt),
        "meta" | "super" | "win" | "cmd" | "command" => Some(enigo::Key::Meta),
        _ => None,
    }
}

/// Map key name to enigo Key.
fn map_key_name(name: &str) -> enigo::Key {
    match name {
        // Special keys
        "Escape" | "Esc" => enigo::Key::Escape,
        "Tab" => enigo::Key::Tab,
        "Return" | "Enter" => enigo::Key::Return,
        "BackSpace" | "Backspace" => enigo::Key::Backspace,
        "Delete" | "Del" => enigo::Key::Delete,
        "Space" => enigo::Key::Unicode(' '),
        "Up" => enigo::Key::UpArrow,
        "Down" => enigo::Key::DownArrow,
        "Left" => enigo::Key::LeftArrow,
        "Right" => enigo::Key::RightArrow,
        "Home" => enigo::Key::Home,
        "End" => enigo::Key::End,
        "Page_Up" | "PageUp" => enigo::Key::PageUp,
        "Page_Down" | "PageDown" => enigo::Key::PageDown,
        "Insert" | "Ins" => enigo::Key::Insert,
        // Function keys
        "F1" => enigo::Key::F1,
        "F2" => enigo::Key::F2,
        "F3" => enigo::Key::F3,
        "F4" => enigo::Key::F4,
        "F5" => enigo::Key::F5,
        "F6" => enigo::Key::F6,
        "F7" => enigo::Key::F7,
        "F8" => enigo::Key::F8,
        "F9" => enigo::Key::F9,
        "F10" => enigo::Key::F10,
        "F11" => enigo::Key::F11,
        "F12" => enigo::Key::F12,
        // Modifiers as standalone keys
        "ctrl" | "control" => enigo::Key::Control,
        "shift" => enigo::Key::Shift,
        "alt" | "option" => enigo::Key::Alt,
        "meta" | "super" | "win" | "cmd" => enigo::Key::Meta,
        // Punctuation / symbols commonly sent
        "slash" | "/" => enigo::Key::Unicode('/'),
        "backslash" | "\\" => enigo::Key::Unicode('\\'),
        "period" | "." => enigo::Key::Unicode('.'),
        "comma" | "," => enigo::Key::Unicode(','),
        "semicolon" | ";" => enigo::Key::Unicode(';'),
        "quote" | "'" => enigo::Key::Unicode('\''),
        "bracketleft" | "[" => enigo::Key::Unicode('['),
        "bracketright" | "]" => enigo::Key::Unicode(']'),
        "minus" | "-" => enigo::Key::Unicode('-'),
        "equal" | "=" => enigo::Key::Unicode('='),
        "grave" | "`" => enigo::Key::Unicode('`'),
        // Single character → Unicode
        _ if name.chars().count() == 1 => enigo::Key::Unicode(name.chars().next().unwrap()),
        // Unknown → try Unicode for the first char (fallback)
        _ => {
            tracing::warn!("Unknown key name: '{}', trying as Unicode", name);
            enigo::Key::Unicode(name.chars().next().unwrap_or(' '))
        }
    }
}

/// Shifted symbol → base key for xdotool.
/// e.g. '!' → Some("1"), '@' → Some("2"), '$' → Some("4")
fn shifted_to_base(ch: char) -> Option<&'static str> {
    match ch {
        '!' => Some("1"), '@' => Some("2"), '#' => Some("3"),
        '$' => Some("4"), '%' => Some("5"), '^' => Some("6"),
        '&' => Some("7"), '*' => Some("8"), '(' => Some("9"),
        ')' => Some("0"), '_' => Some("minus"), '+' => Some("equal"),
        '{' => Some("bracketleft"), '}' => Some("bracketright"),
        '|' => Some("backslash"), ':' => Some("semicolon"),
        '"' => Some("quote"), '<' => Some("comma"), '>' => Some("period"),
        '?' => Some("slash"), '~' => Some("grave"),
        _ => None,
    }
}

/// Convert a frontend key string to xdotool keysym format.
/// Examples:
///   "Space"     → "space"
///   ","         → "comma"
///   "."         → "period"
///   "shift+!"  → "shift+1"
///   "ctrl+space"→ "ctrl+space"
///   "Return"    → "Return"
fn to_xdotool_key(key: &str) -> String {
    // Check if this is a modifier combo
    if let Some(plus_pos) = key.rfind('+') {
        let prefix = &key[..plus_pos];
        let base = &key[plus_pos + 1..];
        if !prefix.is_empty() && (prefix.contains("ctrl") || prefix.contains("shift")
            || prefix.contains("alt") || prefix.contains("meta"))
        {
            // Shifted symbol → base key (e.g. "shift+!" → "shift+1")
            if prefix.contains("shift") {
                if let Some(ch) = base.chars().next() {
                    if base.chars().count() == 1 {
                        if let Some(base_key) = shifted_to_base(ch) {
                            // Rebuild with original prefix
                            let mut result = prefix.to_string();
                            result.push('+');
                            result.push_str(base_key);
                            return result;
                        }
                    }
                }
            }
            // Recursively convert the base key part
            let converted_base = to_xdotool_key(base);
            return format!("{}+{}", prefix, converted_base);
        }
    }

    // Single key — map to xdotool keysym name
    match key {
        "Space" => "space".to_string(),
        "Return" | "Enter" => "Return".to_string(),
        "Tab" => "Tab".to_string(),
        "BackSpace" | "Backspace" => "BackSpace".to_string(),
        "Escape" | "Esc" => "Escape".to_string(),
        "Delete" | "Del" => "Delete".to_string(),
        "Up" => "Up".to_string(),
        "Down" => "Down".to_string(),
        "Left" => "Left".to_string(),
        "Right" => "Right".to_string(),
        "Home" => "Home".to_string(),
        "End" => "End".to_string(),
        "Page_Up" | "PageUp" => "Page_Up".to_string(),
        "Page_Down" | "PageDown" => "Page_Down".to_string(),
        // Single character
        _ if key.chars().count() == 1 => {
            let ch = key.chars().next().unwrap();
            match ch {
                ',' => "comma".to_string(),
                '.' => "period".to_string(),
                '/' => "slash".to_string(),
                ';' => "semicolon".to_string(),
                '\'' => "apostrophe".to_string(),
                '[' => "bracketleft".to_string(),
                ']' => "bracketright".to_string(),
                '-' => "minus".to_string(),
                '=' => "equal".to_string(),
                '`' => "grave".to_string(),
                '\\' => "backslash".to_string(),
                ' ' => "space".to_string(),
                // Letters and digits are valid keysym names as-is
                _ => ch.to_string(),
            }
        }
        _ => key.to_string(),
    }
}
