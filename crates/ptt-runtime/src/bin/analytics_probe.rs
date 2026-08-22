//! Market analytics against the live database.
//!
//! The verification surface for the P10 stage: builds any missing daily
//! rollups, then prints the market pulse — supply/demand pressure, anchor
//! health, classifications — for hand-checking against the game before any
//! of it reaches a page.
//!
//! Usage: `analytics-probe [--rollup] [--pulse]` — default runs both.
//! `--rollup` also applies raw-data retention when it is enabled in settings.

#[cfg(windows)]
fn main() -> Result<(), String> {
    use ptt_runtime::pipeline::{LIVE_LEAGUE, default_database_path};
    use ptt_runtime::rollup;

    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let run_rollup =
        arguments.is_empty() || arguments.iter().any(|argument| argument == "--rollup");
    let run_pulse = arguments.is_empty() || arguments.iter().any(|argument| argument == "--pulse");

    let local = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default());
    let settings = ptt_settings::SettingsStore::release_default_from(&local)
        .load()
        .settings;
    let profile = settings.active_profile;
    let game = profile.game.as_str();
    let tuning = settings.market_tuning(profile.game);
    let language = settings.ui_language;
    let now = chrono::Utc::now();

    let mut store = ptt_storage::MarketStore::open(default_database_path())
        .map_err(|error| format!("storage: {error}"))?;

    if run_rollup {
        let outcome =
            rollup::ensure_daily_rollups(&mut store, game, now, rollup::MAX_ROLLUP_DAYS_PER_RUN)?;
        println!(
            "rollup: contexts={} processed={} already-done={} skipped={}",
            outcome.contexts,
            outcome.days_processed.len(),
            outcome.days_already_done,
            outcome.days_skipped.len(),
        );
        for day in &outcome.days_processed {
            println!("  rolled up {day}");
        }
        for (day, reason) in &outcome.days_skipped {
            println!("  SKIPPED {day}: {reason}");
        }
        let retention = tuning.analytics.raw_retention_days;
        if retention > 0 {
            let pruned = rollup::prune_raw_days(&mut store, game, now, retention)?;
            println!(
                "retention({retention}d): deleted={} refused={} edges={} snapshots={} ~{}KB",
                pruned.days_deleted.len(),
                pruned.days_refused.len(),
                pruned.stats.edges_deleted,
                pruned.stats.snapshots_deleted,
                pruned.stats.freed_bytes_estimate / 1024,
            );
            for (day, reason) in &pruned.days_refused {
                println!("  refused {day}: {reason}");
            }
        }
    }

    if run_pulse {
        let season = store
            .active_season(game)
            .map_err(|error| format!("season: {error}"))?;
        let from_day = season.as_ref().map_or_else(
            || "0001-01-01".to_owned(),
            |row| row.started_at.format("%Y-%m-%d").to_string(),
        );
        let to_day = now.format("%Y-%m-%d").to_string();
        let rollup_rows = store
            .load_rollups(game, &from_day, &to_day)
            .map_err(|error| format!("rollups: {error}"))?;

        // Today's live fold reads across every context of the game, exactly
        // like the rollup builder: a release today must not hide the morning.
        let today_start = now
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .expect("midnight")
            .and_utc();
        let mut window = Vec::new();
        for key in rollup::game_context_keys(&store, game)? {
            window.extend(
                store
                    .load_observations_between(&key, today_start, now + chrono::Duration::hours(1))
                    .map_err(|error| format!("today: {error}"))?,
            );
        }

        println!(
            "pulse: rollup-rows={} today-edges={} season={}",
            rollup_rows.len(),
            window.len(),
            season.as_ref().map_or("none", |row| row.label.as_str()),
        );
        let model = ptt_runtime::reports::analytics_model(
            &rollup_rows,
            &window,
            season.as_ref(),
            LIVE_LEAGUE,
            &tuning,
            language,
        );
        for line in ptt_runtime::reports::analytics_report_lines(&model, language) {
            println!("{line}");
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("windows only: reads the live capture database");
}
