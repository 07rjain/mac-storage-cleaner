use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::SystemTime;

use cleanup::{
    AddError, Basket, Category, Inventory, LeftoverProof, Places, RunningApps, STALE_REASON,
};
use scanner::ScanOptions;

fn write(path: &Path, length: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![7u8; length]).unwrap();
}

fn plist(app: &Path, bundle_id: &str) {
    let contents = app.join("Contents");
    fs::create_dir_all(&contents).unwrap();
    fs::write(
        contents.join("Info.plist"),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>{bundle_id}</string></dict></plist>"#
        ),
    )
    .unwrap();
}

fn home_with_leftovers() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let path = |relative: &str| home.path().join(relative);
    let body = 2_000_000;
    for relative in [
        "Library/Caches/com.example.gone/cache.db",
        "Library/Application Support/com.example.gone/state.db",
        "Library/Containers/com.example.gone/Data/state.db",
        "Library/Saved Application State/com.example.gone.savedState/windows.plist",
        "Library/HTTPStorages/com.example.gone/http.db",
        "Library/Caches/com.example.editor/cache.db",
        "Library/Preferences/com.example.gone.plist",
        "Library/Group Containers/group.com.example.gone/data",
        "Library/LaunchAgents/com.example.gone.plist",
        "Library/Caches/com.apple.Safari/cache.db",
        "Library/Application Support/MobileSync/Backup/device/info.plist",
        "Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw",
        "Library/Containers/com.apple.mail/Data/box",
    ] {
        write(&path(relative), body);
    }
    home
}

fn suggest(home: &Path, inventory: &Inventory) -> (Vec<cleanup::Suggestion>, Places) {
    let tree = scanner::scan(ScanOptions::new(home)).unwrap();
    let places = Places {
        home: home.to_path_buf(),
    };
    let found = cleanup::suggest(
        &tree,
        &places,
        &RunningApps::default(),
        SystemTime::now(),
        inventory,
    );
    (found, places)
}

