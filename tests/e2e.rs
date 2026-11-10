//! End-to-end / regression suite for ROG Control: a Rust port of every scenario in the Python
//! app's `tests/test_e2e.py` (commit 509d5e4), plus guards for rules the Python suite never
//! tested (marked `guards:` instead of `ports:`).
//!
//! Like the Python suite it builds the real widgets in-process and drives them through their
//! real methods and GObject signals instead of simulating clicks. GTK widgets must all live on one
//! thread, so this binary has its own tiny runner (`harness = false`) that prints libtest-style
//! lines. It also installs a fail-closed command guard: only read-only tool invocations ever reach
//! a real process, every write is swallowed and recorded, so no test can change hardware state.

#[path = "e2e/common.rs"]
mod common;
#[path = "e2e/extra.rs"]
mod extra;
#[path = "e2e/ported.rs"]
mod ported;
#[path = "e2e/startup.rs"]
mod startup;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

fn main() {
    if gtk::init().is_err() {
        eprintln!("error: GTK could not open a display; these tests build real widgets");
        std::process::exit(1);
    }
    adw::init().expect("libadwaita init");
    let _guard = common::install_guard();

    let filter: Option<String> = std::env::args().skip(1).find(|a| !a.starts_with('-'));
    let mut all: Vec<(&'static str, fn())> = Vec::new();
    all.extend(ported::tests());
    all.extend(extra::tests());
    let selected: Vec<_> = all.into_iter().filter(|(n, _)| filter.as_deref().is_none_or(|f| n.contains(f))).collect();

    println!("\nrunning {} tests", selected.len());
    let started = Instant::now();
    let (mut passed, mut failed, mut skipped) = (0, Vec::new(), Vec::new());
    for (name, test) in &selected {
        use std::io::Write;
        print!("test {name} ... ");
        let _ = std::io::stdout().flush();
        match catch_unwind(AssertUnwindSafe(test)) {
            Ok(()) => match common::take_skip() {
                Some(reason) => {
                    println!("SKIPPED ({reason})");
                    skipped.push(*name);
                }
                None => {
                    println!("ok");
                    passed += 1;
                }
            },
            Err(_) => {
                common::take_skip();
                println!("FAILED");
                failed.push(*name);
            }
        }
    }
    if !failed.is_empty() {
        println!("\nfailures:");
        for name in &failed {
            println!("    {name}");
        }
    }
    if !skipped.is_empty() {
        println!("\nwarning: {} tests skipped because a fixture is missing on this machine (they did not run):", skipped.len());
        for name in &skipped {
            println!("    {name}");
        }
    }
    let blocked = common::blocked_commands();
    println!("\n({} write commands were swallowed by the fail-closed guard)", blocked.len());
    println!(
        "\ntest result: {}. {} passed; {} failed; {} ignored; 0 measured; {} filtered out; finished in {:.2}s\n",
        if failed.is_empty() { "ok" } else { "FAILED" },
        passed,
        failed.len(),
        skipped.len(),
        0,
        started.elapsed().as_secs_f64()
    );
    std::process::exit(i32::from(!failed.is_empty()));
}
