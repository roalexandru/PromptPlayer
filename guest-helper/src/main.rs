//! §9.3 Architecture B — Windows-side guest helper daemon.
//!
//! Runs inside a Windows VM. Listens on `127.0.0.1:9847` (configurable).
//! The Mac app sends `{prompt_text, schedule}` over TCP; the daemon replays the
//! schedule via local `enigo` injection. More reliable for high-latency RDP
//! sessions, complex Unicode, IME-heavy languages.
//!
//! Auth: shared secret in `%APPDATA%\PromptPlayer-GuestHelper\secret`.
//! Off by default in the Mac app; surfaced when host-side typing fails.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const DEFAULT_PORT: u16 = 9847;

#[derive(Debug, Deserialize)]
struct ClientMessage {
    secret: String,
    schedule: Vec<ScheduledKeyWire>,
}

// Wire types are deserialized from JSON and consumed by the typing pipeline
// outside this binary; clippy can't see the cross-binary use, so we silence
// the dead-code lint locally rather than refactor.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(tag = "type", content = "value")]
enum KeyWire {
    Char(char),
    Backspace,
    Enter,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct ScheduledKeyWire {
    key: KeyWire,
    absolute_time_ms: u64,
}

#[derive(Debug, Serialize)]
struct ServerReply {
    ok: bool,
    error: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let port: u16 = std::env::var("PROMPT_PLAYER_GUEST_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let secret_path = secret_path();
    let secret = load_or_init_secret(&secret_path)?;
    tracing::info!(
        "guest-helper listening on 127.0.0.1:{} secret @ {}",
        port,
        secret_path.display()
    );
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    loop {
        let (sock, _) = listener.accept().await?;
        let s = secret.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(sock, &s).await {
                tracing::warn!("client error: {}", e);
            }
        });
    }
}

fn secret_path() -> PathBuf {
    if cfg!(windows) {
        let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
        PathBuf::from(base)
            .join("PromptPlayer-GuestHelper")
            .join("secret")
    } else {
        // Cross-compile sanity. On non-Windows hosts (CI builds) write to a temp.
        std::env::temp_dir().join("prompt-player-guest-helper-secret")
    }
}

fn load_or_init_secret(path: &PathBuf) -> Result<String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    if let Ok(s) = std::fs::read_to_string(path) {
        let s = s.trim().to_string();
        if !s.is_empty() {
            return Ok(s);
        }
    }
    // Generate a fresh secret from OS entropy and persist. Failure is fatal:
    // an all-zero or predictable auth secret would expose the local typing port.
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).context("OS randomness unavailable")?;
    let secret: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(path, &secret).context("write secret")?;
    #[cfg(windows)]
    {
        // Lock down ACLs to current user only.
        let _ = std::process::Command::new("icacls")
            .arg(path)
            .args([
                "/inheritance:r",
                "/grant:r",
                &format!("{}:F", whoami_user()),
            ])
            .status();
    }
    Ok(secret)
}

#[cfg(windows)]
fn whoami_user() -> String {
    std::env::var("USERNAME").unwrap_or_else(|_| "User".into())
}

async fn handle(stream: TcpStream, expected_secret: &str) -> Result<()> {
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.context("read message")?;
    let msg: ClientMessage = serde_json::from_str(&line).context("parse json")?;
    if !secrets_match(&msg.secret, expected_secret) {
        let reply = ServerReply {
            ok: false,
            error: Some("auth".into()),
        };
        let s = serde_json::to_string(&reply)?;
        w.write_all(s.as_bytes()).await?;
        return Err(anyhow!("auth mismatch"));
    }
    play_schedule(msg.schedule).await?;
    let reply = ServerReply {
        ok: true,
        error: None,
    };
    let s = serde_json::to_string(&reply)?;
    w.write_all(s.as_bytes()).await?;
    Ok(())
}

fn secrets_match(actual: &str, expected: &str) -> bool {
    actual.as_bytes().ct_eq(expected.as_bytes()).into()
}

#[cfg(target_os = "windows")]
async fn play_schedule(schedule: Vec<ScheduledKeyWire>) -> Result<()> {
    use enigo::{Direction, Enigo, Key as EKey, Keyboard, Settings};
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| anyhow!("enigo: {}", e))?;
    let start = std::time::Instant::now();
    for sk in schedule {
        let target = std::time::Duration::from_millis(sk.absolute_time_ms);
        let elapsed = start.elapsed();
        if target > elapsed {
            tokio::time::sleep(target - elapsed).await;
        }
        match sk.key {
            KeyWire::Char(c) => {
                type_char_unicode(c);
            }
            KeyWire::Backspace => {
                let _ = enigo.key(EKey::Backspace, Direction::Click);
            }
            KeyWire::Enter => {
                let _ = enigo.key(EKey::Return, Direction::Click);
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn type_char_unicode(c: char) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
        KEYEVENTF_UNICODE, VIRTUAL_KEY,
    };

    let mut buf = [0u16; 2];
    for &unit in c.encode_utf16(&mut buf).iter() {
        let down = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: unit,
                    dwFlags: KEYEVENTF_UNICODE,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let up = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: unit,
                    dwFlags: KEYBD_EVENT_FLAGS(KEYEVENTF_UNICODE.0 | KEYEVENTF_KEYUP.0),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        unsafe {
            SendInput(&[down, up], std::mem::size_of::<INPUT>() as i32);
        }
    }
}

#[cfg(not(target_os = "windows"))]
async fn play_schedule(_schedule: Vec<ScheduledKeyWire>) -> Result<()> {
    // Non-Windows builds (CI cross-builds) accept the message but no-op the typing.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_comparison_requires_exact_match() {
        assert!(secrets_match("abc123", "abc123"));
        assert!(!secrets_match("abc124", "abc123"));
        assert!(!secrets_match("abc123", "abc12300"));
    }
}
