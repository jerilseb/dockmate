//! Container log streaming.
//!
//! Only one stream is ever open — the one for the selected container. Each
//! stream carries a generation number so that lines still in flight when the
//! user moves the selection are discarded instead of polluting the new buffer.

use std::time::Duration;

use bollard::container::LogOutput;
use bollard::query_parameters::LogsOptions;
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::docker::Client;
use crate::event::AppEvent;

/// How many lines of history to ask Docker for when attaching.
const TAIL: &str = "1000";

/// How long a silent stream is still considered to be connecting.
///
/// Docker has no "attached" signal: the response stays open and says nothing
/// until the container writes. So silence can't be told apart from a connection
/// still being set up, and a container that never logs at all — `sleep
/// infinity`, an idle sidecar — would leave the pane spinning on "attaching…"
/// for as long as it's selected. Long enough not to flicker on a busy daemon,
/// short enough not to look stuck.
const ATTACH_GRACE: Duration = Duration::from_millis(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    /// RFC3339 timestamp Docker prefixed, split off so it can be styled (and
    /// hidden) independently of the message.
    pub timestamp: Option<String>,
    pub text: String,
    pub stream: Stream,
}

/// Opens a log stream and returns its task handle. Dropping/aborting the handle
/// closes the underlying HTTP connection.
pub fn spawn(
    client: Client,
    id: String,
    generation: u64,
    tx: mpsc::UnboundedSender<AppEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let options = LogsOptions {
            follow: true,
            stdout: true,
            stderr: true,
            timestamps: true,
            tail: TAIL.to_string(),
            ..Default::default()
        };

        let mut stream = client.docker.logs(&id, Some(options));

        // Docker frames log output per write, not per line, so a single frame
        // may hold several lines or half of one. Buffer per stream and only
        // emit on a newline.
        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();

        // Only the first frame is raced against the grace timer; once anything
        // has arrived the stream is plainly attached and we wait indefinitely.
        let mut first = true;
        loop {
            let next = if first {
                first = false;
                match timeout(ATTACH_GRACE, stream.next()).await {
                    Ok(item) => item,
                    Err(_) => {
                        let _ = tx.send(AppEvent::LogAttached { generation });
                        continue;
                    }
                }
            } else {
                stream.next().await
            };

            let Some(item) = next else { break };

            let chunk = match item {
                Ok(c) => c,
                Err(e) => {
                    // Whatever already made it through is real output; show it
                    // before the error rather than losing it to the failure.
                    flush(&mut stdout_buf, Stream::Stdout, generation, &tx);
                    flush(&mut stderr_buf, Stream::Stderr, generation, &tx);
                    let _ = tx.send(AppEvent::LogError {
                        generation,
                        message: super::friendly_error(&e),
                    });
                    return;
                }
            };

            let (bytes, which, buf) = match &chunk {
                LogOutput::StdErr { message } => (message, Stream::Stderr, &mut stderr_buf),
                LogOutput::StdOut { message } | LogOutput::Console { message } => {
                    (message, Stream::Stdout, &mut stdout_buf)
                }
                LogOutput::StdIn { .. } => continue,
            };

            buf.extend_from_slice(bytes);
            while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let line = &line[..line.len() - 1];
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                let parsed = parse_line(&String::from_utf8_lossy(line), which);
                if tx
                    .send(AppEvent::LogLine {
                        generation,
                        line: parsed,
                    })
                    .is_err()
                {
                    return;
                }
            }
        }

        flush(&mut stdout_buf, Stream::Stdout, generation, &tx);
        flush(&mut stderr_buf, Stream::Stderr, generation, &tx);
        let _ = tx.send(AppEvent::LogEnd { generation });
    })
}

/// Emit whatever is left in a buffer as a final, unterminated line.
///
/// Docker frames output per write, so the last thing a container printed need
/// not end in a newline — a shell prompt, a progress line, or a program that
/// simply didn't print one. While the stream is live, holding those bytes back
/// is right: the rest of the line is probably still coming. Once the stream is
/// over there is no rest, and dropping the buffer would silently swallow the
/// container's last words.
fn flush(buf: &mut Vec<u8>, which: Stream, generation: u64, tx: &mpsc::UnboundedSender<AppEvent>) {
    if buf.is_empty() {
        return;
    }
    let rest = std::mem::take(buf);
    let rest = rest.strip_suffix(b"\r").unwrap_or(&rest);
    let parsed = parse_line(&String::from_utf8_lossy(rest), which);
    let _ = tx.send(AppEvent::LogLine {
        generation,
        line: parsed,
    });
}

