//! Flattened view models.
//!
//! Bollard's response types are `Option` all the way down because they mirror
//! the Docker API schema. Resolving all that once, at the edge, keeps the UI
//! code free of `unwrap_or_default()` noise.

use std::collections::{HashMap, HashSet};

use bollard::models::{
    ContainerSummary, ContainerSummaryHealthStatusEnum, ContainerSummaryStateEnum, ImageSummary,
    Network, Volume,
};

use crate::util::format;

// ---------------------------------------------------------------------------
// Containers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    Running,
    Restarting,
    Paused,
    Created,
    Stopping,
    Removing,
    Exited,
    Dead,
    Unknown,
}

impl State {
    fn from_api(state: Option<&ContainerSummaryStateEnum>) -> Self {
        match state {
            Some(ContainerSummaryStateEnum::RUNNING) => Self::Running,
            Some(ContainerSummaryStateEnum::RESTARTING) => Self::Restarting,
            Some(ContainerSummaryStateEnum::PAUSED) => Self::Paused,
            Some(ContainerSummaryStateEnum::CREATED) => Self::Created,
            Some(ContainerSummaryStateEnum::STOPPING) => Self::Stopping,
            Some(ContainerSummaryStateEnum::REMOVING) => Self::Removing,
            Some(ContainerSummaryStateEnum::EXITED) => Self::Exited,
            Some(ContainerSummaryStateEnum::DEAD) => Self::Dead,
            _ => Self::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Restarting => "restarting",
            Self::Paused => "paused",
            Self::Created => "created",
            Self::Stopping => "stopping",
            Self::Removing => "removing",
            Self::Exited => "exited",
            Self::Dead => "dead",
            Self::Unknown => "unknown",
        }
    }

    pub fn is_running(self) -> bool {
        matches!(self, Self::Running | Self::Restarting)
    }

