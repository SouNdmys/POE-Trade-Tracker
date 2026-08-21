//! Live watch with the full pipeline: recognition → double-read confirmation
//! → domain capture → SQLite → coherent book → selection → depth engine,
//! printing the current pair's opportunities after every accepted book.
//!
//! Runs the same `LivePipeline` the app does, including the user's saved ROI
//! calibration, so a probe result and an app result are comparable.
//!
//! Usage: `watch-probe [--seconds N] [--db PATH]`
//! (default 120s; default db `%LOCALAPPDATA%\PoeTradeTracker\market.sqlite`)

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use ptt_runtime::pipeline::{
        LivePipeline, PipelineEvent, apply_saved_calibration, default_database_path,
    };
    use std::sync::atomic::AtomicBool;

    let mut seconds: u64 = 120;
    let mut db_path: Option<std::path::PathBuf> = None;
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--seconds" if index + 1 < arguments.len() => {
                seconds = arguments[index + 1].parse()?;
                index += 2;
            }
            "--db" if index + 1 < arguments.len() => {
                db_path = Some(std::path::PathBuf::from(&arguments[index + 1]));
                index += 2;
            }
            other => return Err(format!("unknown argument {other:?}").into()),
        }
    }

    static CANCEL: AtomicBool = AtomicBool::new(false);
    let rejected = apply_saved_calibration();
    if !rejected.is_empty() {
        println!(
            "ignoring invalid saved calibration for: {}",
            rejected.join(", ")
        );
    }
    let resolved_db = db_path.clone().unwrap_or_else(default_database_path);
    println!(
        "watching for {seconds}s, storing to {}",
        resolved_db.display()
    );

    let mut pipeline = LivePipeline::open(
        "live-league",
        db_path.as_deref(),
        ptt_settings::UiLanguage::English,
    )?;
    let stats = pipeline.run(
        std::time::Duration::from_secs(seconds),
        &CANCEL,
        |event| match event {
            PipelineEvent::Accepted(book) => {
                println!(
                    "\nACCEPT #{} [{:.0}ms] {} -> {} rows={}",
                    book.sequence,
                    book.elapsed.as_secs_f64() * 1e3,
                    book.need_asset_id,
                    book.have_asset_id,
                    book.rows.len(),
                );
                for row in &book.rows {
                    println!("  {row}");
                }
                for line in &book.analysis {
                    println!("  {line}");
                }
            }
            // Skips are summarized by the stats block at the end.
            PipelineEvent::Skipped(_) => {}
            PipelineEvent::Fault(message) => eprintln!("FAULT {message}"),
        },
    );

    println!(
        "\nticks {} idle {} recognitions {} accepted {} duplicates {} mismatches {} capture errors {}",
        stats.ticks,
        stats.idle_ticks,
        stats.recognitions,
        stats.accepted,
        stats.duplicates,
        stats.confirmation_mismatches,
        stats.capture_errors,
    );
    let mut skips: Vec<(&String, &u64)> = stats.frame_skips.iter().collect();
    skips.sort_by(|left, right| right.1.cmp(left.1).then_with(|| left.0.cmp(right.0)));
    for (reason, count) in skips {
        println!("  skip {count:>5}  {reason}");
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("watch-probe requires Windows");
}
