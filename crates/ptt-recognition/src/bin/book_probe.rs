//! End-to-end offline probe: screenshot file(s) → recognized order book.
//!
//! Usage:
//!   `book-probe IMAGE`              one screenshot, verbose
//!   `book-probe --dir FOLDER`       every screenshot in a folder, verbose
//!   `book-probe --manifest FILE`    regression mode: compare against the
//!                                   frozen corpus manifest; exit 1 on any
//!                                   mismatch. This is the P1 acceptance gate.

#[cfg(windows)]
#[derive(serde::Deserialize)]
struct Manifest {
    version: u32,
    profile: String,
    screenshot_dir: String,
    cases: Vec<ManifestCase>,
}

#[cfg(windows)]
#[derive(serde::Deserialize)]
struct ManifestCase {
    file: String,
    expect: String,
    #[serde(default)]
    need: Option<String>,
    #[serde(default)]
    have: Option<String>,
    #[serde(default)]
    min_rows: Option<usize>,
    /// Every row the frame actually shows, read off the screenshot by hand.
    ///
    /// Counting rows cannot catch a wrong value, and wrong values are the one
    /// unacceptable outcome: an aggregate `< 24 : 1` read as a plain `24 : 1`
    /// kept a gate green for as long as the row count matched.
    #[serde(default)]
    rows: Vec<ManifestRow>,
}

/// One row of ground truth. `ratio` is the route's normalized form — the
/// comparator, if any, then the pair with no spaces (`<199:1`, `1:9.67`).
#[cfg(windows)]
#[derive(serde::Deserialize)]
struct ManifestRow {
    side: String,
    index: u8,
    /// "accept" (the default) or "skip" for a row the route is known not to
    /// read yet. A skip that starts being read fails the gate rather than
    /// passing quietly, so the value gets checked when it appears.
    #[serde(default)]
    expect: Option<String>,
    #[serde(default)]
    ratio: Option<String>,
    #[serde(default)]
    stock: Option<u64>,
}

/// Compares a frame's ground-truth rows against what the route read.
///
/// Four outcomes matter and each is reported separately, because they mean
/// different things: a value mismatch is a wrong accept, a missing row is a
/// regression, an unexpected row is either a fix (update the manifest) or an
/// invention, and a row that was supposed to stay unread but appeared needs
/// its value checked before it is trusted.
#[cfg(windows)]
fn check_rows(
    expected: &[ManifestRow],
    observed: &[ptt_recognition::book::RowObservation],
) -> Vec<String> {
    use ptt_recognition::rows::Side;

    let mut problems = Vec::new();
    if expected.is_empty() {
        return problems;
    }
    let key = |side: Side, index: u8| format!("{} #{index}", side.as_str());
    let seen: std::collections::BTreeMap<String, &ptt_recognition::book::RowObservation> = observed
        .iter()
        .map(|row| (key(row.side, row.row_index), row))
        .collect();

    let mut accounted = std::collections::BTreeSet::new();
    for want in expected {
        let side = match want.side.as_str() {
            "available" => Side::Available,
            "competing" => Side::Competing,
            other => {
                problems.push(format!("manifest row has unknown side {other:?}"));
                continue;
            }
        };
        let name = key(side, want.index);
        accounted.insert(name.clone());
        let skip_expected = want.expect.as_deref() == Some("skip");
        match (seen.get(&name), skip_expected) {
            (None, true) => {}
            (None, false) => problems.push(format!("{name} missing")),
            (Some(row), true) => problems.push(format!(
                "{name} was expected to stay unread but came back {} stock={}",
                row.ratio.normalized, row.stock
            )),
            (Some(row), false) => {
                if let Some(ratio) = &want.ratio
                    && ratio != &row.ratio.normalized
                {
                    problems.push(format!("{name} ratio {} != {ratio}", row.ratio.normalized));
                }
                if let Some(stock) = want.stock
                    && stock != row.stock
                {
                    problems.push(format!("{name} stock {} != {stock}", row.stock));
                }
            }
        }
    }
    for (name, row) in &seen {
        if !accounted.contains(name) {
            problems.push(format!(
                "{name} is not in the manifest but was read as {} stock={}",
                row.ratio.normalized, row.stock
            ));
        }
    }
    problems
}

/// The panel and the lexicon a manifest's profile name asks for.
///
/// Both, not just the panel. The layout was already derived here while the
/// language stayed on a command-line flag defaulting to Traditional Chinese,
/// so the English corpus only passed when the operator remembered to add
/// `--language en` -- and forgetting it failed all ten frames at once, which
/// is the loud direction. The quiet direction is worse: a Traditional Chinese
/// corpus run with `--language en` still checks every ratio and stock value,
/// so a wrong-lexicon run can look like a pass. A manifest that names its
/// profile should need no flags at all.
#[cfg(windows)]
fn profile_route(
    profile: &str,
) -> Result<
    (
        ptt_recognition::profiles::PanelLayout,
        ptt_recognition::profiles::ProfileLanguage,
    ),
    String,
