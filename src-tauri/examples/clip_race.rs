//! Standalone stress harness for the Windows concurrent-clipboard race.
//!
//! Hypothesis under test: on Windows, ClusterCut writes plain text through a
//! single serialized worker thread (`WorkerCommand::SetText`), but writes
//! *rich* (CF_HTML) content on a freshly-spawned `std::thread` per inbound
//! payload (`set_clipboard_rich_with_ignore` -> `rich::write_clipboard_rich`
//! -> `windows::write_all`). Two threads thus open / empty / set the Win32
//! clipboard with no in-process serialization, which corrupts the heap
//! (faults seen in ntdll: 0xc0000374 heap-corruption, 0xc0000005 AV).
//!
//! CONFIRMED 2026-06-07: the default mode below aborts with 0xc0000374 in ~1s.
//!
//! This harness has no network, no monitor polling, and no sender backpressure
//! to serialize the two writers — so it contends the clipboard far harder than
//! the real app.
//!
//! Two modes:
//!   * default (racy): worker + rich writers each open the clipboard on their
//!     own thread. Reproduces the crash.
//!   * SERIALIZE=1: every clipboard toucher routes its op through ONE executor
//!     thread (mirrors the proposed fix — rich/passthrough writes go through
//!     the same single worker as text/image). Should print "Survived ...".
//!
//! Build & run ON WINDOWS (from src-tauri/):
//!     cargo run --example clip_race --release              # racy -> crash
//!     SERIALIZE=1 cargo run --example clip_race --release  # fixed -> survives
//!
//! Env knobs:
//!     LINES         big-text line count (default 300000 ~ 31 MB)
//!     SECS          seconds to hammer before declaring survival (default 30)
//!     RICH_THREADS  concurrent rich writers per round (default 4)
//!     ARBOARD       set to 1 to also run an `arboard::get_image` contender
//!     SERIALIZE     set to 1 to route all clipboard ops through one thread
//!
//! Exit: a clean "Survived ..." line means no crash in the window. A hard
//! process abort / access violation means the race fired.

#[cfg(not(windows))]
fn main() {
    eprintln!("clip_race is Windows-only; nothing to do on this platform.");
}

#[cfg(windows)]
use std::sync::mpsc;

