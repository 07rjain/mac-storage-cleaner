//! The cleanup half of the window: suggestion cards, the basket, the review sheet, the cleanup
//! history and the item context menu.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use cleanup::{
    Action, Basket, Category, CopyReport, CopySearch, Inventory, ItemState, LeftoverProof,
    LogEntry, LoggedItem, ManagedPlace, Opener, OperationLog, Outcome, PUT_BACK_UNAVAILABLE,
    Places, RunningApps, Safety, Suggestion, put_back_items,
};
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, MouseDownEvent, Pixels, Point, Render, Role,
    SharedString, Task, Window, anchored, deferred, div, prelude::*, px,
};
use scanner::{Measurement, NodeFlags, NodeId};

use super::{StorageView, display_path};
use crate::file_types::FileType;
use crate::format;
use crate::model::Item;
use crate::theme::Theme;
use crate::widgets::{badge, button, primary_button};

/// Path, scan node, size, leftover proof, and exact-copy proof for one basket add.
type BasketAdd = (
    PathBuf,
    Option<NodeId>,
    u64,
    Option<LeftoverProof>,
    Option<cleanup::CopyProof>,
);

/// Rows shown at most in one sheet; the rest are summarized.
const SHEET_ROWS: usize = 300;
const CARD_HEIGHT: f32 = 60.;
const NOTICE_TIME: Duration = Duration::from_secs(5);

pub(super) enum WillFree {
    Empty,
    Measuring,
    Measured(Measurement),
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Sheet {
    Suggestion(usize),
    Basket,
    History,
}

pub(super) struct ContextMenu {
    item: Item,
    position: Point<Pixels>,
    in_basket: bool,
    /// Why the item can't go in the basket, if it can't.
    refusal: Option<String>,
    folder: bool,
}

pub(super) struct CleanupResult {
    outcome: Outcome,
    will_free: Option<u64>,
    available_before: Option<u64>,
    available_after: Option<u64>,
    snapshots: Option<usize>,
    /// Everything in the Trash folders the items went to; `None` if they can't be read, which
    /// needs Full Disk Access.
    trash_size: Option<u64>,
    deleting: bool,
    deleted: bool,
    delete_failures: usize,
    records: Vec<LoggedItem>,
    put_back_unavailable: bool,
}

impl CleanupResult {
    pub(super) fn gained(&self) -> Option<u64> {
        Some(self.available_after?.saturating_sub(self.available_before?))
    }

    pub(super) fn available_before(&self) -> Option<u64> {
        self.available_before
    }
}

pub(super) struct Cleanup {
    pub(super) places: Option<Places>,
    pub(super) basket: Option<Basket>,
    revision: u64,
    pub(super) will_free: WillFree,
    pub(super) suggestions: Rc<Vec<Suggestion>>,
    pub(super) managed: Rc<Vec<ManagedPlace>>,
    suggesting: bool,
    pub(super) sheet: Option<Sheet>,
    pub(super) menu: Option<ContextMenu>,
    notice: Option<(SharedString, Instant)>,
    pub(super) moving: bool,
    pub(super) result: Option<CleanupResult>,
    log: Option<OperationLog>,
    history: Vec<LogEntry>,
    _measure: Option<Task<()>>,
    _suggest: Option<Task<()>>,
    copies_running: bool,
    copies_stop: Option<Arc<AtomicBool>>,
    _add: Option<Task<()>>,
    _work: Option<Task<()>>,
    _copies: Option<Task<()>>,
    _notice: Option<Task<()>>,
}

impl Cleanup {
    pub(super) fn new() -> Self {
        Self {
            places: Places::current(),
            basket: None,
            revision: 0,
            will_free: WillFree::Empty,
            suggestions: Rc::default(),
            managed: Rc::default(),
            suggesting: false,
            sheet: None,
            menu: None,
            notice: None,
            moving: false,
            result: None,
            log: crate::settings::cleanup_log_path().map(OperationLog::new),
            history: Vec::new(),
            _measure: None,
            _suggest: None,
            copies_running: false,
            copies_stop: None,
            _add: None,
            _work: None,
            _copies: None,
            _notice: None,
        }
    }

    /// A new scan starts with an empty basket: its items refer to the old tree.
    pub(super) fn reset(&mut self, root: &Path) {
        self.basket = self.places.clone().map(|places| Basket::new(places, root));
        self.revision += 1;
        self.will_free = WillFree::Empty;
        self.suggestions = Rc::default();
        self.managed = Rc::default();
        self.suggesting = false;
        self.menu = None;
        if self.sheet != Some(Sheet::History) {
            self.sheet = None;
        }
        if let Some(stop) = &self.copies_stop {
            stop.store(true, Ordering::Relaxed);
        }
        self.copies_running = false;
        self.copies_stop = None;
        self._measure = None;
        self._suggest = None;
        self._add = None;
        self._copies = None;
    }

    fn basket_len(&self) -> usize {
        self.basket.as_ref().map_or(0, Basket::len)
    }

    fn in_basket(&self, path: &Path) -> bool {
        self.basket
            .as_ref()
            .is_some_and(|basket| basket.contains(path))
    }

    pub(super) fn notice(&self) -> Option<SharedString> {
        self.notice
            .as_ref()
            .filter(|(_, shown)| shown.elapsed() < NOTICE_TIME)
            .map(|(text, _)| text.clone())
    }

    fn set_notice(&mut self, text: impl Into<SharedString>) {
        self.notice = Some((text.into(), Instant::now()));
    }

    pub(super) fn is_open(&self) -> bool {
        self.sheet.is_some() || self.menu.is_some()
    }
}

/// Dragged from the list onto the basket bar.
#[derive(Clone)]
pub(super) struct DraggedItem {
    pub(super) item: Item,
    pub(super) name: SharedString,
}

pub(super) struct DragPreview {
    name: SharedString,
}

impl DragPreview {
    pub(super) fn new(name: SharedString) -> Self {
        Self { name }
    }
}

impl Render for DragPreview {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::for_appearance(window.appearance());
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(theme.panel)
            .border_1()
            .border_color(theme.accent)
            .text_sm()
            .text_color(theme.text)
            .shadow_md()
            .child(self.name.clone())
    }
}

impl StorageView {
    /// User path, scan size and node of a file or folder in the chart.
    fn item_target(&self, item: Item) -> Option<(PathBuf, u64, NodeId)> {
        let Item::Node(id) = item else {
            return None;
        };
        self.with_tree(|tree| {
            (id != tree.root()).then(|| (Places::user_path(&tree.path(id)), tree.allocated(id), id))
        })
        .flatten()
    }

    pub(super) fn add_item(&mut self, item: Item, cx: &mut Context<Self>) {
        match self.item_target(item) {
            Some((path, size, node)) => {
                self.add_to_basket(
                    vec![(path, Some(node), size, None, None)],
                    Category::Chosen,
                    cx,
                );
            }
            None => self.show_notice("Only files and folders can go in the basket", cx),
        }
    }