> {
    use ptt_recognition::profiles::ProfileLanguage;
    let name = profile.to_ascii_lowercase();
    let layout = if name.starts_with("poe1-") {
        ptt_recognition::profiles::poe1::LAYOUT
    } else if name.starts_with("poe2-") {
        ptt_recognition::profiles::poe2::LAYOUT
    } else {
        return Err(format!("manifest profile {profile:?} names no game"));
    };
    // Rejected rather than defaulted. A default is what let the language drift
    // away from the corpus in the first place.
    let language = match name.split_once('-').map(|(_, rest)| rest) {
        Some(rest) if rest.starts_with("en") => ProfileLanguage::English,
        Some(rest) if rest.starts_with("zh") => ProfileLanguage::TraditionalChinese,
        _ => return Err(format!("manifest profile {profile:?} names no language")),
    };
    Ok((layout, language))
}

#[cfg(windows)]
fn run_manifest(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    use ptt_recognition::route::Route;

    let manifest: Manifest = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    assert_eq!(manifest.version, 1, "unsupported manifest version");
    println!(
        "manifest: {} profile {} ({} cases)",
        path,
        manifest.profile,
        manifest.cases.len()
    );
    // The manifest names its profile, so the corpus drives both the panel and
    // the lexicon with no flags to keep in sync with the file.
    let (layout, language) = profile_route(&manifest.profile)?;
    println!("profile resolves to {language:?} on {}", layout.key_prefix);
    let route = Route::new_with(layout, language)
        .map_err(|reason| format!("route init failed: {reason:?}"))?;
    let mut failures = 0usize;
    let started = std::time::Instant::now();
    let mut durations = Vec::new();
    for case in &manifest.cases {
        let file = std::path::Path::new(&manifest.screenshot_dir).join(&case.file);
        // Checked before recognition rather than left to it. A missing file
        // fails to decode, decoding failure is a skip, and a negative case
        // expects a skip — so moving the corpus somewhere else turned every
        // negative case green while proving nothing at all.
        if !file.is_file() {
            failures += 1;
            println!(
                "FAIL {} — screenshot missing at {}",
                case.file,
                file.display()
            );
            durations.push(0.0);
            continue;
        }
        let case_started = std::time::Instant::now();
        let outcome = route.recognize_screenshot(&file);
        durations.push(case_started.elapsed().as_secs_f64() * 1e3);
        let verdict = match (case.expect.as_str(), &outcome) {
            ("accept", Ok(book)) => {
                let identity = &book.observation.identity;
                let mut problems = Vec::new();
                if let Some(need) = &case.need
                    && need != &identity.need_asset_id
                {
                    problems.push(format!("need {} != {need}", identity.need_asset_id));
                }
                if let Some(have) = &case.have
                    && have != &identity.have_asset_id
                {
                    problems.push(format!("have {} != {have}", identity.have_asset_id));
                }
                if let Some(min_rows) = case.min_rows
                    && book.observation.rows.len() < min_rows
                {
                    problems.push(format!(
                        "rows {} < min {min_rows}",
                        book.observation.rows.len()
                    ));
                }
                problems.extend(check_rows(&case.rows, &book.observation.rows));
                if problems.is_empty() {
                    Ok(format!(
                        "accept rows={}{}",
                        book.observation.rows.len(),
                        if case.rows.is_empty() {
                            String::new()
                        } else {
                            format!(", {} verified", case.rows.len())
                        }
                    ))
                } else {
                    Err(problems.join("; "))
                }
            }
            ("skip", Err(reason)) => Ok(format!("skip {reason:?}")),
            ("accept", Err(reason)) => Err(format!("expected accept, skipped: {reason:?}")),
            ("skip", Ok(book)) => Err(format!(
                "expected skip, accepted {} -> {}",
                book.observation.identity.need_asset_id, book.observation.identity.have_asset_id
            )),
            (other, _) => Err(format!("unknown expectation {other:?}")),
        };
        match verdict {
            Ok(summary) => println!("PASS {} ({summary})", case.file),
            Err(problem) => {
                failures += 1;
                println!("FAIL {} — {problem}", case.file);
            }
        }
    }
    durations.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = durations[durations.len() / 2];
    let p95 = durations[(durations.len() * 95 / 100).min(durations.len() - 1)];
    println!(
        "
{} cases, {failures} failures, wall {:.1}s, per-frame p50 {p50:.1}ms p95 {p95:.1}ms",
        manifest.cases.len(),
        started.elapsed().as_secs_f64(),
    );
    if failures > 0 {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use ptt_recognition::profiles::ProfileLanguage;
    use ptt_recognition::route::Route;

    let mut arguments: Vec<String> = std::env::args().skip(1).collect();
    // `--language en` reads the identity slots against the catalog's English
    // names. The corpus is Traditional Chinese, so this exists for the day
    // English screenshots arrive rather than for the frozen manifest.
    let mut language = ProfileLanguage::TraditionalChinese;
    // Which panel the loose-file path reads. The manifest path takes this
    // from the manifest's own profile name instead.
    let mut layout = ptt_recognition::profiles::poe2::LAYOUT;
    if let Some(index) = arguments.iter().position(|value| value == "--game") {
        let value = arguments
            .get(index + 1)
            .ok_or("--game needs a value: poe1 or poe2")?
            .clone();
        layout = match value.to_ascii_lowercase().as_str() {
            "poe1" => ptt_recognition::profiles::poe1::LAYOUT,
            "poe2" => ptt_recognition::profiles::poe2::LAYOUT,
            other => return Err(format!("unknown game {other:?}").into()),
        };
        arguments.drain(index..=index + 1);
    }
    if let Some(index) = arguments.iter().position(|value| value == "--language") {
        let value = arguments
            .get(index + 1)
            .ok_or("--language needs a value: zh-TW or en")?
            .clone();
        language = match value.to_ascii_lowercase().as_str() {
            "zh-tw" | "zhtw" | "zh" => ProfileLanguage::TraditionalChinese,
            "en" | "en-us" | "english" => ProfileLanguage::English,
            other => return Err(format!("unknown language {other:?}").into()),
        };
        arguments.drain(index..=index + 1);
    }
    if let [flag, manifest] = arguments.as_slice()
        && flag == "--manifest"
    {
        return run_manifest(manifest);
    }
    let paths: Vec<std::path::PathBuf> = match arguments.as_slice() {
        [single] if !single.starts_with("--") => vec![single.into()],
        [flag, folder] if flag == "--dir" => {
            let mut entries: Vec<_> = std::fs::read_dir(folder)?
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case("png"))
                })
                .collect();
            entries.sort();
            entries
        }
        _ => return Err("usage: book-probe IMAGE | book-probe --dir FOLDER".into()),
    };

    let route = Route::new_with(layout, language)
        .map_err(|reason| format!("route init failed: {reason:?}"))?;
    let mut accepted = 0usize;
    let mut skipped_frames = 0usize;
    for path in &paths {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let started = std::time::Instant::now();
        match route.recognize_screenshot(path) {
            Ok(book) => {
                accepted += 1;
                let elapsed = started.elapsed().as_secs_f64() * 1e3;
                println!(
                    "ACCEPT {name} [{:.1}ms] {} -> {} sig={:016X} rows={} row_skips={}",
                    elapsed,
                    book.observation.identity.need_asset_id,
                    book.observation.identity.have_asset_id,
                    book.observation.signature.0,
                    book.observation.rows.len(),
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
            Err(reason) => {
                skipped_frames += 1;
                println!("SKIP   {name}: {reason:?}");
            }
        }
    }
    println!("\ntotal: {accepted} accepted, {skipped_frames} skipped frames");
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("book-probe requires Windows");
}

#[cfg(all(test, windows))]
mod tests {
    use super::profile_route;
    use ptt_recognition::profiles::ProfileLanguage;

    #[test]
    fn a_profile_name_decides_both_the_panel_and_the_lexicon() {
        let (layout, language) = profile_route("poe1-en").unwrap();
        assert_eq!(layout.game, ptt_core::Game::Poe1);
        assert_eq!(language, ProfileLanguage::English);

        // Suffixed variants are the same profile: the corpus date in
        // `poe1-zhtw-0818` names the batch, not a different client.
        for name in ["poe1-zhtw", "poe1-zhtw-0818", "POE1-ZhTw"] {
            let (layout, language) = profile_route(name).unwrap();
            assert_eq!(layout.game, ptt_core::Game::Poe1, "{name}");
            assert_eq!(language, ProfileLanguage::TraditionalChinese, "{name}");
        }

        let (layout, language) = profile_route("poe2-zh-TW").unwrap();
        assert_eq!(layout.game, ptt_core::Game::Poe2);
        assert_eq!(language, ProfileLanguage::TraditionalChinese);
    }

    /// A name that says nothing is an error, never a default.
    ///
    /// Defaulting is what let the lexicon drift away from the corpus: the
    /// English manifest was silently read against Traditional Chinese names
    /// unless a flag said otherwise.
    #[test]
    fn a_profile_that_names_no_game_or_language_is_refused() {
        for name in ["poe1", "poe3-en", "en", "poe2-fr", ""] {
            assert!(profile_route(name).is_err(), "{name:?} should be refused");
        }
    }

    /// Every manifest in the tree resolves without a flag.
    ///
    /// The gate that matters: adding a corpus with a profile name this cannot
    /// read should fail here, at the moment it is added, rather than at
    /// whatever hour someone next runs the probe.
    #[test]
    fn every_shipped_manifest_names_a_profile_that_resolves() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/manifests")
            .canonicalize()
            .expect("manifest directory");
        let mut seen = 0usize;
        for entry in std::fs::read_dir(&directory).expect("read manifests") {
            let path = entry.expect("entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read manifest");
            let manifest: serde_json::Value = serde_json::from_str(&text).expect("parse manifest");
            let profile = manifest["profile"].as_str().unwrap_or_default();
            assert!(
                profile_route(profile).is_ok(),
                "{} names profile {profile:?}, which resolves to nothing",
                path.display()
            );
            seen += 1;
        }
        assert!(seen >= 4, "expected the four corpora, found {seen}");
    }
}