/// Clipboard operations the serialized executor can perform. Each carries a
/// reply channel so the requesting thread blocks until the op completes —
/// exactly the request/response shape `write_text` uses with the real worker.
#[cfg(windows)]
enum ClipCmd {
    SetBigText(std::sync::Arc<String>, mpsc::Sender<Result<(), String>>),
    ReadUnicode(mpsc::Sender<Result<(), String>>),
    WriteRich(String, mpsc::Sender<Result<(), String>>),
    ReadImage(mpsc::Sender<Result<(), String>>),
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
    let serialize = std::env::var("SERIALIZE").ok().as_deref() == Some("1");

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
        "clip_race: big text = {} bytes | hammering {}s | rich_threads/round = {} | arboard = {} | serialize = {}",
        big.len(),
        secs,
        rich_threads,
        use_arboard,
        serialize
    );
    if serialize {
        println!("SERIALIZE mode: all clipboard ops routed through one thread (mirrors the fix). Expect \"Survived\".\n");
    } else {
        println!("Racy mode: if this aborts (0xc0000374 / 0xc0000005), the race reproduced.\n");
    }

    let html_fragment = "<p><b>clip_race fragment</b></p><p>\u{03a9} \u{0416} \u{1f600}</p>";

    let stop = Arc::new(AtomicBool::new(false));
    let worker_iters = Arc::new(AtomicU64::new(0));
    let rich_iters = Arc::new(AtomicU64::new(0));

    // In SERIALIZE mode, one executor thread owns ALL clipboard access (and the
    // arboard handle). Every other thread sends it commands and blocks on the
    // reply — so no two clipboard ops ever overlap. This is the fix, in miniature.
    let (clip_tx, executor): (Option<mpsc::Sender<ClipCmd>>, Option<thread::JoinHandle<()>>) =
        if serialize {
            let (tx, rx) = mpsc::channel::<ClipCmd>();
            let h = thread::spawn(move || {
                let mut arb = if use_arboard {
                    arboard::Clipboard::new().ok()
                } else {
                    None
                };
                while let Ok(cmd) = rx.recv() {
                    match cmd {
                        ClipCmd::SetBigText(t, resp) => {
                            let _ = resp.send(worker_set_text(&t, ATTEMPTS));
                        }
                        ClipCmd::ReadUnicode(resp) => {
                            let _ = resp.send(worker_read_unicode(ATTEMPTS));
                        }
                        ClipCmd::WriteRich(h, resp) => {
                            let _ = resp.send(rich_write_all(&h, ATTEMPTS));
                        }
                        ClipCmd::ReadImage(resp) => {
                            if let Some(a) = arb.as_mut() {
                                let _ = a.get_image();
                            }
                            let _ = resp.send(Ok(()));
                        }
                    }
                }
            });
            (Some(tx), Some(h))
        } else {
            (None, None)
        };

    // Persistent "worker": empty + 31 MB CF_UNICODETEXT write, then a 31 MB
    // read-back, in a tight loop. Mirrors set_text_clearing_clipboard + the
    // monitor's read-back of the text it just applied.
    let worker = {
        let big = big.clone();
        let stop = stop.clone();
        let worker_iters = worker_iters.clone();
        let clip_tx = clip_tx.clone();
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if let Some(tx) = &clip_tx {
                    let _ = call(tx, |r| ClipCmd::SetBigText(big.clone(), r));
                    let _ = call(tx, ClipCmd::ReadUnicode);
                } else {
                    if let Err(e) = worker_set_text(&big, ATTEMPTS) {
                        eprintln!("[worker] set_text: {}", e);
                    }
                    let _ = worker_read_unicode(ATTEMPTS);
                }
                worker_iters.fetch_add(1, Ordering::Relaxed);
            }
        })
    };

    // Rich writers: a fresh batch of std::threads each round, exactly like the
    // app spawns one std::thread per inbound rich payload.
    let rich_spawner = {
        let stop = stop.clone();
        let rich_iters = rich_iters.clone();
        let frag = html_fragment.to_string();
        let clip_tx = clip_tx.clone();
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let mut handles = Vec::with_capacity(rich_threads);
                for _ in 0..rich_threads {
                    let frag = frag.clone();
                    let rich_iters = rich_iters.clone();
                    let clip_tx = clip_tx.clone();
                    handles.push(thread::spawn(move || {
                        if let Some(tx) = &clip_tx {
                            let _ = call(tx, |r| ClipCmd::WriteRich(frag.clone(), r));
                        } else if let Err(e) = rich_write_all(&frag, ATTEMPTS) {
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

    // Optional arboard image-probe loop. In racy mode it opens the clipboard on
    // its own thread (a third contender); in SERIALIZE mode it routes through
    // the executor like everything else.
    let arb_thread = if use_arboard {
        let stop = stop.clone();
        let clip_tx = clip_tx.clone();
        Some(thread::spawn(move || {
            if let Some(tx) = &clip_tx {
                while !stop.load(Ordering::Relaxed) {
                    let _ = call(tx, ClipCmd::ReadImage);
                }
            } else {
                match arboard::Clipboard::new() {
                    Ok(mut clip) => {
                        while !stop.load(Ordering::Relaxed) {
                            let _ = clip.get_image();
                        }
                    }
                    Err(e) => eprintln!("[arboard] init: {}", e),
                }
            }
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
    if let Some(a) = arb_thread {
        let _ = a.join();
    }
    // Drop the command sender so the executor's recv() loop ends, then join it.
    drop(clip_tx);
    if let Some(h) = executor {
        let _ = h.join();
    }
    println!(
        "\nSurvived {}s with no crash. worker_iters={} rich_iters={}",
        secs,
        worker_iters.load(Ordering::Relaxed),
        rich_iters.load(Ordering::Relaxed)
    );
}

/// Send a command to the executor and block until it replies — the harness
/// analogue of `write_text`'s channel round-trip with the real worker.
#[cfg(windows)]
fn call<F>(tx: &mpsc::Sender<ClipCmd>, make: F) -> Result<(), String>
where
    F: FnOnce(mpsc::Sender<Result<(), String>>) -> ClipCmd,
{
    let (rtx, rrx) = mpsc::channel();
    tx.send(make(rtx)).map_err(|_| "executor gone".to_string())?;
    rrx.recv().map_err(|_| "executor dropped reply".to_string())?
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
