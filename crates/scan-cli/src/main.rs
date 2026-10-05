//! Command-line harness for benchmarking and checking the scanner without the UI.

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use scanner::{NodeFlags, NodeId, ScanOptions, Tree};
use volumes::{Accounting, DATA_VOLUME_MOUNT_POINT, StartupDisk};

const USAGE: &str = "\
Usage: msc-scan [PATH] [--threads N] [--top N] [--du]

Scans PATH, or the startup disk's Data volume when PATH is omitted.

  --threads N   worker threads (default: CPU cores)
  --top N       largest children of the root to list (default: 10)
  --du          also run `du -sk PATH` and compare totals
  --list-dataless N
                print up to N paths flagged as dataless";

struct Args {
    root: Option<PathBuf>,
    threads: Option<NonZeroUsize>,
    top: usize,
    compare_du: bool,
    list_dataless: usize,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        root: None,
        threads: None,
        top: 10,
        compare_du: false,
        list_dataless: 0,
    };
    let mut raw = std::env::args().skip(1);
    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "--threads" => {
                let value = raw.next().ok_or("--threads needs a value")?;
                args.threads = Some(value.parse().map_err(|_| "invalid --threads value")?);
            }
            "--top" => {
                let value = raw.next().ok_or("--top needs a value")?;
                args.top = value.parse().map_err(|_| "invalid --top value")?;
            }
            "--du" => args.compare_du = true,
            "--list-dataless" => {
                let value = raw.next().ok_or("--list-dataless needs a value")?;
                args.list_dataless = value.parse().map_err(|_| "invalid --list-dataless value")?;
            }
            "-h" | "--help" => return Err(String::new()),
            path if !path.starts_with('-') && args.root.is_none() => {
                args.root = Some(PathBuf::from(path));
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(args)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(error) => {
            if !error.is_empty() {
                eprintln!("{error}\n");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    let startup_disk_mode = args.root.is_none();
    let root = args
        .root
        .clone()
        .unwrap_or_else(|| PathBuf::from(DATA_VOLUME_MOUNT_POINT));

    let mut options = ScanOptions::new(&root);
    options.threads = args.threads;
    let started = Instant::now();
    let handle = match scanner::start(options) {
        Ok(handle) => handle,
        Err(error) => {
            eprintln!("cannot scan {}: {error}", root.display());
            return ExitCode::FAILURE;
        }
    };
    let mut first_progress = None;
    while !handle.is_finished() {
        std::thread::sleep(Duration::from_millis(20));
        if first_progress.is_none() && handle.progress().entries() > 0 {
            first_progress = Some(started.elapsed());
        }
    }
    let tree = handle.wait();
    let elapsed = started.elapsed();

    print_scan(&tree, elapsed, first_progress, args.top);
    if startup_disk_mode {
        print_accounting(&tree);
    }
    if args.compare_du {
        compare_with_du(&tree);
    }
    if args.list_dataless > 0 {
        list_dataless(&tree, args.list_dataless);
    }
    ExitCode::SUCCESS
}

fn print_scan(tree: &Tree, elapsed: Duration, first_progress: Option<Duration>, top: usize) {
    let stats = tree.stats();
    let root = tree.root();
    let seconds = elapsed.as_secs_f64();

    println!("Scanned {}", tree.root_path().display());
    println!(
        "  entries        {} ({} files, {} folders, {} symlinks, {} other)",
        stats.entries(),
        stats.files,
        stats.directories,
        stats.symlinks,
        stats.other
    );
    println!("  time           {seconds:.2} s");
    if let Some(first) = first_progress {
        println!("  first results  {} ms", first.as_millis());
    }
    println!(
        "  throughput     {:.0} entries/s",
        stats.entries() as f64 / seconds.max(f64::EPSILON)
    );
    println!("  peak memory    {}", format_bytes(peak_resident_bytes()));
    println!("  on disk        {}", format_bytes(tree.allocated(root)));
    println!("  logical        {}", format_bytes(tree.logical(root)));
    println!("  unreadable     {} folders", stats.unreadable_directories);
    println!(
        "  dataless       {} items (not opened)",
        stats.dataless_items
    );
    println!(
        "  other volumes  {} mount points skipped",
        stats.mount_points_skipped
    );
    println!(
        "  hard links     {} duplicates counted once",
        stats.hard_link_duplicates
    );
    println!(
        "  APFS clones    {} files, {} shared data counted once",
        stats.clones,
        format_bytes(stats.clone_shared_bytes)
    );
    println!(
        "  edited clones  {} files, {} counted in full",
        stats.edited_clones,
        format_bytes(stats.edited_clone_bytes)
    );
    println!("  entry errors   {}", stats.entry_errors);
    if !tree.is_complete() {
        println!("  (incomplete: scan was cancelled)");
    }

    println!("\nLargest items in {}:", tree.root_path().display());
    for child in tree.children_by_size(root).into_iter().take(top) {
        let mut notes = Vec::new();
        let flags = tree.flags(child);
        if flags.contains(NodeFlags::UNREADABLE) {
            notes.push("unreadable");
        }
        if flags.contains(NodeFlags::MOUNT_POINT) {
            notes.push("other volume");
        }
        if flags.contains(NodeFlags::DATALESS) {
            notes.push("dataless");
        }
        println!(
            "  {:>10}  {}{}",
            format_bytes(tree.allocated(child)),
            tree.name(child).to_string_lossy(),
            if notes.is_empty() {
                String::new()
            } else {
                format!("  ({})", notes.join(", "))
            }
        );
    }
}

fn print_accounting(tree: &Tree) {
    let disk = match StartupDisk::read() {
        Ok(disk) => disk,
        Err(error) => {
            eprintln!("cannot read startup disk: {error}");
            return;
        }
    };
    let accounting = Accounting::new(&disk, tree.allocated(tree.root()));

    println!("\nStartup disk ({})", disk.data_volume.device);
    println!("  capacity         {}", format_bytes(disk.capacity()));
    println!("  available        {}", format_bytes(disk.available()));
    println!(
        "  used             {}",
        format_bytes(accounting.container_used)
    );
    println!("    scanned        {}", format_bytes(accounting.scanned));
    println!(
        "    not scanned    {}",
        format_bytes(accounting.not_scanned)
    );
    println!(
        "    other volumes  {}",
        format_bytes(accounting.other_volumes)
    );
    if accounting.overcount > 0 {
        println!(
            "    overcount      {} (scan exceeds Data volume use)",
            format_bytes(accounting.overcount)
        );
    }
    println!("  Data volume used {}", format_bytes(disk.data_volume.used));
    match disk.purgeable {
        Some(purgeable) => println!("  purgeable        {}", format_bytes(purgeable)),
        None => println!("  purgeable        unknown"),
    }
    match disk.local_snapshots {
        Some(count) => println!("  local snapshots  {count}"),
        None => println!("  local snapshots  unknown"),
    }
    let accounted = accounting.scanned + accounting.not_scanned + accounting.other_volumes;
    println!(
        "  chart total      {} ({})",
        format_bytes(accounted),
        if accounted == accounting.container_used {
            "matches container use"
        } else {
            "differs from container use"
        }
    );
}

fn compare_with_du(tree: &Tree) {
    let output = Command::new("/usr/bin/du")
        .arg("-sk")
        .arg(tree.root_path())
        .output();
    let Ok(output) = output else {
        eprintln!("could not run du");
        return;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let Some(kibibytes) = text
        .split_whitespace()
        .next()
        .and_then(|value| value.parse::<u64>().ok())
    else {
        eprintln!("could not parse du output");
        return;
    };
    let du_bytes = kibibytes * 1024;
    let ours = tree.allocated(tree.root());
    let difference = ours as f64 - du_bytes as f64;
    println!("\nCompared with du -sk");
    println!("  du               {}", format_bytes(du_bytes));
    println!("  scanner          {}", format_bytes(ours));
    println!(
        "  difference       {:+.3}%",
        difference / (du_bytes.max(1) as f64) * 100.0
    );
}

fn list_dataless(tree: &Tree, limit: usize) {
    println!("\nDataless items:");
    let flagged = (0..tree.len() as NodeId)
        .filter(|&node| tree.flags(node).contains(NodeFlags::DATALESS))
        .take(limit);
    for node in flagged {
        println!("  {}", tree.path(node).display());
    }
}

fn peak_resident_bytes() -> u64 {
    // SAFETY: `rusage` is plain data, and `getrusage` only writes into it.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `usage` is valid for writes.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    u64::try_from(usage.ru_maxrss).unwrap_or(0)
}

/// Decimal units, matching Finder.
fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}
