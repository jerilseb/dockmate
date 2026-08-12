pub mod actions;
pub mod exec;
pub mod logs;
pub mod model;
pub mod refresh;
pub mod stats;

use std::sync::Arc;

use anyhow::{Context, Result};
use bollard::Docker;

use model::DaemonInfo;

/// Thin wrapper around bollard's client so the rest of the app passes one
/// cheap-to-clone handle around.
#[derive(Clone)]
pub struct Client {
    pub docker: Arc<Docker>,
}

impl Client {
    /// Connect using `DOCKER_HOST` when set, otherwise auto-detect the local
    /// socket. `host` overrides both.
    pub fn connect(host: Option<&str>) -> Result<Self> {
        let docker = match host {
            Some(h) => Docker::connect_with_host(h)
                .with_context(|| format!("connecting to docker at {h}"))?,
            None => Docker::connect_with_defaults().context("connecting to the docker daemon")?,
        };
        Ok(Self {
            docker: Arc::new(docker),
        })
    }

    /// Confirm the daemon is actually reachable and grab its banner details.
    /// `connect` only builds a transport; it never touches the network.
    pub async fn ping(&self) -> Result<DaemonInfo> {
        let v = self
            .docker
            .version()
            .await
            .context("querying docker version")?;
        Ok(DaemonInfo {
            version: v.version.unwrap_or_else(|| "unknown".into()),
            api_version: v.api_version.unwrap_or_default(),
            os: v.os.unwrap_or_default(),
            arch: v.arch.unwrap_or_default(),
        })
    }
}

/// Bollard errors are verbose and often wrap a JSON body. Pull out the part a
/// user can act on for toasts.
pub fn friendly_error(err: &bollard::errors::Error) -> String {
    use bollard::errors::Error;
    let raw = match err {
        Error::DockerResponseServerError {
            message,
            status_code,
        } => {
            let msg = message.trim();
            if msg.is_empty() {
                return format!("docker returned HTTP {status_code}");
            }
            msg.to_string()
        }
        other => other.to_string(),
    };
    shorten_ids(&raw)
}

/// Docker spells out full 64-character ids in its error text, which crowds out
/// the part of the message that actually says what went wrong. Clip them to the
/// 12-character prefix everyone recognises.
fn shorten_ids(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();

    let flush = |run: &mut String, out: &mut String| {
        if run.len() >= 32 {
            out.push_str(&run[..12]);
        } else {
            out.push_str(run);
        }
        run.clear();
    };

    for c in text.chars() {
        if c.is_ascii_hexdigit() {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::shorten_ids;

    #[test]
    fn long_ids_are_clipped() {
        let msg = "network dockmate-testnet id \
                   4f3a2b1c9d8e7f6a5b4c3d2e1f0a9b8c7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a \
                   has active endpoints";
        let out = shorten_ids(msg);
        assert!(out.contains("id 4f3a2b1c9d8e "), "{out}");
        assert!(out.ends_with("has active endpoints"));
    }

    #[test]
    fn ordinary_words_survive() {
        // "decade" and "added" are all hex digits but far too short to be ids.
        let msg = "the decade added beef to cafe";
        assert_eq!(shorten_ids(msg), msg);
    }
}
