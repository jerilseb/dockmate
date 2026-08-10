//! Clipboard support via the OSC 52 terminal escape.
//!
//! This deliberately avoids a system clipboard crate: OSC 52 is handled by the
//! terminal emulator itself, so it works identically over SSH and inside tmux,
//! and needs no X11/Wayland connection.

use std::io::Write;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

/// Ask the terminal to place `text` on the system clipboard.
///
/// Not every terminal honours OSC 52 (and some require it to be enabled), so
/// the caller should treat success as "the request was sent", not "the
/// clipboard definitely changed".
pub fn copy(text: &str) -> std::io::Result<()> {
    let encoded = STANDARD.encode(text.as_bytes());
    let mut out = std::io::stdout();

    if std::env::var_os("TMUX").is_some() {
        // tmux swallows OSC sequences unless they're wrapped in a passthrough,
        // and needs `set -g allow-passthrough on`.
        write!(out, "\x1bPtmux;\x1b\x1b]52;c;{encoded}\x07\x1b\\")?;
    } else {
        write!(out, "\x1b]52;c;{encoded}\x07")?;
    }
    out.flush()
}
