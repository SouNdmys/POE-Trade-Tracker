//! Why does coverage say what it says, against the live database.
//!
//! Offline diagnosis for "I captured this pair and it still reads missing":
//! prints, for every focus pair, which of the three views selected an edge,
//! what that edge's roles and verdicts are, and which rejection stopped it.
//!
//! Usage: `coverage-probe [FROM TO]` — with a pair, prints every candidate
//! edge for it in all three views; without, prints the coverage table.

#[cfg(windows)]
fn main() -> Result<(), String> {
    use ptt_runtime::pipeline::{LIVE_LEAGUE, default_database_path};

    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let filter: Option<(String, String)> = match arguments.as_slice() {
        [from, to] => Some((from.clone(), to.clone())),
        _ => None,
    };

    let local = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default());
    let settings = ptt_settings::SettingsStore::release_default_from(&local)
        .load()
        .settings;
    let profile = settings.active_profile;
    let tuning = settings.market_tuning(profile.game);

    let store = ptt_storage::MarketStore::open(default_database_path())
        .map_err(|error| format!("storage: {error}"))?;
    let context = ptt_runtime::live::live_context(profile, LIVE_LEAGUE)
        .map_err(|error| format!("{error:?}"))?;
    let context_key = context.stable_key();
    let window_hours = i64::try_from(tuning.report_window_hours)
        .unwrap_or(24)
        .clamp(1, 24 * 365);
    let observations = store
        .load_observations(
            &context_key,
            Some(chrono::Utc::now() - chrono::Duration::hours(window_hours)),
        )
        .map_err(|error| format!("load: {error}"))?;
    println!(
        "context={context_key} window={window_hours}h observations={}",
        observations.len()
    );

    if let Some((from, to)) = filter {
        ptt_runtime::reports::debug_pair(&observations, &context_key, &tuning, &from, &to)?;
        return Ok(());
    }

    let model = ptt_runtime::reports::watchlist_model(
        &observations,
        &context_key,
        LIVE_LEAGUE,
        &tuning,
        ptt_settings::UiLanguage::English,
    )
    .map_err(|error| format!("watchlist: {error}"))?;
    match &model.coverage {
        ptt_runtime::reports::CoverageOutcome::Ready(coverage) => {
            println!("scope status: {:?}", coverage.status);
            for entry in &coverage.entries {
                println!(
                    "{:<28} -> {:<28} {:?} instant={} maker={} freshness={:?}",
                    entry.from_asset_id.as_str(),
                    entry.to_asset_id.as_str(),
                    entry.status,
                    entry.instant_available,
                    entry.maker_reference_available,
                    entry.freshness_status,
                );
            }
        }
        other => println!("coverage: {other:?}"),
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("windows only: reads the live capture database");
}
