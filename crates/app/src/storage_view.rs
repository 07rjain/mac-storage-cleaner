mod basket_ui;

use std::cell::Cell;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Bounds, ClickEvent, Context, Div, FocusHandle, FontWeight, Hsla, MouseButton,
    MouseDownEvent, MouseMoveEvent, PathPromptOptions, Pixels, Point, Rgba, Role, ScrollStrategy,
    SharedString, Stateful, Subscription, Task, UniformListScrollHandle, Window, actions, anchored,
    canvas, deferred, div, prelude::*, px, relative, uniform_list,
};
use scanner::{EventBatch, NodeId, NodeKind, Refresh, ScanHandle, ScanOptions, Tree};
use volumes::{DATA_VOLUME_MOUNT_POINT, StartupDisk};

use self::basket_ui::{Cleanup, DragPreview, DraggedItem};
use crate::access;
use crate::compare;
use crate::file_types::FileType;
use crate::format;
use crate::model::{self, Accounting, Item, Row};
use crate::settings::{Chart, Settings};
use crate::sunburst::{self, Geometry, Hit, Segment};
use crate::theme::{self, Theme};
use crate::treemap::{self, Kind as TileKind};
use crate::widgets::button;
use crate::{ShowSunburst, ShowTreemap};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const WATCH_DEBOUNCE: Duration = Duration::from_millis(1_200);
const ROW_HEIGHT: f32 = 28.0;
const DROPDOWN_LIMIT: usize = 60;

actions!(
    storage,
    [
        SelectNext,
        SelectPrevious,
        OpenSelected,
        GoUp,
        GoToTop,
        Dismiss,
        RevealInFinder,
        Rescan,
        StopScan,
        ScanStartupDisk,
        ScanHomeFolder,
        ScanFolder,
        AddToBasket,
        QuickLook,
        ReviewBasket,
        ShowHistory,
        EmptyBasket,
    ]
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    StartupDisk,
    Folder(PathBuf),
}

impl Scope {
    fn root(&self) -> PathBuf {
        match self {
            Self::StartupDisk => PathBuf::from(DATA_VOLUME_MOUNT_POINT),
            Self::Folder(path) => path.clone(),
        }
    }
}

fn home_folder() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Capacity of the volume being scanned, for the bar at the top.
struct Disk {
    name: String,
    capacity: u64,
    available: u64,
    purgeable: Option<u64>,
    snapshots: Option<usize>,
    /// Only for the startup disk, where the chart's root accounts for the whole container.
    data_volume_used: Option<u64>,
}

impl Disk {
    fn read(scope: &Scope) -> Option<Self> {
        match scope {
            Scope::StartupDisk => {
                let disk = StartupDisk::read().ok()?;
                Some(Self {
                    name: volumes::volume_name(Path::new("/"))
                        .unwrap_or_else(|| "Startup disk".into()),
                    capacity: disk.capacity(),
                    available: disk.available(),
                    purgeable: disk.purgeable,
                    snapshots: disk.local_snapshots,
                    data_volume_used: Some(disk.data_volume.used),
                })
            }
            Scope::Folder(path) => {
                let volume = volumes::volume_at(path).ok()?;
                Some(Self {
                    name: volumes::volume_name(path)
                        .unwrap_or_else(|| volume.mount_point.display().to_string()),
                    capacity: volume.capacity,
                    available: volume.available,
                    purgeable: None,
                    snapshots: None,
                    data_volume_used: None,
                })
            }
        }
    }

    fn used(&self) -> u64 {
        self.capacity.saturating_sub(self.available)
    }
}

struct Scan {
    handle: ScanHandle,
    started: Instant,
    /// Set once the scan thread has exited, finished or stopped.
    elapsed: Option<Duration>,
}

/// One folder scanned again, to be swapped into the finished scan's tree.
struct FolderRescan {
    name: SharedString,
    _task: Task<()>,
}

/// What the chart center, the status bar and the tooltips say about one item.
#[derive(Debug, Clone)]
struct Info {
    item: Item,
    name: SharedString,
    size: u64,
    settled: bool,
    items: Option<u64>,
    note: Option<SharedString>,
    path: Option<PathBuf>,
}

/// Everything a frame needs from the tree, read under one short lock.
struct Snapshot {
    crumbs: Vec<(NodeId, SharedString)>,
    dropdown: Vec<(NodeId, SharedString, String)>,
    folder: Info,
    hovered: Option<Info>,
    selected: Option<Info>,
    entries: u64,
    complete: bool,
    tile_labels: Vec<TileLabel>,
}

/// Text drawn on a treemap rectangle.
struct TileLabel {
    /// Into `StorageView::tiles`.
    index: usize,
    name: SharedString,
    size: Option<String>,
    /// Folders show their name in the strip above their contents.
    header: bool,
}

/// What is under the pointer in either chart.
enum ChartHit {
    /// The sunburst's center, which goes up a level.
    Center,
    Item {
        item: Item,
        folder: bool,
    },
}

pub struct StorageView {
    focus_handle: FocusHandle,
    scope: Scope,
    generation: u64,
    scan: Option<Scan>,
    error: Option<String>,
    disk: Option<Disk>,
    folder: NodeId,
    selected: Option<Item>,
    hovered: Option<Item>,
    dropdown: Option<NodeId>,
    scroll_to_selection: bool,
    rows: Rc<Vec<Row>>,
    chart: Chart,
    segments: Rc<Vec<Segment>>,
    geometry: Rc<Cell<Option<Geometry>>>,
    tiles: Rc<treemap::Layout>,
    /// Where the treemap was last drawn; the next layout uses its size.
    treemap_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    list_scroll: UniformListScrollHandle,
    cleanup: Cleanup,
    folder_rescan: Option<FolderRescan>,
    watch: Option<scanner::Watch>,
    pending_changes: Vec<PathBuf>,
    pending_flags: u32,
    history_done: bool,
    comparison: Option<compare::Comparison>,
    /// As last checked; `None` until the window is first activated.
    full_disk_access: Option<bool>,
    _poll: Option<Task<()>>,
    _disk_read: Option<Task<()>>,
    _watch_task: Option<Task<()>>,
    _compare_task: Option<Task<()>>,
    _activation: Option<Subscription>,
}

