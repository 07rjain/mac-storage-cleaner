//! Large data that apps manage themselves. Its size is shown with a way to the app, but it is
//! never added to the basket.

use std::path::PathBuf;

use scanner::{NodeFlags, NodeId, NodeKind, Tree};

use crate::Places;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedKind {
    PhotosLibrary,
    DeviceBackups,
    MailDownloads,
    MessagesAttachments,
    DockerDisk,
    VirtualMachines,
}

/// How to get to the place where the data can be managed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opener {
    /// An app, by bundle ID.
    App(&'static str),
    /// A System Settings pane, by URL.
    Settings(&'static str),
    RevealInFinder,
}

const STORAGE_SETTINGS: &str = "x-apple.systempreferences:com.apple.settings.Storage";

impl ManagedKind {
    pub fn title(self) -> &'static str {
        match self {
            Self::PhotosLibrary => "Photos library",
            Self::DeviceBackups => "iPhone and iPad backups",
            Self::MailDownloads => "Mail downloads",
            Self::MessagesAttachments => "Messages attachments",
            Self::DockerDisk => "Docker disk image",
            Self::VirtualMachines => "Virtual machines",
        }
    }

    pub fn advice(self) -> &'static str {
        match self {
            Self::PhotosLibrary => "Delete photos in Photos, or turn on Optimize Mac Storage",
            Self::DeviceBackups => "Remove old backups in System Settings > General > Storage",
            Self::MailDownloads => "Attachments you opened from Mail; Mail manages them",
            Self::MessagesAttachments => {
                "Review large attachments in System Settings > General > Storage"
            }
            Self::DockerDisk => "Prune images and volumes in Docker, or shrink its disk limit",
            Self::VirtualMachines => "Delete or compact virtual machines in their app",
        }
    }

    pub fn opener(self) -> Opener {
        match self {
            Self::PhotosLibrary => Opener::App("com.apple.Photos"),
            Self::DeviceBackups | Self::MessagesAttachments => Opener::Settings(STORAGE_SETTINGS),
            Self::MailDownloads => Opener::App("com.apple.mail"),
            Self::DockerDisk => Opener::App("com.docker.docker"),
            Self::VirtualMachines => Opener::RevealInFinder,
        }
    }

    pub fn open_label(self) -> &'static str {
        match self.opener() {
            Opener::App(_) => match self {
                Self::PhotosLibrary => "Open Photos",
                Self::MailDownloads => "Open Mail",
                _ => "Open Docker",
            },
            Opener::Settings(_) => "Open Storage settings",
            Opener::RevealInFinder => "Show in Finder",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ManagedPlace {
    pub kind: ManagedKind,
    pub node: NodeId,
    pub path: PathBuf,
    pub size: u64,
}

const PLACES: &[(ManagedKind, &str)] = &[
    (
        ManagedKind::DeviceBackups,
        "Library/Application Support/MobileSync/Backup",
    ),
    (
        ManagedKind::MailDownloads,
        "Library/Containers/com.apple.mail/Data/Library/Mail Downloads",
    ),
    (
        ManagedKind::MessagesAttachments,
        "Library/Messages/Attachments",
    ),
    (
        ManagedKind::DockerDisk,
        "Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw",
    ),
    (ManagedKind::VirtualMachines, "Parallels"),
    (ManagedKind::VirtualMachines, "Virtual Machines.localized"),
    (
        ManagedKind::VirtualMachines,
        "Library/Containers/com.utmapp.UTM/Data/Documents",
    ),
];

/// Managed places found in the scan, largest first. Empty ones are left out.
pub fn managed_places(tree: &Tree, places: &Places) -> Vec<ManagedPlace> {
    let find = |relative: &str| {
        tree.find(&Places::tree_path(
            tree.root_path(),
            &places.in_home(relative),
        ))
        .filter(|&id| !tree.flags(id).contains(NodeFlags::REMOVED) && tree.allocated(id) > 0)
    };
    let place = |kind, id: NodeId| ManagedPlace {
        kind,
        node: id,
        path: Places::user_path(&tree.path(id)),
        size: tree.allocated(id),
    };
    let mut found = Vec::new();
    if let Some(pictures) = find("Pictures") {
        for child in tree.children(pictures) {
            let name = tree.name(child).to_string_lossy().to_lowercase();
            if tree.kind(child) == NodeKind::Directory
                && name.ends_with(".photoslibrary")
                && tree.allocated(child) > 0
            {
                found.push(place(ManagedKind::PhotosLibrary, child));
            }
        }
    }
    for (kind, relative) in PLACES {
        if let Some(id) = find(relative) {
            found.push(place(*kind, id));
        }
    }
    found.sort_by_key(|place| std::cmp::Reverse(place.size));
    found
}
