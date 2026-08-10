//! Interactive shell sessions.
//!
//! This hands the real terminal over to a process inside the container and
//! takes it back afterwards. Two details make or break it:
//!
//! * The stdin reader must be *interruptible*. `tokio::io::stdin()` parks a
//!   blocking thread in `read(2)`; when the session ends that thread is still
//!   parked and eats the user's next keystroke. So we poll(2) the fd with a
//!   timeout and check a shutdown flag between polls.
//! * Window resizes have to be forwarded, or anything full-screen run inside
//!   the container (an editor, `top`) draws at the wrong size.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
use futures::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use crate::docker::Client;

/// Try progressively nicer shells and settle for whatever exists. Running this
/// through `sh -c` means one exec attempt covers Alpine, Debian and distroless
/// images that happen to ship busybox.
const SHELL_PROBE: &str = "exec $(command -v bash || command -v ash || command -v sh)";

/// Result of a session, for the toast shown afterwards.
pub enum Outcome {
    /// The shell ran and exited with this status.
    Exited(i64),
    /// The exec could not be created — no shell in the image, usually.
    Failed(String),
}

/// Run an interactive shell in `id`, using the current terminal.
///
/// The caller is responsible for having left the alternate screen (and for
/// putting it back) — this function only owns the conversation with Docker.
pub async fn run(client: &Client, id: &str, size: (u16, u16)) -> Result<Outcome> {
    let config = CreateExecOptions {
        attach_stdin: Some(true),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        tty: Some(true),
        cmd: Some(vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            SHELL_PROBE.to_string(),
        ]),
        env: Some(vec!["TERM=xterm-256color".to_string()]),
        ..Default::default()
    };

    let created = match client.docker.create_exec(id, config).await {
        Ok(c) => c,
        Err(e) => return Ok(Outcome::Failed(super::friendly_error(&e))),
    };
    let exec_id = created.id;

    let started = client
        .docker
        .start_exec(
            &exec_id,
            Some(StartExecOptions {
                tty: true,
                ..Default::default()
            }),
        )
        .await;

    let (mut output, mut input) = match started {
        Ok(StartExecResults::Attached { output, input }) => (output, input),
        Ok(StartExecResults::Detached) => {
            return Ok(Outcome::Failed("docker detached the exec session".into()));
        }
        Err(e) => return Ok(Outcome::Failed(super::friendly_error(&e))),
    };

    // Tell the container about the terminal size before anything draws.
    let (cols, rows) = size;
    let _ = resize(client, &exec_id, rows, cols).await;

    let shutdown = Arc::new(AtomicBool::new(false));

    // stdin → container
    let (stdin_tx, mut stdin_rx) = mpsc::channel::<Vec<u8>>(32);
    let reader = spawn_stdin_reader(stdin_tx, shutdown.clone());

    // SIGWINCH → resize
    let resizer = spawn_resize_watcher(client.clone(), exec_id.clone(), shutdown.clone());

    let mut stdout = tokio::io::stdout();

    loop {
        tokio::select! {
            // Bytes from the container's pty. With tty:true Docker sends raw
            // pty output as `Console` frames rather than multiplexed streams.
            frame = output.next() => {
                match frame {
                    Some(Ok(chunk)) => {
                        let bytes: &[u8] = match &chunk {
                            LogOutput::Console { message }
                            | LogOutput::StdOut { message }
                            | LogOutput::StdErr { message } => message,
                            LogOutput::StdIn { .. } => continue,
                        };
                        stdout.write_all(bytes).await?;
                        stdout.flush().await?;
                    }
                    // Stream closed: the shell exited.
                    Some(Err(_)) | None => break,
                }
            }

            // Keystrokes.
            data = stdin_rx.recv() => {
                match data {
                    Some(bytes) => {
                        if input.write_all(&bytes).await.is_err() {
                            break;
                        }
                        let _ = input.flush().await;
                    }
                    None => break,
                }
            }
        }
    }

    shutdown.store(true, Ordering::SeqCst);
    reader.abort();
    resizer.abort();
    let _ = stdout.flush().await;

    // Report the shell's exit status; a non-zero code here is often the sign
    // that no shell existed in the image at all.
    let code = client
        .docker
        .inspect_exec(&exec_id)
        .await
        .ok()
        .and_then(|i| i.exit_code)
        .unwrap_or(0);

    Ok(Outcome::Exited(code))
}

async fn resize(client: &Client, exec_id: &str, rows: u16, cols: u16) -> Result<()> {
    client
        .docker
        .resize_exec(
            exec_id,
            bollard::exec::ResizeExecOptions {
                height: rows,
                width: cols,
            },
        )
        .await
        .context("resizing exec tty")?;
    Ok(())
}

/// Watch for terminal resizes and forward them to the container.
#[cfg(unix)]
fn spawn_resize_watcher(
    client: Client,
    exec_id: String,
    shutdown: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    use tokio::signal::unix::{SignalKind, signal};

    tokio::spawn(async move {
        let Ok(mut winch) = signal(SignalKind::window_change()) else {
            return;
        };
        while winch.recv().await.is_some() {
            if shutdown.load(Ordering::SeqCst) {
                return;
            }
            if let Ok((cols, rows)) = crossterm::terminal::size() {
                let _ = resize(&client, &exec_id, rows, cols).await;
            }
        }
    })
}

#[cfg(not(unix))]
fn spawn_resize_watcher(
    _client: Client,
    _exec_id: String,
    _shutdown: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async {})
}

/// Read raw stdin on a dedicated thread, in chunks, without ever blocking
/// uninterruptibly.
#[cfg(unix)]
fn spawn_stdin_reader(
    tx: mpsc::Sender<Vec<u8>>,
    shutdown: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        use std::io::Read;

        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 4096];

        while !shutdown.load(Ordering::SeqCst) {
            // Wait up to 100ms for input so the shutdown flag is checked
            // regularly. Without this the thread would sit in read(2) after the
            // session ends and swallow the next key the user pressed.
            let mut fds = libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut fds, 1, 100) };
            if ready < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return;
            }
            if ready == 0 {
                continue;
            }

            match stdin.read(&mut buf) {
                Ok(0) => return,
                Ok(n) => {
                    if tx.blocking_send(buf[..n].to_vec()).is_err() {
                        return;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return,
            }
        }
    })
}

#[cfg(not(unix))]
fn spawn_stdin_reader(
    tx: mpsc::Sender<Vec<u8>>,
    shutdown: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 4096];
        while !shutdown.load(Ordering::SeqCst) {
            match stdin.read(&mut buf) {
                Ok(0) => return,
                Ok(n) => {
                    if tx.blocking_send(buf[..n].to_vec()).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    })
}
