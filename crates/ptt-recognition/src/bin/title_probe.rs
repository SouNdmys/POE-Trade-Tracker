//! Measurement probe for the book-state gate (task #76 calibration).
//!
//! OCRs the available table's title strip — the top of the tables region —
//! on every screenshot given, in both OCR lanes. In the book view that strip
//! reads "Available Trades" (or its Traditional Chinese equivalent); in the
//! order-entry view the same pixels read "Market Ratio". The output decides
//! the gate's strip bounds, the per-language title words, and proves the
//! title reads reliably on every corpus frame before the gate is allowed to
//! skip anything.
//!
//! Usage: `gap-probe [--game poe1|poe2] [--zh] --dir FOLDER [--dir FOLDER ...]`

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use ptt_ocr_win::{OcrLanguagePreference, OcrWorker, OwnedBgraImage};
    use ptt_vision::{CaptureRegion, WicScreenshotDecoder};

    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let mut folders = Vec::new();
    let mut layout = ptt_recognition::profiles::poe2::LAYOUT;
    let mut language = ptt_recognition::profiles::ProfileLanguage::English;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--dir" => {
                folders.push(arguments[index + 1].clone());
                index += 2;
            }
            "--game" => {
                layout = match arguments[index + 1].as_str() {
                    "poe1" => ptt_recognition::profiles::poe1::LAYOUT,
                    _ => ptt_recognition::profiles::poe2::LAYOUT,
                };
                index += 2;
            }
            "--zh" => {
                language = ptt_recognition::profiles::ProfileLanguage::TraditionalChinese;
                index += 1;
            }
            _ => index += 1,
        }
    }
    if folders.is_empty() {
        return Err("usage: gap-probe [--game poe1|poe2] [--zh] --dir FOLDER ...".into());
    }

    let decoder = WicScreenshotDecoder::new().map_err(|error| format!("{error:?}"))?;
    let worker = OcrWorker::start().map_err(|error| format!("{error:?}"))?;
    let (x, y, width, _height) = layout.tables_for(language);
    // The title strip: the top of the tables region, above the column
    // heading. Generous, so calibration slop cannot push the title out.
    let strip_height = 48u32;
    let region =
        CaptureRegion::new(x, y, width, strip_height).map_err(|error| format!("{error:?}"))?;

    for folder in &folders {
        let mut entries: Vec<_> = std::fs::read_dir(folder)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
            })
            .collect();
        entries.sort();
        for path in entries {
            let frame = match decoder.decode(&path, Some(region)) {
                Ok(frame) => frame,
                Err(error) => {
                    println!("DECODE-FAIL {} {error:?}", path.display());
                    continue;
                }
            };
            // 2x nearest-neighbour luminance upscale, as the route does.
            let factor = 2usize;
            let scaled_width = frame.width() * factor;
            let scaled_height = frame.height() * factor;
            let source = frame.bgra_pixels();
            let mut pixels = vec![0u8; scaled_width * scaled_height];
            for row in 0..scaled_height {
                let source_row = (row / factor) * frame.stride();
                for column in 0..scaled_width {
                    let pixel = source_row + (column / factor) * 4;
                    let luminance = (u32::from(source[pixel + 2]) * 299
                        + u32::from(source[pixel + 1]) * 587
                        + u32::from(source[pixel]) * 114)
                        / 1000;
                    pixels[row * scaled_width + column] = luminance as u8;
                }
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            for (label, language) in [
                ("en", OcrLanguagePreference::English),
                ("zh", OcrLanguagePreference::TraditionalChinese),
            ] {
                let image = OwnedBgraImage::gray8(
                    scaled_width,
                    scaled_height,
                    scaled_width,
                    pixels.clone(),
                )
                .map_err(|error| format!("{error:?}"))?;
                match worker.recognize(language, image) {
                    Ok(recognition) => {
                        println!("{name:58} {label}: {:?}", recognition.text());
                    }
                    Err(error) => println!("{name:58} {label}: OCR-FAIL {error:?}"),
                }
            }
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("gap-probe requires Windows");
}