impl StorageView {
    pub fn new(scope: Scope, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            focus_handle: cx.focus_handle(),
            scope: scope.clone(),
            generation: 0,
            scan: None,
            error: None,
            disk: None,
            folder: 0,
            selected: None,
            hovered: None,
            dropdown: None,
            scroll_to_selection: false,
            rows: Rc::default(),
            chart: Chart::default(),
            segments: Rc::default(),
            geometry: Rc::default(),
            tiles: Rc::default(),
            treemap_bounds: Rc::default(),
            list_scroll: UniformListScrollHandle::new(),
            cleanup: Cleanup::new(),
            folder_rescan: None,
            watch: None,
            pending_changes: Vec::new(),
            pending_flags: 0,
            history_done: false,
            comparison: None,
            full_disk_access: None,
            _poll: None,
            _disk_read: None,
            _watch_task: None,
            _compare_task: None,
            _activation: None,
        };
        view.start_scan(scope, cx);
        view
    }

    /// Tells the user to rescan when Full Disk Access is turned on while the app runs.
    pub fn watch_full_disk_access(
        &mut self,
        check_access: fn() -> bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self._activation = Some(
            cx.observe_window_activation(window, move |view, window, cx| {
                if !window.is_window_active() {
                    return;
                }
                let granted = check_access();
                let was = view.full_disk_access.replace(granted);
                if was == Some(false) && granted && view.scope == Scope::StartupDisk {
                    view.show_notice(
                        "Full Disk Access is on. Rescan (⌘R) to measure protected folders.",
                        cx,
                    );
                }
            }),
        );
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    fn start_scan(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if let Some(scan) = self.scan.take() {
            scan.handle.cancel();
        }
        self.generation += 1;
        self.scope = scope.clone();
        self.error = None;
        self.folder = 0;
        self.selected = None;
        self.hovered = None;
        self.dropdown = None;
        self.rows = Rc::default();
        self.segments = Rc::default();
        self.tiles = Rc::default();
        self.disk = None;
        self.cleanup.reset(&scope.root());
        self.folder_rescan = None;
        self.watch = None;
        self._watch_task = None;
        self.pending_changes.clear();
        self.pending_flags = 0;
        self.history_done = false;
        self.comparison = None;
        self._compare_task = None;
        self.read_disk(cx);

        let generation = self.generation;
        match scanner::start(ScanOptions::new(scope.root())) {
            Ok(handle) => {
                self.scan = Some(Scan {
                    handle,
                    started: Instant::now(),
                    elapsed: None,
                });
                self._poll = Some(cx.spawn(async move |this, cx| {
                    loop {
                        cx.background_executor().timer(POLL_INTERVAL).await;
                        let running = this.update(cx, |view, cx| view.poll(generation, cx));
                        if !matches!(running, Ok(true)) {
                            break;
                        }
                    }
                }));
            }
            Err(error) => {
                tracing::warn!("scan could not start: {error}");
                self.error = Some(format!("This location can't be scanned: {error}."));
            }
        }
        cx.notify();
    }

    fn read_disk(&mut self, cx: &mut Context<Self>) {
        let generation = self.generation;
        let scope = self.scope.clone();
        self._disk_read = Some(cx.spawn(async move |this, cx| {
            let disk = cx
                .background_executor()
                .spawn(async move { Disk::read(&scope) })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.generation == generation {
                    view.disk = disk;
                    cx.notify();
                }
            });
        }));
    }

    /// Repaints while the scan runs. Returns `false` once there is nothing more to show.
    fn poll(&mut self, generation: u64, cx: &mut Context<Self>) -> bool {
        if generation != self.generation {
            return false;
        }
        if self.scan.is_none() {
            return false;
        }
        cx.notify();
        let finished = self
            .scan
            .as_ref()
            .is_some_and(|scan| scan.handle.is_finished());
        if finished {
            if let Some(scan) = &mut self.scan {
                scan.elapsed = Some(scan.started.elapsed());
            }
            self.read_disk(cx);
            self.compute_suggestions(cx);
            self.start_watch(cx);
            self.save_comparison(cx);
            return false;
        }
        true
    }

    fn is_scanning(&self) -> bool {
        self.scan
            .as_ref()
            .is_some_and(|scan| scan.elapsed.is_none())
    }

    /// Whether "Rescan This Folder" can run now: the scan finished and nothing else is changing
    /// the tree.
    fn can_rescan_folder(&self) -> bool {
        !self.is_scanning()
            && self.folder_rescan.is_none()
            && !self.cleanup.moving
            && self.with_tree(Tree::is_complete).unwrap_or(false)
    }

    /// Scans one folder again and swaps the result into the tree, without a full rescan.
    fn rescan_folder(&mut self, id: NodeId, cx: &mut Context<Self>) {
        if self.with_tree(|tree| id == tree.root()).unwrap_or(false) {
            self.start_scan(self.scope.clone(), cx);
            return;
        }
        if !self.can_rescan_folder() {
            self.show_notice("Wait for the scan to finish before rescanning a folder", cx);
            return;
        }
        let Some(scan) = &self.scan else {
            return;
        };
        let shared = scan.handle.shared_tree();
        let Some((path, name)) = self.with_tree(|tree| (tree.path(id), self.name(tree, id))) else {
            return;
        };
        let generation = self.generation;
        let task = cx.spawn(async move |this, cx| {
            let rescan = cx
                .background_executor()
                .spawn(async move { scanner::scan(ScanOptions::new(path)) })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.generation != generation {
                    return;
                }
                let name = view
                    .folder_rescan
                    .take()
                    .map_or_else(SharedString::default, |rescan| rescan.name);
                match rescan {
                    Ok(rescan) if shared.replace(id, &rescan) => {
                        view.tree_changed(cx);
                        view.show_notice(format!("Rescanned {name}"), cx);
                        view.apply_pending_watch(cx);
                    }
                    Ok(_) => view.show_notice(format!("{name} couldn't be updated"), cx),
                    Err(error) => {
                        tracing::warn!(kind = ?error.kind(), "rescanning a folder failed");
                        view.show_notice(format!("{name} can't be scanned: {error}"), cx);
                    }
                }
            });
        });
        self.folder_rescan = Some(FolderRescan { name, _task: task });
        cx.notify();
    }

    fn start_watch(&mut self, cx: &mut Context<Self>) {
        if cfg!(test) {
            return;
        }
        self.watch = None;
        self._watch_task = None;
        self.pending_changes.clear();
        self.pending_flags = 0;
        self.history_done = false;
        let Some(scan) = &self.scan else {
            return;
        };
        if !scan.handle.tree().is_complete() {
            return;
        }
        let path = self.scope.root();
        let since = scan.handle.since_event_id();
        let (watch, rx) = match scanner::Watch::start(&path, since) {
            Ok(pair) => pair,
            Err(error) => {
                tracing::warn!("watching for file changes failed: {error}");
                return;
            }
        };
        self.watch = Some(watch);
        let rx = Arc::new(Mutex::new(rx));
        let generation = self.generation;
        self._watch_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let rx_recv = Arc::clone(&rx);
                let first = cx
                    .background_executor()
                    .spawn(async move { rx_recv.lock().ok()?.recv().ok() })
                    .await;
                let Some(mut batch) = first else {
                    break;
                };
                let deadline = Instant::now() + WATCH_DEBOUNCE;
                loop {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    let rx_wait = Arc::clone(&rx);
                    let extra = cx
                        .background_executor()
                        .spawn(async move { rx_wait.lock().ok()?.recv_timeout(remaining).ok() })
                        .await;
                    let Some(more) = extra else {
                        break;
                    };
                    batch.paths.extend(more.paths);
                    batch.flags |= more.flags;
                }
                let running =
                    this.update(cx, |view, cx| view.apply_watch_batch(generation, batch, cx));
                if !matches!(running, Ok(true)) {
                    break;
                }
            }
        }));
    }

    fn apply_watch_batch(
        &mut self,
        generation: u64,
        batch: EventBatch,
        cx: &mut Context<Self>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        if batch.history_done() {
            self.history_done = true;
        }
        if batch.paths.is_empty() && !batch.needs_full_scan() {
            return true;
        }
        if !self.can_rescan_folder() {
            self.pending_changes.extend(batch.paths);
            self.pending_flags |= batch.flags;
            return true;
        }
        self.refresh_from_events(batch.paths, batch.flags, cx);
        true
    }

    fn apply_pending_watch(&mut self, cx: &mut Context<Self>) {
        if self.pending_changes.is_empty() && self.pending_flags == 0 {
            return;
        }
        if !self.can_rescan_folder() {
            return;
        }
        let paths = std::mem::take(&mut self.pending_changes);
        let flags = std::mem::replace(&mut self.pending_flags, 0);
        self.refresh_from_events(paths, flags, cx);
    }

    fn refresh_from_events(&mut self, paths: Vec<PathBuf>, flags: u32, cx: &mut Context<Self>) {
        let Some(scan) = &self.scan else {
            return;
        };
        if !self.can_rescan_folder() {
            self.pending_changes.extend(paths);
            self.pending_flags |= flags;
            return;
        }
        let batch = EventBatch {
            paths: paths.clone(),
            flags,
        };
        let shared = scan.handle.shared_tree();
        let tree = scan.handle.tree();
        let targets = if batch.needs_full_scan() {
            Refresh::Root
        } else {
            scanner::refresh_targets(&tree, &paths)
        };
        if !self.history_done && (matches!(targets, Refresh::Root) || batch.needs_full_scan()) {
            return;
        }
        let jobs: Vec<(NodeId, PathBuf)> = match targets {
            Refresh::None => return,
            Refresh::Root => vec![(tree.root(), self.scope.root())],
            Refresh::Folders(ids) => ids.into_iter().map(|id| (id, tree.path(id))).collect(),
        };
        drop(tree);
        let generation = self.generation;
        let task = cx.spawn(async move |this, cx| {
            let mut results = Vec::new();
            for (id, path) in jobs {
                let rescan = cx
                    .background_executor()
                    .spawn(async move { scanner::scan(ScanOptions::new(path)) })
                    .await;
                results.push((id, rescan));
            }
            let _ = this.update(cx, |view, cx| {
                if view.generation != generation {
                    return;
                }
                view.folder_rescan.take();
                let mut any = false;
                for (id, rescan) in results {
                    if let Ok(rescan) = rescan {
                        any |= shared.replace(id, &rescan);
                    }
                }
                if any {
                    view.tree_changed(cx);
                }
                view.apply_pending_watch(cx);
            });
        });
        self.folder_rescan = Some(FolderRescan {
            name: "changes".into(),
            _task: task,
        });
        cx.notify();
    }

    fn save_comparison(&mut self, cx: &mut Context<Self>) {
        let Some(scan) = &self.scan else {
            return;
        };
        if !scan.handle.tree().is_complete() {
            return;
        }
        let shared = scan.handle.shared_tree();
        let persist = cx
            .try_global::<Settings>()
            .is_some_and(Settings::is_persistent);
        let generation = self.generation;
        self._compare_task = Some(cx.spawn(async move |this, cx| {
            let comparison = cx
                .background_executor()
                .spawn(async move {
                    let snapshot = compare::Snapshot::capture(&shared.read());
                    let previous = persist
                        .then(|| compare::Snapshot::load_for(&snapshot.root))
                        .flatten();
                    let comparison = previous.as_ref().and_then(|p| p.compare(&snapshot));
                    if persist && let Err(error) = snapshot.save() {
                        tracing::warn!("failed to save scan snapshot: {error}");
                    }
                    comparison
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.generation == generation {
                    view.comparison = comparison;
                    cx.notify();
                }
            });
        }));
    }

    fn accounting(&self, tree: &Tree) -> Option<Accounting> {
        if self.scope != Scope::StartupDisk || self.folder != tree.root() {
            return None;
        }
        let disk = self.disk.as_ref()?;
        Some(Accounting::new(
            disk.used(),
            disk.data_volume_used?,
            tree.allocated(tree.root()),
        ))
    }

    fn root_label(&self) -> SharedString {
        match &self.scope {
            Scope::StartupDisk => self
                .disk
                .as_ref()
                .map_or_else(|| "Startup disk".into(), |disk| disk.name.clone().into()),
            Scope::Folder(path) => path
                .file_name()
                .map_or_else(
                    || path.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                )
                .into(),
        }
    }

    fn name(&self, tree: &Tree, id: NodeId) -> SharedString {
        if id == tree.root() {
            self.root_label()
        } else {
            tree.name(id).to_string_lossy().into_owned().into()
        }
    }

    fn info(&self, tree: &Tree, item: Item, complete: bool) -> Info {
        let segment_size = || {
            self.segments
                .iter()
                .find(|segment| segment.item == item)
                .map(|segment| segment.size)
                .or_else(|| {
                    self.tiles
                        .tiles
                        .iter()
                        .find(|tile| tile.item == item)
                        .map(|tile| tile.size)
                })
                .unwrap_or(0)
        };
        let row_size = || {
            self.rows
                .iter()
                .find(|row| row.item == item)
                .map_or(0, |row| row.size)
        };
        match item {
            Item::Node(id) => Info {
                item,
                name: self.name(tree, id),
                size: tree.allocated(id),
                settled: tree.is_settled(id),
                items: (tree.kind(id) == NodeKind::Directory).then(|| u64::from(tree.items(id))),
                note: self.item_note(tree, item, complete),
                path: Some(tree.path(id)),
            },
            Item::OtherVolumes => Info {
                item,
                name: "macOS and other volumes".into(),
                size: row_size(),
                settled: complete,
                items: None,
                note: Some("The macOS, VM, Preboot and Recovery volumes on this disk".into()),
                path: None,
            },
            Item::NotMeasured => Info {
                item,
                name: if complete {
                    "Not measured"
                } else {
                    "Not scanned yet"
                }
                .into(),
                size: row_size(),
                settled: complete,
                items: None,
                note: Some(
                    "Protected folders, snapshots and file-system data the scan couldn't see"
                        .into(),
                ),
                path: None,
            },
            Item::Smaller(parent) => Info {
                item,
                name: "Smaller items".into(),
                size: segment_size(),
                settled: tree.is_settled(parent),
                items: None,
                note: Some("Items too small to draw one by one".into()),
                path: None,
            },
        }
    }

    fn item_note(&self, tree: &Tree, item: Item, complete: bool) -> Option<SharedString> {
        let flag = model::note(tree, item, complete);
        let growth = match item {
            Item::Node(id) if tree.kind(id) == NodeKind::Directory => compare::relative(tree, id)
                .and_then(|rel| {
                    self.comparison
                        .as_ref()
                        .and_then(|comparison| comparison.folder_note(&rel))
                }),
            _ => None,
        };
        match (flag, growth) {
            (Some(flag), Some(growth)) => Some(format!("{flag} · {growth}").into()),
            (Some(flag), None) => Some(flag.into()),
            (None, Some(growth)) => Some(growth.into()),
            (None, None) => None,
        }
    }

    /// Reads the tree once for this frame and refreshes the rows and chart layout.
    fn snapshot(&mut self) -> Option<Snapshot> {
        let scan = self.scan.as_ref()?;
        let tree = scan.handle.tree();
        let complete = tree.is_complete();
        let accounting = self.accounting(&tree);

        let rows = model::rows(&tree, self.folder, accounting, complete);
        match self.chart {
            Chart::Sunburst => {
                self.segments = Rc::new(sunburst::layout(&tree, self.folder, &rows));
                self.tiles = Rc::default();
            }
            Chart::Treemap => {
                let size = self
                    .treemap_bounds
                    .get()
                    .map_or(gpui::size(px(700.), px(480.)), |bounds| bounds.size);
                self.tiles = Rc::new(treemap::layout(
                    &tree,
                    self.folder,
                    &rows,
                    f32::from(size.width),
                    f32::from(size.height),
                ));
                self.segments = Rc::default();
            }
        }
        let tile_labels = self.tile_labels(&tree, complete);
        let folder_size: u64 = if accounting.is_some() {
            rows.iter().map(|row| row.size).sum()
        } else {
            tree.allocated(self.folder)
        };
        if self.scroll_to_selection {
            self.scroll_to_selection = false;
            if let Some(index) = rows.iter().position(|row| Some(row.item) == self.selected) {
                self.list_scroll
                    .scroll_to_item(index, ScrollStrategy::Nearest);
            }
        }
        self.rows = Rc::new(rows);

        let crumbs = model::breadcrumb(&tree, self.folder)
            .into_iter()
            .map(|id| (id, self.name(&tree, id)))
            .collect();
        let dropdown = self
            .dropdown
            .map(|parent| {
                model::subfolders(&tree, parent)
                    .into_iter()
                    .take(DROPDOWN_LIMIT)
                    .map(|id| {
                        let size = format::size(tree.allocated(id), tree.is_settled(id));
                        (id, self.name(&tree, id), size)
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut folder = self.info(&tree, Item::Node(self.folder), complete);
        folder.size = folder_size;
        Some(Snapshot {
            crumbs,
            dropdown,
            hovered: self.hovered.map(|item| self.info(&tree, item, complete)),
            selected: self.selected.map(|item| self.info(&tree, item, complete)),
            folder,
            entries: tree.stats().entries(),
            complete,
            tile_labels,
        })
    }

    /// Names for treemap rectangles large enough to hold one, biggest levels first.
    fn tile_labels(&self, tree: &Tree, complete: bool) -> Vec<TileLabel> {
        const MAX_LABELS: usize = 300;
        self.tiles
            .tiles
            .iter()
            .enumerate()
            .filter_map(|(index, tile)| {
                let header = tile.header;
                let fits = header || (tile.rect.w >= 50.0 && tile.rect.h >= 20.0);
                fits.then(|| {
                    let name = match tile.item {
                        Item::Node(id) => self.name(tree, id),
                        item => self.info(tree, item, complete).name,
                    };
                    let size = (header || tile.rect.h >= 36.0)
                        .then(|| format::size(tile.size, tile.settled));
                    TileLabel {
                        index,
                        name,
                        size,
                        header,
                    }
                })
            })
            .take(MAX_LABELS)
            .collect()
    }

    fn with_tree<R>(&self, read: impl FnOnce(&Tree) -> R) -> Option<R> {
        self.scan.as_ref().map(|scan| read(&scan.handle.tree()))
    }

    fn open_folder(&mut self, id: NodeId, cx: &mut Context<Self>) {
        self.folder = id;
        self.selected = None;
        self.hovered = None;
        self.dropdown = None;
        self.list_scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn select(&mut self, item: Option<Item>, cx: &mut Context<Self>) {
        self.selected = item;
        self.scroll_to_selection = true;
        cx.notify();
    }

    fn set_hovered(&mut self, item: Option<Item>, cx: &mut Context<Self>) {
        if self.hovered != item {
            self.hovered = item;
            cx.notify();
        }
    }

    fn go_up(&mut self, _: &GoUp, _: &mut Window, cx: &mut Context<Self>) {
        if self.cleanup.is_open() {
            return;
        }
        let folder = self.folder;
        if let Some(Some(parent)) = self.with_tree(|tree| tree.parent(folder)) {
            self.folder = parent;
            self.hovered = None;
            self.dropdown = None;
            self.select(Some(Item::Node(folder)), cx);
        }
    }

    fn go_to_top(&mut self, _: &GoToTop, _: &mut Window, cx: &mut Context<Self>) {
        self.open_folder(0, cx);
    }

    fn dismiss(&mut self, _: &Dismiss, _: &mut Window, cx: &mut Context<Self>) {
        if self.cleanup.menu.is_some() {
            self.cleanup.menu = None;
        } else if self.cleanup.sheet.is_some() {
            self.close_sheet(cx);
        } else if self.dropdown.take().is_none() {
            self.selected = None;
        }
        cx.notify();
    }

    fn add_to_basket_action(&mut self, _: &AddToBasket, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(item) = self.selected {
            self.toggle_in_basket(item, cx);
        }
    }

    fn quick_look_action(&mut self, _: &QuickLook, _: &mut Window, cx: &mut Context<Self>) {
        self.quick_look(cx);
    }

    fn review_basket(&mut self, _: &ReviewBasket, _: &mut Window, cx: &mut Context<Self>) {
        self.open_basket(cx);
    }

    fn show_history(&mut self, _: &ShowHistory, _: &mut Window, cx: &mut Context<Self>) {
        self.open_history(cx);
    }

    fn empty_basket_action(&mut self, _: &EmptyBasket, _: &mut Window, cx: &mut Context<Self>) {
        self.empty_basket(cx);
    }

    fn move_selection(&mut self, step: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() || self.cleanup.is_open() {
            return;
        }
        let current = self
            .selected
            .and_then(|item| self.rows.iter().position(|row| row.item == item));
        let last = self.rows.len() - 1;
        let next = match current {
            None if step > 0 => 0,
            None => last,
            Some(index) => index.saturating_add_signed(step).min(last),
        };
        self.select(Some(self.rows[next].item), cx);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(1, cx);
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(-1, cx);
    }

    fn open_selected(&mut self, _: &OpenSelected, _: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.selected.filter(|_| !self.cleanup.is_open()) else {
            return;
        };
        if let Item::Node(id) = item
            && self.with_tree(|tree| model::is_folder(tree, item)) == Some(true)
        {
            self.open_folder(id, cx);
        }
    }

    fn reveal_in_finder(&mut self, _: &RevealInFinder, _: &mut Window, cx: &mut Context<Self>) {
        let id = match self.selected {
            Some(Item::Node(id)) => id,
            _ => self.folder,
        };
        if let Some(path) = self.with_tree(|tree| tree.path(id)) {
            cx.reveal_path(&path);
        }
    }

    fn rescan(&mut self, _: &Rescan, _: &mut Window, cx: &mut Context<Self>) {
        self.start_scan(self.scope.clone(), cx);
    }

    fn stop_scan(&mut self, _: &StopScan, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(scan) = &self.scan {
            scan.handle.cancel();
        }
        cx.notify();
    }

    fn scan_startup_disk(&mut self, _: &ScanStartupDisk, _: &mut Window, cx: &mut Context<Self>) {
        self.start_scan(Scope::StartupDisk, cx);
    }

    fn scan_home_folder(&mut self, _: &ScanHomeFolder, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(home) = home_folder() {
            self.start_scan(Scope::Folder(home), cx);
        }
    }

    fn scan_folder(&mut self, _: &ScanFolder, _: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Scan".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = chosen.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update(cx, |view, cx| view.start_scan(Scope::Folder(path), cx));
            }
        })
        .detach();
    }

    fn chart_hit(&self, position: Point<Pixels>) -> Option<ChartHit> {
        match self.chart {
            Chart::Sunburst => {
                match sunburst::hit_test(&self.segments, self.geometry.get()?, position)? {
                    Hit::Center => Some(ChartHit::Center),
                    Hit::Segment(index) => {
                        let segment = self.segments[index];
                        Some(ChartHit::Item {
                            item: segment.item,
                            folder: segment.folder,
                        })
                    }
                }
            }
            Chart::Treemap => {
                let index = treemap::hit_test(&self.tiles, self.treemap_bounds.get()?, position)?;
                let tile = self.tiles.tiles[index];
                Some(ChartHit::Item {
                    item: tile.item,
                    folder: tile.kind == TileKind::Folder,
                })
            }
        }
    }

    fn hover_chart(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let item = match self.chart_hit(event.position) {
            Some(ChartHit::Item { item, .. }) => Some(item),
            _ => None,
        };
        self.set_hovered(item, cx);
    }

    fn click_chart(&mut self, event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        match self.chart_hit(event.position()) {
            Some(ChartHit::Center) => self.go_up(&GoUp, window, cx),
            Some(ChartHit::Item { item, folder }) => match item {
                Item::Node(id) if folder => self.open_folder(id, cx),
                Item::Smaller(parent) if parent != self.folder => self.open_folder(parent, cx),
                item => self.select(Some(item), cx),
            },
            None => self.select(None, cx),
        }
    }

    fn click_row(&mut self, item: Item, click_count: usize, cx: &mut Context<Self>) {
        match item {
            Item::Node(id)
                if click_count >= 2
                    && self.with_tree(|tree| model::is_folder(tree, item)) == Some(true) =>
            {
                self.open_folder(id, cx);
            }
            _ => {
                self.selected = Some(item);
                cx.notify();
            }
        }
    }

    fn toggle_dropdown(&mut self, folder: NodeId, cx: &mut Context<Self>) {
        self.dropdown = if self.dropdown == Some(folder) {
            None
        } else {
            Some(folder)
        };
        cx.notify();
    }
}

/// Paths as the user knows them: firmlinked Data volume paths without the volume prefix, and
/// the home folder as "~".
fn display_path(path: &Path) -> String {
    let path = path
        .strip_prefix(DATA_VOLUME_MOUNT_POINT)
        .map_or_else(|_| path.to_path_buf(), |rest| Path::new("/").join(rest));
    if let Some(home) = home_folder()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() {
            "~".into()
        } else {
            format!("~/{}", rest.display())
        };
    }
    path.display().to_string()
}

