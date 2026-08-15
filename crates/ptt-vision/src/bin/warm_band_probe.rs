//! Offline probe: decode a real exchange-panel screenshot, build the warm
//! text mask over the tables region, and print detected row bands with
//! timings. Used to validate the Rust hot path against the corpus-derived
//! geometry in docs/P1-CALIBRATION-NOTES.md.

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use ptt_vision::{
        BandDetectionSettings, CaptureRegion, PhysicalBandDetector, WarmMaskSettings,
        WicScreenshotDecoder, build_warm_mask,
    };

    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let path = arguments
        .first()
        .ok_or("usage: warm-band-probe IMAGE [X Y WIDTH HEIGHT]")?;
    let crop = if arguments.len() == 5 {
        let parse = |index: usize| -> Result<i32, Box<dyn std::error::Error>> {
            Ok(arguments[index].to_string_lossy().parse::<i32>()?)
        };
        Some(CaptureRegion::new(
            parse(1)?,
            parse(2)?,
            u32::try_from(parse(3)?)?,
            u32::try_from(parse(4)?)?,
        )?)
    } else if arguments.len() == 1 {
        // Default: the poe2-zh-TW 2560x1440 tables region from calibration.
        Some(CaptureRegion::new(1150, 220, 320, 560)?)
    } else {
        return Err("usage: warm-band-probe IMAGE [X Y WIDTH HEIGHT]".into());
    };

    let frame = WicScreenshotDecoder::new()?.decode(path, crop)?;
    let started = std::time::Instant::now();
    let mask = build_warm_mask(&frame, WarmMaskSettings::default());
    let mask_elapsed = started.elapsed();
    let band_started = std::time::Instant::now();
    let bands = PhysicalBandDetector::new().detect(&mask, BandDetectionSettings::default())?;
    let band_elapsed = band_started.elapsed();

    println!(
        "{}x{} fingerprint={:016X} mask={:.2}ms bands={:.2}ms",
        frame.width(),
        frame.height(),
        mask.fingerprint(),
        mask_elapsed.as_secs_f64() * 1e3,
        band_elapsed.as_secs_f64() * 1e3,
    );
    for band in &bands.bands {
        let rect = band.crop.source_rect;
        println!(
            "band top={:3} h={:2} x={:3} w={:3} fp={:016X}",
            rect.y, rect.height, rect.x, rect.width, band.identity.content_fingerprint
        );
    }
    if let Some(fallback) = &bands.fallback_crop {
        let rect = fallback.source_rect;
        println!("fallback crop: top={} h={}", rect.y, rect.height);
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("warm-band-probe requires Windows (WIC decode)");
}