    /// A short message in the status bar, for example why an item can't go in the basket.
    pub(super) fn show_notice(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.cleanup.set_notice(text);
        self.cleanup._notice = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NOTICE_TIME).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        }));
        cx.notify();
    }

    /// Adds items, and says why any were refused. Returns how many were added.
    fn add_to_basket(
        &mut self,
        items: Vec<BasketAdd>,
        category: Category,
        cx: &mut Context<Self>,
    ) -> usize {
        if items.iter().any(|item| item.3.is_some()) {
            self.add_checked_leftovers(items, cx);
            return 0;
        }
        let (added, refused) = {
            let Some(basket) = &mut self.cleanup.basket else {
                self.show_notice(
                    "The basket isn't available: the home folder wasn't found",
                    cx,
                );
                return 0;
            };
            let mut added = 0;
            let mut refused = Vec::new();
            for (path, node, size, _, copy) in items {
                let added_one = if let Some(copy) = copy.as_ref() {
                    basket.add_copy(&path, node, size, copy)
                } else {
                    basket.add(&path, node, category, size)
                };
                match added_one {
                    Ok(_) => added += 1,
                    Err(error) => refused.push((path, error.reason())),
                }
            }
            (added, refused)
        };
        self.report_basket_adds(added, refused, cx);
        added
    }

    /// Leftover proofs are checked against a fresh Applications inventory, off the main thread.
    fn add_checked_leftovers(&mut self, items: Vec<BasketAdd>, cx: &mut Context<Self>) {
        let Some(home) = self
            .cleanup
            .places
            .as_ref()
            .map(|places| places.home.clone())
        else {
            self.show_notice(
                "The basket isn't available: the home folder wasn't found",
                cx,
            );
            return;
        };
        let generation = self.generation;
        self.cleanup._add = Some(cx.spawn(async move |this, cx| {
            let inventory = cx
                .background_executor()
                .spawn(async move {
                    let running = RunningApps::current();
                    Inventory::for_this_mac(&home, &running)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.generation == generation {
                    view.finish_leftover_add(items, &inventory, cx);
                }
            });
        }));
        cx.notify();
    }

    fn finish_leftover_add(
        &mut self,
        items: Vec<BasketAdd>,
        inventory: &Inventory,
        cx: &mut Context<Self>,
    ) {
        let (added, refused) = {
            let Some(basket) = &mut self.cleanup.basket else {
                return;
            };
            let mut added = 0;
            let mut refused = Vec::new();
            for (path, node, size, proof, _copy) in items {
                if basket.contains(&path) {
                    continue;
                }
                let result = match proof {
                    Some(proof) => basket.add_leftover(&path, node, size, &proof, inventory),
                    None => basket.add(&path, node, Category::Leftovers, size),
                };
                match result {
                    Ok(_) => added += 1,
                    Err(error) => refused.push((path, error.reason())),
                }
            }
            (added, refused)
        };
        self.report_basket_adds(added, refused, cx);
    }

    fn report_basket_adds(
        &mut self,
        added: usize,
        refused: Vec<(PathBuf, String)>,
        cx: &mut Context<Self>,
    ) {
        let notice = match refused.as_slice() {
            [] => None,
            [(path, reason)] => Some(format!(
                "{} can't go in the basket: {reason}",
                file_name(path)
            )),
            [(_, reason), rest @ ..] => Some(format!(
                "{} items can't go in the basket: {reason}{}",
                rest.len() + 1,
                if rest.iter().all(|(_, other)| other == reason) {
                    ""
                } else {
                    ", and other reasons"
                }
            )),
        };
        if let Some(notice) = notice {
            self.show_notice(notice, cx);
        }
        if added > 0 {
            self.basket_changed(cx);
        }
        cx.notify();
    }

    pub(super) fn remove_from_basket(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(basket) = &mut self.cleanup.basket {
            basket.remove(path);
            self.basket_changed(cx);
        }
    }

    /// Adds the item, or takes it out if it is in the basket itself.
    pub(super) fn toggle_in_basket(&mut self, item: Item, cx: &mut Context<Self>) {
        let Some((path, _, _)) = self.item_target(item) else {
            self.add_item(item, cx);
            return;
        };
        let exact = self
            .cleanup
            .basket
            .as_ref()
            .is_some_and(|basket| basket.items().iter().any(|entry| entry.path == path));
        if exact {
            self.remove_from_basket(&path, cx);
        } else {
            self.add_item(item, cx);
        }
    }

    pub(super) fn empty_basket(&mut self, cx: &mut Context<Self>) {
        if let Some(basket) = &mut self.cleanup.basket {
            basket.clear();
            self.basket_changed(cx);
        }
    }

    fn basket_changed(&mut self, cx: &mut Context<Self>) {
        self.cleanup.revision += 1;
        let revision = self.cleanup.revision;
        let paths = self
            .cleanup
            .basket
            .as_ref()
            .map(Basket::paths)
            .unwrap_or_default();
        if paths.is_empty() {
            self.cleanup.will_free = WillFree::Empty;
            self.cleanup._measure = None;
            cx.notify();
            return;
        }
        self.cleanup.will_free = WillFree::Measuring;
        self.cleanup._measure = Some(cx.spawn(async move |this, cx| {
            let measured = cx
                .background_executor()
                .spawn(async move { scanner::measure(&paths) })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.cleanup.revision == revision {
                    view.cleanup.will_free = match measured {
                        Ok(measurement) => WillFree::Measured(measurement),
                        Err(error) => {
                            tracing::warn!(kind = ?error.kind(), "measuring the basket failed");
                            WillFree::Failed
                        }
                    };
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    /// Runs the suggestion rules on the finished scan, off the main thread.
    pub(super) fn compute_suggestions(&mut self, cx: &mut Context<Self>) {
        let (Some(scan), Some(places)) = (&self.scan, self.cleanup.places.clone()) else {
            return;
        };
        let shared = scan.handle.shared_tree();
        let generation = self.generation;
        self.cleanup.suggesting = true;
        self.cleanup._suggest = Some(cx.spawn(async move |this, cx| {
            let (suggestions, managed) = cx
                .background_executor()
                .spawn(async move {
                    let running = RunningApps::current();
                    let inventory = Inventory::for_this_mac(&places.home, &running);
                    let tree = shared.read();
                    (
                        cleanup::suggest(&tree, &places, &running, SystemTime::now(), &inventory),
                        cleanup::managed_places(&tree, &places),
                    )
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.generation == generation {
                    view.cleanup.suggestions = Rc::new(suggestions);
                    view.cleanup.managed = Rc::new(managed);
                    view.cleanup.suggesting = false;
                    view.start_copies(cx);
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    pub(super) fn click_suggestion(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(suggestion) = self.cleanup.suggestions.get(index).cloned() else {
            return;
        };
        if suggestion.preselected() {
            let items = suggestion
                .items
                .iter()
                .filter(|item| !self.cleanup.in_basket(&item.path))
                .map(|item| {
                    (
                        item.path.clone(),
                        Some(item.node),
                        item.size,
                        item.proof.clone(),
                        item.copy.clone(),
                    )
                })
                .collect();
            self.add_to_basket(items, suggestion.category, cx);
        }
        self.cleanup.sheet = Some(Sheet::Suggestion(index));
        cx.notify();
    }

    fn add_all_of_suggestion(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(suggestion) = self.cleanup.suggestions.get(index).cloned() else {
            return;
        };
        let items = suggestion
            .items
            .iter()
            .filter(|item| !self.cleanup.in_basket(&item.path))
            .map(|item| {
                (
                    item.path.clone(),
                    Some(item.node),
                    item.size,
                    item.proof.clone(),
                    item.copy.clone(),
                )
            })
            .collect();
        self.add_to_basket(items, suggestion.category, cx);
    }

    fn toggle_candidate(&mut self, index: usize, row: usize, cx: &mut Context<Self>) {
        let Some(suggestion) = self.cleanup.suggestions.get(index).cloned() else {
            return;
        };
        let Some(item) = suggestion.items.get(row) else {
            return;
        };
        if self.cleanup.in_basket(&item.path) {
            self.remove_from_basket(&item.path, cx);
        } else {
            self.add_to_basket(
                vec![(
                    item.path.clone(),
                    Some(item.node),
                    item.size,
                    item.proof.clone(),
                    item.copy.clone(),
                )],
                suggestion.category,
                cx,
            );
        }
    }

    fn open_managed(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(place) = self.cleanup.managed.get(index).cloned() else {
            return;
        };
        self.show_notice(place.kind.advice(), cx);
        match place.kind.opener() {
            Opener::App(bundle_id) => {
                let _ = Command::new("/usr/bin/open")
                    .args(["-b", bundle_id])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
            }
            Opener::Settings(url) => cx.open_url(url),
            Opener::RevealInFinder => cx.reveal_path(&place.path),
        }
    }

    pub(super) fn open_basket(&mut self, cx: &mut Context<Self>) {
        self.cleanup.menu = None;
        self.cleanup.sheet = Some(Sheet::Basket);
        cx.notify();
    }

    fn start_copies(&mut self, cx: &mut Context<Self>) {
        if let Some(stop) = &self.cleanup.copies_stop {
            stop.store(true, Ordering::Relaxed);
        }
        let (Some(scan), Some(places)) = (&self.scan, self.cleanup.places.clone()) else {
            return;
        };
        let shared = scan.handle.shared_tree();
        let generation = self.generation;
        let claimed: HashSet<NodeId> = self
            .cleanup
            .suggestions
            .iter()
            .flat_map(|suggestion| suggestion.items.iter().map(|item| item.node))
            .collect();
        let stop = Arc::new(AtomicBool::new(false));
        self.cleanup.copies_stop = Some(Arc::clone(&stop));
        self.cleanup.copies_running = true;
        self.set_copies(Vec::new(), vec![CopyReport::progress(0, 0)]);
        self.cleanup._copies = Some(cx.spawn(async move |this, cx| {
            let mut search = cx
                .background_executor()
                .spawn({
                    let shared = shared.clone();
                    let places = places.clone();
                    async move {
                        let tree = shared.read();
                        CopySearch::start(&tree, &places, &claimed)
                    }
                })
                .await;
            loop {
                let flag = Arc::clone(&stop);
                let (next, more, checked, total) = cx
                    .background_executor()
                    .spawn(async move {
                        let more = search.step(&flag);
                        let checked = search.checked();
                        let total = search.total();
                        (search, more, checked, total)
                    })
                    .await;
                search = next;
                let current = this.update(cx, |view, cx| {
                    if view.generation != generation {
                        return false;
                    }
                    view.set_copies(Vec::new(), vec![CopyReport::progress(checked, total)]);
                    cx.notify();
                    true
                });
                if !current.unwrap_or(false) {
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
                if !more {
                    let report = search.finish();
                    let _ = this.update(cx, |view, cx| {
                        if view.generation != generation {
                            return;
                        }
                        view.cleanup.copies_running = false;
                        let skipped = report.skipped();
                        view.set_copies(report.items, skipped);
                        cx.notify();
                    });
                    break;
                }
            }
        }));
    }

    fn stop_copies(&mut self, cx: &mut Context<Self>) {
        if let Some(stop) = &self.cleanup.copies_stop {
            stop.store(true, Ordering::Relaxed);
        }
        cx.notify();
    }

    fn set_copies(&mut self, items: Vec<cleanup::Candidate>, skipped: Vec<String>) {
        let mut suggestions = (*self.cleanup.suggestions).clone();
        suggestions.retain(|suggestion| suggestion.category != Category::ExactCopies);
        suggestions.extend(exact_copy_cards(items, skipped));
        self.cleanup.suggestions = Rc::new(suggestions);
    }

    fn put_back_history(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(log) = self.cleanup.log.clone() else {
            return;
        };
        let Some(home) = self
            .cleanup
            .places
            .as_ref()
            .map(|places| places.home.clone())
        else {
            return;
        };
        let mut entries = self.cleanup.history.clone();
        self.cleanup._work = Some(cx.spawn(async move |this, cx| {
            let entries = cx
                .background_executor()
                .spawn(async move {
                    let Some(entry) = entries.get_mut(index) else {
                        return entries;
                    };
                    let before = entry
                        .items
                        .iter()
                        .filter(|item| item.state == ItemState::Restored)
                        .count();
                    let category = entry.category;
                    let time = SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    put_back_items(&mut entry.items, &home);
                    let restored: Vec<_> = entry
                        .items
                        .iter()
                        .filter(|item| item.state == ItemState::Restored)
                        .skip(before)
                        .cloned()
                        .collect();
                    if !restored.is_empty() {
                        entries.insert(
                            0,
                            LogEntry {
                                time,
                                action: Action::Restored,
                                category,
                                count: restored.len(),
                                bytes: restored.iter().map(|item| item.size).sum(),
                                failed: 0,
                                paths: restored
                                    .iter()
                                    .map(|item| item.original_path().to_string_lossy().into_owned())
                                    .collect(),
                                items: Vec::new(),
                            },
                        );
                    }
                    entries
                })
                .await;
            if let Err(error) = log.replace(&entries) {
                tracing::warn!(kind = ?error.kind(), "writing the cleanup log failed");
            }
            let _ = this.update(cx, |view, cx| view.open_history(cx));
        }));
        cx.notify();
    }

    pub(super) fn open_history(&mut self, cx: &mut Context<Self>) {
        self.cleanup.menu = None;
        self.cleanup.history = match &self.cleanup.log {
            Some(log) => log.read().unwrap_or_else(|error| {
                tracing::warn!(kind = ?error.kind(), "reading the cleanup log failed");
                Vec::new()
            }),
            None => Vec::new(),
        };
        self.cleanup.sheet = Some(Sheet::History);
        cx.notify();
    }

    pub(super) fn close_sheet(&mut self, cx: &mut Context<Self>) {
        self.cleanup.sheet = None;
        if self
            .cleanup
            .result
            .as_ref()
            .is_some_and(|result| result.deleted || result.outcome.moved.is_empty())
        {
            self.cleanup.result = None;
        }
        cx.notify();
    }

    pub(super) fn quick_look(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self
            .selected
            .or(self.cleanup.menu.as_ref().map(|menu| menu.item))
        else {
            return;
        };
        self.cleanup.menu = None;
        if let Some((path, _, _)) = self.item_target(item) {
            let _ = Command::new("/usr/bin/qlmanage")
                .arg("-p")
                .arg(path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
        cx.notify();
    }

    pub(super) fn open_menu(
        &mut self,
        item: Item,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.selected = Some(item);
        self.dropdown = None;
        let target = self.item_target(item);
        let folder = self
            .with_tree(|tree| crate::model::is_folder(tree, item))
            .unwrap_or(false);
        let (in_basket, refusal) = match (&target, &self.cleanup.basket) {
            (Some((path, _, _)), Some(basket)) if basket.contains(path) => (true, None),
            (Some((path, _, _)), Some(basket)) => (
                false,
                cleanup::safety::check(path, basket.places(), basket.scan_root())
                    .err()
                    .map(|refusal| refusal.reason().to_string()),
            ),
            _ => (
                false,
                Some("Only files and folders can go in the basket".into()),
            ),
        };
        self.cleanup.menu = Some(ContextMenu {
            item,
            position,
            in_basket,
            refusal,
            folder,
        });
        cx.notify();
    }

    pub(super) fn right_click_row(
        &mut self,
        item: Item,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        self.open_menu(item, event.position, cx);
    }

    pub(super) fn right_click_chart(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        if let Some(super::ChartHit::Item { item, .. }) = self.chart_hit(event.position) {
            self.open_menu(item, event.position, cx);
        }
    }

    pub(super) fn move_basket_to_trash(&mut self, cx: &mut Context<Self>) {
        if self.is_scanning() || self.folder_rescan.is_some() {
            self.show_notice(
                "Wait for the scan to finish, or stop it, before moving items",
                cx,
            );
            return;
        }
        let Some(basket) = &self.cleanup.basket else {
            return;
        };
        if basket.is_empty() || self.cleanup.moving {
            return;
        }
        let items = basket.items().to_vec();
        let places = basket.places().clone();
        let scan_root = basket.scan_root().to_path_buf();
        let will_free = match &self.cleanup.will_free {
            WillFree::Measured(measurement) => Some(measurement.freeable),
            _ => None,
        };
        let root = self.scope.root();
        let snapshots = self.disk.as_ref().and_then(|disk| disk.snapshots);
        self.cleanup.moving = true;
        self.cleanup._work = Some(cx.spawn(async move |this, cx| {
            let moved = cx
                .background_executor()
                .spawn(async move {
                    let before = volumes::volume_at(&root)
                        .ok()
                        .map(|volume| volume.available);
                    let running = RunningApps::current();
                    let inventory = Inventory::for_this_mac(&places.home, &running);
                    let outcome =
                        cleanup::move_to_trash(&items, &places, &scan_root, &running, &inventory);
                    let trash_size = trash_size(&outcome);
                    (outcome, before, trash_size)
                })
                .await;
            let (outcome, before, trash_size) = moved;
            let _ = this.update(cx, |view, cx| {
                view.finish_move(outcome, will_free, before, snapshots, cx);
                if let Some(result) = &mut view.cleanup.result {
                    result.trash_size = trash_size;
                }
            });
        }));
        cx.notify();
    }

    /// Treats `home` as the home folder, and keeps the operation log away from the real one.
    #[cfg(test)]
    pub(super) fn use_home(&mut self, home: &Path) {
        self.cleanup.places = Some(Places {
            home: home.to_path_buf(),
        });
        self.cleanup.log = None;
        self.cleanup.reset(&self.scope.root());
    }

    pub(super) fn finish_move(
        &mut self,
        outcome: Outcome,
        will_free: Option<u64>,
        available_before: Option<u64>,
        snapshots: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        self.cleanup.moving = false;
        if let Some(scan) = &self.scan {
            let shared = scan.handle.shared_tree();
            for moved in &outcome.moved {
                // A folder rescan since the item was added gives it a new node.
                let node = {
                    let tree = shared.read();
                    moved
                        .node
                        .filter(|&node| !tree.flags(node).contains(NodeFlags::REMOVED))
                        .or_else(|| tree.find(&Places::tree_path(tree.root_path(), &moved.path)))
                };
                if let Some(node) = node {
                    shared.remove(node);
                }
            }
        }
        if let Some(basket) = &mut self.cleanup.basket {
            for moved in &outcome.moved {
                basket.remove(&moved.path);
            }
        }
        let entries = LogEntry::from_outcome(&outcome, SystemTime::now());
        let (records, put_back_unavailable) = if let Some(log) = &self.cleanup.log {
            match log.append(&entries) {
                Ok(()) => (
                    entries.into_iter().flat_map(|entry| entry.items).collect(),
                    false,
                ),
                Err(error) => {
                    tracing::warn!(kind = ?error.kind(), "writing the cleanup log failed");
                    (Vec::new(), true)
                }
            }
        } else {
            (Vec::new(), true)
        };
        self.cleanup.result = Some(CleanupResult {
            outcome,
            will_free,
            available_before,
            available_after: None,
            snapshots,
            trash_size: None,
            deleting: false,
            deleted: false,
            delete_failures: 0,
            records,
            put_back_unavailable,
        });
        self.cleanup.sheet = Some(Sheet::Basket);
        self.tree_changed(cx);
    }

    /// Refreshes everything that depends on the tree after items were removed or replaced.
    pub(super) fn tree_changed(&mut self, cx: &mut Context<Self>) {
        let folder_path = self.with_tree(|tree| tree.path(self.folder));
        if let Some(path) = folder_path {
            if let Some(id) = self.with_tree(|tree| tree.find(&path)).flatten() {
                self.folder = id;
            } else {
                self.leave_removed_folders();
            }
        }
        self.read_disk(cx);
        self.compute_suggestions(cx);
        self.basket_changed(cx);
    }

    /// After items are removed from the tree, steps out of any folder that went with them.
    fn leave_removed_folders(&mut self) {
        let folder = self.folder;
        let fixed = self.with_tree(|tree| {
            let mut target = folder;
            let mut current = Some(folder);
            while let Some(node) = current {
                if tree.flags(node).contains(NodeFlags::REMOVED) {
                    target = tree.parent(node).unwrap_or(tree.root());
                }
                current = tree.parent(node);
            }
            target
        });
        if let Some(fixed) = fixed {
            self.folder = fixed;
        }
        let removed = |view: &Self, item: Option<Item>| match item {
            Some(Item::Node(id)) => view
                .with_tree(|tree| tree.flags(id).contains(NodeFlags::REMOVED))
                .unwrap_or(false),
            _ => false,
        };
        if removed(self, self.selected) {
            self.selected = None;
        }
        if removed(self, self.hovered) {
            self.hovered = None;
        }
    }

    fn delete_trashed(&mut self, cx: &mut Context<Self>) {
        let Some(result) = &mut self.cleanup.result else {
            return;
        };
        if result.deleting || result.deleted {
            return;
        }
        let paths: Vec<PathBuf> = if result.put_back_unavailable {
            result
                .outcome
                .moved
                .iter()
                .filter_map(|moved| moved.trashed.clone())
                .collect()
        } else {
            result
                .records
                .iter()
                .filter(|item| item.state == ItemState::Trashed)
                .map(LoggedItem::trashed_path)
                .collect()
        };
        result.deleting = true;
        let root = self.scope.root();
        self.cleanup._work = Some(cx.spawn(async move |this, cx| {
            let (failures, after) = cx
                .background_executor()
                .spawn(async move {
                    let failures = cleanup::delete_permanently(&paths);
                    (failures, settled_available(&root))
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                let saved = if let Some(result) = &mut view.cleanup.result {
                    result.deleting = false;
                    result.deleted = true;
                    result.available_after = after;
                    result.delete_failures = failures.len();
                    for record in &mut result.records {
                        if record.state != ItemState::Trashed {
                            continue;
                        }
                        let failed = failures
                            .iter()
                            .any(|(path, _)| path == &record.trashed_path());
                        if failed {
                            record.failure = Some("Couldn't delete it from the Trash".into());
                        } else {
                            record.state = ItemState::Deleted;
                            record.failure = None;
                        }
                    }
                    let records = result.records.clone();
                    let mut entries = LogEntry::from_outcome(&result.outcome, SystemTime::now());
                    for entry in &mut entries {
                        entry.action = Action::DeletedFromTrash;
                        entry.failed = if result.put_back_unavailable {
                            result
                                .outcome
                                .moved
                                .iter()
                                .filter(|moved| {
                                    moved.category == entry.category
                                        && moved.trashed.as_ref().is_some_and(|path| {
                                            failures.iter().any(|(failed, _)| failed == path)
                                        })
                                })
                                .count()
                        } else {
                            records
                                .iter()
                                .filter(|item| {
                                    item.category == entry.category
                                        && item.state == ItemState::Trashed
                                        && item.failure.is_some()
                                })
                                .count()
                        };
                    }
                    entries.retain(|entry| entry.count > 0);
                    Some((records, entries))
                } else {
                    None
                };
                if let Some((records, entries)) = saved
                    && let Some(log) = &view.cleanup.log
                {
                    if let Ok(mut history) = log.read() {
                        for entry in &mut history {
                            for item in &mut entry.items {
                                if let Some(updated) =
                                    records.iter().find(|record| record.id == item.id)
                                {
                                    *item = updated.clone();
                                }
                            }
                        }
                        if let Err(error) = log.replace(&history) {
                            tracing::warn!(kind = ?error.kind(), "writing the cleanup log failed");
                        }
                    }
                    if let Err(error) = log.append(&entries) {
                        tracing::warn!(kind = ?error.kind(), "writing the cleanup log failed");
                    }
                }
                view.read_disk(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }
}

/// Everything in the Trash folders `outcome` moved items into. Reading a Trash folder needs Full
/// Disk Access.
fn trash_size(outcome: &Outcome) -> Option<u64> {
    let mut folders: Vec<&Path> = outcome
        .moved
        .iter()
        .filter_map(|moved| moved.trashed.as_deref()?.parent())
        .collect();
    folders.sort_unstable();
    folders.dedup();
    if folders.is_empty() {
        return None;
    }
    let mut total = 0;
    for folder in folders {
        let items: Vec<PathBuf> = std::fs::read_dir(folder)
            .ok()?
            .filter_map(|entry| Some(entry.ok()?.path()))
            .collect();
        total += scanner::measure(&items).ok()?.allocated;
    }
    Some(total)
}

/// Free space once APFS has finished releasing blocks, which happens shortly after deletion.
fn settled_available(root: &Path) -> Option<u64> {
    // SAFETY: `sync` has no preconditions.
    unsafe { libc::sync() };
    let read = || volumes::volume_at(root).ok().map(|volume| volume.available);
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut last = read();
    let mut steady = 0;
    while Instant::now() < deadline && steady < 3 {
        std::thread::sleep(Duration::from_millis(300));
        let now = read();
        steady = if now == last { steady + 1 } else { 0 };
        last = now;
    }
    last
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || display_path(path),
        |name| name.to_string_lossy().into_owned(),
    )
}

impl StorageView {
    /// Suggestion cards and the basket bar, between the chart and the status bar.
    pub(super) fn render_cleanup_bar(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut cards = div()
            .id("suggestions")
            .flex()
            .gap_2()
            .px_3()
            .py_2()
            .overflow_x_scroll();
        let complete = self
            .scan
            .as_ref()
            .is_some_and(|scan| scan.elapsed.is_some());
        let suggestions = Rc::clone(&self.cleanup.suggestions);
        let managed = Rc::clone(&self.cleanup.managed);
        if !complete {
            cards = cards.child(
                div()
                    .h(px(CARD_HEIGHT))
                    .flex()
                    .items_center()
                    .text_color(theme.muted)
                    .child("Suggestions appear when the scan finishes"),
            );
        } else if self.cleanup.suggesting && suggestions.is_empty() {
            cards = cards.child(
                div()
                    .h(px(CARD_HEIGHT))
                    .flex()
                    .items_center()
                    .text_color(theme.muted)
                    .child("Looking for things to clean…"),
            );
        } else if suggestions.is_empty() && managed.is_empty() {
            cards = cards.child(
                div()
                    .h(px(CARD_HEIGHT))
                    .flex()
                    .items_center()
                    .text_color(theme.muted)
                    .child("No suggestions here. Right-click anything in the chart or list to add it to the basket."),
            );
        }
        for (index, suggestion) in suggestions.iter().enumerate() {
            cards = cards.child(self.render_suggestion_card(index, suggestion, theme, cx));
        }
        for (index, place) in managed.iter().enumerate() {
            cards = cards.child(self.render_managed_card(index, place, theme, cx));
        }

        div()
            .flex_none()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(theme.border)
            .child(cards)
            .child(self.render_basket_bar(theme, cx))
    }

    fn render_suggestion_card(
        &self,
        index: usize,
        suggestion: &Suggestion,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hover = theme.hover;
        let (label, color) = match suggestion.category.safety() {
            Safety::SafeToDelete => ("Safe to delete", theme.safe),
            Safety::ReviewFirst => ("Review first", theme.review),
        };
        let in_basket = suggestion
            .items
            .iter()
            .filter(|item| self.cleanup.in_basket(&item.path))
            .count();
        let detail = if suggestion.items.is_empty() {
            suggestion.skipped.first().cloned().unwrap_or_default()
        } else if in_basket > 0 {
            format!(
                "{} · {in_basket} in basket",
                format::bytes(suggestion.size())
            )
        } else {
            format!(
                "{} · {}",
                format::bytes(suggestion.size()),
                format::items(suggestion.items.len() as u64)
            )
        };
        div()
            .id(("suggestion", index))
            .role(Role::Button)
            .aria_label(format!("{}, {detail}, {label}", suggestion.title()))
            .flex_none()
            .w(px(220.))
            .h(px(CARD_HEIGHT))
            .px_2p5()
            .py_1p5()
            .flex()
            .flex_col()
            .justify_between()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.panel)
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .child(
                div()
                    .truncate()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(suggestion.title().to_string()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(theme.muted)
                            .truncate()
                            .child(detail),
                    )
                    .child(badge(label, color)),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.click_suggestion(index, cx);
            }))
    }

    fn render_managed_card(
        &self,
        index: usize,
        place: &ManagedPlace,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hover = theme.hover;
        let detail = format!(
            "{} · {}",
            format::bytes(place.size),
            place.kind.open_label()
        );
        div()
            .id(("managed", index))
            .role(Role::Button)
            .aria_label(format!("{}, {detail}, managed by app", place.kind.title()))
            .flex_none()
            .w(px(220.))
            .h(px(CARD_HEIGHT))
            .px_2p5()
            .py_1p5()
            .flex()
            .flex_col()
            .justify_between()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .child(
                div()
                    .truncate()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(place.kind.title()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(theme.muted)
                            .truncate()
                            .child(detail),
                    )
                    .child(badge("Managed by app", theme.muted)),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.open_managed(index, cx);
            }))
    }

    fn render_basket_bar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.cleanup.basket_len();
        let estimate = self.cleanup.basket.as_ref().map_or(0, Basket::estimate);
        let summary = match (&self.cleanup.will_free, count) {
            (_, 0) => {
                "Basket is empty · drag rows here, right-click an item, or click a suggestion"
                    .to_string()
            }
            (WillFree::Measured(measurement), _) => format!(
                "Basket: {} · will free {}",
                format::items(count as u64),
                format::bytes(measurement.freeable)
            ),
            (WillFree::Failed, _) => format!(
                "Basket: {} · about {}",
                format::items(count as u64),
                format::bytes(estimate)
            ),
            _ => format!(
                "Basket: {} · checking how much it frees… (about {})",
                format::items(count as u64),
                format::bytes(estimate)
            ),
        };
        let accent = theme.accent;
        div()
            .id("basket-bar")
            .role(Role::Group)
            .aria_label(summary.clone())
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .h(px(36.))
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.panel)
            .drag_over::<DraggedItem>(move |style, _, _, _| style.bg(accent.opacity(0.18)))
            .on_drop(cx.listener(|this, dragged: &DraggedItem, _, cx| {
                this.add_item(dragged.item, cx);
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(count == 0, |this| this.text_color(theme.muted))
                    .child(summary),
            )
            .child(
                button("history", "History", theme)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_history(cx))),
            )
            .child(
                button("review", "Review…", theme)
                    .when(count > 0, |this| {
                        this.border_color(accent).text_color(accent)
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_basket(cx))),
            )
    }

    pub(super) fn render_menu(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.cleanup.menu.as_ref()?;
        let hover = theme.hover;
        let muted = theme.muted;
        let entry = |id: &'static str, label: SharedString, enabled: bool| {
            div()
                .id(id)
                .role(Role::MenuItem)
                .aria_label(label.clone())
                .when(!enabled, |this| this.aria_description("Unavailable"))
                .px_3()
                .py_1()
                .rounded_sm()
                .whitespace_nowrap()
                .when(enabled, move |this| {
                    this.cursor_pointer().hover(move |style| style.bg(hover))
                })
                .when(!enabled, move |this| this.text_color(muted))
                .child(label)
        };
        let item = menu.item;
        let mut list = div()
            .id("context-menu")
            .role(Role::Menu)
            .occlude()
            .min_w(px(220.))
            .max_w(px(360.))
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.panel)
            .text_color(theme.text)
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.cleanup.menu = None;
                cx.notify();
            }));
        if menu.in_basket {
            list = list.child(
                entry("menu-basket", "Remove from Basket".into(), true).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        this.cleanup.menu = None;
                        this.toggle_in_basket(item, cx);
                    },
                )),
            );
        } else {
            list = list.child(
                entry(
                    "menu-basket",
                    "Add to Basket".into(),
                    menu.refusal.is_none(),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    let refused = this
                        .cleanup
                        .menu
                        .as_ref()
                        .is_some_and(|menu| menu.refusal.is_some());
                    this.cleanup.menu = None;
                    if !refused {
                        this.add_item(item, cx);
                    }
                    cx.notify();
                })),
            );
            if let Some(refusal) = &menu.refusal {
                list = list.child(
                    div()
                        .px_3()
                        .pb_1()
                        .max_w(px(340.))
                        .text_xs()
                        .text_color(theme.muted)
                        .child(refusal.clone()),
                );
            }
        }
        if menu.folder {
            let can_rescan = self.can_rescan_folder();
            list = list
                .child(
                    entry("menu-open", "Open".into(), true).on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.cleanup.menu = None;
                            if let Item::Node(id) = item {
                                this.open_folder(id, cx);
                            }
                        },
                    )),
                )
                .child(
                    entry("menu-rescan", "Rescan This Folder".into(), can_rescan).on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.cleanup.menu = None;
                            if let (Item::Node(id), true) = (item, this.can_rescan_folder()) {
                                this.rescan_folder(id, cx);
                            }
                            cx.notify();
                        }),
                    ),
                );
        }
        list = list
            .child(
                entry("menu-quick-look", "Quick Look".into(), true)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.quick_look(cx))),
            )
            .child(
                entry("menu-reveal", "Reveal in Finder".into(), true).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        this.cleanup.menu = None;
                        if let Some((path, _, _)) = this.item_target(item) {
                            cx.reveal_path(&path);
                        }
                        cx.notify();
                    },
                )),
            );
        Some(
            deferred(
                anchored()
                    .position(menu.position)
                    .snap_to_window_with_margin(px(8.))
                    .child(list),
            )
            .priority(2)
            .into_any_element(),
        )
    }

    pub(super) fn render_sheet(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sheet = self.cleanup.sheet?;
        let (title, body): (SharedString, AnyElement) = match sheet {
            Sheet::Suggestion(index) => {
                let suggestion = self.cleanup.suggestions.get(index)?.clone();
                (
                    suggestion.title().into(),
                    self.render_suggestion_sheet(index, &suggestion, theme, cx),
                )
            }
            Sheet::Basket => ("Review basket".into(), self.render_basket_sheet(theme, cx)),
            Sheet::History => (
                "Cleanup history".into(),
                self.render_history_sheet(theme, cx),
            ),
        };
        let card =
            div()
                .id("sheet")
                .role(Role::Dialog)
                .aria_label(title.clone())
                .occlude()
                .w(px(700.))
                .max_h(px(600.))
                .flex()
                .flex_col()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.panel)
                .shadow_lg()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .px_4()
                        .py_3()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .flex_1()
                                .text_lg()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(button("sheet-close", "Done", theme).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.close_sheet(cx)),
                        )),
                )
                .child(body);
        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.backdrop)
                .child(card)
                .into_any_element(),
        )
    }

    fn render_suggestion_sheet(
        &self,
        index: usize,
        suggestion: &Suggestion,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut rows = div()
            .id("suggestion-rows")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_2()
            .py_1();
        for (row, item) in suggestion.items.iter().enumerate().take(SHEET_ROWS) {
            let in_basket = self.cleanup.in_basket(&item.path);
            rows = rows.child(
                sheet_row(
                    ("candidate", row),
                    &item.path,
                    item.note.clone(),
                    item.size,
                    theme,
                )
                .child(
                    button(
                        ("candidate-toggle", row),
                        if in_basket { "Remove" } else { "Add" },
                        theme,
                    )
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.toggle_candidate(index, row, cx);
                        },
                    )),
                ),
            );
        }
        if suggestion.items.len() > SHEET_ROWS {
            rows = rows.child(
                div()
                    .px_2()
                    .py_1()
                    .text_color(theme.muted)
                    .child(format!("and {} more", suggestion.items.len() - SHEET_ROWS)),
            );
        }
        let all_in = suggestion
            .items
            .iter()
            .all(|item| self.cleanup.in_basket(&item.path));
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("suggestion-reason")
                    .role(Role::Group)
                    .aria_label(
                        std::iter::once(suggestion.reason().to_string())
                            .chain(suggestion.skipped.iter().cloned())
                            .collect::<Vec<_>>()
                            .join(" "),
                    )
                    .px_4()
                    .py_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(suggestion.reason().to_string())
                    .children(
                        suggestion.skipped.iter().map(|note| {
                            div().text_xs().text_color(theme.muted).child(note.clone())
                        }),
                    ),
            )
            .child(rows)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(div().flex_1().text_color(theme.muted).child(format!(
                        "{} in {}",
                        format::bytes(suggestion.size()),
                        format::items(suggestion.items.len() as u64)
                    )))
                    .when(
                        self.cleanup.copies_running && suggestion.category == Category::ExactCopies,
                        |this| {
                            this.child(button("stop-copies", "Stop", theme).on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.stop_copies(cx)),
                            ))
                        },
                    )
                    .child(
                        primary_button(
                            "add-all",
                            "Add all to basket",
                            theme.accent_fill,
                            !all_in && !suggestion.items.is_empty(),
                        )
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| {
                                this.add_all_of_suggestion(index, cx);
                            },
                        )),
                    )
                    .child(
                        button("open-basket", "Review basket…", theme).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.open_basket(cx)),
                        ),
                    ),
            )
            .into_any_element()
    }

    fn render_basket_sheet(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut body = div()
            .id("basket-rows")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col();
        if let Some(result) = &self.cleanup.result {
            body = body.child(self.render_result(result, theme, cx));
        }
        let items = self
            .cleanup
            .basket
            .as_ref()
            .map(|basket| basket.items().to_vec())
            .unwrap_or_default();
        if items.is_empty() {
            if self.cleanup.result.is_none() {
                body = body.child(
                    div()
                        .px_4()
                        .py_6()
                        .text_color(theme.muted)
                        .child("The basket is empty. Add items from a suggestion, by right-clicking a slice or row, or by dragging rows onto the basket bar."),
                );
            }
        } else {
            let mut rows = div().px_2().py_1().flex().flex_col();
            for (row, item) in items.iter().enumerate().take(SHEET_ROWS) {
                let path = item.path.clone();
                rows = rows.child(
                    sheet_row(
                        ("basket-item", row),
                        &item.path,
                        Some(item.category.title().to_string()),
                        item.size,
                        theme,
                    )
                    .child(
                        button(("basket-remove", row), "Remove", theme).on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| this.remove_from_basket(&path, cx),
                        )),
                    ),
                );
            }
            if items.len() > SHEET_ROWS {
                rows = rows.child(
                    div()
                        .px_2()
                        .text_color(theme.muted)
                        .child(format!("and {} more", items.len() - SHEET_ROWS)),
                );
            }
            body = body
                .child(
                    div()
                        .px_4()
                        .pt_2()
                        .text_xs()
                        .text_color(theme.muted)
                        .child("IN THE BASKET"),
                )
                .child(rows);
        }

        let count = items.len();
        let (will_free, details): (String, Vec<String>) = match &self.cleanup.will_free {
            WillFree::Measured(measurement) => {
                let mut details = Vec::new();
                if measurement.shared() > 0 {
                    details.push(format!(
                        "{} stays on disk: it is shared with clones or hard links outside the basket.",
                        format::bytes(measurement.shared())
                    ));
                }
                if measurement.unreadable > 0 {
                    details.push(format!(
                        "{} couldn't be read, so what is inside isn't counted.",
                        format::items(measurement.unreadable)
                    ));
                }
                if measurement.missing > 0 {
                    details.push(format!(
                        "{} no longer exist.",
                        format::items(measurement.missing)
                    ));
                }
                (
                    format!("Will free {}", format::bytes(measurement.freeable)),
                    details,
                )
            }
            WillFree::Measuring => ("Checking how much this frees…".into(), Vec::new()),
            WillFree::Failed => ("Couldn't measure the basket".into(), Vec::new()),
            WillFree::Empty => (String::new(), Vec::new()),
        };
        let scanning = self.is_scanning();
        let can_move = count > 0 && !scanning && !self.cleanup.moving;
        let footer_note = if scanning {
            "Wait for the scan to finish, or stop it, to move items."
        } else {
            "Items go to the Trash, so you can put them back. Space is freed when the Trash is emptied."
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(body)
            .when(count > 0, |this| {
                let spoken = std::iter::once(will_free.clone())
                    .chain(details.iter().cloned())
                    .chain(std::iter::once(footer_note.to_string()))
                    .collect::<Vec<_>>()
                    .join(". ");
                this.child(
                    div()
                        .id("basket-footer")
                        .role(Role::Group)
                        .aria_label(spoken)
                        .px_4()
                        .py_3()
                        .border_t_1()
                        .border_color(theme.border)
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(will_free),
                                )
                                .child(button("empty-basket", "Empty basket", theme).on_click(
                                    cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.empty_basket(cx)
                                    }),
                                ))
                                .child(
                                    primary_button(
                                        "move-to-trash",
                                        if self.cleanup.moving {
                                            "Moving…".to_string()
                                        } else {
                                            format!("Move {} to Trash", format::items(count as u64))
                                        },
                                        theme.destructive_fill,
                                        can_move,
                                    )
                                    .on_click(cx.listener(
                                        |this, _: &ClickEvent, _, cx| {
                                            this.move_basket_to_trash(cx);
                                        },
                                    )),
                                ),
                        )
                        .children(
                            details.into_iter().map(|detail| {
                                div().text_xs().text_color(theme.muted).child(detail)
                            }),
                        )
                        .child(div().text_xs().text_color(theme.muted).child(footer_note)),
                )
            })
            .into_any_element()
    }

    fn render_result(
        &self,
        result: &CleanupResult,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let moved = result.outcome.moved.len();
        let moved_bytes = result.outcome.moved_bytes();
        let mut section = div()
            .mx_4()
            .my_2()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .flex()
            .flex_col()
            .gap_1p5();
        let mut spoken = Vec::new();
        if moved > 0 {
            let text = format!(
                "Moved {} ({}) to the Trash",
                format::items(moved as u64),
                format::bytes(moved_bytes)
            );
            spoken.push(format!("{text}."));
            section = section.child(div().font_weight(FontWeight::SEMIBOLD).child(text));
        }
        if !result.outcome.failed.is_empty() {
            let text = format!(
                "{} stayed where they were:",
                format::items(result.outcome.failed.len() as u64)
            );
            spoken.push(text.clone());
            section = section.child(div().text_color(theme.destructive).child(text));
            for failed in result.outcome.failed.iter().take(20) {
                let text = format!("{} · {}", file_name(&failed.path), failed.reason);
                spoken.push(format!("{text}."));
                section = section.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted)
                        .truncate()
                        .child(text),
                );
            }
        }
        let trashed = result
            .outcome
            .moved
            .iter()
            .filter(|moved| moved.trashed.is_some())
            .count();
        if result.deleted {
            let gained = result.gained();
            let mut text = match (result.available_before, result.available_after) {
                (Some(before), Some(after)) => format!(
                    "Available space went from {} to {} ({} freed).",
                    format::bytes(before),
                    format::bytes(after),
                    format::bytes(after.saturating_sub(before))
                ),
                _ => "Deleted from the Trash.".into(),
            };
            if result.delete_failures > 0 {
                text.push_str(&format!(
                    " {} couldn't be deleted; empty the Trash in Finder to remove them.",
                    format::items(result.delete_failures as u64)
                ));
            }
            spoken.push(text.clone());
            section = section.child(div().child(text));
            if let (Some(gained), Some(expected)) = (gained, result.will_free)
                && gained.saturating_mul(10) < expected.saturating_mul(9)
            {
                let why = if result.snapshots.unwrap_or(0) > 0 {
                    "Less than expected: local Time Machine snapshots still hold the data. macOS frees it as the snapshots expire, usually within 24 hours."
                } else {
                    "Less than expected: some data is still shared with clones, or other apps wrote to the disk meanwhile."
                };
                spoken.push(why.to_string());
                section = section.child(div().text_xs().text_color(theme.muted).child(why));
            }
        } else if trashed > 0 {
            let expected = result
                .will_free
                .map(|bytes| format!(" (frees about {})", format::bytes(bytes)))
                .unwrap_or_default();
            let still = if result.put_back_unavailable {
                PUT_BACK_UNAVAILABLE
            } else {
                "They still take up space until the Trash is emptied. Put them back from Cleanup History."
            };
            let trash_note = result.trash_size.map(|size| {
                format!(
                    "The Trash holds {} in all. Emptying it in Finder deletes everything in it, not just these items.",
                    format::bytes(size)
                )
            });
            spoken.push(still.to_string());
            spoken.extend(trash_note.clone());
            section = section
                .child(div().text_xs().text_color(theme.muted).child(still))
                .children(
                    trash_note.map(|note| div().text_xs().text_color(theme.muted).child(note)),
                )
                .child(
                    div().flex().child(
                        primary_button(
                            "delete-trashed",
                            if result.deleting {
                                "Deleting…".to_string()
                            } else {
                                format!(
                                    "Delete these {} from the Trash now{expected}",
                                    format::items(trashed as u64)
                                )
                            },
                            theme.destructive_fill,
                            !result.deleting,
                        )
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.delete_trashed(cx)),
                        ),
                    ),
                );
        }
        section
            .id("cleanup-result")
            .role(Role::Status)
            .aria_label(spoken.join(" "))
    }

    fn render_history_sheet(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut rows = div()
            .id("history-rows")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_4()
            .py_2()
            .flex()
            .flex_col()
            .gap_2();
        if self.cleanup.history.is_empty() {
            rows =
                rows.child(div().py_4().text_color(theme.muted).child(
                    "Nothing cleaned yet. Every cleanup is recorded here, on this Mac only.",
                ));
        }
        for (index, entry) in self.cleanup.history.iter().take(SHEET_ROWS).enumerate() {
            let action = match entry.action {
                Action::MovedToTrash => "Moved to Trash",
                Action::DeletedFromTrash => "Deleted from Trash",
                Action::Restored => "Put back",
            };
            let can_restore = entry.items.iter().any(LoggedItem::can_put_back);
            let mut summary = format!(
                "{action} · {} · {} · {}",
                entry.category.title(),
                format::items(entry.count as u64),
                format::bytes(entry.bytes)
            );
            if entry.failed > 0 {
                summary.push_str(&format!(" · {} failed", entry.failed));
            }
            let time = format_time(entry.time);
            rows = rows.child(
                div()
                    .id(("history", index))
                    .role(Role::ListItem)
                    .aria_label(format!("{time} · {summary}"))
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(div().text_color(theme.muted).child(time))
                            .child(summary),
                    )
                    .children(entry.paths.iter().take(3).map(|path| {
                        div()
                            .text_xs()
                            .text_color(theme.muted)
                            .truncate()
                            .child(display_path(Path::new(path)))
                    }))
                    .when(entry.paths.len() > 3, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted)
                                .child(format!("and {} more", entry.paths.len() - 3)),
                        )
                    })
                    .children(entry.items.iter().take(8).map(|item| {
                        let mut line = format!(
                            "{} · {}",
                            display_path(&item.original_path()),
                            item.state.label()
                        );
                        if let Some(failure) = &item.failure {
                            line.push_str(" · ");
                            line.push_str(failure);
                        }
                        div()
                            .text_xs()
                            .text_color(theme.muted)
                            .truncate()
                            .child(line)
                    }))
                    .when(can_restore, |this| {
                        this.child(button(("put-back", index), "Put back", theme).on_click(
                            cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.put_back_history(index, cx);
                            }),
                        ))
                    }),
            );
        }
        rows.into_any_element()
    }
}

