//! Standalone stress harness for the Windows concurrent-clipboard race.
//!
//! Hypothesis under test: on Windows, ClusterCut writes plain text through a
//! single serialized worker thread (`WorkerCommand::SetText`), but writes
//! *rich* (CF_HTML) content on a freshly-spawned `std::thread` per inbound
//! payload (`set_clipboard_rich_with_ignore` -> `rich::write_clipboard_rich`
//! -> `windows::write_all`). Two threads thus open / empty / set the Win32
//! clipboard with no in-process serialization, which we believe corrupts the
//! heap (faults seen in ntdll: 0xc0000374 heap-corruption, 0xc0000005 AV).
//!
//! This harness reproduces *only that mechanism*, with no network, no monitor
//! polling, and no sender backpressure to serialize the two writers — so it
//! contends the clipboard far harder than the real app and should surface the
//! corruption fast if the hypothesis holds.
//!
//! Build & run ON WINDOWS (from src-tauri/):
//!     cargo run --example clip_race --release
//!
//! Env knobs:
//!     LINES         big-text line count (default 300000 ~ 31 MB)
//!     SECS          seconds to hammer before declaring survival (default 30)
//!     RICH_THREADS  concurrent rich writers per round (default 4)
//!     ARBOARD       set to 1 to also run an `arboard::get_image` read loop
//!                   (mirrors the worker's image probe contending the lock)
//!
//! Exit: a clean "Survived …" line means no crash in the window. A hard
//! process abort / access violation means the race reproduced — that is the
//! result we want, and it gives us a local pass/fail oracle for the fix.

#[cfg(not(windows))]
fn main() {
    eprintln!("clip_race is Windows-only; nothing to do on this platform.");
}

#[cfg(windows)]
fn main() {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    const ATTEMPTS: usize = 10;

    let lines: usize = env_usize("LINES", 300_000);
    let secs: u64 = env_usize("SECS", 30) as u64;
    let rich_threads: usize = env_usize("RICH_THREADS", 4).max(1);
    let use_arboard = std::env::var("ARBOARD").ok().as_deref() == Some("1");

    // Build ~31 MB text identical in shape to crasher.sh's payload.
    let mut big = String::with_capacity(lines * 100);
    for i in 0..lines {
        big.push_str(&format!("line {} \u{03a9} \u{0416} \u{1f600} ", i));
        for _ in 0..80 {
            big.push('x');
        }
        big.push('\n');
    }
    let big = Arc::new(big);

    println!(
        "clip_race: big text = {} bytes | hammering {}s | rich_threads/round = {} | arboard = {}",
        big.len(),
        secs,
        rich_threads,
        use_arboard
    );
    println!("If this aborts (0xc0000374 / 0xc0000005), the race reproduced.\n");

    let html_fragment = "<p><b>clip_race fragment</b></p><p>\u{03a9} \u{0416} \u{1f600}</p>";

    let stop = Arc::new(AtomicBool::new(false));
    let worker_iters = Arc::new(AtomicU64::new(0));
    let rich_iters = Arc::new(AtomicU64::new(0));

    // Persistent "worker": empty + 31 MB CF_UNICODETEXT write, then a 31 MB
    // read-back, in a tight loop. Mirrors set_text_clearing_clipboard + the
    // monitor's read-back of the text it just applied.
    let worker = {
        let big = big.clone();
        let stop = stop.clone();
        let worker_iters = worker_iters.clone();
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if let Err(e) = worker_set_text(&big, ATTEMPTS) {
                    eprintln!("[worker] set_text: {}", e);
                }
                if let Err(e) = worker_read_unicode(ATTEMPTS) {
                    // ERROR_CLIPBOARD_NOT_OPEN under contention is expected and
                    // benign; print at most so we can eyeball the rate.
                    let _ = e;
                }
                worker_iters.fetch_add(1, Ordering::Relaxed);
            }
        })
    };

    // Rich writers: a fresh batch of std::threads each round, exactly like the
    // app spawns one std::thread per inbound rich payload. Each does write_all
    // (empty + small CF_UNICODETEXT + CF_HTML) directly on its own thread.
    let rich_spawner = {
        let stop = stop.clone();
        let rich_iters = rich_iters.clone();
        let frag = html_fragment.to_string();
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let mut handles = Vec::with_capacity(rich_threads);
                for _ in 0..rich_threads {
                    let frag = frag.clone();
                    let rich_iters = rich_iters.clone();
                    handles.push(thread::spawn(move || {
                        if let Err(e) = rich_write_all(&frag, ATTEMPTS) {
                            eprintln!("[rich] write_all: {}", e);
                        }
                        rich_iters.fetch_add(1, Ordering::Relaxed);
                    }));
                }
                for h in handles {
                    let _ = h.join();
                }
            }
        })
    };

    // Optional arboard image-probe loop (a third contender, like the worker's
    // get_image on every poll).
    let arb = if use_arboard {
        let stop = stop.clone();
        Some(thread::spawn(move || match arboard::Clipboard::new() {
            Ok(mut clip) => {
                while !stop.load(Ordering::Relaxed) {
                    let _ = clip.get_image();
                }
            }
            Err(e) => eprintln!("[arboard] init: {}", e),
        }))
    } else {
        None
    };

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        thread::sleep(Duration::from_millis(1000));
        println!(
            "  alive @ {:>2}s: worker={} rich={}",
            start.elapsed().as_secs(),
            worker_iters.load(Ordering::Relaxed),
            rich_iters.load(Ordering::Relaxed)
        );
    }
    stop.store(true, Ordering::Relaxed);
    let _ = worker.join();
    let _ = rich_spawner.join();
    if let Some(a) = arb {
        let _ = a.join();
    }
    println!(
        "\nSurvived {}s with no crash. worker_iters={} rich_iters={}",
        secs,
        worker_iters.load(Ordering::Relaxed),
        rich_iters.load(Ordering::Relaxed)
    );
}

