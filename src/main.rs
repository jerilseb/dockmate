//! dockyard — a terminal UI for managing Docker.

mod action;
mod app;
mod docker;
mod event;
mod tui;
mod ui;
mod util;

use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::app::{App, ToastKind};
use crate::docker::Client;
use crate::event::AppEvent;
use crate::ui::theme::{Glyphs, Palette, Theme};

/// How often the UI wakes up to animate spinners and expire toasts.
const TICK: Duration = Duration::from_millis(100);

#[derive(Parser, Debug)]
#[command(
    name = "dockyard",
    version,
    about = "A terminal UI for managing Docker containers, images, volumes and networks"
)]
struct Args {
    /// Docker host to connect to, e.g. tcp://10.0.0.5:2375 or unix:///var/run/docker.sock.
    /// Defaults to DOCKER_HOST, then the local socket.
    #[arg(long, value_name = "URL")]
    host: Option<String>,

    /// How often to poll the daemon, in milliseconds.
    #[arg(long, value_name = "MS", default_value_t = 2000)]
    interval: u64,

    /// Draw with plain ASCII instead of Unicode glyphs.
    #[arg(long)]
    ascii: bool,

    /// Use Nerd Font icons in the tab bar.
    #[arg(long)]
    icons: bool,

    /// Use the terminal's own 16 ANSI colours instead of the built-in palette.
    #[arg(long)]
    ansi: bool,

    /// Draw without colour.
    #[arg(long)]
    no_color: bool,

    /// Don't capture the mouse. Mouse reporting is on by default; turn it off
    /// if you'd rather your terminal handle click-drag selection unmodified.
    #[arg(long)]
    no_mouse: bool,
}

impl Args {
    fn theme(&self) -> Theme {
        // NO_COLOR is a de-facto standard; honour it without needing a flag.
        let no_color = self.no_color || std::env::var_os("NO_COLOR").is_some();
        let palette = if no_color {
            Palette::Mono
        } else if self.ansi {
            Palette::Ansi
        } else {
            Palette::TrueColor
        };

        let glyphs = if self.ascii || std::env::var_os("DOCKYARD_ASCII").is_some() {
            Glyphs::Ascii
        } else if self.icons || std::env::var_os("DOCKYARD_ICONS").is_some() {
            Glyphs::Nerd
        } else {
            Glyphs::Unicode
        };

        Theme::new(palette, glyphs)
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;

    let result = runtime.block_on(run(args));

    // Restore before reporting, so an error message isn't swallowed by the
    // alternate screen.
    let _ = tui::leave();
    result
}

async fn run(args: Args) -> Result<()> {
    let client = Client::connect(args.host.as_deref())?;

    // Fail before taking over the terminal if the daemon isn't there — a bare
    // error message is much friendlier than an empty TUI.
    let daemon = client.ping().await.map_err(|e| {
        anyhow::anyhow!(
            "{e:#}\n\nIs the docker daemon running, and do you have permission to reach it?\n\
             Try: docker version"
        )
    })?;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let refresher = docker::refresh::spawn(
        client.clone(),
        tx.clone(),
        Duration::from_millis(args.interval),
    );

    let mut app = App::new(client.clone(), tx.clone(), refresher, args.theme());
    app.on_app_event(AppEvent::Daemon(Box::new(daemon)));

    let mut terminal = tui::enter(!args.no_mouse)?;
    let outcome = event_loop(&mut terminal, &mut app, &mut rx, &client).await;

    tui::leave()?;
    outcome
}

async fn event_loop(
    terminal: &mut tui::Term,
    app: &mut App,
    rx: &mut mpsc::UnboundedReceiver<AppEvent>,
    client: &Client,
) -> Result<()> {
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut keys = Some(EventStream::new());

    loop {
        if app.dirty {
            terminal.draw(|frame| ui::draw(frame, app))?;
            app.dirty = false;
        }

        {
            // `keys` is only ever None while an exec session owns the terminal,
            // and we never reach the select in that state.
            let stream = keys.as_mut().expect("event stream present");
            tokio::select! {
                Some(term_event) = stream.next() => match term_event {
                    Ok(Event::Key(key)) => app.on_key(key),
                    Ok(Event::Mouse(mouse)) => app.on_mouse(mouse),
                    Ok(Event::Resize(_, _)) => app.dirty = true,
                    Ok(_) => {}
                    // A read error here is almost always the terminal going
                    // away; treat it as a quit rather than spinning.
                    Err(_) => break,
                },
                Some(app_event) = rx.recv() => {
                    app.on_app_event(app_event);
                    // Stats and log lines arrive in bursts. Draining what's
                    // already queued means one redraw instead of dozens.
                    while let Ok(next) = rx.try_recv() {
                        app.on_app_event(next);
                    }
                }
                _ = ticker.tick() => app.on_tick(),
            }
        }

        if app.should_quit {
            break;
        }

        if let Some((id, name)) = app.exec_request.take() {
            // Hand the terminal over. The crossterm reader has to be dropped
            // first: two readers on the same tty steal each other's input.
            drop(keys.take());
            let outcome = run_exec(terminal, client, &id, &name).await;
            keys = Some(EventStream::new());
            app.dirty = true;

            match outcome {
                Ok(docker::exec::Outcome::Exited(0)) => {}
                Ok(docker::exec::Outcome::Exited(code)) => app.toast(
                    ToastKind::Info,
                    format!("shell in {name} exited with status {code}"),
                ),
                Ok(docker::exec::Outcome::Failed(msg)) => {
                    app.toast(ToastKind::Error, format!("could not open a shell: {msg}"))
                }
                Err(e) => app.toast(ToastKind::Error, format!("exec failed: {e}")),
            }
        }
    }

    Ok(())
}

/// Suspend the TUI, run an interactive shell, and put everything back.
async fn run_exec(
    terminal: &mut tui::Term,
    client: &Client,
    id: &str,
    name: &str,
) -> Result<docker::exec::Outcome> {
    let size = crossterm::terminal::size().unwrap_or((80, 24));

    tui::suspend(terminal).context("suspending the tui")?;
    println!("\r\n── dockyard: shell in {name} ── type `exit` to come back ──\r\n");

    let result = docker::exec::run(client, id, size)
        .await
        .context("running the exec session");

    // Restore the TUI even if the session errored.
    tui::resume(terminal).context("restoring the tui")?;
    result
}
