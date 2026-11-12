//! Scan the home folder from the terminal: `cargo run --release -p storage-core --example scan [-- --json]`.
//! Read-only; nothing is deleted.

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;
use storage_core::{Scanner, sys};

fn gb(n: u64) -> String {
    format!("{:.1} GB", n as f64 / 1e9)
}

fn main() {
    let json = std::env::args().any(|a| a == "--json");
    let scanner = Arc::new(Scanner::new(sys::home()).expect("can't read home folder"));
    let s = scanner.clone();
    let worker = std::thread::spawn(move || s.run());
    while !worker.is_finished() {
        let p = scanner.progress();
        eprint!("\r\x1b[K  {} files, {} · {}", p.files, gb(p.bytes), p.path.chars().take(60).collect::<String>());
        let _ = std::io::stderr().flush();
        std::thread::sleep(Duration::from_millis(200));
    }
    let report = worker.join().expect("scan panicked");
    eprintln!("\r\x1b[K");
    if json {
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
        return;
    }
    println!("Home folder: {} in {} files ({} s)", gb(report.scanned), report.files, report.seconds);
    for (cat, bytes) in &report.cats {
        println!("  {:<12} {:>10}", format!("{cat:?}"), gb(*bytes));
    }
    println!("\nWhere it is:");
    for n in &report.tree.children {
        println!("  {:<28} {:>10}", n.name, gb(n.size));
    }
    println!("\nLargest cleanup candidates:");
    for e in report.entries.iter().take(25) {
        let extra = match &e.worktree {
            Some(w) => format!(
                "  [{} {} dirty={} merged={} orphaned={}]",
                w.agent.unwrap_or("git"),
                w.branch.as_deref().unwrap_or("(detached)"),
                w.dirty,
                w.merged,
                w.orphaned
            ),
            None => String::new(),
        };
        let path = if e.path.starts_with('/') { e.path.clone() } else { format!("~/{}", e.path) };
        println!("  {:>9}  {:<12} {}{}", gb(e.size), e.rec, path, extra);
    }
    if !report.unreadable.is_empty() {
        println!("\n{} folders couldn't be read.", report.unreadable.len());
    }
}