#[cfg(windows)]
fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Mirror of `plugin::set_text_clearing_clipboard`.
#[cfg(windows)]
fn worker_set_text(text: &str, attempts: usize) -> Result<(), String> {
    use clipboard_win::{formats::Unicode, raw, Clipboard, Setter};
    let _clip = Clipboard::new_attempts(attempts).map_err(|e| format!("open: {}", e))?;
    raw::empty().map_err(|e| format!("empty: {}", e))?;
    Unicode
        .write_clipboard(&text)
        .map_err(|e| format!("CF_UNICODETEXT: {}", e))?;
    Ok(())
}

/// Read CF_UNICODETEXT (format id 13) back as raw bytes — mirrors the monitor
/// reading the ~31 MB text it just applied. RawData::read_clipboard is the
/// same getter the rich/RTF read path uses, so it definitely compiles.
#[cfg(windows)]
fn worker_read_unicode(attempts: usize) -> Result<(), String> {
    use clipboard_win::{formats::RawData, Clipboard, Getter};
    const CF_UNICODETEXT: u32 = 13;
    let _clip = Clipboard::new_attempts(attempts).map_err(|e| format!("open: {}", e))?;
    let mut buf: Vec<u8> = Vec::new();
    RawData(CF_UNICODETEXT)
        .read_clipboard(&mut buf)
        .map_err(|e| format!("read CF_UNICODETEXT: {}", e))?;
    Ok(())
}

/// Mirror of `rich::windows::try_write` for the text/html case.
#[cfg(windows)]
fn rich_write_all(html: &str, attempts: usize) -> Result<(), String> {
    use clipboard_win::{
        formats::{Html, Unicode},
        raw, Clipboard, Setter,
    };
    let _clip = Clipboard::new_attempts(attempts).map_err(|e| format!("open: {}", e))?;
    raw::empty().map_err(|e| format!("empty: {}", e))?;
    let small = "clip_race plain";
    Unicode
        .write_clipboard(&small)
        .map_err(|e| format!("CF_UNICODETEXT: {}", e))?;
    let h = Html::new().ok_or_else(|| "couldn't register CF_HTML atom".to_string())?;
    h.write_clipboard(&html)
        .map_err(|e| format!("CF_HTML: {}", e))?;
    Ok(())
}