/// Split Docker's `timestamps: true` prefix off the front of a line.
fn parse_line(raw: &str, stream: Stream) -> LogLine {
    // Docker emits RFC3339Nano followed by a single space. Recognising it by
    // shape rather than parsing keeps this cheap, and a line that merely looks
    // like a timestamp is still displayed identically.
    if let Some((maybe_ts, rest)) = raw.split_once(' ')
        && looks_like_timestamp(maybe_ts)
    {
        return LogLine {
            timestamp: Some(short_time(maybe_ts)),
            text: rest.to_string(),
            stream,
        };
    }
    LogLine {
        timestamp: None,
        text: raw.to_string(),
        stream,
    }
}

fn looks_like_timestamp(s: &str) -> bool {
    // 2026-08-10T10:22:14.123456789Z
    s.len() >= 20
        && s.as_bytes()[4] == b'-'
        && s.as_bytes()[7] == b'-'
        && s.as_bytes()[10] == b'T'
        && s.as_bytes()[13] == b':'
        && s.chars().next().is_some_and(|c| c.is_ascii_digit())
}

/// `2026-08-10T10:22:14.123456789Z` → `15:52:14` in the viewer's own timezone.
///
/// Docker stamps logs in UTC, which is a constant source of confusion when
/// you're correlating them against anything else on your machine. Sub-second
/// precision is dropped: it's noise when scanning by eye.
fn short_time(ts: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(ts) {
        Ok(dt) => dt
            .with_timezone(&chrono::Local)
            .format("%H:%M:%S")
            .to_string(),
        // Unparseable but timestamp-shaped: fall back to the raw time field
        // rather than dropping information.
        Err(_) => ts.get(11..19).unwrap_or(ts).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_is_split_off_and_localised() {
        let l = parse_line("2026-08-10T10:22:14.123456789Z hello world", Stream::Stdout);
        assert_eq!(l.text, "hello world");

        // Docker stamps UTC; we render in the machine's zone, so derive the
        // expectation rather than hard-coding one timezone's answer.
        let expected = chrono::DateTime::parse_from_rfc3339("2026-08-10T10:22:14.123456789Z")
            .unwrap()
            .with_timezone(&chrono::Local)
            .format("%H:%M:%S")
            .to_string();
        assert_eq!(l.timestamp.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn unparseable_timestamp_falls_back_to_the_raw_field() {
        // Right shape, impossible date — keep the characters rather than
        // dropping the column.
        let l = parse_line("2026-99-99T10:22:14.000000000Z boom", Stream::Stdout);
        assert_eq!(l.timestamp.as_deref(), Some("10:22:14"));
        assert_eq!(l.text, "boom");
    }

    #[test]
    fn untimestamped_line_survives_intact() {
        let l = parse_line("plain message here", Stream::Stdout);
        assert!(l.timestamp.is_none());
        assert_eq!(l.text, "plain message here");
    }

    #[test]
    fn a_final_line_without_a_newline_still_arrives() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut buf = b"2026-08-10T10:22:14.000000000Z no trailing newline".to_vec();

        flush(&mut buf, Stream::Stdout, 7, &tx);

        assert!(buf.is_empty(), "the buffer should be consumed");
        match rx.try_recv().expect("a line should have been sent") {
            AppEvent::LogLine { generation, line } => {
                assert_eq!(generation, 7);
                assert_eq!(line.text, "no trailing newline");
                assert_eq!(line.stream, Stream::Stdout);
            }
            other => panic!("expected a log line, got {other:?}"),
        }
    }

    #[test]
    fn flushing_an_empty_buffer_sends_nothing() {
        // Otherwise every stream that ends cleanly would append a blank line.
        let (tx, mut rx) = mpsc::unbounded_channel();
        flush(&mut Vec::new(), Stream::Stdout, 1, &tx);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_flushed_line_keeps_its_stream() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut buf = b"panic: something broke".to_vec();

        flush(&mut buf, Stream::Stderr, 1, &tx);

        match rx.try_recv().expect("a line should have been sent") {
            AppEvent::LogLine { line, .. } => {
                assert_eq!(line.stream, Stream::Stderr);
                assert!(line.timestamp.is_none());
                assert_eq!(line.text, "panic: something broke");
            }
            other => panic!("expected a log line, got {other:?}"),
        }
    }

    #[test]
    fn line_with_no_space_is_not_mangled() {
        let l = parse_line("no-spaces-at-all", Stream::Stderr);
        assert!(l.timestamp.is_none());
        assert_eq!(l.text, "no-spaces-at-all");
    }
}
