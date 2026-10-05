//! The cleanup half of the window: suggestion cards, the basket, the review sheet, the cleanup
//! history and the item context menu.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use cleanup::{
    Action, Basket, Category, LogEntry, ManagedPlace, Opener, OperationLog, Outcome, Places,
    RunningApps, Safety, Suggestion,
};
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, MouseDownEvent, Pixels, Point, Render,
    SharedString, Task, Window, anchored, deferred, div, prelude::*, px,
};
use scanner::{Measurement, NodeFlags, NodeId};

use super::{StorageView, display_path};
use crate::format;
use crate::model::Item;
use crate::theme::Theme;
use crate::widgets::{badge, button, primary_button};

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
    deleting: bool,
    deleted: bool,
    delete_failures: usize,
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
    moving: bool,
    pub(super) result: Option<CleanupResult>,
    log: Option<OperationLog>,
    history: Vec<LogEntry>,
    _measure: Option<Task<()>>,
    _suggest: Option<Task<()>>,
    _work: Option<Task<()>>,
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
            _work: None,
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
        self._measure = None;
        self._suggest = None;
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
                self.add_to_basket(vec![(path, Some(node), size)], Category::Chosen, cx);
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
        items: Vec<(PathBuf, Option<NodeId>, u64)>,
        category: Category,
        cx: &mut Context<Self>,
    ) -> usize {
        let Some(basket) = &mut self.cleanup.basket else {
            self.show_notice(
                "The basket isn't available: the home folder wasn't found",
                cx,
            );
            return 0;
        };
        let mut added = 0;
        let mut refused = Vec::new();
        for (path, node, size) in items {
            match basket.add(&path, node, category, size) {
                Ok(_) => added += 1,
                Err(error) => refused.push((path, error.reason())),
            }
        }
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
        added
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
                    let tree = shared.read();
                    (
                        cleanup::suggest(&tree, &places, &running, SystemTime::now()),
                        cleanup::managed_places(&tree, &places),
                    )
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.generation == generation {
                    view.cleanup.suggestions = Rc::new(suggestions);
                    view.cleanup.managed = Rc::new(managed);
                    view.cleanup.suggesting = false;
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
                .map(|item| (item.path.clone(), Some(item.node), item.size))
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
            .map(|item| (item.path.clone(), Some(item.node), item.size))
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
                vec![(item.path.clone(), Some(item.node), item.size)],
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
        if let Some(super::Hit::Segment(index)) = self.hit(event.position) {
            let item = self.segments[index].item;
            self.open_menu(item, event.position, cx);
        }
    }

    pub(super) fn move_basket_to_trash(&mut self, cx: &mut Context<Self>) {
        if self.is_scanning() {
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
            let (outcome, before) = cx
                .background_executor()
                .spawn(async move {
                    let before = volumes::volume_at(&root)
                        .ok()
                        .map(|volume| volume.available);
                    let running = RunningApps::current();
                    let outcome = cleanup::move_to_trash(&items, &places, &scan_root, &running);
                    (outcome, before)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.finish_move(outcome, will_free, before, snapshots, cx);
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
                if let Some(node) = moved.node {
                    shared.remove(node);
                }
            }
        }
        if let Some(basket) = &mut self.cleanup.basket {
            for moved in &outcome.moved {
                basket.remove(&moved.path);
            }
        }
        if let Some(log) = &self.cleanup.log
            && let Err(error) = log.append(&LogEntry::from_outcome(&outcome, SystemTime::now()))
        {
            tracing::warn!(kind = ?error.kind(), "writing the cleanup log failed");
        }
        self.leave_removed_folders();
        self.cleanup.result = Some(CleanupResult {
            outcome,
            will_free,
            available_before,
            available_after: None,
            snapshots,
            deleting: false,
            deleted: false,
            delete_failures: 0,
        });
        self.cleanup.sheet = Some(Sheet::Basket);
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
        let paths: Vec<PathBuf> = result
            .outcome
            .moved
            .iter()
            .filter_map(|moved| moved.trashed.clone())
            .collect();
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
                if let Some(result) = &mut view.cleanup.result {
                    result.deleting = false;
                    result.deleted = true;
                    result.available_after = after;
                    result.delete_failures = failures.len();
                    let mut entries = LogEntry::from_outcome(&result.outcome, SystemTime::now());
                    for entry in &mut entries {
                        entry.action = Action::DeletedFromTrash;
                        entry.failed = 0;
                    }
                    entries.retain(|entry| entry.count > 0);
                    if let Some(log) = &view.cleanup.log
                        && let Err(error) = log.append(&entries)
                    {
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
                    .child(suggestion.category.title()),
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
        div()
            .id(("managed", index))
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
                            .child(format!(
                                "{} · {}",
                                format::bytes(place.size),
                                place.kind.open_label()
                            )),
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
            list = list.child(
                entry("menu-open", "Open".into(), true).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        this.cleanup.menu = None;
                        if let Item::Node(id) = item {
                            this.open_folder(id, cx);
                        }
                    },
                )),
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
                    suggestion.category.title().into(),
                    self.render_suggestion_sheet(index, &suggestion, theme, cx),
                )
            }
            Sheet::Basket => ("Review basket".into(), self.render_basket_sheet(theme, cx)),
            Sheet::History => ("Cleanup history".into(), self.render_history_sheet(theme)),
        };
        let card =
            div()
                .id("sheet")
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
                    .px_4()
                    .py_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(suggestion.category.reason())
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
                    .child(
                        primary_button(
                            "add-all",
                            "Add all to basket",
                            theme.accent,
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
                this.child(
                    div()
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
                                        theme.destructive,
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
        if moved > 0 {
            section = section.child(div().font_weight(FontWeight::SEMIBOLD).child(format!(
                "Moved {} ({}) to the Trash",
                format::items(moved as u64),
                format::bytes(moved_bytes)
            )));
        }
        if !result.outcome.failed.is_empty() {
            section = section.child(div().text_color(theme.destructive).child(format!(
                "{} stayed where they were:",
                format::items(result.outcome.failed.len() as u64)
            )));
            for failed in result.outcome.failed.iter().take(20) {
                section = section.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted)
                        .truncate()
                        .child(format!("{} · {}", file_name(&failed.path), failed.reason)),
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
            section = section.child(div().child(text));
            if let (Some(gained), Some(expected)) = (gained, result.will_free)
                && gained.saturating_mul(10) < expected.saturating_mul(9)
            {
                let why = if result.snapshots.unwrap_or(0) > 0 {
                    "Less than expected: local Time Machine snapshots still hold the data. macOS frees it as the snapshots expire, usually within 24 hours."
                } else {
                    "Less than expected: some data is still shared with clones, or other apps wrote to the disk meanwhile."
                };
                section = section.child(div().text_xs().text_color(theme.muted).child(why));
            }
        } else if trashed > 0 {
            let expected = result
                .will_free
                .map(|bytes| format!(" (frees about {})", format::bytes(bytes)))
                .unwrap_or_default();
            section = section
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted)
                        .child("They still take up space until the Trash is emptied. You can put them back from the Trash in Finder."),
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
                            theme.destructive,
                            !result.deleting,
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.delete_trashed(cx))),
                    ),
                );
        }
        section
    }

    fn render_history_sheet(&self, theme: &Theme) -> AnyElement {
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
        for entry in self.cleanup.history.iter().take(SHEET_ROWS) {
            let action = match entry.action {
                Action::MovedToTrash => "Moved to Trash",
                Action::DeletedFromTrash => "Deleted from Trash",
            };
            let mut summary = format!(
                "{action} · {} · {} · {}",
                entry.category.title(),
                format::items(entry.count as u64),
                format::bytes(entry.bytes)
            );
            if entry.failed > 0 {
                summary.push_str(&format!(" · {} failed", entry.failed));
            }
            rows = rows.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(div().text_color(theme.muted).child(format_time(entry.time)))
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
    div()
        .id(id)
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
                .child(div().truncate().child(file_name(path)))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted)
                        .truncate()
                        .child(match note {
                            Some(note) => format!("{note} · {}", display_path(path)),
                            None => display_path(path),
                        }),
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
