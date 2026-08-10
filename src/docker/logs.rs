//! Container log streaming.
//!
//! Only one stream is ever open — the one for the selected container. Each
//! stream carries a generation number so that lines still in flight when the
//! user moves the selection are discarded instead of polluting the new buffer.

use bollard::container::LogOutput;
use bollard::query_parameters::LogsOptions;
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::docker::Client;
use crate::event::AppEvent;

/// How many lines of history to ask Docker for when attaching.
const TAIL: &str = "1000";

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

        while let Some(item) = stream.next().await {
            let chunk = match item {
                Ok(c) => c,
                Err(e) => {
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

        let _ = tx.send(AppEvent::LogEnd { generation });
    })
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
    fn line_with_no_space_is_not_mangled() {
        let l = parse_line("no-spaces-at-all", Stream::Stderr);
        assert!(l.timestamp.is_none());
        assert_eq!(l.text, "no-spaces-at-all");
    }
}