fn sheet_row(
    id: impl Into<gpui::ElementId>,
    path: &Path,
    note: Option<String>,
    size: u64,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let name = file_name(path);
    let detail = match note {
        Some(note) => format!("{note} · {}", display_path(path)),
        None => display_path(path),
    };
    div()
        .id(id)
        .role(Role::ListItem)
        .aria_label(format!("{name}, {}, {detail}", format::bytes(size)))
        .flex()
        .items_center()
        .gap_3()
        .px_2()
        .py_1()
        .rounded_sm()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().truncate().child(name))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted)
                        .truncate()
                        .child(detail),
                ),
        )
        .child(
            div()
                .flex_none()
                .w(px(80.))
                .text_right()
                .child(format::bytes(size)),
        )
}

/// One exact-copy card per file type. An empty result stays a single "Exact copies" card so
/// progress and "Nothing to remove" still have a place to show.
fn exact_copy_cards(items: Vec<cleanup::Candidate>, skipped: Vec<String>) -> Vec<Suggestion> {
    if items.is_empty() {
        return vec![Suggestion {
            category: Category::ExactCopies,
            items,
            skipped,
            title_override: None,
            reason_override: None,
        }];
    }
    let mut groups: Vec<(FileType, Vec<cleanup::Candidate>)> = Vec::new();
    for item in items {
        let kind = item
            .path
            .file_name()
            .map(FileType::of_file)
            .unwrap_or(FileType::Other);
        if let Some((_, bucket)) = groups.iter_mut().find(|(existing, _)| *existing == kind) {
            bucket.push(item);
        } else {
            groups.push((kind, vec![item]));
        }
    }
    let mut cards: Vec<Suggestion> = groups
        .into_iter()
        .map(|(kind, items)| Suggestion {
            category: Category::ExactCopies,
            items,
            skipped: Vec::new(),
            title_override: Some(format!("Exact copies · {}", kind.title())),
            reason_override: None,
        })
        .collect();
    cards.sort_by_key(|card| std::cmp::Reverse(card.size()));
    if let Some(first) = cards.first_mut() {
        first.skipped = skipped;
    }
    cards
}

