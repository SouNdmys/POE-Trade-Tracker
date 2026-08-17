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
}

#[cfg(windows)]
fn run_manifest(
    path: &str,
    language: ptt_recognition::profiles::ProfileLanguage,
) -> Result<(), Box<dyn std::error::Error>> {
    use ptt_recognition::profiles::poe2::Route;

    let manifest: Manifest = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    assert_eq!(manifest.version, 1, "unsupported manifest version");
    println!(
        "manifest: {} profile {} ({} cases)",
        path,
        manifest.profile,
        manifest.cases.len()
    );
    // The manifest names its profile, so a POE1 corpus drives the POE1 panel
    // without a second flag to keep in sync with the file.
    let layout = if manifest.profile.starts_with("poe1") {
        ptt_recognition::profiles::poe1::LAYOUT
    } else {
        ptt_recognition::profiles::poe2::LAYOUT
    };
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
                if problems.is_empty() {
                    Ok(format!("accept rows={}", book.observation.rows.len()))
                } else {
                    Err(problems.join(", "))
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
    use ptt_recognition::profiles::{ProfileLanguage, poe2::Route};

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
        return run_manifest(manifest, language);
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
