use std::cell::Cell;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, ClickEvent, Context, Div, ElementId, FocusHandle, FontWeight, Hsla, MouseMoveEvent,
    PathPromptOptions, Pixels, Point, Rgba, ScrollStrategy, SharedString, Stateful, Task,
    UniformListScrollHandle, Window, actions, anchored, canvas, deferred, div, prelude::*, px,
    relative, uniform_list,
};
use scanner::{NodeId, NodeKind, ScanHandle, ScanOptions, Tree};
use volumes::{DATA_VOLUME_MOUNT_POINT, StartupDisk};

use crate::format;
use crate::model::{self, Accounting, Item, Row};
use crate::settings::Settings;
use crate::sunburst::{self, Geometry, Hit, Segment};
use crate::theme::Theme;

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const ROW_HEIGHT: f32 = 28.0;
const DROPDOWN_LIMIT: usize = 60;
const FULL_DISK_ACCESS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

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
        ToggleCrashReports,
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

/// What the chart center, the status bar and the tooltips say about one item.
#[derive(Debug, Clone)]
struct Info {
    item: Item,
    name: SharedString,
    size: u64,
    settled: bool,
    items: Option<u64>,
    note: Option<&'static str>,
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
}

pub struct StorageView {
    focus_handle: FocusHandle,
    settings: Settings,
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
    segments: Rc<Vec<Segment>>,
    geometry: Rc<Cell<Option<Geometry>>>,
    list_scroll: UniformListScrollHandle,
    _poll: Option<Task<()>>,
    _disk_read: Option<Task<()>>,
}

