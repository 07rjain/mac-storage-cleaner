use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use cleanup::{Category, Places, RunningApps, Suggestion};
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

fn suggestions(home: &Path, running: &RunningApps) -> Vec<Suggestion> {
    let tree = scanner::scan(ScanOptions::new(home)).unwrap();
    let places = Places {
        home: home.to_path_buf(),
    };
    cleanup::suggest(&tree, &places, running, SystemTime::now())
}

fn names(suggestions: &[Suggestion], category: Category) -> Vec<String> {
    suggestions
        .iter()
        .find(|suggestion| suggestion.category == category)
        .map(|suggestion| {
            suggestion
                .items
                .iter()
                .map(|item| {
                    item.path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn suggests_only_what_the_rules_recognize() {
    let home = sample_home();
    let running = RunningApps::from_parts(
        ["com.example.player".to_string()],
        ["Player".to_string()],
        Vec::new(),
    );

    let found = suggestions(home.path(), &running);

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

    let found = suggestions(home.path(), &running);

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

    let found = suggestions(home.path(), &RunningApps::default());

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
