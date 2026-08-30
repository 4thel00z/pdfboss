//! Clipboard transport for the yank menu: the native clipboard via
//! arboard, falling back to the OSC 52 escape sequence (which reaches
//! the local clipboard even over SSH, but cannot confirm delivery).

use std::io::Write;
use std::sync::{Mutex, OnceLock};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

/// Which transport a copy went through.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Transport {
    /// The OS clipboard; delivery is confirmed.
    Native,
    /// The OSC 52 escape sequence; the terminal may or may not honor it.
    Osc52,
}

/// The OSC 52 sequence putting `text` on the clipboard of the terminal
/// that renders it.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", BASE64.encode(text))
}

/// The session-long native clipboard handle. On X11 the selection is
/// owned by the connection that set it, so a handle dropped right after
/// `set_text` would clear what was just copied.
fn native() -> &'static Mutex<Option<arboard::Clipboard>> {
    static NATIVE: OnceLock<Mutex<Option<arboard::Clipboard>>> = OnceLock::new();
    NATIVE.get_or_init(|| Mutex::new(arboard::Clipboard::new().ok()))
}

/// Copies `text`: the native clipboard first, OSC 52 to stdout when no
/// native clipboard is reachable (headless, SSH).
pub fn copy(text: &str) -> Result<Transport, String> {
    let copied = native().lock().ok().is_some_and(|mut guard| {
        guard
            .as_mut()
            .is_some_and(|clip| clip.set_text(text.to_string()).is_ok())
    });
    if copied {
        return Ok(Transport::Native);
    }
    let mut stdout = std::io::stdout();
    stdout
        .write_all(osc52(text).as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|error| error.to_string())?;
    Ok(Transport::Osc52)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_wraps_base64_in_the_escape_sequence() {
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
        assert_eq!(osc52(""), "\x1b]52;c;\x07");
    }
}
