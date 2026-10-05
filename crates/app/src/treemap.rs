//! Treemap: the focused folder fills the chart, and every item inside gets a rectangle with an
//! area in proportion to its size. Folders hold their contents, up to `MAX_DEPTH` levels down.
//!
//! The layout is squarified (Bruls, Huizing and van Wijk, 2000), so rectangles stay close to
//! square and sizes are easy to compare.

use std::cmp::Reverse;

use gpui::{BorderStyle, Bounds, Hsla, Pixels, Point, Window, fill, point, px, quad, size};
use scanner::{NodeId, Tree};

use crate::file_types::FileType;
use crate::model::{self, Item, Row};

/// Items smaller than this many square pixels are merged into one "Smaller items" rectangle.
const MIN_AREA: f32 = 24.0;
const MAX_DEPTH: u8 = 10;
/// Space inside a folder's rectangle around its contents.
const PAD: f32 = 2.0;
/// Height of the name strip at the top of folders that are large enough to label.
pub const HEADER: f32 = 16.0;
/// Gap between neighbouring rectangles.
const GAP: f32 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    fn area(&self) -> f32 {
        self.w * self.h
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Folder,
    File(FileType),
    /// Other volumes and "Not measured" at the top of the startup disk.
    Accounting,
    Smaller,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tile {
    pub item: Item,
    /// In layout coordinates: (0, 0) is the chart's top left corner.
    pub rect: Rect,
    /// 1 for the focused folder's own items.
    pub depth: u8,
    pub kind: Kind,
    pub size: u64,
    pub settled: bool,
    /// Whether the folder has a name strip; its contents start below it.
    pub header: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Layout {
    pub width: f32,
    pub height: f32,
    /// Folders come before their contents, so later tiles are drawn on top.
    pub tiles: Vec<Tile>,
}

/// Lays out `folder` in a `width` × `height` chart. `rows` are its items as shown in the list.
pub fn layout(tree: &Tree, folder: NodeId, rows: &[Row], width: f32, height: f32) -> Layout {
    let mut layout = Layout {
        width,
        height,
        tiles: Vec::new(),
    };
    let rect = Rect {
        x: 0.0,
        y: 0.0,
        w: width,
        h: height,
    };
    let total: u64 = rows.iter().map(|row| row.size).sum();
    if total == 0 || rect.area() < MIN_AREA {
        return layout;
    }
    let min_size = min_size(total, rect.area());
    let mut items = Vec::new();
    let mut smaller = 0;
    for row in rows.iter().filter(|row| row.size > 0) {
        if row.size >= min_size {
            items.push((row.item, row.size, row.settled));
        } else {
            smaller += row.size;
        }
    }
    if smaller > 0 {
        items.push((Item::Smaller(folder), smaller, tree.is_settled(folder)));
    }
    let inherited = model::breadcrumb(tree, folder)
        .into_iter()
        .skip(1)
        .find_map(|id| FileType::of_bundle(tree.name(id)));
    place(tree, &items, rect, 1, inherited, &mut layout.tiles);
    layout
}

fn min_size(total: u64, area: f32) -> u64 {
    ((f64::from(MIN_AREA) * total as f64 / f64::from(area)).ceil() as u64).max(1)
}

fn place(
    tree: &Tree,
    items: &[(Item, u64, bool)],
    rect: Rect,
    depth: u8,
    inherited: Option<FileType>,
    tiles: &mut Vec<Tile>,
) {
    let total: u64 = items.iter().map(|&(_, size, _)| size).sum();
    if total == 0 {
        return;
    }
    let scale = rect.area() / total as f32;
    let areas: Vec<f32> = items
        .iter()
        .map(|&(_, size, _)| size as f32 * scale)
        .collect();
    for (&(item, size, settled), rect) in items.iter().zip(squarify(&areas, rect)) {
        let (kind, inside) = match item {
            Item::Node(id) if model::is_folder(tree, item) => (
                Kind::Folder,
                Some((id, inherited.or_else(|| FileType::of_bundle(tree.name(id))))),
            ),
            Item::Node(id) => (
                Kind::File(inherited.unwrap_or_else(|| FileType::of_file(tree.name(id)))),
                None,
            ),
            Item::Smaller(_) => (Kind::Smaller, None),
            Item::OtherVolumes | Item::NotMeasured => (Kind::Accounting, None),
        };
        let header = inside.is_some() && rect.w >= 60.0 && rect.h >= 40.0;
        tiles.push(Tile {
            item,
            rect,
            depth,
            kind,
            size,
            settled,
            header,
        });
        if let Some((id, inherited)) = inside
            && depth < MAX_DEPTH
        {
            place_children(tree, id, rect, header, depth + 1, inherited, tiles);
        }
    }
}

fn place_children(
    tree: &Tree,
    folder: NodeId,
    rect: Rect,
    header: bool,
    depth: u8,
    inherited: Option<FileType>,
    tiles: &mut Vec<Tile>,
) {
    let top = if header { HEADER } else { PAD };
    let inner = Rect {
        x: rect.x + PAD,
        y: rect.y + top,
        w: rect.w - 2.0 * PAD,
        h: rect.h - top - PAD,
    };
    if inner.w < 4.0 || inner.h < 4.0 {
        return;
    }
    let total = tree.allocated(folder);
    if total == 0 {
        return;
    }
    let min_size = min_size(total, inner.area());
    let mut drawn: Vec<(Item, u64, bool)> = Vec::new();
    let mut rest = 0;
    for child in tree.children(folder) {
        let size = tree.allocated(child);
        if size >= min_size {
            drawn.push((Item::Node(child), size, tree.is_settled(child)));
        } else {
            rest += size;
        }
    }
    drawn.sort_unstable_by_key(|&(item, size, _)| {
        (
            Reverse(size),
            match item {
                Item::Node(id) => id,
                _ => NodeId::MAX,
            },
        )
    });
    if rest > 0 {
        drawn.push((Item::Smaller(folder), rest, tree.is_settled(folder)));
    }
    place(tree, &drawn, inner, depth, inherited, tiles);
}

/// Splits `rect` into rectangles with the given areas, which should be largest first and add up
/// to the area of `rect`. Returns them in the same order.
fn squarify(areas: &[f32], mut rect: Rect) -> Vec<Rect> {
    let mut out = Vec::with_capacity(areas.len());
    let mut start = 0;
    while start < areas.len() {
        let side = rect.w.min(rect.h);
        let (mut end, mut sum) = (start + 1, areas[start]);
        let (mut largest, mut smallest) = (areas[start], areas[start]);
        let mut ratio = worst(largest, smallest, sum, side);
        while end < areas.len() {
            let area = areas[end];
            let next = worst(largest.max(area), smallest.min(area), sum + area, side);
            if next > ratio {
                break;
            }
            (ratio, sum) = (next, sum + area);
            (largest, smallest) = (largest.max(area), smallest.min(area));
            end += 1;
        }
        let row = &areas[start..end];
        if rect.w >= rect.h {
            let width = if rect.h > 0.0 { sum / rect.h } else { 0.0 };
            let mut y = rect.y;
            for &area in row {
                let h = if width > 0.0 { area / width } else { 0.0 };
                out.push(Rect {
                    x: rect.x,
                    y,
                    w: width,
                    h,
                });
                y += h;
            }
            rect.x += width;
            rect.w = (rect.w - width).max(0.0);
        } else {
            let height = if rect.w > 0.0 { sum / rect.w } else { 0.0 };
            let mut x = rect.x;
            for &area in row {
                let w = if height > 0.0 { area / height } else { 0.0 };
                out.push(Rect {
                    x,
                    y: rect.y,
                    w,
                    h: height,
                });
                x += w;
            }
            rect.y += height;
            rect.h = (rect.h - height).max(0.0);
        }
        start = end;
    }
    out
}

/// The worst aspect ratio in a row of rectangles laid along a side of length `side`.
fn worst(largest: f32, smallest: f32, sum: f32, side: f32) -> f32 {
    if sum <= 0.0 || side <= 0.0 || smallest <= 0.0 {
        return f32::INFINITY;
    }
    let (sum2, side2) = (sum * sum, side * side);
    (side2 * largest / sum2).max(sum2 / (side2 * smallest))
}

/// Maps a layout rectangle onto the chart's current bounds, which can differ from the layout's
/// size for a frame while the window is resized.
pub fn to_screen(layout: &Layout, bounds: Bounds<Pixels>, rect: Rect) -> Bounds<Pixels> {
    let sx = f32::from(bounds.size.width) / layout.width.max(1.0);
    let sy = f32::from(bounds.size.height) / layout.height.max(1.0);
    Bounds {
        origin: point(
            bounds.origin.x + px(rect.x * sx),
            bounds.origin.y + px(rect.y * sy),
        ),
        size: size(px(rect.w * sx), px(rect.h * sy)),
    }
}

/// The innermost tile under `position`.
pub fn hit_test(layout: &Layout, bounds: Bounds<Pixels>, position: Point<Pixels>) -> Option<usize> {
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let x = f32::from(position.x - bounds.origin.x) * layout.width / width;
    let y = f32::from(position.y - bounds.origin.y) * layout.height / height;
    layout
        .tiles
        .iter()
        .rposition(|tile| tile.rect.contains(x, y))
}

pub struct Paint {
    pub colors: Vec<Hsla>,
    /// Tile to outline, and the outline color.
    pub outline: Option<(usize, Hsla)>,
}

pub fn paint(window: &mut Window, bounds: Bounds<Pixels>, layout: &Layout, style: &Paint) {
    let gap = px(GAP / 2.0);
    for (tile, color) in layout.tiles.iter().zip(&style.colors) {
        let mut rect = to_screen(layout, bounds, tile.rect);
        if rect.size.width > px(2.0 * GAP) && rect.size.height > px(2.0 * GAP) {
            rect.origin.x += gap;
            rect.origin.y += gap;
            rect.size.width -= gap * 2.0;
            rect.size.height -= gap * 2.0;
        }
        if rect.size.width >= px(0.5) && rect.size.height >= px(0.5) {
            window.paint_quad(fill(rect, *color));
        }
    }
    if let Some((index, color)) = style.outline
        && let Some(tile) = layout.tiles.get(index)
    {
        window.paint_quad(quad(
            to_screen(layout, bounds, tile.rect),
            px(0.0),
            gpui::transparent_black(),
            px(2.0),
            color,
            BorderStyle::Solid,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlaps(a: &Rect, b: &Rect) -> bool {
        const EPSILON: f32 = 1e-3;
        a.x + EPSILON < b.x + b.w
            && b.x + EPSILON < a.x + a.w
            && a.y + EPSILON < b.y + b.h
            && b.y + EPSILON < a.y + a.h
    }

    #[test]
    fn squarify_keeps_areas_and_fills_the_rectangle_without_overlap() {
        let rect = Rect {
            x: 10.0,
            y: 20.0,
            w: 600.0,
            h: 400.0,
        };
        let sizes = [6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0];
        let total: f32 = sizes.iter().sum();
        let areas: Vec<f32> = sizes.iter().map(|s| s / total * rect.area()).collect();
        let out = squarify(&areas, rect);
        assert_eq!(out.len(), areas.len());
        for (r, area) in out.iter().zip(&areas) {
            assert!(
                (r.area() - area).abs() / area < 1e-3,
                "{r:?} should have area {area}"
            );
            assert!(r.x >= rect.x - 1e-3 && r.x + r.w <= rect.x + rect.w + 1e-2);
            assert!(r.y >= rect.y - 1e-3 && r.y + r.h <= rect.y + rect.h + 1e-2);
        }
        for (i, a) in out.iter().enumerate() {
            for b in &out[i + 1..] {
                assert!(!overlaps(a, b), "{a:?} overlaps {b:?}");
            }
        }
        let covered: f32 = out.iter().map(Rect::area).sum();
        assert!((covered - rect.area()).abs() / rect.area() < 1e-3);
        // Squarified rectangles stay reasonably close to square.
        for r in &out {
            assert!(r.w.max(r.h) / r.w.min(r.h) < 4.0, "{r:?} is too thin");
        }
    }

    #[test]
    fn layout_nests_contents_inside_folders_and_merges_tiny_items() {
        let root = tempfile::tempdir().unwrap();
        let movies = root.path().join("Movies");
        let tiny = root.path().join("tiny");
        let app = root.path().join("Tool.app");
        std::fs::create_dir(&movies).unwrap();
        std::fs::create_dir(&tiny).unwrap();
        std::fs::create_dir(&app).unwrap();
        std::fs::write(movies.join("trip.mov"), vec![1u8; 3_000_000]).unwrap();
        std::fs::write(movies.join("notes.txt"), vec![2u8; 600_000]).unwrap();
        std::fs::write(app.join("Tool"), vec![3u8; 800_000]).unwrap();
        for index in 0..300 {
            std::fs::write(tiny.join(format!("{index}.txt")), vec![4u8; 4_000]).unwrap();
        }
        let tree = scanner::scan(scanner::ScanOptions::new(root.path())).unwrap();
        let rows = model::rows(&tree, tree.root(), None, true);
        let layout = layout(&tree, tree.root(), &rows, 600.0, 400.0);

        let find = |name: &str| {
            layout
                .tiles
                .iter()
                .position(|tile| matches!(tile.item, Item::Node(id) if tree.name(id) == name))
                .unwrap_or_else(|| panic!("{name} has a tile"))
        };
        let movies_tile = layout.tiles[find("Movies")];
        assert_eq!(movies_tile.kind, Kind::Folder);
        assert_eq!(movies_tile.depth, 1);
        let trip = layout.tiles[find("trip.mov")];
        assert_eq!(trip.kind, Kind::File(FileType::Video));
        assert_eq!(trip.depth, 2);
        let inside = |inner: Rect, outer: Rect| {
            inner.x >= outer.x - 1e-3
                && inner.y >= outer.y - 1e-3
                && inner.x + inner.w <= outer.x + outer.w + 1e-2
                && inner.y + inner.h <= outer.y + outer.h + 1e-2
        };
        assert!(inside(trip.rect, movies_tile.rect));
        assert!(trip.rect.area() > layout.tiles[find("notes.txt")].rect.area());
        assert_eq!(
            layout.tiles[find("Tool")].kind,
            Kind::File(FileType::App),
            "files in an app bundle count as the app"
        );

        let top: Vec<&Tile> = layout.tiles.iter().filter(|tile| tile.depth == 1).collect();
        let covered: f32 = top.iter().map(|tile| tile.rect.area()).sum();
        assert!(
            (covered - 600.0 * 400.0).abs() < 1.0,
            "top level fills the chart"
        );

        // In a small chart each 4 KB file would get a few square pixels, so they are merged.
        let small = super::layout(&tree, tree.root(), &rows, 120.0, 80.0);
        let tiny_folder = tree
            .children(tree.root())
            .find(|&child| tree.name(child) == "tiny")
            .unwrap();
        let tiny_files = small
            .tiles
            .iter()
            .filter(
                |tile| matches!(tile.item, Item::Node(id) if tree.parent(id) == Some(tiny_folder)),
            )
            .count();
        assert_eq!(tiny_files, 0, "small files are merged");
        assert!(
            small
                .tiles
                .iter()
                .any(|tile| tile.item == Item::Smaller(tiny_folder)),
        );
        assert!(
            small
                .tiles
                .iter()
                .all(|tile| tile.rect.area() >= MIN_AREA * 0.99 || tile.depth > 1),
            "top-level tiles are never smaller than the merge threshold"
        );
    }

    /// `cargo test -p app --release -- --ignored --nocapture treemap_layout_time`
    #[test]
    #[ignore = "scans the home folder"]
    fn treemap_layout_time_on_the_home_folder() {
        let home = std::env::var_os("HOME").unwrap();
        let tree = scanner::scan(scanner::ScanOptions::new(&home)).unwrap();
        let mut folder = tree.root();
        for depth in 0..4 {
            let rows = model::rows(&tree, folder, None, true);
            let started = std::time::Instant::now();
            let layout = layout(&tree, folder, &rows, 900.0, 640.0);
            let elapsed = started.elapsed();
            println!(
                "depth {depth}: {} rectangles, {:.2} ms",
                layout.tiles.len(),
                elapsed.as_secs_f64() * 1000.0
            );
            assert!(elapsed.as_millis() < 16, "layout must fit in a frame");
            let Some(next) = rows.iter().find_map(|row| match row.item {
                Item::Node(id) if model::is_folder(&tree, row.item) => Some(id),
                _ => None,
            }) else {
                break;
            };
            folder = next;
        }
    }

    #[test]
    fn hit_test_finds_the_innermost_tile_and_scales_with_the_bounds() {
        let tile = |item, rect, depth| Tile {
            item,
            rect,
            depth,
            kind: Kind::Folder,
            size: 1,
            settled: true,
            header: false,
        };
        let layout = Layout {
            width: 100.0,
            height: 100.0,
            tiles: vec![
                tile(
                    Item::Node(1),
                    Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 100.0,
                        h: 50.0,
                    },
                    1,
                ),
                tile(
                    Item::Node(2),
                    Rect {
                        x: 10.0,
                        y: 10.0,
                        w: 20.0,
                        h: 20.0,
                    },
                    2,
                ),
                tile(
                    Item::Node(3),
                    Rect {
                        x: 0.0,
                        y: 50.0,
                        w: 100.0,
                        h: 50.0,
                    },
                    1,
                ),
            ],
        };
        let bounds = Bounds {
            origin: point(px(100.0), px(100.0)),
            size: size(px(200.0), px(200.0)),
        };
        let at = |x: f32, y: f32| hit_test(&layout, bounds, point(px(x), px(y)));
        assert_eq!(at(140.0, 140.0), Some(1), "inside the nested tile");
        assert_eq!(at(280.0, 120.0), Some(0));
        assert_eq!(at(150.0, 250.0), Some(2));
        assert_eq!(at(50.0, 50.0), None);
    }
}
