//! End-to-end offline probe: screenshot file(s) → recognized order book.
//!
//! Usage: `book-probe IMAGE` or `book-probe --dir FOLDER`. Prints the
//! resolved pair, every accepted row, every skipped row with its raw OCR
//! text, or the typed frame-skip reason. This is the P1 acceptance surface.

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use ptt_recognition::profiles::poe2_zhtw::Route;

    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let paths: Vec<std::path::PathBuf> = match arguments.as_slice() {
        [single] if single != "--dir" => vec![single.into()],
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

    let route = Route::new().map_err(|reason| format!("route init failed: {reason:?}"))?;
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
