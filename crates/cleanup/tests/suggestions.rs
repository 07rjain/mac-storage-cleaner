use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use cleanup::{Category, Inventory, Places, RunningApps, Suggestion};
use scanner::ScanOptions;

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

fn write(path: &Path, length: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![7u8; length]).unwrap();
}

fn age(path: &Path, days: u64) {
    let time = SystemTime::now() - DAY * days as u32;
    fs::File::open(path).unwrap().set_modified(time).unwrap();
}

/// A home folder with one item for most rules, plus decoys that must not be suggested.
fn sample_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let path = |relative: &str| home.path().join(relative);

    write(&path("Downloads/Xcode_15.xip"), 50_000);
    age(&path("Downloads/Xcode_15.xip"), 200);
    write(&path("Downloads/new.dmg"), 50_000);
    write(&path("Downloads/notes.txt"), 50_000);
    age(&path("Downloads/notes.txt"), 200);

    write(
        &path("Library/Developer/Xcode/DerivedData/App-abc/Build/x.o"),
        80_000,
    );
    write(
        &path("Library/Developer/Xcode/iOS DeviceSupport/16.4 (20E247)/Symbols/a"),
        30_000,
    );
    write(
        &path("Library/Developer/Xcode/iOS DeviceSupport/17.2 (21C62)/Symbols/a"),
        30_000,
    );

    write(
        &path("Library/Caches/com.example.editor/cache.db"),
        2_000_000,
    );
    write(
        &path("Library/Caches/com.example.player/cache.db"),
        2_000_000,
    );
    write(&path("Library/Caches/com.apple.Safari/cache.db"), 2_000_000);
    write(&path("Library/Logs/Example/old.log"), 10_000);
    write(&path(".npm/_cacache/index"), 10_000);

    write(&path("code/site/package.json"), 10);
    write(
        &path("code/site/node_modules/left-pad/index.js"),
        11_000_000,
    );
    age(&path("code/site/node_modules/left-pad"), 30);
    age(&path("code/site/node_modules"), 30);
    write(&path("code/fresh/package.json"), 10);
    write(&path("code/fresh/node_modules/x/index.js"), 11_000_000);
    write(&path("code/plain/node_modules/x/index.js"), 11_000_000);
    home
}

fn suggestions(home: &Path, running: &RunningApps, inventory: &Inventory) -> Vec<Suggestion> {
    let tree = scanner::scan(ScanOptions::new(home)).unwrap();
    let places = Places {
        home: home.to_path_buf(),
    };
    cleanup::suggest(&tree, &places, running, SystemTime::now(), inventory)
}