impl StorageView {
    fn render_toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = theme.selected;
        let scope_button = |id: &'static str, label: SharedString, active: bool| {
            button(id, label, theme).when(active, move |this| this.bg(selected))
        };
        let home = home_folder();
        let disk_label: SharedString = match (&self.scope, &self.disk) {
            (Scope::StartupDisk, Some(disk)) => disk.name.clone().into(),
            _ => "Startup disk".into(),
        };
        let is_home = home
            .as_ref()
            .is_some_and(|home| self.scope == Scope::Folder(home.clone()));
        let is_other_folder = matches!(self.scope, Scope::Folder(_)) && !is_home;

        div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                scope_button("scope-disk", disk_label, self.scope == Scope::StartupDisk).on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.scan_startup_disk(&ScanStartupDisk, window, cx);
                    }),
                ),
            )
            .child(
                scope_button("scope-home", "Home folder".into(), is_home).on_click(cx.listener(
                    |this, _: &ClickEvent, window, cx| {
                        this.scan_home_folder(&ScanHomeFolder, window, cx)
                    },
                )),
            )
            .child(
                scope_button(
                    "scope-folder",
                    if is_other_folder {
                        self.root_label()
                    } else {
                        "Choose folder…".into()
                    },
                    is_other_folder,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.scan_folder(&ScanFolder, window, cx);
                })),
            )
            .child(div().flex_1())
            .child(if self.is_scanning() {
                button("stop", "Stop", theme).on_click(cx.listener(
                    |this, _: &ClickEvent, window, cx| {
                        this.stop_scan(&StopScan, window, cx);
                    },
                ))
            } else {
                button("rescan", "Rescan", theme).on_click(cx.listener(
                    |this, _: &ClickEvent, window, cx| {
                        this.rescan(&Rescan, window, cx);
                    },
                ))
            })
    }

    fn render_capacity(&self, theme: &Theme) -> AnyElement {
        let container = div()
            .flex()
            .flex_col()
            .gap_1p5()
            .px_4()
            .py_2p5()
            .border_b_1()
            .border_color(theme.border);
        let Some(disk) = &self.disk else {
            return container
                .child(
                    div()
                        .h(px(38.))
                        .flex()
                        .items_center()
                        .text_color(theme.muted)
                        .child("Reading disk…"),
                )
                .into_any_element();
        };
        let capacity = disk.capacity.max(1) as f32;
        let used = disk.used();
        let purgeable = disk.purgeable.unwrap_or(0).min(used);
        let mut details = Vec::new();
        if let Some(purgeable) = disk.purgeable {
            details.push(format!("Purgeable {}", format::bytes(purgeable)));
        }
        match disk.snapshots {
            Some(0) => details.push("No local snapshots".into()),
            Some(1) => details.push("1 local snapshot".into()),
            Some(count) => details.push(format!("{count} local snapshots")),
            None => {}
        }
        if let Some(result) = &self.cleanup.result
            && let (Some(before), Some(gained)) = (result.available_before(), result.gained())
        {
            details.push(format!(
                "Before cleanup {} available, {} freed",
                format::bytes(before),
                format::bytes(gained)
            ));
        }
        let accent = theme.accent;
        let mut summary = format!(
            "{}: {} used of {}, {} available",
            disk.name,
            format::bytes(used),
            format::bytes(disk.capacity),
            format::bytes(disk.available)
        );
        for detail in &details {
            summary.push_str(". ");
            summary.push_str(detail);
        }

        container
            .id("capacity")
            .role(Role::Meter)
            .aria_label(summary)
            .aria_numeric_value(used as f64 / f64::from(capacity))
            .aria_min_numeric_value(0.0)
            .aria_max_numeric_value(1.0)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(disk.name.clone()),
                    )
                    .child(div().text_color(theme.muted).child(format!(
                        "{} used of {}",
                        format::bytes(used),
                        format::bytes(disk.capacity)
                    )))
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_color(theme.muted)
                            .child(format!("{} available", format::bytes(disk.available))),
                    ),
            )
            .child(
                div()
                    .h(px(10.))
                    .w_full()
                    .rounded_full()
                    .overflow_hidden()
                    .flex()
                    .bg(theme.track)
                    .child(
                        div()
                            .h_full()
                            .w(relative((used - purgeable) as f32 / capacity))
                            .bg(accent),
                    )
                    .child(
                        div()
                            .h_full()
                            .w(relative(purgeable as f32 / capacity))
                            .bg(accent.opacity(0.4)),
                    ),
            )
            .when(!details.is_empty(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted)
                        .child(details.join(" · ")),
                )
            })
            .into_any_element()
    }

    fn render_breadcrumb(
        &self,
        snapshot: &Snapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hover = theme.hover;
        let last = snapshot.crumbs.len().saturating_sub(1);
        let mut bar = div()
            .flex()
            .items_center()
            .px_3()
            .py_1()
            .min_h(px(32.))
            .border_b_1()
            .border_color(theme.border);
        for (index, (id, name)) in snapshot.crumbs.iter().enumerate() {
            let id = *id;
            bar = bar.child(
                div()
                    .id(("crumb", index))
                    .role(Role::Button)
                    .aria_label(name.clone())
                    .px_1p5()
                    .py_0p5()
                    .rounded_sm()
                    .cursor_pointer()
                    .whitespace_nowrap()
                    .hover(move |style| style.bg(hover))
                    .when(index == last, |this| this.font_weight(FontWeight::SEMIBOLD))
                    .child(name.clone())
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.open_folder(id, cx)),
                    ),
            );
            let open = self.dropdown == Some(id);
            bar =
                bar.child(
                    div()
                        .id(("crumb-menu", index))
                        .role(Role::Button)
                        .aria_label(format!("Folders in {name}"))
                        .aria_expanded(open)
                        .px_1()
                        .py_0p5()
                        .rounded_sm()
                        .cursor_pointer()
                        .text_color(theme.muted)
                        .hover(move |style| style.bg(hover))
                        .when(open, |this| this.bg(hover))
                        .child("›")
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.toggle_dropdown(id, cx)
                        }))
                        .when(open, |this| {
                            this.child(self.render_dropdown(snapshot, theme, cx))
                        }),
                );
        }
        bar.child(div().flex_1())
            .child(self.render_chart_switch(theme))
    }

    fn render_chart_switch(&self, theme: &Theme) -> impl IntoElement {
        let selected = theme.selected;
        let choice = |id: &'static str, label: &'static str, chart: Chart| {
            let active = self.chart == chart;
            div()
                .id(id)
                .role(Role::RadioButton)
                .aria_label(label)
                .aria_selected(active)
                .px_2()
                .py_0p5()
                .rounded_sm()
                .text_xs()
                .cursor_pointer()
                .when(active, move |this| this.bg(selected))
                .when(!active, |this| this.text_color(theme.muted))
                .child(label)
        };
        div()
            .id("chart-switch")
            .role(Role::RadioGroup)
            .aria_label("Chart")
            .flex_none()
            .flex()
            .gap_0p5()
            .p_0p5()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .child(
                choice("show-sunburst", "Sunburst", Chart::Sunburst)
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(ShowSunburst), cx)),
            )
            .child(
                choice("show-treemap", "Treemap", Chart::Treemap)
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(ShowTreemap), cx)),
            )
    }

    fn render_dropdown(
        &self,
        snapshot: &Snapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hover = theme.hover;
        let mut list = div()
            .id("crumb-dropdown")
            .role(Role::Menu)
            .occlude()
            .mt_6()
            .min_w(px(260.))
            .max_w(px(420.))
            .max_h(px(380.))
            .overflow_y_scroll()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.panel)
            .text_color(theme.text)
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.dropdown = None;
                cx.notify();
            }));
        if snapshot.dropdown.is_empty() {
            list = list.child(
                div()
                    .px_3()
                    .py_1()
                    .text_color(theme.muted)
                    .child("No folders inside"),
            );
        }
        for (index, (id, name, size)) in snapshot.dropdown.iter().enumerate() {
            let id = *id;
            list = list.child(
                div()
                    .id(("dropdown-item", index))
                    .role(Role::MenuItem)
                    .aria_label(format!("{name}, {size}"))
                    .flex()
                    .gap_3()
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.muted)
                            .child(size.clone()),
                    )
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.open_folder(id, cx)),
                    ),
            );
        }
        deferred(anchored().snap_to_window_with_margin(px(8.)).child(list)).priority(1)
    }

    fn render_chart(
        &self,
        snapshot: &Snapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let chart = match self.chart {
            Chart::Sunburst => self.render_sunburst(snapshot, theme),
            Chart::Treemap => self.render_treemap(snapshot, theme),
        };
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(
                chart
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .cursor_pointer()
                    .on_mouse_move(cx.listener(Self::hover_chart))
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        if !*hovered {
                            this.set_hovered(None, cx);
                        }
                    }))
                    .on_click(cx.listener(Self::click_chart))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.right_click_chart(event, cx)
                        }),
                    ),
            )
            .when(self.chart == Chart::Treemap, |this| {
                this.child(self.render_legend(theme))
            })
    }

    fn render_sunburst(&self, snapshot: &Snapshot, theme: &Theme) -> Stateful<Div> {
        let segments = Rc::clone(&self.segments);
        let colors = segments
            .iter()
            .map(|segment| theme.slice(segment, self.hovered == Some(segment.item)))
            .collect();
        let outline = self
            .selected
            .and_then(|item| segments.iter().position(|segment| segment.item == item))
            .map(|index| (index, Hsla::from(theme.text)));
        let style = sunburst::Paint {
            center: theme.chart_center.into(),
            colors,
            outline,
        };
        let geometry = Rc::clone(&self.geometry);
        let label_width = self
            .geometry
            .get()
            .map_or(150.0, |geometry| geometry.hole * 1.7) as f32;
        let info = snapshot.hovered.as_ref().unwrap_or(&snapshot.folder);

        div()
            .id("chart")
            .role(Role::Image)
            .aria_label(format!(
                "Chart of {}, {}. The list beside it has the same items.",
                snapshot.folder.name,
                format::size(snapshot.folder.size, snapshot.folder.settled)
            ))
            .relative()
            .child(
                canvas(
                    move |bounds, _, _| {
                        let fitted = Geometry::fit(bounds);
                        geometry.set(Some(fitted));
                        fitted
                    },
                    move |_, geometry, window, _| {
                        sunburst::paint(window, geometry, &segments, &style)
                    },
                )
                .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .w(px(label_width))
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_0p5()
                            .text_center()
                            .child(
                                div()
                                    .w_full()
                                    .truncate()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(info.name.clone()),
                            )
                            .child(div().text_lg().child(format::size(info.size, info.settled)))
                            .when_some(info.items, |this, items| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted)
                                        .child(format::items(items)),
                                )
                            }),
                    ),
            )
    }

    fn render_treemap(&self, snapshot: &Snapshot, theme: &Theme) -> Stateful<Div> {
        let layout = Rc::clone(&self.tiles);
        let colors: Vec<Hsla> = layout
            .tiles
            .iter()
            .map(|tile| theme.tile(tile, self.hovered == Some(tile.item)))
            .collect();
        let outline = self
            .selected
            .and_then(|item| layout.tiles.iter().position(|tile| tile.item == item))
            .map(|index| (index, Hsla::from(theme.text)));
        let labels: Vec<_> = snapshot
            .tile_labels
            .iter()
            .map(|label| {
                let tile = &layout.tiles[label.index];
                let rect = tile.rect;
                let ink = theme::ink_on(colors[label.index]);
                let height = if label.header {
                    treemap::HEADER
                } else {
                    rect.h - 4.0
                };
                div()
                    .absolute()
                    .left(px(rect.x + 4.0))
                    .top(px(rect.y + 1.0))
                    .w(px((rect.w - 8.0).max(0.0)))
                    .h(px(height.max(0.0)))
                    .overflow_hidden()
                    .text_xs()
                    .text_color(ink)
                    .flex()
                    .when(label.header, |this| this.flex_row().gap_1p5())
                    .when(!label.header, |this| this.flex_col())
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .when(label.header, |this| this.font_weight(FontWeight::SEMIBOLD))
                            .child(label.name.clone()),
                    )
                    .children(label.size.clone().map(|size| {
                        div()
                            .flex_none()
                            .when(label.header, |this| this.opacity(0.8))
                            .child(size)
                    }))
            })
            .collect();
        let bounds_cell = Rc::clone(&self.treemap_bounds);
        let style = treemap::Paint { colors, outline };
        div()
            .id("chart")
            .role(Role::Image)
            .aria_label(format!(
                "Treemap of {}, {}. The list beside it has the same items.",
                snapshot.folder.name,
                format::size(snapshot.folder.size, snapshot.folder.settled)
            ))
            .relative()
            .overflow_hidden()
            .m_2()
            .child({
                let (width, height) = (layout.width, layout.height);
                canvas(
                    move |bounds, window, _| {
                        let resized = (f32::from(bounds.size.width) - width).abs() > 1.0
                            || (f32::from(bounds.size.height) - height).abs() > 1.0;
                        bounds_cell.set(Some(bounds));
                        if resized {
                            window.refresh();
                        }
                        bounds
                    },
                    move |_, bounds, window, _| treemap::paint(window, bounds, &layout, &style),
                )
                .size_full()
            })
            .children(labels)
    }

    fn render_legend(&self, theme: &Theme) -> impl IntoElement {
        let mut shown: Vec<FileType> = Vec::new();
        for tile in &self.tiles.tiles {
            if let TileKind::File(kind) = tile.kind
                && !shown.contains(&kind)
            {
                shown.push(kind);
            }
        }
        shown.sort_by_key(|kind| FileType::ALL.iter().position(|known| known == kind));
        div()
            .id("treemap-legend")
            .role(Role::Group)
            .aria_label(format!(
                "Colors: {}",
                shown
                    .iter()
                    .map(|kind| kind.title())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
            .flex()
            .flex_wrap()
            .gap_x_3()
            .gap_y_1()
            .px_3()
            .pb_2()
            .text_xs()
            .text_color(theme.muted)
            .children(shown.into_iter().map(|kind| {
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .size(px(10.))
                            .flex_none()
                            .rounded_sm()
                            .bg(theme.file_type(kind)),
                    )
                    .child(kind.title())
            }))
    }

    fn render_list(
        &self,
        snapshot: &Snapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let count = self.rows.len();
        div()
            .id("list")
            .role(Role::List)
            .aria_label(format!("Items in {}", snapshot.folder.name))
            .w(px(380.))
            .h_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(theme.border)
            .bg(theme.panel)
            .child(
                div()
                    .px_3()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme.muted)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(format::items(count as u64)),
            )
            .child(
                uniform_list("rows", count, cx.processor(Self::render_rows))
                    .track_scroll(&self.list_scroll)
                    .flex_1(),
            )
    }

    fn render_rows(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Stateful<Div>> {
        let theme = Theme::for_appearance(window.appearance());
        let Some(scan) = &self.scan else {
            return Vec::new();
        };
        let tree = scan.handle.tree();
        let complete = tree.is_complete();
        let largest = self.rows.first().map_or(1, |row| row.size.max(1)) as f32;
        let hover = theme.hover;

        range
            .filter_map(|index| {
                let row = *self.rows.get(index)?;
                let name = self.info(&tree, row.item, complete).name;
                let folder = model::is_folder(&tree, row.item);
                let color = theme.slice(
                    &Segment {
                        item: row.item,
                        ring: 1,
                        start: 0.0,
                        end: 0.0,
                        branch: index,
                        size: row.size,
                        settled: row.settled,
                        folder,
                    },
                    false,
                );
                let selected = self.selected == Some(row.item);
                let highlighted = self.hovered == Some(row.item);
                let item = row.item;
                let in_basket = match (item, &self.cleanup.basket) {
                    (Item::Node(id), Some(basket)) if !basket.is_empty() => {
                        basket.contains(&tree.path(id))
                    }
                    _ => false,
                };
                let note = self.item_note(&tree, row.item, complete);
                let mut label = format!("{name}, {}", format::size(row.size, row.settled));
                if folder {
                    label.push_str(", folder");
                }
                if let Some(note) = &note {
                    label.push_str(&format!(", {note}"));
                }
                if in_basket {
                    label.push_str(", in basket");
                }
                Some(
                    div()
                        .id(("row", index))
                        .role(Role::ListItem)
                        .aria_label(label)
                        .aria_selected(selected)
                        .aria_position_in_set(index + 1)
                        .aria_size_of_set(self.rows.len())
                        .when(selected, |this| this.aria_active_descendant())
                        .relative()
                        .w_full()
                        .h(px(ROW_HEIGHT))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .when(selected, |this| this.bg(theme.selected))
                        .when(!selected && highlighted, |this| this.bg(hover))
                        .hover(move |style| style.bg(hover))
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .bottom_0()
                                .h(px(2.))
                                .w(relative(row.size as f32 / largest))
                                .bg(color.opacity(0.55)),
                        )
                        .child(div().size(px(10.)).flex_none().rounded_sm().bg(color))
                        .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                        .when(in_basket, |this| {
                            this.child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(theme.accent)
                                    .child("In basket"),
                            )
                        })
                        .when_some(note, |this, note| {
                            this.child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(theme.muted)
                                    .child(note),
                            )
                        })
                        .child(
                            div()
                                .flex_none()
                                .w(px(78.))
                                .text_right()
                                .when(!row.settled, |this| this.text_color(theme.muted))
                                .child(format::size(row.size, row.settled)),
                        )
                        .child(
                            div()
                                .flex_none()
                                .w(px(10.))
                                .text_color(theme.muted)
                                .child(if folder { "›" } else { "" }),
                        )
                        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                            this.click_row(item, event.click_count(), cx);
                        }))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                this.right_click_row(item, event, cx);
                            }),
                        )
                        .when(matches!(item, Item::Node(_)), |this| {
                            this.on_drag(
                                DraggedItem {
                                    item,
                                    name: name.clone(),
                                },
                                |dragged, _, _, cx| {
                                    cx.new(|_| DragPreview::new(dragged.name.clone()))
                                },
                            )
                        })
                        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            if *hovered {
                                this.set_hovered(Some(item), cx);
                            } else if this.hovered == Some(item) {
                                this.set_hovered(None, cx);
                            }
                        })),
                )
            })
            .collect()
    }

    fn render_status(&self, snapshot: &Snapshot, theme: &Theme) -> impl IntoElement {
        let info = snapshot.hovered.as_ref().or(snapshot.selected.as_ref());
        let description = info.map(|info| {
            let mut text = info
                .path
                .as_deref()
                .map_or_else(|| info.name.to_string(), display_path);
            text.push_str(&format!(" · {}", format::size(info.size, info.settled)));
            if let Some(items) = info.items {
                text.push_str(&format!(" · {}", format::items(items)));
            }
            if let Some(note) = &info.note {
                text.push_str(&format!(" · {note}"));
            }
            text
        });
        let progress = match (&self.scan, &self.folder_rescan) {
            (_, Some(rescan)) if rescan.name.as_ref() == "changes" => "Updating…".into(),
            (_, Some(rescan)) => format!("Rescanning {}…", rescan.name),
            (Some(scan), None) if scan.elapsed.is_none() => format!(
                "Scanning… {} · {}",
                format::items(snapshot.entries),
                format::duration(scan.started.elapsed())
            ),
            (Some(scan), None) if snapshot.complete => {
                let mut text = format!(
                    "{} scanned in {}",
                    format::items(snapshot.entries),
                    format::duration(scan.elapsed.unwrap_or_default())
                );
                if let Some(suffix) = self
                    .comparison
                    .as_ref()
                    .and_then(|comparison| comparison.status_suffix())
                {
                    text.push_str(" · ");
                    text.push_str(&suffix);
                }
                text
            }
            (Some(_), None) => format!(
                "Scan stopped · showing {} found so far",
                format::items(snapshot.entries)
            ),
            (None, None) => String::new(),
        };
        let shows_not_measured = info.is_some_and(|info| info.item == Item::NotMeasured);
        let accent: Rgba = theme.accent;
        let notice = self.cleanup.notice();
        let has_notice = notice.is_some();
        let description = notice.map(String::from).or(description);
        let summary = match &description {
            Some(description) if !progress.is_empty() => format!("{description}. {progress}"),
            Some(description) => description.clone(),
            None => progress.clone(),
        };

        div()
            .id("status")
            .role(Role::Status)
            .aria_label(summary)
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .h(px(28.))
            .border_t_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.muted)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(has_notice, |this| this.text_color(theme.review))
                    .child(description.unwrap_or_default()),
            )
            .when(shows_not_measured, |this| {
                this.child(
                    div()
                        .id("full-disk-access")
                        .role(Role::Link)
                        .aria_label("Open Full Disk Access settings")
                        .flex_none()
                        .text_color(accent)
                        .cursor_pointer()
                        .child("Open Full Disk Access settings")
                        .on_click(|_, _, cx| access::open_settings(cx)),
                )
            })
            .child(div().flex_none().child(progress))
    }
}