/// Local date and time, for example "2026-10-05 14:03".
fn format_time(seconds: u64) -> String {
    let time = seconds as libc::time_t;
    // SAFETY: `tm` is plain data, and all-zero is a valid value for it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the call.
    if unsafe { libc::localtime_r(&time, &mut tm) }.is_null() {
        return String::new();
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use cleanup::{Category, Moved};

    use super::*;

    #[test]
    fn trash_size_counts_everything_in_the_trash_folders_used() {
        let trash = tempfile::tempdir().unwrap();
        fs::write(trash.path().join("moved.bin"), vec![1u8; 300_000]).unwrap();
        fs::create_dir(trash.path().join("older")).unwrap();
        fs::write(trash.path().join("older/file.bin"), vec![2u8; 200_000]).unwrap();
        let moved = |trashed: Option<PathBuf>| Moved {
            path: PathBuf::from("/Users/test/moved.bin"),
            trashed,
            node: None,
            category: Category::Chosen,
            size: 0,
        };
        let outcome = |moved| Outcome {
            moved,
            failed: Vec::new(),
        };

        let expected =
            scanner::measure(&[trash.path().join("moved.bin"), trash.path().join("older")])
                .unwrap()
                .allocated;
        let both = outcome(vec![
            moved(Some(trash.path().join("moved.bin"))),
            moved(Some(trash.path().join("older"))),
        ]);
        assert_eq!(trash_size(&both), Some(expected));
        assert_eq!(trash_size(&outcome(vec![moved(None)])), None);
        let unreadable = outcome(vec![moved(Some(PathBuf::from("/nonexistent/.Trash/x")))]);
        assert_eq!(trash_size(&unreadable), None);
    }

    fn copy_candidate(path: &str, size: u64) -> cleanup::Candidate {
        cleanup::Candidate {
            node: 1,
            path: PathBuf::from(path),
            size,
            note: None,
            proof: None,
            copy: None,
        }
    }

    #[test]
    fn exact_copies_are_one_card_per_file_type() {
        let cards = exact_copy_cards(
            vec![
                copy_candidate("/clips/old.mp4", 60_000_000),
                copy_candidate("/clips/older.mov", 60_000_000),
                copy_candidate("/pics/scan.jpg", 80_000_000),
            ],
            vec!["The search stopped early.".into()],
        );
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].title(), "Exact copies · Videos");
        assert_eq!(cards[0].items.len(), 2);
        assert_eq!(cards[0].skipped, ["The search stopped early."]);
        assert_eq!(cards[1].title(), "Exact copies · Images");
        assert!(cards[1].skipped.is_empty());
        assert!(cards.iter().all(|card| !card.preselected()));

        let empty = exact_copy_cards(Vec::new(), vec!["Nothing to remove".into()]);
        assert_eq!(empty.len(), 1);
        assert_eq!(empty[0].title(), "Exact copies");
        assert!(empty[0].items.is_empty());
    }
}
