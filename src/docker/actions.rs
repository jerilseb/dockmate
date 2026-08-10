//! Mutations.
//!
//! Every one of these runs as a detached task so the UI never blocks on the
//! daemon — a `stop` on a container that ignores SIGTERM takes ten seconds, and
//! the rest of the app has to stay responsive throughout.

use bollard::query_parameters::{
    KillContainerOptions, PruneContainersOptions, PruneImagesOptions, PruneNetworksOptions,
    PruneVolumesOptions, RemoveContainerOptions, RemoveImageOptions, RemoveVolumeOptions,
    RestartContainerOptions, StopContainerOptions,
};
use tokio::sync::mpsc;

use crate::docker::{Client, friendly_error};
use crate::event::AppEvent;

/// A mutation, fully resolved against a target. Held by the confirm dialog
/// between "the user pressed `d`" and "the user said yes".
#[derive(Debug, Clone)]
pub enum Job {
    Start {
        id: String,
        name: String,
    },
    Stop {
        id: String,
        name: String,
    },
    Restart {
        id: String,
        name: String,
    },
    Pause {
        id: String,
        name: String,
    },
    Unpause {
        id: String,
        name: String,
    },
    Kill {
        id: String,
        name: String,
    },
    RemoveContainer {
        id: String,
        name: String,
        force: bool,
    },
    RemoveImage {
        id: String,
        name: String,
        force: bool,
    },
    RemoveVolume {
        name: String,
        force: bool,
    },
    RemoveNetwork {
        id: String,
        name: String,
    },
    PruneContainers,
    PruneImages,
    PruneVolumes,
    PruneNetworks,
}

impl Job {
    /// The container id this job is in flight against, so the table can show a
    /// spinner on the right row.
    pub fn target_id(&self) -> Option<&str> {
        match self {
            Self::Start { id, .. }
            | Self::Stop { id, .. }
            | Self::Restart { id, .. }
            | Self::Pause { id, .. }
            | Self::Unpause { id, .. }
            | Self::Kill { id, .. }
            | Self::RemoveContainer { id, .. }
            | Self::RemoveImage { id, .. }
            | Self::RemoveNetwork { id, .. } => Some(id),
            Self::RemoveVolume { name, .. } => Some(name),
            _ => None,
        }
    }

    /// One present-tense word, shown in the row's state column while the job is
    /// in flight. The full description doesn't fit there and truncates to
    /// something unreadable like `stopping do…`.
    pub fn verb(&self) -> &'static str {
        match self {
            Self::Start { .. } => "starting",
            Self::Stop { .. } => "stopping",
            Self::Restart { .. } => "restarting",
            Self::Pause { .. } => "pausing",
            Self::Unpause { .. } => "resuming",
            Self::Kill { .. } => "killing",
            Self::RemoveContainer { .. }
            | Self::RemoveImage { .. }
            | Self::RemoveVolume { .. }
            | Self::RemoveNetwork { .. } => "removing",
            Self::PruneContainers
            | Self::PruneImages
            | Self::PruneVolumes
            | Self::PruneNetworks => "pruning",
        }
    }

    /// Present-tense description, shown in toasts and the pending list.
    pub fn describe(&self) -> String {
        match self {
            Self::Start { name, .. } => format!("starting {name}"),
            Self::Stop { name, .. } => format!("stopping {name}"),
            Self::Restart { name, .. } => format!("restarting {name}"),
            Self::Pause { name, .. } => format!("pausing {name}"),
            Self::Unpause { name, .. } => format!("resuming {name}"),
            Self::Kill { name, .. } => format!("killing {name}"),
            Self::RemoveContainer { name, .. } => format!("removing container {name}"),
            Self::RemoveImage { name, .. } => format!("removing image {name}"),
            Self::RemoveVolume { name, .. } => format!("removing volume {name}"),
            Self::RemoveNetwork { name, .. } => format!("removing network {name}"),
            Self::PruneContainers => "pruning stopped containers".into(),
            Self::PruneImages => "pruning dangling images".into(),
            Self::PruneVolumes => "pruning unused volumes".into(),
            Self::PruneNetworks => "pruning unused networks".into(),
        }
    }

    /// Past-tense confirmation, shown in the success toast.
    fn done_message(&self) -> String {
        match self {
            Self::Start { name, .. } => format!("started {name}"),
            Self::Stop { name, .. } => format!("stopped {name}"),
            Self::Restart { name, .. } => format!("restarted {name}"),
            Self::Pause { name, .. } => format!("paused {name}"),
            Self::Unpause { name, .. } => format!("resumed {name}"),
            Self::Kill { name, .. } => format!("killed {name}"),
            Self::RemoveContainer { name, .. } => format!("removed container {name}"),
            Self::RemoveImage { name, .. } => format!("removed image {name}"),
            Self::RemoveVolume { name, .. } => format!("removed volume {name}"),
            Self::RemoveNetwork { name, .. } => format!("removed network {name}"),
            _ => "done".into(),
        }
    }

    /// Destructive jobs get a confirmation dialog first.
    pub fn needs_confirmation(&self) -> bool {
        matches!(
            self,
            Self::Kill { .. }
                | Self::RemoveContainer { .. }
                | Self::RemoveImage { .. }
                | Self::RemoveVolume { .. }
                | Self::RemoveNetwork { .. }
                | Self::PruneContainers
                | Self::PruneImages
                | Self::PruneVolumes
                | Self::PruneNetworks
        )
    }

    /// The question the confirm dialog asks.
    pub fn confirm_prompt(&self) -> (String, String) {
        match self {
            Self::Kill { name, .. } => (
                "Kill container?".into(),
                format!("{name} will be sent SIGKILL and stopped immediately."),
            ),
            Self::RemoveContainer { name, .. } => (
                "Remove container?".into(),
                format!("{name} will be deleted. Anonymous volumes are kept."),
            ),
            Self::RemoveImage { name, .. } => {
                ("Remove image?".into(), format!("{name} will be deleted."))
            }
            Self::RemoveVolume { name, .. } => (
                "Remove volume?".into(),
                format!("{name} and everything stored in it will be deleted."),
            ),
            Self::RemoveNetwork { name, .. } => {
                ("Remove network?".into(), format!("{name} will be deleted."))
            }
            Self::PruneContainers => (
                "Prune containers?".into(),
                "Every stopped container will be deleted.".into(),
            ),
            Self::PruneImages => (
                "Prune images?".into(),
                "Every dangling image will be deleted.".into(),
            ),
            Self::PruneVolumes => (
                "Prune volumes?".into(),
                "Every volume not used by a container will be deleted, along with its data.".into(),
            ),
            Self::PruneNetworks => (
                "Prune networks?".into(),
                "Every network not used by a container will be deleted.".into(),
            ),
            other => ("Are you sure?".into(), other.describe()),
        }
    }
}