fn names(suggestions: &[Suggestion], category: Category) -> Vec<String> {
    suggestions
        .iter()
        .filter(|suggestion| suggestion.category == category)
        .flat_map(|suggestion| {
            suggestion.items.iter().map(|item| {
                item.path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .collect()
}

#[test]
fn suggests_only_what_the_rules_recognize() {
    let home = sample_home();
    let running = RunningApps::from_parts(
        ["com.example.player".to_string()],
        ["Player".to_string()],
        Vec::new(),
    );

    let found = suggestions(
        home.path(),
        &running,
        &Inventory::known(["com.example.editor", "com.example.player"]),
    );

    assert_eq!(names(&found, Category::OldInstallers), ["Xcode_15.xip"]);
    assert_eq!(names(&found, Category::XcodeDerivedData), ["App-abc"]);
    assert_eq!(
        names(&found, Category::XcodeDeviceSupport),
        ["16.4 (20E247)"]
    );
    assert_eq!(names(&found, Category::AppCaches), ["com.example.editor"]);
    assert_eq!(names(&found, Category::Logs), ["Example"]);
    assert_eq!(names(&found, Category::PackageCaches), ["_cacache"]);
    assert_eq!(
        names(&found, Category::BuildFolders),
        ["node_modules"],
        "recent folders and folders outside a project are left out"
    );
    let build = found
        .iter()
        .find(|suggestion| suggestion.category == Category::BuildFolders)
        .unwrap();
    assert!(build.items[0].path.ends_with("code/site/node_modules"));

    let caches = found
        .iter()
        .find(|suggestion| suggestion.category == Category::AppCaches)
        .unwrap();
    assert_eq!(caches.skipped, ["Skipped 1 cache of a running app"]);
    assert!(
        found
            .iter()
            .find(|suggestion| suggestion.category == Category::OldInstallers)
            .unwrap()
            .preselected()
    );
}

#[test]
fn leaves_tools_alone_while_they_run() {
    let home = sample_home();
    let running = RunningApps::from_parts(Vec::new(), ["Xcode".to_string()], ["npm".to_string()]);

    let found = suggestions(
        home.path(),
        &running,
        &Inventory::known(Vec::<String>::new()),
    );

    assert!(names(&found, Category::XcodeDerivedData).is_empty());
    assert!(names(&found, Category::PackageCaches).is_empty());
    let derived = found
        .iter()
        .find(|suggestion| suggestion.category == Category::XcodeDerivedData)
        .unwrap();
    assert_eq!(derived.skipped, ["Skipped while Xcode is running"]);
}

#[test]
fn suggested_items_never_overlap_and_all_pass_the_safety_rules() {
    let home = sample_home();
    let places = Places {
        home: home.path().to_path_buf(),
    };

    let found = suggestions(
        home.path(),
        &RunningApps::default(),
        &Inventory::known(Vec::<String>::new()),
    );

    let paths: Vec<_> = found
        .iter()
        .flat_map(|suggestion| suggestion.items.iter().map(|item| item.path.clone()))
        .collect();
    for (index, path) in paths.iter().enumerate() {
        assert!(
            cleanup::safety::check_path(path, &places).is_ok(),
            "{}",
            path.display()
        );
        for other in &paths[index + 1..] {
            assert!(!path.starts_with(other) && !other.starts_with(path));
        }
    }
}

#[test]
fn build_folders_are_one_card_per_project() {
    let home = tempfile::tempdir().unwrap();
    let path = |relative: &str| home.path().join(relative);
    let project = |relative: &str| {
        write(&path(&format!("{relative}/package.json")), 20);
        write(
            &path(&format!("{relative}/node_modules/pkg/index.js")),
            11_000_000,
        );
        age(&path(&format!("{relative}/node_modules")), 30);
        age(&path(&format!("{relative}/node_modules/pkg")), 30);
    };
    project("code/site");
    write(&path("code/site/dist/app.js"), 11_000_000);
    age(&path("code/site/dist"), 30);
    age(&path("code/site/dist/app.js"), 30);
    project("work/a/widget");
    project("work/b/widget");
    write(&path("rust/plain/Cargo.toml"), 20);
    write(&path("rust/plain/target/debug/app"), 11_000_000);
    age(&path("rust/plain/target"), 30);
    write(&path("rust/ready/Cargo.toml"), 20);
    write(&path("rust/ready/target/CACHEDIR.TAG"), 10);
    write(&path("rust/ready/target/debug/app"), 11_000_000);
    age(&path("rust/ready/target"), 30);
    age(&path("rust/ready/target/debug"), 30);
    age(&path("rust/ready/target/CACHEDIR.TAG"), 30);
    write(&path("code/site/.next/server/app.js"), 11_000_000);
    age(&path("code/site/.next"), 30);
    age(&path("code/site/.next/server"), 30);
    write(&path("web/turbo-app/package.json"), 20);
    write(&path("web/turbo-app/.turbo/cache/x"), 11_000_000);
    age(&path("web/turbo-app/.turbo"), 30);
    age(&path("web/turbo-app/.turbo/cache"), 30);
    write(&path("ios/pods-app/Podfile"), 20);
    write(&path("ios/pods-app/Pods/Alamofire/x"), 11_000_000);
    age(&path("ios/pods-app/Pods"), 30);
    age(&path("ios/pods-app/Pods/Alamofire"), 30);
    write(&path("py/venv-app/pyproject.toml"), 20);
    write(&path("py/venv-app/.venv/lib/x"), 11_000_000);
    age(&path("py/venv-app/.venv"), 30);
    age(&path("py/venv-app/.venv/lib"), 30);
    write(&path("py/reqs-app/requirements.txt"), 20);
    write(&path("py/reqs-app/.venv/lib/x"), 11_000_000);
    age(&path("py/reqs-app/.venv"), 30);
    age(&path("py/reqs-app/.venv/lib"), 30);
    write(&path("loose/.next/x"), 11_000_000);
    age(&path("loose/.next"), 30);
    write(&path("loose/.venv/lib/x"), 11_000_000);
    age(&path("loose/.venv"), 30);
    age(&path("loose/.venv/lib"), 30);

    let found = suggestions(
        home.path(),
        &RunningApps::default(),
        &Inventory::known(Vec::<String>::new()),
    );
    let cards: Vec<_> = found
        .iter()
        .filter(|suggestion| suggestion.category == Category::BuildFolders)
        .collect();
    let site = cards
        .iter()
        .find(|card| card.title() == "site")
        .expect("site card");
    let mut site_names: Vec<_> = site
        .items
        .iter()
        .map(|item| {
            item.path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    site_names.sort();
    assert_eq!(site_names, [".next", "dist", "node_modules"]);
    assert!(site.reason().contains("next build recreates them"));
    assert!(
        site.items[0]
            .path
            .starts_with(home.path().join("code/site"))
    );
    assert!(cards.iter().any(|card| card.title() == "widget · a"));
    assert!(cards.iter().any(|card| card.title() == "widget · b"));
    assert!(cards.iter().all(|card| {
        !card
            .items
            .iter()
            .any(|item| item.path.ends_with("rust/plain/target"))
    }));
    assert!(cards.iter().any(|card| card.title() == "ready"));
    for title in ["turbo-app", "pods-app", "venv-app", "reqs-app"] {
        assert!(
            cards.iter().any(|card| card.title() == title),
            "missing {title}"
        );
    }
    assert!(cards.iter().all(|card| {
        !card
            .items
            .iter()
            .any(|item| item.path.ends_with("loose/.next") || item.path.ends_with("loose/.venv"))
    }));
}

#[test]
fn old_screenshots_in_the_screenshot_folder_are_review_only() {
    let home = tempfile::tempdir().unwrap();
    let path = |relative: &str| home.path().join(relative);
    let old = "Screenshot 2020-01-01 at 1.00.00 AM.png";
    write(&path(&format!("Desktop/{old}")), 250_000);
    age(&path(&format!("Desktop/{old}")), 30);
    write(
        &path("Desktop/Screenshot 2026-10-06 at 1.00.00 AM.png"),
        250_000,
    );
    write(
        &path("Desktop/Screenshot 2020-02-01 at 1.00.00 AM.png"),
        1_000,
    );
    age(&path("Desktop/Screenshot 2020-02-01 at 1.00.00 AM.png"), 30);
    write(&path("Desktop/notes.png"), 250_000);
    age(&path("Desktop/notes.png"), 30);
    write(
        &path(&format!("Pictures/Photos Library.photoslibrary/{old}")),
        250_000,
    );
    age(
        &path(&format!("Pictures/Photos Library.photoslibrary/{old}")),
        30,
    );

    let found = suggestions(
        home.path(),
        &RunningApps::default(),
        &Inventory::known(Vec::<String>::new()),
    );
    let cards: Vec<_> = found
        .iter()
        .filter(|suggestion| suggestion.category == Category::Screenshots)
        .collect();
    assert_eq!(cards.len(), 1);
    assert!(!cards[0].preselected());
    assert_eq!(cards[0].items.len(), 1);
    assert!(cards[0].items[0].path.ends_with(format!("Desktop/{old}")));
}

#[test]
fn screenshots_use_the_folder_from_screen_capture_settings() {
    let home = tempfile::tempdir().unwrap();
    let shots = home.path().join("Pictures/Shots");
    let plist = home
        .path()
        .join("Library/Preferences/com.apple.screencapture.plist");
    fs::create_dir_all(plist.parent().unwrap()).unwrap();
    fs::write(
        &plist,
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>location</key><string>{}</string></dict></plist>"#,
            shots.display()
        ),
    )
    .unwrap();
    let name = "Screen Shot 2020-01-01 at 1.00.00 AM.jpg";
    write(&shots.join(name), 250_000);
    age(&shots.join(name), 40);
    write(&home.path().join(format!("Desktop/{name}")), 250_000);
    age(&home.path().join(format!("Desktop/{name}")), 40);

    let found = suggestions(
        home.path(),
        &RunningApps::default(),
        &Inventory::known(Vec::<String>::new()),
    );
    let cards: Vec<_> = found
        .iter()
        .filter(|suggestion| suggestion.category == Category::Screenshots)
        .collect();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].items.len(), 1);
    assert!(cards[0].items[0].path.starts_with(&shots));
}