impl Render for StorageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::for_appearance(window.appearance());
        self.chart = cx
            .try_global::<Settings>()
            .map_or_else(Chart::default, |settings| settings.chart);
        let snapshot = self.snapshot();

        let body = match (&self.error, &snapshot) {
            (Some(error), _) => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted)
                .child(error.clone())
                .into_any_element(),
            (None, Some(snapshot)) => div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(self.render_breadcrumb(snapshot, &theme, cx))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .child(self.render_chart(snapshot, &theme, cx))
                        .child(self.render_list(snapshot, &theme, cx)),
                )
                .child(self.render_cleanup_bar(&theme, cx))
                .child(self.render_status(snapshot, &theme))
                .into_any_element(),
            (None, None) => div().flex_1().into_any_element(),
        };
        let sheet = self.render_sheet(&theme, cx);
        let menu = self.render_menu(&theme, cx);

        div()
            .id("storage-view")
            .role(Role::Group)
            .key_context("StorageView")
            .track_focus(&self.focus_handle)
            .relative()
            .on_action(cx.listener(Self::add_to_basket_action))
            .on_action(cx.listener(Self::quick_look_action))
            .on_action(cx.listener(Self::review_basket))
            .on_action(cx.listener(Self::show_history))
            .on_action(cx.listener(Self::empty_basket_action))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::open_selected))
            .on_action(cx.listener(Self::go_up))
            .on_action(cx.listener(Self::go_to_top))
            .on_action(cx.listener(Self::dismiss))
            .on_action(cx.listener(Self::reveal_in_finder))
            .on_action(cx.listener(Self::rescan))
            .on_action(cx.listener(Self::stop_scan))
            .on_action(cx.listener(Self::scan_startup_disk))
            .on_action(cx.listener(Self::scan_home_folder))
            .on_action(cx.listener(Self::scan_folder))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.text)
            .text_sm()
            .child(self.render_toolbar(&theme, cx))
            .child(self.render_capacity(&theme))
            .child(body)
            .children(sheet)
            .children(menu)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

    use super::*;

    /// `big` (3 MB in two files, one nested), `small` (100 KB) and a 10 KB file.
    fn sample_folder() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("big/inner")).unwrap();
        fs::create_dir(root.path().join("small")).unwrap();
        fs::write(root.path().join("big/a.bin"), vec![1u8; 2_000_000]).unwrap();
        fs::write(root.path().join("big/inner/b.bin"), vec![2u8; 1_000_000]).unwrap();
        fs::write(root.path().join("small/c.bin"), vec![3u8; 100_000]).unwrap();
        fs::write(root.path().join("notes.txt"), vec![4u8; 10_000]).unwrap();
        root
    }

    fn open<'a>(
        root: &Path,
        cx: &'a mut TestAppContext,
    ) -> (Entity<StorageView>, &'a mut VisualTestContext) {
        cx.update(|cx| cx.bind_keys(crate::key_bindings()));
        let scope = Scope::Folder(root.to_path_buf());
        let (view, cx) = cx.add_window_view(|_, cx| StorageView::new(scope, cx));
        cx.update(|window, cx| {
            let focus = view.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
        });
        for _ in 0..500 {
            cx.executor().advance_clock(POLL_INTERVAL);
            cx.run_until_parked();
            if view.read_with(cx, |view, _| !view.is_scanning()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            view.read_with(cx, |view, _| !view.is_scanning()),
            "scan finished"
        );
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        (view, cx)
    }

    fn child(view: &Entity<StorageView>, cx: &mut VisualTestContext, name: &str) -> Item {
        view.read_with(cx, |view, _| {
            view.with_tree(|tree| {
                let id = tree
                    .children(view.folder)
                    .find(|&child| tree.name(child) == name)
                    .unwrap();
                Item::Node(id)
            })
            .unwrap()
        })
    }

    fn folder(view: &Entity<StorageView>, cx: &mut VisualTestContext) -> NodeId {
        view.read_with(cx, |view, _| view.folder)
    }

    /// Middle of the first slice of `item` in ring `ring`.
    fn slice_center(
        view: &Entity<StorageView>,
        cx: &mut VisualTestContext,
        item: Item,
    ) -> Point<Pixels> {
        view.read_with(cx, |view, _| {
            let geometry = view.geometry.get().expect("chart was drawn");
            let segment = view
                .segments
                .iter()
                .find(|segment| segment.item == item)
                .unwrap();
            let radius = geometry.inner_radius(segment.ring) + geometry.ring_width / 2.0;
            geometry.point((segment.start + segment.end) / 2.0, radius)
        })
    }

    #[gpui::test]
    fn lists_children_largest_first(cx: &mut TestAppContext) {
        let root = sample_folder();
        let (view, cx) = open(root.path(), cx);

        let big = child(&view, cx, "big");
        let small = child(&view, cx, "small");
        let notes = child(&view, cx, "notes.txt");
        let rows: Vec<Item> =
            view.read_with(cx, |view, _| view.rows.iter().map(|row| row.item).collect());
        assert_eq!(rows, [big, small, notes]);
        assert!(view.read_with(cx, |view, _| view.rows.iter().all(|row| row.settled)));
    }

    #[gpui::test]
    fn keyboard_moves_selection_and_opens_folders(cx: &mut TestAppContext) {
        let root = sample_folder();
        let (view, cx) = open(root.path(), cx);
        let big = child(&view, cx, "big");
        let small = child(&view, cx, "small");

        cx.simulate_keystrokes("down down");
        assert_eq!(view.read_with(cx, |view, _| view.selected), Some(small));
        cx.simulate_keystrokes("up enter");
        let Item::Node(big_id) = big else {
            unreachable!()
        };
        assert_eq!(folder(&view, cx), big_id);
        assert_eq!(view.read_with(cx, |view, _| view.selected), None);

        cx.simulate_keystrokes("left");
        assert_eq!(folder(&view, cx), 0);
        assert_eq!(
            view.read_with(cx, |view, _| view.selected),
            Some(big),
            "came-from folder stays selected"
        );

        cx.simulate_keystrokes("escape");
        assert_eq!(view.read_with(cx, |view, _| view.selected), None);
    }

    #[gpui::test]
    fn clicking_a_slice_zooms_in_and_the_center_zooms_out(cx: &mut TestAppContext) {
        let root = sample_folder();
        let (view, cx) = open(root.path(), cx);
        let big = child(&view, cx, "big");
        let Item::Node(big_id) = big else {
            unreachable!()
        };

        let position = slice_center(&view, cx, big);
        cx.simulate_mouse_move(position, None, Modifiers::default());
        assert_eq!(view.read_with(cx, |view, _| view.hovered), Some(big));

        cx.simulate_click(position, Modifiers::default());
        assert_eq!(folder(&view, cx), big_id);

        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let center = view.read_with(cx, |view, _| {
            let geometry = view.geometry.get().unwrap();
            geometry.point(0.0, 0.0)
        });
        cx.simulate_click(center, Modifiers::default());
        assert_eq!(folder(&view, cx), 0);
    }

    /// A point inside `item`'s treemap rectangle: its middle, or for folders a pixel inside the
    /// border, where no contents are drawn.
    fn tile_point(
        view: &Entity<StorageView>,
        cx: &mut VisualTestContext,
        item: Item,
    ) -> Point<Pixels> {
        view.read_with(cx, |view, _| {
            let bounds = view.treemap_bounds.get().expect("treemap was drawn");
            let tile = view
                .tiles
                .tiles
                .iter()
                .find(|tile| tile.item == item)
                .expect("item has a rectangle");
            let rect = treemap::to_screen(&view.tiles, bounds, tile.rect);
            if tile.kind == TileKind::Folder {
                rect.origin + gpui::point(px(1.5), px(1.5))
            } else {
                rect.center()
            }
        })
    }

    #[gpui::test]
    fn treemap_opens_folders_and_selects_files(cx: &mut TestAppContext) {
        let root = sample_folder();
        cx.update(|cx| crate::init(Settings::default(), cx));
        let (view, cx) = open(root.path(), cx);
        let big = child(&view, cx, "big");
        let notes = child(&view, cx, "notes.txt");
        let Item::Node(big_id) = big else {
            unreachable!()
        };

        cx.simulate_keystrokes("alt-cmd-2");
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |view, _| view.chart), Chart::Treemap);
        let fills_bounds = view.read_with(cx, |view, _| {
            let bounds = view.treemap_bounds.get().unwrap();
            (view.tiles.width - f32::from(bounds.size.width)).abs() <= 1.0
        });
        assert!(fills_bounds, "the layout is redone at the drawn size");

        let position = tile_point(&view, cx, notes);
        cx.simulate_mouse_move(position, None, Modifiers::default());
        assert_eq!(view.read_with(cx, |view, _| view.hovered), Some(notes));
        cx.simulate_click(position, Modifiers::default());
        assert_eq!(view.read_with(cx, |view, _| view.selected), Some(notes));

        let position = tile_point(&view, cx, big);
        cx.simulate_click(position, Modifiers::default());
        assert_eq!(folder(&view, cx), big_id);

        cx.simulate_keystrokes("alt-cmd-1");
        assert_eq!(view.read_with(cx, |view, _| view.chart), Chart::Sunburst);
    }

    #[gpui::test]
    fn clicking_a_file_slice_selects_it(cx: &mut TestAppContext) {
        let root = sample_folder();
        let (view, cx) = open(root.path(), cx);
        let notes = child(&view, cx, "notes.txt");

        let position = slice_center(&view, cx, notes);
        cx.simulate_click(position, Modifiers::default());

        assert_eq!(view.read_with(cx, |view, _| view.selected), Some(notes));
        assert_eq!(folder(&view, cx), 0);
    }

    #[gpui::test]
    fn rows_select_on_click_and_open_on_double_click(cx: &mut TestAppContext) {
        let root = sample_folder();
        let (view, cx) = open(root.path(), cx);
        let big = child(&view, cx, "big");
        let Item::Node(big_id) = big else {
            unreachable!()
        };

        view.update(cx, |view, cx| view.click_row(big, 1, cx));
        assert_eq!(view.read_with(cx, |view, _| view.selected), Some(big));
        assert_eq!(folder(&view, cx), 0);

        view.update(cx, |view, cx| view.click_row(big, 2, cx));
        assert_eq!(folder(&view, cx), big_id);
    }

    fn basket_paths(view: &Entity<StorageView>, cx: &mut VisualTestContext) -> Vec<PathBuf> {
        view.read_with(cx, |view, _| {
            view.cleanup
                .basket
                .as_ref()
                .map(|basket| basket.paths())
                .unwrap_or_default()
        })
    }

    #[gpui::test]
    fn keyboard_adds_the_selection_and_will_free_is_measured(cx: &mut TestAppContext) {
        let root = sample_folder();
        let (view, cx) = open(root.path(), cx);
        view.update(cx, |view, _| view.use_home(root.path()));

        cx.simulate_keystrokes("down cmd-backspace");
        cx.run_until_parked();

        assert_eq!(basket_paths(&view, cx), [root.path().join("big")]);
        let expected = scanner::measure(&[root.path().join("big")])
            .unwrap()
            .freeable;
        let measured = view.read_with(cx, |view, _| match &view.cleanup.will_free {
            basket_ui::WillFree::Measured(measurement) => Some(measurement.freeable),
            _ => None,
        });
        assert_eq!(measured, Some(expected));

        cx.simulate_keystrokes("cmd-backspace");
        cx.run_until_parked();
        assert!(
            basket_paths(&view, cx).is_empty(),
            "the same keys take it out again"
        );
    }

    /// Layout, prepaint and paint of the whole window, every 100 ms while the home folder is
    /// scanned. GPU work is not included.
    /// `cargo test -p app --release -- --ignored --nocapture frame_time`
    #[gpui::test]
    #[ignore = "scans the home folder"]
    fn frame_time_while_scanning_the_home_folder(cx: &mut TestAppContext) {
        let home = home_folder().unwrap();
        let (view, cx) = cx.add_window_view(|_, cx| StorageView::new(Scope::Folder(home), cx));
        let mut frames = Vec::new();
        while view.read_with(cx, |view, _| view.is_scanning()) {
            std::thread::sleep(POLL_INTERVAL);
            cx.executor().advance_clock(POLL_INTERVAL);
            let started = Instant::now();
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
            if view.read_with(cx, |view, _| view.is_scanning()) {
                frames.push(started.elapsed());
            }
        }
        frames.sort_unstable();
        let ms = |duration: Duration| duration.as_secs_f64() * 1000.0;
        let entries = view.read_with(cx, |view, _| {
            view.with_tree(|tree| tree.stats().entries()).unwrap()
        });
        println!(
            "{} frames over {entries} entries: median {:.2} ms, 99th percentile {:.2} ms, slowest {:.2} ms",
            frames.len(),
            ms(frames[frames.len() / 2]),
            ms(frames[frames.len() * 99 / 100]),
            ms(*frames.last().unwrap()),
        );
        assert!(
            *frames.last().unwrap() < Duration::from_millis(16),
            "every frame fits in 16 ms"
        );
    }

    #[gpui::test]
    fn rescanning_a_folder_updates_the_chart_without_a_full_scan(cx: &mut TestAppContext) {
        let root = sample_folder();
        let (view, cx) = open(root.path(), cx);
        view.update(cx, |view, _| view.use_home(root.path()));
        let Item::Node(small) = child(&view, cx, "small") else {
            unreachable!()
        };
        let generation = view.read_with(cx, |view, _| view.generation);
        fs::write(root.path().join("small/new.bin"), vec![5u8; 2_000_000]).unwrap();

        view.update(cx, |view, cx| view.rescan_folder(small, cx));
        assert!(
            !view.read_with(cx, |view, _| view.can_rescan_folder()),
            "one rescan at a time"
        );
        for _ in 0..500 {
            cx.run_until_parked();
            if view.read_with(cx, |view, _| view.folder_rescan.is_none()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        let fresh = scanner::scan(ScanOptions::new(root.path())).unwrap();
        view.read_with(cx, |view, _| {
            assert_eq!(view.generation, generation, "no full scan started");
            view.with_tree(|tree| {
                assert_eq!(tree.allocated(tree.root()), fresh.allocated(fresh.root()));
                assert!(tree.allocated(small) >= 2_000_000);
            });
            let notice = view.cleanup.notice().unwrap();
            assert_eq!(notice.as_ref(), "Rescanned small");
        });
    }

    #[gpui::test]
    fn refused_items_stay_out_of_the_basket_with_a_reason(cx: &mut TestAppContext) {
        let root = sample_folder();
        fs::create_dir_all(root.path().join("Library/Preferences")).unwrap();
        fs::write(
            root.path().join("Library/Preferences/x.plist"),
            vec![0u8; 4_000_000],
        )
        .unwrap();
        let (view, cx) = open(root.path(), cx);
        view.update(cx, |view, _| view.use_home(root.path()));
        let library = child(&view, cx, "Library");

        view.update(cx, |view, cx| view.add_item(library, cx));

        assert!(basket_paths(&view, cx).is_empty());
        let notice = view.read_with(cx, |view, _| view.cleanup.notice()).unwrap();
        assert!(notice.contains("can't go in the basket"), "{notice}");
    }

    #[gpui::test]
    fn right_click_opens_a_menu_that_escape_closes(cx: &mut TestAppContext) {
        let root = sample_folder();
        let (view, cx) = open(root.path(), cx);
        view.update(cx, |view, _| view.use_home(root.path()));
        let notes = child(&view, cx, "notes.txt");

        let position = slice_center(&view, cx, notes);
        cx.simulate_event(gpui::MouseDownEvent {
            button: MouseButton::Right,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        assert!(view.read_with(cx, |view, _| view.cleanup.menu.is_some()));
        assert_eq!(view.read_with(cx, |view, _| view.selected), Some(notes));

        cx.simulate_keystrokes("escape");
        assert!(view.read_with(cx, |view, _| view.cleanup.menu.is_none()));
        assert_eq!(
            view.read_with(cx, |view, _| view.selected),
            Some(notes),
            "the first escape only closes the menu"
        );
    }

    #[gpui::test]
    fn moved_items_leave_the_chart_and_the_basket(cx: &mut TestAppContext) {
        let root = sample_folder();
        let (view, cx) = open(root.path(), cx);
        view.update(cx, |view, _| view.use_home(root.path()));
        let big = child(&view, cx, "big");
        let Item::Node(big_id) = big else {
            unreachable!()
        };
        let (before, big_size) = view.read_with(cx, |view, _| {
            view.with_tree(|tree| (tree.allocated(tree.root()), tree.allocated(big_id)))
                .unwrap()
        });
        view.update(cx, |view, cx| {
            view.add_item(big, cx);
            view.open_folder(big_id, cx);
        });
        assert_eq!(basket_paths(&view, cx), [root.path().join("big")]);

        let outcome = cleanup::Outcome {
            moved: vec![cleanup::Moved {
                path: root.path().join("big"),
                trashed: None,
                node: Some(big_id),
                category: cleanup::Category::Chosen,
                size: big_size,
            }],
            failed: Vec::new(),
        };
        view.update(cx, |view, cx| {
            view.finish_move(outcome, None, None, None, cx)
        });
        cx.run_until_parked();

        assert_eq!(folder(&view, cx), 0, "the open folder went to the Trash");
        assert!(basket_paths(&view, cx).is_empty());
        let after = view.read_with(cx, |view, _| {
            view.with_tree(|tree| tree.allocated(tree.root())).unwrap()
        });
        assert_eq!(after, before - big_size);
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let rows: Vec<Item> =
            view.read_with(cx, |view, _| view.rows.iter().map(|row| row.item).collect());
        assert!(!rows.contains(&big));
        assert_eq!(
            view.read_with(cx, |view, _| view.cleanup.sheet),
            Some(basket_ui::Sheet::Basket)
        );
    }

    #[gpui::test]
    fn breadcrumb_dropdown_lists_subfolders_and_escape_closes_it(cx: &mut TestAppContext) {
        let root = sample_folder();
        let (view, cx) = open(root.path(), cx);

        view.update(cx, |view, cx| view.toggle_dropdown(0, cx));
        cx.run_until_parked();
        let entries: Vec<String> = view.update(cx, |view, _| {
            let snapshot = view.snapshot().unwrap();
            snapshot
                .dropdown
                .iter()
                .map(|(_, name, _)| name.to_string())
                .collect()
        });
        assert_eq!(entries, ["big", "small"]);

        cx.simulate_keystrokes("escape");
        assert_eq!(view.read_with(cx, |view, _| view.dropdown), None);
    }
}
