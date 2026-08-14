//! How much disk each volume is using.
//!
//! Docker's volume list never carries this: `GET /volumes` returns no usage
//! data at all, and the only endpoint that computes it is `/system/df`, which
//! walks every volume directory. On a machine with a large build cache that
//! takes tens of seconds, so this is never on the poll loop — the user asks for
//! it, once, and the answer is cached until they ask again.

use std::collections::HashMap;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::docker::Client;
use crate::event::AppEvent;

/// Kick off a measurement. The handle is kept only so the app can abort it.
pub fn spawn(client: Client, tx: mpsc::UnboundedSender<AppEvent>) -> JoinHandle<()> {
    tokio::spawn(async move {
        // No `type=volume` filter, for two reasons. bollard can't URL-encode
        // the repeated parameter it would take — it fails the request outright
        // with "unsupported value". And it wouldn't buy much anyway: measured
        // against a daemon with 40 volumes, filtering took 4.4s against 4.6s
        // unfiltered, because nearly all of the time goes on walking the volume
        // directories either way. (The first call of the day costs far more —
        // ~19s there — but that's a cold page cache, which the filter can't
        // help with either.)
        let event = match client.docker.df(None).await {
            Ok(df) => match df.volume_usage {
                Some(usage) => AppEvent::VolumeUsage(sizes_from(Some(usage))),
                // Older daemons report volumes only under a legacy field that
                // bollard doesn't model, so there is nothing to read. Say that,
                // rather than reporting every volume as unmeasured.
                None => AppEvent::VolumeUsageError(
                    "this daemon's API doesn't report volume usage".into(),
                ),
            },
            Err(e) => AppEvent::VolumeUsageError(super::friendly_error(&e)),
        };

        let _ = tx.send(event);
    })
}

/// Pull `name → size` out of the df response.
///
/// bollard types the volume items as bare JSON, so this reads the two fields it
/// needs by name. Anything shaped differently is skipped rather than guessed
/// at: a missing size is a volume we don't know the size of, which the table
/// already knows how to say.
fn sizes_from(usage: Option<bollard::models::VolumesDiskUsage>) -> HashMap<String, i64> {
    usage
        .and_then(|u| u.items)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|item| {
            let name = item.get("Name")?.as_str()?.to_string();
            // A negative size is Docker's way of saying it didn't compute one —
            // a driver that can't be measured, usually. Leave those unknown
            // rather than reporting them as empty.
            let size = item.get("UsageData")?.get("Size")?.as_i64()?;
            (size >= 0).then_some((name, size))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::sizes_from;
    use bollard::models::VolumesDiskUsage;

    fn usage(items: serde_json::Value) -> Option<VolumesDiskUsage> {
        Some(VolumesDiskUsage {
            items: Some(items.as_array().unwrap().clone()),
            ..Default::default()
        })
    }

    #[test]
    fn sizes_are_read_by_name() {
        let got = sizes_from(usage(serde_json::json!([
            {"Name": "pgdata", "UsageData": {"Size": 4096, "RefCount": 1}},
            {"Name": "cache", "UsageData": {"Size": 0, "RefCount": 0}},
        ])));
        assert_eq!(got.get("pgdata"), Some(&4096));
        assert_eq!(got.get("cache"), Some(&0));
    }

    #[test]
    fn unmeasured_volumes_are_left_out() {
        // -1 is "I didn't compute this", which is not the same as empty. It has
        // to stay absent so the table shows a dash rather than 0B.
        let got = sizes_from(usage(serde_json::json!([
            {"Name": "remote", "UsageData": {"Size": -1, "RefCount": 0}},
            {"Name": "nodata"},
        ])));
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn a_missing_section_is_not_an_error() {
        assert!(sizes_from(None).is_empty());
    }
}