impl StorageView {
    pub fn new(settings: Settings, scope: Scope, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            focus_handle: cx.focus_handle(),
            settings,
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
            segments: Rc::default(),
            geometry: Rc::default(),
            list_scroll: UniformListScrollHandle::new(),
            _poll: None,
            _disk_read: None,
        };
        view.start_scan(scope, cx);
        view
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
        self.disk = None;
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
        let Some(scan) = &mut self.scan else {
            return false;
        };
        cx.notify();
        if scan.handle.is_finished() {
            scan.elapsed = Some(scan.started.elapsed());
            self.read_disk(cx);
            return false;
        }
        true
    }

    fn is_scanning(&self) -> bool {
        self.scan
            .as_ref()
            .is_some_and(|scan| scan.elapsed.is_none())
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
                .map_or(0, |segment| segment.size)
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
                note: model::note(tree, item, complete),
                path: Some(tree.path(id)),
            },
            Item::OtherVolumes => Info {
                item,
                name: "macOS and other volumes".into(),
                size: row_size(),
                settled: complete,
                items: None,
                note: Some("The macOS, VM, Preboot and Recovery volumes on this disk"),
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
                    "Protected folders, snapshots and file-system data the scan couldn't see",
                ),
                path: None,
            },
            Item::Smaller(parent) => Info {
                item,
                name: "Smaller items".into(),
                size: segment_size(),
                settled: tree.is_settled(parent),
                items: None,
                note: Some("Items too small to draw one by one"),
                path: None,
            },
        }
    }

    /// Reads the tree once for this frame and refreshes the rows and chart layout.
    fn snapshot(&mut self) -> Option<Snapshot> {
        let scan = self.scan.as_ref()?;
        let tree = scan.handle.tree();
        let complete = tree.is_complete();
        let accounting = self.accounting(&tree);

        let rows = model::rows(&tree, self.folder, accounting, complete);
        let segments = sunburst::layout(&tree, self.folder, &rows);
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
        self.segments = Rc::new(segments);

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
        })
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
        if self.dropdown.take().is_none() {
            self.selected = None;
        }
        cx.notify();
    }

    fn move_selection(&mut self, step: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
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
        let Some(item) = self.selected else {
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

    fn toggle_crash_reports(
        &mut self,
        _: &ToggleCrashReports,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.crash_reports = !self.settings.crash_reports;
        telemetry::set_enabled(self.settings.crash_reports);
        if let Err(error) = self.settings.save() {
            tracing::warn!("failed to save settings: {error}");
        }
        cx.set_menus(crate::menus(self.settings.crash_reports));
        cx.notify();
    }

    fn hit(&self, position: Point<Pixels>) -> Option<Hit> {
        sunburst::hit_test(&self.segments, self.geometry.get()?, position)
    }

    fn hover_chart(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let item = match self.hit(event.position) {
            Some(Hit::Segment(index)) => Some(self.segments[index].item),
            _ => None,
        };
        self.set_hovered(item, cx);
    }

    fn click_chart(&mut self, event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        match self.hit(event.position()) {
            Some(Hit::Center) => self.go_up(&GoUp, window, cx),
            Some(Hit::Segment(index)) => {
                let segment = self.segments[index];
                match segment.item {
                    Item::Node(id) if segment.folder => self.open_folder(id, cx),
                    Item::Smaller(parent) if parent != self.folder => self.open_folder(parent, cx),
                    item => self.select(Some(item), cx),
                }
            }
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

fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    theme: &Theme,
) -> Stateful<Div> {
    let hover = theme.hover;
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .bg(theme.panel)
        .cursor_pointer()
        .whitespace_nowrap()
        .hover(move |style| style.bg(hover))
        .child(label.into())
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
        let accent = theme.accent;

        container
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
        bar
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
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .cursor_pointer()
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
            .on_mouse_move(cx.listener(Self::hover_chart))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered {
                    this.set_hovered(None, cx);
                }
            }))
            .on_click(cx.listener(Self::click_chart))
    }

    fn render_list(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.rows.len();
        div()
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
                Some(
                    div()
                        .id(("row", index))
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
                        .child(div().flex_1().min_w_0().truncate().child(name))
                        .when_some(model::note(&tree, row.item, complete), |this, note| {
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

    fn render_status(
        &self,
        snapshot: &Snapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
            if let Some(note) = info.note {
                text.push_str(&format!(" · {note}"));
            }
            text
        });
        let progress = match &self.scan {
            Some(scan) if scan.elapsed.is_none() => format!(
                "Scanning… {} · {}",
                format::items(snapshot.entries),
                format::duration(scan.started.elapsed())
            ),
            Some(scan) if snapshot.complete => format!(
                "{} scanned in {}",
                format::items(snapshot.entries),
                format::duration(scan.elapsed.unwrap_or_default())
            ),
            Some(_) => format!(
                "Scan stopped · showing {} found so far",
                format::items(snapshot.entries)
            ),
            None => String::new(),
        };
        let shows_not_measured = info.is_some_and(|info| info.item == Item::NotMeasured);
        let accent: Rgba = theme.accent;

        div()
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
                    .child(description.unwrap_or_default()),
            )
            .when(shows_not_measured, |this| {
                this.child(
                    div()
                        .id("full-disk-access")
                        .flex_none()
                        .text_color(accent)
                        .cursor_pointer()
                        .child("Open Full Disk Access settings")
                        .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                            cx.open_url(FULL_DISK_ACCESS_URL)
                        })),
                )
            })
            .child(div().flex_none().child(progress))
    }
}

impl Render for StorageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::for_appearance(window.appearance());
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
                        .child(self.render_list(&theme, cx)),
                )
                .child(self.render_status(snapshot, &theme, cx))
                .into_any_element(),
            (None, None) => div().flex_1().into_any_element(),
        };

        div()
            .key_context("StorageView")
            .track_focus(&self.focus_handle)
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
            .on_action(cx.listener(Self::toggle_crash_reports))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.text)
            .text_sm()
            .child(self.render_toolbar(&theme, cx))
            .child(self.render_capacity(&theme))
            .child(body)
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
        let (view, cx) =
            cx.add_window_view(|_, cx| StorageView::new(Settings::default(), scope, cx));
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