fn leftover_names(found: &[cleanup::Suggestion]) -> Vec<String> {
    let mut names: Vec<String> = found
        .iter()
        .find(|suggestion| suggestion.category == Category::Leftovers)
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
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn missing_apps_become_one_review_only_card() {
    let home = home_with_leftovers();
    let apps = tempfile::tempdir().unwrap();
    plist(&apps.path().join("Vendor/Editor.app"), "com.example.editor");
    let inventory = Inventory::scan(&[apps.path()], &[], &RunningApps::default());
    assert!(
        inventory.contains("com.example.editor"),
        "nested apps count"
    );

    let (found, places) = suggest(home.path(), &inventory);
    let leftovers = found
        .iter()
        .find(|suggestion| suggestion.category == Category::Leftovers)
        .expect("leftover card");
    assert!(!leftovers.preselected());
    assert_eq!(
        leftover_names(&found),
        [
            "com.example.gone",
            "com.example.gone",
            "com.example.gone",
            "com.example.gone",
            "com.example.gone.savedState",
        ]
    );
    assert!(
        leftovers
            .items
            .iter()
            .all(|item| item.note.as_deref().is_some_and(|note| {
                note.contains("Not found under Applications on this Mac")
                    && !note.contains("removed")
            }))
    );
    let paths: Vec<&Path> = found
        .iter()
        .flat_map(|suggestion| suggestion.items.iter().map(|item| item.path.as_path()))
        .collect();
    for refused in [
        "Preferences",
        "Group Containers",
        "LaunchAgents",
        "MobileSync",
    ] {
        assert!(
            paths.iter().all(|path| !path
                .components()
                .any(|component| { component.as_os_str().to_string_lossy() == refused })),
            "{refused} was suggested"
        );
    }
    assert!(
        paths
            .iter()
            .all(|path| !path.ends_with("Library/Containers/com.docker.docker"))
    );
    assert!(
        leftovers
            .items
            .iter()
            .all(|item| !item.path.ends_with("Library/Caches/com.example.editor"))
    );
    assert!(found.iter().any(|suggestion| {
        suggestion.category == Category::AppCaches
            && suggestion
                .items
                .iter()
                .any(|item| item.path.ends_with("com.example.editor"))
    }));
    let container = leftovers
        .items
        .iter()
        .find(|item| item.path.ends_with("Library/Containers/com.example.gone"))
        .unwrap();
    assert_eq!(
        cleanup::safety::check_path(&container.path, &places),
        Err(cleanup::Refusal::ManagedByApp)
    );
    assert!(container.proof.is_some());
}

#[test]
fn an_unknown_app_or_process_hides_leftovers() {
    let home = home_with_leftovers();
    let apps = tempfile::tempdir().unwrap();
    fs::create_dir_all(apps.path().join("Broken.app/Contents")).unwrap();
    let unknown = Inventory::scan(&[apps.path()], &[], &RunningApps::default());

    let (found, _) = suggest(home.path(), &unknown);
    let leftovers = found
        .iter()
        .find(|suggestion| suggestion.category == Category::Leftovers)
        .unwrap();
    assert!(leftovers.items.is_empty());
    assert_eq!(
        leftovers.skipped.first().map(String::as_str),
        Some("1 app could not be identified")
    );

    let tree = scanner::scan(ScanOptions::new(home.path())).unwrap();
    let places = Places {
        home: home.path().to_path_buf(),
    };
    let found = cleanup::suggest(
        &tree,
        &places,
        &RunningApps::default().with_unreadable_process(),
        SystemTime::now(),
        &Inventory::known(Vec::<String>::new()),
    );
    let leftovers = found
        .iter()
        .find(|suggestion| suggestion.category == Category::Leftovers)
        .unwrap();
    assert!(leftovers.items.is_empty());
    assert_eq!(
        leftovers.skipped.first().map(String::as_str),
        Some("A running process could not be identified")
    );
}

#[test]
fn a_container_needs_a_fresh_proof_to_be_added_or_moved() {
    let home = home_with_leftovers();
    let inventory = Inventory::known(Vec::<String>::new());
    let (found, places) = suggest(home.path(), &inventory);
    let container = found
        .iter()
        .find(|suggestion| suggestion.category == Category::Leftovers)
        .unwrap()
        .items
        .iter()
        .find(|item| {
            item.path.ends_with("com.example.gone")
                && item.proof.as_ref().is_some_and(LeftoverProof::is_container)
        })
        .unwrap()
        .clone();
    let proof = container.proof.clone().unwrap();

    let mut basket = Basket::new(places.clone(), home.path());
    assert_eq!(
        basket.add(
            &container.path,
            Some(container.node),
            Category::Leftovers,
            container.size
        ),
        Err(AddError::Refused(cleanup::Refusal::ManagedByApp))
    );
    assert!(basket.is_empty());

    let installed = Inventory::known(["com.example.gone"]);
    assert_eq!(
        basket.add_leftover(
            &container.path,
            Some(container.node),
            container.size,
            &proof,
            &installed
        ),
        Err(AddError::Stale)
    );

    basket
        .add_leftover(
            &container.path,
            Some(container.node),
            container.size,
            &proof,
            &inventory,
        )
        .unwrap();
    assert_eq!(basket.len(), 1);

    fs::remove_dir_all(&container.path).unwrap();
    fs::create_dir_all(container.path.join("Data")).unwrap();
    let outcome = cleanup::move_to_trash(
        basket.items(),
        basket.places(),
        basket.scan_root(),
        &RunningApps::default(),
        &inventory,
    );
    assert_eq!(outcome.failed.len(), 1);
    assert_eq!(outcome.failed[0].reason, STALE_REASON);
    assert!(outcome.moved.is_empty());

    let apple = home.path().join("Library/Containers/com.apple.mail");
    assert!(LeftoverProof::issue(&apple, "com.apple.mail", true, &inventory, &places).is_none());
    assert!(basket.add(&apple, None, Category::Chosen, 1).is_err());
}

#[test]
fn reinstalling_before_confirm_refuses_the_move() {
    let home = tempfile::tempdir().unwrap();
    let cache = home.path().join("Library/Caches/com.example.gone");
    write(&cache.join("cache.db"), 2_000_000);
    let inventory = Inventory::known(Vec::<String>::new());
    let (found, places) = suggest(home.path(), &inventory);
    let item = found
        .iter()
        .find(|suggestion| suggestion.category == Category::Leftovers)
        .unwrap()
        .items
        .iter()
        .find(|item| item.path == cache)
        .unwrap();
    let mut basket = Basket::new(places, home.path());
    basket
        .add_leftover(
            &item.path,
            Some(item.node),
            item.size,
            item.proof.as_ref().unwrap(),
            &inventory,
        )
        .unwrap();

    let outcome = cleanup::move_to_trash(
        basket.items(),
        basket.places(),
        basket.scan_root(),
        &RunningApps::default(),
        &Inventory::known(["com.example.gone"]),
    );
    assert!(outcome.moved.is_empty());
    assert_eq!(outcome.failed[0].reason, STALE_REASON);
    assert!(cache.exists());
}

#[test]
fn an_unreadable_applications_folder_omits_leftovers() {
    let home = home_with_leftovers();
    let locked = tempfile::tempdir().unwrap();
    let applications = locked.path().join("Applications");
    fs::create_dir(&applications).unwrap();
    let mut permissions = fs::metadata(&applications).unwrap().permissions();
    permissions.set_mode(0o0);
    fs::set_permissions(&applications, permissions).unwrap();
    let inventory = Inventory::scan(&[&applications], &[], &RunningApps::default());
    let mut permissions = fs::metadata(&applications).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&applications, permissions).unwrap();

    assert_eq!(inventory.reason(), Some("The app list could not be read"));
    let (found, _) = suggest(home.path(), &inventory);
    let leftovers = found
        .iter()
        .find(|suggestion| suggestion.category == Category::Leftovers)
        .unwrap();
    assert!(leftovers.items.is_empty());
    assert_eq!(
        leftovers.skipped.first().map(String::as_str),
        Some("The app list could not be read")
    );
}
