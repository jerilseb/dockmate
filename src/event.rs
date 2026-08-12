//! Everything the background tasks can tell the UI.

use crate::docker::actions::Job;
use crate::docker::logs::LogLine;
use crate::docker::model::DaemonInfo;
use crate::docker::refresh::Snapshot;
use crate::docker::stats::StatSample;

#[derive(Debug)]
pub enum AppEvent {
    /// A fresh read of every resource list.
    Snapshot(Box<Snapshot>),
    /// One stats reading for one container.
    Stat { id: String, sample: StatSample },
    /// A log line from the stream identified by `generation`.
    LogLine { generation: u64, line: LogLine },
    /// The stream is open, but the container hasn't said anything yet. Docker
    /// has no "attached" signal of its own, so this stands in for one.
    LogAttached { generation: u64 },
    /// The log stream ended (container stopped, or `follow` finished).
    LogEnd { generation: u64 },
    /// The log stream failed.
    LogError { generation: u64, message: String },
    /// A mutation finished, successfully or not.
    JobDone {
        job: Job,
        result: Result<String, String>,
    },
    /// Daemon banner details, fetched once at startup.
    Daemon(Box<DaemonInfo>),
    /// The daemon became unreachable.
    DaemonError(String),
    /// The daemon came back.
    DaemonOk,
}
