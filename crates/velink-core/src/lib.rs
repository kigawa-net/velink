//! Shared, UI-independent state model for the Velink desktop MVP.
//!
//! This crate intentionally has no GUI or runtime dependencies so the IDs,
//! tab ownership, split layout, and launch lifecycle can be used by the UI
//! and runtime adapters without coupling them together.

pub mod remote;

use std::collections::BTreeMap;
use std::process::{Child, Command, Stdio};

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(pub u64);
    };
}

id_type!(TabId);
id_type!(ContentId);
id_type!(LaunchRequestId);
id_type!(RuntimeInstanceId);
id_type!(GuiSessionId);
id_type!(ManagedWindowId);
id_type!(SurfaceId);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserKind {
    Firefox,
    Chromium,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserLaunch {
    pub kind: BrowserKind,
    pub url: String,
}

impl BrowserLaunch {
    pub fn validate(&self) -> Result<(), ModelError> {
        let url = self.url.trim();
        if url.is_empty() {
            return Err(ModelError::EmptyUrl);
        }
        if url.starts_with("velink:") {
            return Err(ModelError::InternalSchemeNotAllowed);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContentTarget {
    ExternalBrowser {
        launch_request_id: LaunchRequestId,
        runtime_instance_id: RuntimeInstanceId,
        session_id: Option<GuiSessionId>,
        window_id: Option<ManagedWindowId>,
        surface_id: Option<SurfaceId>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tab {
    pub id: TabId,
    pub content_id: ContentId,
    pub title: String,
    pub target: ContentTarget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SplitAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LayoutNode {
    Leaf {
        tab_id: TabId,
    },
    Split {
        axis: SplitAxis,
        ratio: f32,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

impl LayoutNode {
    pub fn split(self, other: Self, axis: SplitAxis, ratio: f32) -> Result<Self, ModelError> {
        if !ratio.is_finite() || !(0.1..=0.9).contains(&ratio) {
            return Err(ModelError::InvalidSplitRatio);
        }
        Ok(Self::Split {
            axis,
            ratio,
            first: Box::new(self),
            second: Box::new(other),
        })
    }

    pub fn contains(&self, target: TabId) -> bool {
        match self {
            Self::Leaf { tab_id } => *tab_id == target,
            Self::Split { first, second, .. } => first.contains(target) || second.contains(target),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelError {
    EmptyUrl,
    InternalSchemeNotAllowed,
    InvalidSplitRatio,
    DuplicateTab,
    UnknownTab,
    LayoutReferencesUnknownTab,
}

#[derive(Debug)]
pub enum LaunchError {
    InvalidRequest(ModelError),
    ExecutableNotFound(Vec<String>),
    Spawn(std::io::Error),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest(error) => write!(f, "invalid launch request: {error:?}"),
            Self::ExecutableNotFound(candidates) => {
                write!(
                    f,
                    "browser executable not found (tried {})",
                    candidates.join(", ")
                )
            }
            Self::Spawn(error) => write!(f, "failed to launch browser: {error}"),
        }
    }
}

impl std::error::Error for LaunchError {}

/// Handle to a browser process started by the Local Native runtime adapter.
///
/// This adapter does not claim to isolate the browser or capture its windows;
/// it only owns the child process lifecycle. A future Wayland runtime can
/// replace it while preserving the BrowserLaunch request type.
pub struct BrowserProcess {
    child: Child,
}

impl BrowserProcess {
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>, std::io::Error> {
        self.child.try_wait()
    }

    pub fn terminate(&mut self) -> Result<(), std::io::Error> {
        self.child.kill()
    }
}

pub fn launch_local_browser(request: &BrowserLaunch) -> Result<BrowserProcess, LaunchError> {
    request.validate().map_err(LaunchError::InvalidRequest)?;
    let candidates: &[&str] = match request.kind {
        BrowserKind::Firefox => &["firefox", "firefox.exe"],
        BrowserKind::Chromium => &[
            "chromium",
            "chromium-browser",
            "google-chrome",
            "chrome.exe",
            "msedge.exe",
        ],
    };
    let executable = candidates
        .iter()
        .find_map(|candidate| executable_on_path(candidate))
        .ok_or_else(|| {
            LaunchError::ExecutableNotFound(candidates.iter().map(|s| (*s).to_owned()).collect())
        })?;

    let mut command = Command::new(executable);
    if request.kind == BrowserKind::Firefox {
        command.arg("--new-window");
    }
    command
        .arg(request.url.trim())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = command.spawn().map_err(LaunchError::Spawn)?;
    Ok(BrowserProcess { child })
}

fn executable_on_path(name: &str) -> Option<std::path::PathBuf> {
    let path_exts: Vec<String> = if cfg!(windows) {
        std::env::var_os("PATHEXT")
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into())
            .to_string_lossy()
            .split(';')
            .map(str::to_owned)
            .collect()
    } else {
        vec![String::new()]
    };
    let directories: Vec<_> = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .collect();
    for directory in directories {
        let direct = directory.join(name);
        if direct.is_file() {
            return Some(direct);
        }
        if cfg!(windows) && std::path::Path::new(name).extension().is_none() {
            for ext in &path_exts {
                let candidate = directory.join(format!("{name}{ext}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

#[derive(Default)]
pub struct BrowserState {
    tabs: BTreeMap<TabId, Tab>,
    layout: Option<LayoutNode>,
}

impl BrowserState {
    pub fn tabs(&self) -> impl Iterator<Item = &Tab> {
        self.tabs.values()
    }

    pub fn layout(&self) -> Option<&LayoutNode> {
        self.layout.as_ref()
    }

    pub fn add_tab(&mut self, tab: Tab) -> Result<(), ModelError> {
        if self.tabs.contains_key(&tab.id) {
            return Err(ModelError::DuplicateTab);
        }
        let id = tab.id;
        self.tabs.insert(id, tab);
        if self.layout.is_none() {
            self.layout = Some(LayoutNode::Leaf { tab_id: id });
        }
        Ok(())
    }

    pub fn set_layout(&mut self, layout: LayoutNode) -> Result<(), ModelError> {
        if layout_tab_ids(&layout)
            .iter()
            .any(|id| !self.tabs.contains_key(id))
        {
            return Err(ModelError::LayoutReferencesUnknownTab);
        }
        self.layout = Some(layout);
        Ok(())
    }

    pub fn close_tab(&mut self, id: TabId) -> Result<Tab, ModelError> {
        let tab = self.tabs.remove(&id).ok_or(ModelError::UnknownTab)?;
        self.layout = self
            .layout
            .take()
            .and_then(|layout| prune_layout(layout, id));
        Ok(tab)
    }
}

fn layout_tab_ids(node: &LayoutNode) -> Vec<TabId> {
    match node {
        LayoutNode::Leaf { tab_id } => vec![*tab_id],
        LayoutNode::Split { first, second, .. } => {
            let mut ids = layout_tab_ids(first);
            ids.extend(layout_tab_ids(second));
            ids
        }
    }
}

fn prune_layout(node: LayoutNode, removed: TabId) -> Option<LayoutNode> {
    match node {
        LayoutNode::Leaf { tab_id } if tab_id == removed => None,
        leaf @ LayoutNode::Leaf { .. } => Some(leaf),
        LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            match (
                prune_layout(*first, removed),
                prune_layout(*second, removed),
            ) {
                (Some(first), Some(second)) => Some(LayoutNode::Split {
                    axis,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            }
        }
    }
}
