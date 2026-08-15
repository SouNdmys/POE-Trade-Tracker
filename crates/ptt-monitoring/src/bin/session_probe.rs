//! Headless live-watch probe: run with POE2 open on the exchange panel.
//!
//! Usage: `session-probe [--seconds N]` (default 60). Prints every accepted
//! book and a final histogram. Ctrl+C ends the session early.

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use ptt_monitoring::{SessionConfig, SessionEvent, run_session};
    use ptt_recognition::profiles::poe2_zhtw::Route;
    use std::sync::atomic::AtomicBool;

    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let seconds: u64 = match arguments.as_slice() {
        [] => 60,
        [flag, value] if flag == "--seconds" => value.parse()?,
        _ => return Err("usage: session-probe [--seconds N]".into()),
    };

    static CANCEL: AtomicBool = AtomicBool::new(false);
    ctrlc_handler(&CANCEL)?;

    println!("starting watch for {seconds}s — bring POE2's exchange panel up");
    let route = Route::new().map_err(|reason| format!("route init failed: {reason:?}"))?;
    let stats = run_session(
        &route,
        &SessionConfig::default(),
        std::time::Duration::from_secs(seconds),
        &CANCEL,
        |event| match event {
            SessionEvent::Accepted { book, elapsed, .. } => {
                println!(
                    "ACCEPT [{:.0}ms] {} -> {} rows={} sig={:016X} row_skips={}",
                    elapsed.as_secs_f64() * 1e3,
                    book.observation.identity.need_asset_id,
                    book.observation.identity.have_asset_id,
                    book.observation.rows.len(),
                    book.observation.signature.0,
                    book.skipped_rows.len(),
                );
                for row in &book.observation.rows {
                    println!(
                        "  {} #{} {} stock={}",
                        row.side.as_str(),
                        row.row_index,
                        row.ratio.normalized,
                        row.stock
                    );
                }
                for skip in &book.skipped_rows {
                    println!("  SKIPPED-ROW {skip:?}");
                }
            }
            SessionEvent::FrameSkipped { reason } => println!("skip: {reason:?}"),
            SessionEvent::ConfirmationMismatch => println!("skip: double-read mismatch"),
            SessionEvent::Duplicate => {}
            SessionEvent::CaptureError(error) => println!("capture error: {error}"),
        },
    );

    println!(
        "\nticks={} idle={} recognitions={} accepted={} duplicates={} \
         confirm-mismatch={} capture-errors={}",
        stats.ticks,
        stats.idle_ticks,
        stats.recognitions,
        stats.accepted,
        stats.duplicates,
        stats.confirmation_mismatches,
        stats.capture_errors,
    );
    if !stats.frame_skips.is_empty() {
        println!("frame skips by reason:");
        for (reason, count) in &stats.frame_skips {
            println!("  {reason}: {count}");
        }
    }
    Ok(())
}

#[cfg(windows)]
fn ctrlc_handler(
    cancel: &'static std::sync::atomic::AtomicBool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Minimal Ctrl+C hook without extra dependencies: a helper thread parked
    // on stdin EOF is not reliable in consoles, so use SetConsoleCtrlHandler.
    use std::sync::OnceLock;
    static TARGET: OnceLock<&'static std::sync::atomic::AtomicBool> = OnceLock::new();
    TARGET.set(cancel).ok();
    unsafe extern "system" fn handler(_ctrl_type: u32) -> i32 {
        if let Some(target) = TARGET.get() {
            target.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        1
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }
    // SAFETY: handler is a static function; the target atomic is 'static.
    let installed = unsafe { SetConsoleCtrlHandler(Some(handler), 1) };
    if installed == 0 {
        return Err("failed to install Ctrl+C handler".into());
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("session-probe requires Windows");
}
