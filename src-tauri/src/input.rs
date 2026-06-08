use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use anyhow::Result;
use tracing::info;

/// Persistent xdotool subprocess for low-latency input simulation.
pub struct InputSimulator {
    proc: Child,
}

impl InputSimulator {
    pub async fn new() -> Result<Self> {
        let proc = Command::new("xdotool")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to start xdotool: {}. Install with: apt install xdotool", e))?;

        info!("xdotool process started (pid: {})", proc.id().unwrap_or(0));
        Ok(Self { proc })
    }

    async fn write(&mut self, cmd: &str) -> Result<()> {
        if let Some(ref mut stdin) = self.proc.stdin {
            stdin.write_all(format!("{}\n", cmd).as_bytes()).await?;
            stdin.flush().await?;
        }
        Ok(())
    }

    pub async fn mouse_move(&mut self, dx: f64, dy: f64) -> Result<()> {
        self.write(&format!("mousemove_relative -- {} {}", dx as i32, dy as i32)).await
    }

    pub async fn mouse_click(&mut self, button: u8) -> Result<()> {
        self.write(&format!("click {}", button)).await
    }

    pub async fn mouse_double_click(&mut self) -> Result<()> {
        self.write("click --repeat 2 1").await
    }

    pub async fn mouse_down(&mut self, button: u8) -> Result<()> {
        self.write(&format!("mousedown {}", button)).await
    }

    pub async fn mouse_up(&mut self, button: u8) -> Result<()> {
        self.write(&format!("mouseup {}", button)).await
    }

    pub async fn mouse_scroll(&mut self, dy: i32) -> Result<()> {
        let btn = if dy > 0 { "5" } else { "4" };
        for _ in 0..dy.abs() {
            self.write(&format!("click {}", btn)).await?;
        }
        Ok(())
    }

    pub async fn send_key(&mut self, key: &str) -> Result<()> {
        self.write(&format!("key --clearmodifiers {}", key)).await
    }

    pub async fn type_text(&mut self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        for (i, part) in text.split('\n').enumerate() {
            if !part.is_empty() {
                // Use xclip + Ctrl+V for reliable CJK input
                if let Ok(_) = self.clipboard_paste(part).await {
                    // Success via clipboard
                } else {
                    // Fallback: xdotool type
                    self.xdotool_type(part).await?;
                }
            }
            if i < text.split('\n').count() - 1 {
                self.send_key("Return").await?;
            }
        }
        Ok(())
    }

    async fn clipboard_paste(&mut self, text: &str) -> Result<()> {
        let mut proc = Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        if let Some(ref mut stdin) = proc.stdin {
            stdin.write_all(text.as_bytes()).await?;
        }
        proc.wait().await?;

        // Small delay to ensure clipboard is populated
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
        self.send_key("ctrl+v").await
    }

    async fn xdotool_type(&mut self, text: &str) -> Result<()> {
        let mut proc = Command::new("xdotool")
            .args(["type", "--clearmodifiers", "--file", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        if let Some(ref mut stdin) = proc.stdin {
            stdin.write_all(text.as_bytes()).await?;
        }
        proc.wait().await?;
        Ok(())
    }

    pub async fn close(&mut self) {
        let _ = self.proc.kill().await;
    }
}
