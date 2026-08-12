//! Terminal ownership.
//!
//! The one rule: whatever happens — clean exit, error, panic, or an exec
//! session going sideways — the user's shell gets its terminal back with raw
//! mode off and the alternate screen closed.

use std::io::{Stdout, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::{Backend, ClearType, CrosstermBackend};

pub type Term = Terminal<CrosstermBackend<Stdout>>;

/// Whether we currently own the terminal. Guards [`leave`] so the unconditional
/// cleanup on the error path doesn't spray escape sequences over a message
/// printed before the TUI ever started.
static ENTERED: AtomicBool = AtomicBool::new(false);

/// Whether mouse reporting was requested. Tracked separately from [`ENTERED`]
/// because the exec handoff has to drop and restore it independently.
static MOUSE: AtomicBool = AtomicBool::new(false);

/// Enter the TUI: raw mode, alternate screen, hidden cursor, optional mouse
/// reporting, and a panic hook that undoes all of it before the message prints.
pub fn enter(mouse: bool) -> Result<Term> {
    install_panic_hook();
    enable_raw_mode()?;
    let mut out = std::io::stdout();
    execute!(out, EnterAlternateScreen, crossterm::cursor::Hide)?;
    if mouse {
        execute!(out, EnableMouseCapture)?;
        MOUSE.store(true, Ordering::SeqCst);
    }
    ENTERED.store(true, Ordering::SeqCst);
    let terminal = Terminal::new(CrosstermBackend::new(out))?;
    Ok(terminal)
}

/// Restore the terminal. Safe to call more than once, and a no-op if we never
/// took it over.
pub fn leave() -> Result<()> {
    if !ENTERED.swap(false, Ordering::SeqCst) {
        return Ok(());
    }
    let mut out = std::io::stdout();
    if MOUSE.load(Ordering::SeqCst) {
        // Leaving this on would make the user's shell emit escape gibberish on
        // every click.
        execute!(out, DisableMouseCapture)?;
    }
    execute!(out, LeaveAlternateScreen, crossterm::cursor::Show)?;
    disable_raw_mode()?;
    out.flush()?;
    Ok(())
}

/// Step out of the TUI without giving up raw mode, so an interactive process
/// can use the real screen. Raw mode stays on because the remote pty does its
/// own echo and line editing; mouse reporting does *not*, because the program
/// running inside the container should decide that for itself.
pub fn suspend(terminal: &mut Term) -> Result<()> {
    let mut out = std::io::stdout();
    if MOUSE.load(Ordering::SeqCst) {
        execute!(out, DisableMouseCapture)?;
    }
    execute!(out, LeaveAlternateScreen, crossterm::cursor::Show)?;

    // Leaving the alternate screen hands back the primary one, still holding
    // whatever the user's shell printed before dockmate started. A session that
    // opens on top of somebody else's prompt reads as a glitch, so give it a
    // blank screen.
    //
    // Scrolled away rather than erased: `ESC[2J` is inconsistent about whether
    // the lines it clears reach the scrollback, and that history is the user's,
    // not ours to drop. A newline at the bottom margin appends to scrollback in
    // every terminal there is, so park the cursor there and feed it a full
    // screen's worth.
    let (_, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    execute!(out, crossterm::cursor::MoveTo(0, rows.saturating_sub(1)))?;
    out.write_all(&b"\n".repeat(rows as usize))?;
    execute!(out, crossterm::cursor::MoveTo(0, 0))?;

    out.flush()?;
    let _ = terminal; // the backend writes to the same stdout
    Ok(())
}

/// Come back from [`suspend`], discarding whatever the child left on screen.
pub fn resume(terminal: &mut Term) -> Result<()> {
    let mut out = std::io::stdout();
    execute!(out, EnterAlternateScreen, crossterm::cursor::Hide)?;
    if MOUSE.load(Ordering::SeqCst) {
        execute!(out, EnableMouseCapture)?;
    }
    out.flush()?;

    // Deliberately *not* `Terminal::clear()`: that round-trips a DSR cursor
    // query (`ESC[6n`) and blocks waiting for the reply. Right after handing
    // the tty back from a container shell that reply is unreliable — the shell
    // has been issuing its own DSR queries — and it fails outright.
    //
    // Entering the alternate screen already gives us a blank canvas; all that's
    // left is to make ratatui forget what it thinks is on screen. Resetting
    // both buffers does that with no I/O, so the next draw repaints every cell.
    terminal.backend_mut().clear_region(ClearType::All)?;
    terminal.swap_buffers();
    terminal.swap_buffers();

    // The container's shell may have left cursor-position replies in our input
    // queue. Drop them so they aren't parsed as keystrokes.
    while crossterm::event::poll(std::time::Duration::ZERO)? {
        let _ = crossterm::event::read()?;
    }
    Ok(())
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Best-effort: if this fails there's nothing useful left to do, and
        // swallowing the error keeps the original panic message intact.
        let _ = leave();
        previous(info);
    }));
}