    /// Running-ish states can be stopped; everything else can be started.
    pub fn is_live(self) -> bool {
        matches!(
            self,
            Self::Running | Self::Restarting | Self::Paused | Self::Stopping
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    None,
    Starting,
    Healthy,
    Unhealthy,
}

impl Health {
    fn from_api(status: Option<&ContainerSummaryHealthStatusEnum>) -> Self {
        match status {
            Some(ContainerSummaryHealthStatusEnum::STARTING) => Self::Starting,
            Some(ContainerSummaryHealthStatusEnum::HEALTHY) => Self::Healthy,
            Some(ContainerSummaryHealthStatusEnum::UNHEALTHY) => Self::Unhealthy,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PortBinding {
    pub ip: Option<String>,
    pub public: Option<u16>,
    pub private: u16,
    pub proto: String,
}

impl PortBinding {
    pub fn display(&self) -> String {
        match self.public {
            Some(pub_port) => {
                let ip = self.ip.as_deref().unwrap_or("0.0.0.0");
                // The v6 wildcard is noise; collapse it like the CLI does.
                if ip == "::" || ip == "0.0.0.0" {
                    format!("{pub_port}→{}/{}", self.private, self.proto)
                } else {
                    format!("{ip}:{pub_port}→{}/{}", self.private, self.proto)
                }
            }
            None => format!("{}/{}", self.private, self.proto),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ContainerRow {
    pub id: String,
    pub name: String,
    pub image: String,
    pub image_id: String,
    pub command: String,
    pub state: State,
    pub health: Health,
    /// Docker's own free-text status, e.g. `Up 2 days (healthy)`.
    pub status: String,
    pub created: i64,
    pub ports: Vec<PortBinding>,
    pub networks: Vec<String>,
    pub mounts: Vec<String>,
    pub compose_project: Option<String>,
    pub compose_service: Option<String>,
}

impl ContainerRow {
    pub fn from_api(c: ContainerSummary) -> Self {
        let labels = c.labels.unwrap_or_default();
        let name = c
            .names
            .and_then(|names| names.into_iter().next())
            .map(|n| n.trim_start_matches('/').to_string())
            .unwrap_or_else(|| "<unnamed>".into());

        let mut ports: Vec<PortBinding> = c
            .ports
            .unwrap_or_default()
            .into_iter()
            .map(|p| PortBinding {
                ip: p.ip,
                public: p.public_port,
                private: p.private_port,
                proto: p.typ.map(|t| t.to_string()).unwrap_or_else(|| "tcp".into()),
            })
            .collect();
        // Docker reports one entry per host IP family; collapse the duplicates.
        ports.sort_by_key(|p| (p.public.unwrap_or(0), p.private));
        ports.dedup_by_key(|p| (p.public, p.private, p.proto.clone()));

        let mut networks: Vec<String> = c
            .network_settings
            .and_then(|ns| ns.networks)
            .map(|n| n.into_keys().collect())
            .unwrap_or_default();
        networks.sort();

        let mounts = c
            .mounts
            .unwrap_or_default()
            .into_iter()
            .filter_map(|m| m.name.or(m.source))
            .filter(|s| !s.is_empty())
            .collect();

        Self {
            id: c.id.unwrap_or_default(),
            name,
            image: c.image.unwrap_or_default(),
            image_id: c.image_id.unwrap_or_default(),
            command: c.command.unwrap_or_default(),
            state: State::from_api(c.state.as_ref()),
            health: Health::from_api(c.health.and_then(|h| h.status).as_ref()),
            status: c.status.unwrap_or_default(),
            created: c.created.unwrap_or_default(),
            ports,
            networks,
            mounts,
            compose_project: labels.get("com.docker.compose.project").cloned(),
            compose_service: labels.get("com.docker.compose.service").cloned(),
        }
    }

    /// Everything the fuzzy filter should be able to see.
    pub fn search_key(&self) -> String {
        format!(
            "{} {} {}",
            self.name,
            self.image,
            format::short_id(&self.id)
        )
    }

    pub fn ports_display(&self) -> String {
        if self.ports.is_empty() {
            return "-".into();
        }
        self.ports
            .iter()
            .map(PortBinding::display)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ImageRow {
    pub id: String,
    pub repository: String,
    pub tag: String,
    pub all_tags: Vec<String>,
    pub size: i64,
    pub shared_size: i64,
    pub created: i64,
    pub containers: i64,
    pub dangling: bool,
}

impl ImageRow {
    /// One API image can carry several tags; Docker shows one row per tag, and
    /// so do we, because that's how people think about them.
    pub fn from_api(img: &ImageSummary) -> Vec<Self> {
        let tags: Vec<String> = img
            .repo_tags
            .iter()
            .filter(|t| t.as_str() != "<none>:<none>")
            .cloned()
            .collect();

        let make = |repository: String, tag: String, dangling: bool| Self {
            id: img.id.clone(),
            repository,
            tag,
            all_tags: img.repo_tags.clone(),
            size: img.size,
            shared_size: img.shared_size,
            created: img.created,
            containers: img.containers,
            dangling,
        };

        if tags.is_empty() {
            return vec![make("<none>".into(), "<none>".into(), true)];
        }

        tags.iter()
            .map(|t| {
                // Split on the last colon, but only if it's part of the tag and
                // not a registry port (`registry:5000/app`).
                match t.rsplit_once(':') {
                    Some((repo, tag)) if !tag.contains('/') => {
                        make(repo.to_string(), tag.to_string(), false)
                    }
                    _ => make(t.clone(), "latest".into(), false),
                }
            })
            .collect()
    }

    pub fn reference(&self) -> String {
        if self.dangling {
            format::short_id(&self.id)
        } else {
            format!("{}:{}", self.repository, self.tag)
        }
    }

    pub fn search_key(&self) -> String {
        format!("{} {}", self.reference(), format::short_id(&self.id))
    }
}

// ---------------------------------------------------------------------------
// Volumes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct VolumeRow {
    pub name: String,
    pub driver: String,
    pub mountpoint: String,
    pub created: Option<i64>,
    /// Only populated when Docker gave us usage data (it usually doesn't
    /// without a `df` call).
    pub size: Option<i64>,
    pub compose_project: Option<String>,
    /// Filled in by the app by cross-referencing container mounts.
    pub used_by: usize,
}

impl VolumeRow {
    pub fn from_api(v: Volume) -> Self {
        Self {
            name: v.name,
            driver: v.driver,
            mountpoint: v.mountpoint,
            created: v.created_at.map(|d| d.timestamp()),
            size: v
                .usage_data
                .and_then(|u| if u.size < 0 { None } else { Some(u.size) }),
            compose_project: v.labels.get("com.docker.compose.project").cloned(),
            used_by: 0,
        }
    }

    pub fn search_key(&self) -> String {
        self.name.clone()
    }
}

// ---------------------------------------------------------------------------
// Networks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct NetworkRow {
    pub id: String,
    pub name: String,
    pub driver: String,
    pub scope: String,
    pub subnets: Vec<String>,
    pub internal: bool,
    pub ipv6: bool,
    pub created: Option<i64>,
    pub compose_project: Option<String>,
    /// Filled in by the app by cross-referencing container networks.
    pub used_by: usize,
}

impl NetworkRow {
    pub fn from_api(n: Network) -> Self {
        let subnets = n
            .ipam
            .and_then(|i| i.config)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|c| c.subnet)
            .collect();

        Self {
            id: n.id.unwrap_or_default(),
            name: n.name.unwrap_or_default(),
            driver: n.driver.unwrap_or_default(),
            scope: n.scope.unwrap_or_default(),
            subnets,
            internal: n.internal.unwrap_or(false),
            ipv6: n.enable_ipv6.unwrap_or(false),
            created: n.created.map(|d| d.timestamp()),
            compose_project: n
                .labels
                .unwrap_or_default()
                .get("com.docker.compose.project")
                .cloned(),
            used_by: 0,
        }
    }

    /// The three built-in networks can't be removed; don't offer to.
    pub fn is_predefined(&self) -> bool {
        matches!(self.name.as_str(), "bridge" | "host" | "none")
    }

    pub fn search_key(&self) -> String {
        format!("{} {}", self.name, self.driver)
    }
}

// ---------------------------------------------------------------------------
// Cross-referencing
// ---------------------------------------------------------------------------

/// Docker's volume and network listings don't say what is using them, but the
/// container listing does. Recomputing the reverse index whenever containers
/// refresh is cheap and lets the UI grey out things that are safe to prune.
pub fn annotate_usage(
    containers: &[ContainerRow],
    volumes: &mut [VolumeRow],
    networks: &mut [NetworkRow],
) {
    let mut vol_users: HashMap<&str, usize> = HashMap::new();
    let mut net_users: HashMap<&str, usize> = HashMap::new();

    for c in containers {
        // A container mounting the same volume twice still counts once.
        let mut seen: HashSet<&str> = HashSet::new();
        for m in &c.mounts {
            if seen.insert(m.as_str()) {
                *vol_users.entry(m.as_str()).or_default() += 1;
            }
        }
        for n in &c.networks {
            *net_users.entry(n.as_str()).or_default() += 1;
        }
    }

    for v in volumes.iter_mut() {
        v.used_by = vol_users.get(v.name.as_str()).copied().unwrap_or(0);
    }
    for n in networks.iter_mut() {
        n.used_by = net_users.get(n.name.as_str()).copied().unwrap_or(0);
    }
}

// ---------------------------------------------------------------------------
// Daemon
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct DaemonInfo {
    pub version: String,
    pub api_version: String,
    pub os: String,
    pub arch: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_rows_split_by_tag() {
        let img = ImageSummary {
            id: "sha256:abc".into(),
            parent_id: String::new(),
            repo_tags: vec!["nginx:alpine".into(), "nginx:latest".into()],
            repo_digests: vec![],
            created: 0,
            size: 100,
            shared_size: 0,
            labels: Default::default(),
            containers: 1,
            manifests: None,
            descriptor: None,
        };
        let rows = ImageRow::from_api(&img);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].reference(), "nginx:alpine");
        assert_eq!(rows[1].tag, "latest");
    }

    #[test]
    fn registry_port_is_not_a_tag() {
        let img = ImageSummary {
            id: "sha256:abc".into(),
            parent_id: String::new(),
            repo_tags: vec!["registry:5000/team/app".into()],
            repo_digests: vec![],
            created: 0,
            size: 100,
            shared_size: 0,
            labels: Default::default(),
            containers: 0,
            manifests: None,
            descriptor: None,
        };
        let rows = ImageRow::from_api(&img);
        assert_eq!(rows[0].repository, "registry:5000/team/app");
        assert_eq!(rows[0].tag, "latest");
    }

    #[test]
    fn untagged_image_is_dangling() {
        let img = ImageSummary {
            id: "sha256:abc".into(),
            parent_id: String::new(),
            repo_tags: vec!["<none>:<none>".into()],
            repo_digests: vec![],
            created: 0,
            size: 100,
            shared_size: 0,
            labels: Default::default(),
            containers: 0,
            manifests: None,
            descriptor: None,
        };
        let rows = ImageRow::from_api(&img);
        assert!(rows[0].dangling);
    }

    #[test]
    fn usage_is_counted_once_per_container() {
        let containers = vec![ContainerRow {
            id: "1".into(),
            name: "a".into(),
            image: String::new(),
            image_id: String::new(),
            command: String::new(),
            state: State::Running,
            health: Health::None,
            status: String::new(),
            created: 0,
            ports: vec![],
            networks: vec!["bridge".into()],
            mounts: vec!["data".into(), "data".into()],
            compose_project: None,
            compose_service: None,
        }];
        let mut volumes = vec![VolumeRow {
            name: "data".into(),
            driver: "local".into(),
            mountpoint: String::new(),
            created: None,
            size: None,
            compose_project: None,
            used_by: 0,
        }];
        let mut networks = vec![NetworkRow {
            id: "n".into(),
            name: "bridge".into(),
            driver: "bridge".into(),
            scope: "local".into(),
            subnets: vec![],
            internal: false,
            ipv6: false,
            created: None,
            compose_project: None,
            used_by: 0,
        }];
        annotate_usage(&containers, &mut volumes, &mut networks);
        assert_eq!(volumes[0].used_by, 1);
        assert_eq!(networks[0].used_by, 1);
    }
}