/// Run a job and report the outcome back to the UI.
pub fn spawn(client: Client, job: Job, tx: mpsc::UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        let result = run(&client, &job).await;
        let _ = tx.send(AppEvent::JobDone { job, result });
    });
}

async fn run(client: &Client, job: &Job) -> Result<String, String> {
    let d = &client.docker;

    let outcome = match job {
        Job::Start { id, .. } => d
            .start_container(id, None)
            .await
            .map(|_| job.done_message()),
        Job::Stop { id, .. } => d
            .stop_container(id, Some(StopContainerOptions::default()))
            .await
            .map(|_| job.done_message()),
        Job::Restart { id, .. } => d
            .restart_container(id, Some(RestartContainerOptions::default()))
            .await
            .map(|_| job.done_message()),
        Job::Pause { id, .. } => d.pause_container(id).await.map(|_| job.done_message()),
        Job::Unpause { id, .. } => d.unpause_container(id).await.map(|_| job.done_message()),
        Job::Kill { id, .. } => d
            .kill_container(
                id,
                Some(KillContainerOptions {
                    signal: "SIGKILL".into(),
                }),
            )
            .await
            .map(|_| job.done_message()),
        Job::RemoveContainer { id, force, .. } => d
            .remove_container(
                id,
                Some(RemoveContainerOptions {
                    force: *force,
                    ..Default::default()
                }),
            )
            .await
            .map(|_| job.done_message()),
        Job::RemoveImage { id, force, .. } => d
            .remove_image(
                id,
                Some(RemoveImageOptions {
                    force: *force,
                    ..Default::default()
                }),
                None,
            )
            .await
            .map(|_| job.done_message()),
        Job::RemoveVolume { name, force } => d
            .remove_volume(name, Some(RemoveVolumeOptions { force: *force }))
            .await
            .map(|_| job.done_message()),
        Job::RemoveNetwork { id, .. } => d.remove_network(id).await.map(|_| job.done_message()),

        Job::PruneContainers => d
            .prune_containers(Some(PruneContainersOptions::default()))
            .await
            .map(|r| {
                let n = r.containers_deleted.map(|v| v.len()).unwrap_or(0);
                format!(
                    "pruned {n} container{} ({} reclaimed)",
                    plural(n),
                    crate::util::format::bytes(r.space_reclaimed.unwrap_or(0).max(0) as u64)
                )
            }),
        Job::PruneImages => d
            .prune_images(Some(PruneImagesOptions::default()))
            .await
            .map(|r| {
                let n = r.images_deleted.map(|v| v.len()).unwrap_or(0);
                format!(
                    "pruned {n} image{} ({} reclaimed)",
                    plural(n),
                    crate::util::format::bytes(r.space_reclaimed.unwrap_or(0).max(0) as u64)
                )
            }),
        Job::PruneVolumes => d
            .prune_volumes(Some(PruneVolumesOptions::default()))
            .await
            .map(|r| {
                let n = r.volumes_deleted.map(|v| v.len()).unwrap_or(0);
                format!(
                    "pruned {n} volume{} ({} reclaimed)",
                    plural(n),
                    crate::util::format::bytes(r.space_reclaimed.unwrap_or(0).max(0) as u64)
                )
            }),
        Job::PruneNetworks => d
            .prune_networks(Some(PruneNetworksOptions::default()))
            .await
            .map(|r| {
                let n = r.networks_deleted.map(|v| v.len()).unwrap_or(0);
                format!("pruned {n} network{}", plural(n))
            }),
    };

    outcome.map_err(|e| friendly_error(&e))
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
