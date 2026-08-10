//! Keeping the resource lists current.
//!
//! Two cooperating tasks: a poller on a fixed interval, and a subscription to
//! the daemon's own event stream that nudges the poller the moment something
//! actually changes. Polling alone feels laggy; events alone miss things like a
//! container's health flipping without an event we filtered for.

use std::sync::Arc;
use std::time::Duration;

use bollard::query_parameters::{
    EventsOptions, ListContainersOptions, ListImagesOptions, ListNetworksOptions,
    ListVolumesOptions,
};
use futures::StreamExt;
use tokio::sync::{Notify, mpsc};

use super::model::{ContainerRow, ImageRow, NetworkRow, VolumeRow};
use crate::docker::Client;
use crate::event::AppEvent;

/// Handle used by the app to force an immediate refresh (on `R`, after an
/// action completes, or when a tab is opened for the first time).
#[derive(Clone)]
pub struct Refresher {
    wake: Arc<Notify>,
}

impl Refresher {
    pub fn refresh_now(&self) {
        self.wake.notify_one();
    }
}

/// Spawn the poller and the event listener. Both run until the process exits.
pub fn spawn(client: Client, tx: mpsc::UnboundedSender<AppEvent>, interval: Duration) -> Refresher {
    let wake = Arc::new(Notify::new());

    tokio::spawn(poll_loop(
        client.clone(),
        tx.clone(),
        interval,
        wake.clone(),
    ));
    tokio::spawn(event_loop(client, tx, wake.clone()));

    Refresher { wake }
}

async fn poll_loop(
    client: Client,
    tx: mpsc::UnboundedSender<AppEvent>,
    interval: Duration,
    wake: Arc<Notify>,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // Tracks whether we're currently in the "daemon is down" state so we only
    // announce transitions, not every failed poll.
    let mut degraded = false;

    loop {
        // The first `tick()` resolves immediately, so this doubles as the
        // initial load.
        tokio::select! {
            _ = ticker.tick() => {}
            _ = wake.notified() => {}
        }

        match load(&client).await {
            Ok(snapshot) => {
                if degraded {
                    degraded = false;
                    let _ = tx.send(AppEvent::DaemonOk);
                }
                if tx.send(AppEvent::Snapshot(Box::new(snapshot))).is_err() {
                    return; // app is gone
                }
            }
            Err(msg) => {
                if !degraded {
                    degraded = true;
                    if tx.send(AppEvent::DaemonError(msg)).is_err() {
                        return;
                    }
                }
            }
        }
    }
}

/// One consistent read of every resource list.
#[derive(Debug)]
pub struct Snapshot {
    pub containers: Vec<ContainerRow>,
    pub images: Vec<ImageRow>,
    pub volumes: Vec<VolumeRow>,
    pub networks: Vec<NetworkRow>,
}

async fn load(client: &Client) -> Result<Snapshot, String> {
    let docker = &client.docker;

    let containers_fut = docker.list_containers(Some(ListContainersOptions {
        all: true,
        ..Default::default()
    }));
    let images_fut = docker.list_images(Some(ListImagesOptions {
        all: false,
        ..Default::default()
    }));
    let volumes_fut = docker.list_volumes(Some(ListVolumesOptions::default()));
    let networks_fut = docker.list_networks(Some(ListNetworksOptions::default()));

    let (containers, images, volumes, networks) =
        tokio::join!(containers_fut, images_fut, volumes_fut, networks_fut);

    // The container list is the one we can't do without; the rest degrade to
    // empty so a permission quirk on, say, volumes doesn't blank the whole UI.
    let containers = containers.map_err(|e| super::friendly_error(&e))?;

    let mut containers: Vec<ContainerRow> =
        containers.into_iter().map(ContainerRow::from_api).collect();
    containers.sort_by(|a, b| a.name.cmp(&b.name));

    let mut images: Vec<ImageRow> = images
        .unwrap_or_default()
        .iter()
        .flat_map(ImageRow::from_api)
        .collect();
    images.sort_by_key(|i| std::cmp::Reverse(i.created));

    let mut volumes: Vec<VolumeRow> = volumes
        .map(|v| v.volumes.unwrap_or_default())
        .unwrap_or_default()
        .into_iter()
        .map(VolumeRow::from_api)
        .collect();
    volumes.sort_by(|a, b| a.name.cmp(&b.name));

    let mut networks: Vec<NetworkRow> = networks
        .unwrap_or_default()
        .into_iter()
        .map(NetworkRow::from_api)
        .collect();
    networks.sort_by(|a, b| a.name.cmp(&b.name));

    super::model::annotate_usage(&containers, &mut volumes, &mut networks);

    Ok(Snapshot {
        containers,
        images,
        volumes,
        networks,
    })
}

/// Subscribe to `docker events` and wake the poller on anything interesting.
/// Reconnects with backoff so a daemon restart is survivable.
async fn event_loop(client: Client, tx: mpsc::UnboundedSender<AppEvent>, wake: Arc<Notify>) {
    let mut backoff = Duration::from_millis(500);

    loop {
        let mut stream = client.docker.events(Some(EventsOptions::default()));

        while let Some(item) = stream.next().await {
            match item {
                Ok(_msg) => {
                    // Every event class we subscribe to (containers, images,
                    // volumes, networks) invalidates the snapshot, so there's
                    // nothing to discriminate on — just refresh.
                    backoff = Duration::from_millis(500);
                    wake.notify_one();
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::DaemonError(super::friendly_error(&e)));
                    break;
                }
            }
        }

        if tx.is_closed() {
            return;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(10));
    }
}
