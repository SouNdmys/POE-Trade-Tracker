//! Proves the HUD really is excluded from capture.
//!
//! The claim under test is load-bearing: the watcher screenshots the currency
//! panel, and a HUD drawn on top of it would be read back as part of the
//! panel — the tracker would recognise its own output. `WDA_EXCLUDEFROMCAPTURE`
//! is what prevents that, and a flag you never verify is a flag you do not
//! have.
//!
//! The test has two halves, and the first is what makes the second mean
//! anything: with the affinity set to Include the capture *must* see the card,
//! or the probe is measuring nothing. Then with Exclude it must not.
//!
//! Usage: `hud-probe`

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use ptt_platform_win::{
        CaptureAffinity, HudContent, HudInteractionMode, HudWindow, HudWindowConfig,
        HudWindowPolicy, RectI, pump_thread_messages,
    };
    use ptt_vision::{CaptureRegion, GdiScreenCapture, ScreenCapture};

    const ORIGIN: (i32, i32) = (60, 60);
    const SIZE: (i32, i32) = (400, 200);
    /// The card's canvas, which nothing else on a desktop is likely to be.
    const CANVAS: [u8; 3] = [0xEC, 0xF2, 0xF5]; // BGR order, as captured.

    fn canvas_pixel_share(frame: &ptt_vision::CapturedFrame) -> f64 {
        let pixels = frame.bgra_pixels();
        let mut hits = 0_usize;
        let mut total = 0_usize;
        for chunk in pixels.chunks_exact(4) {
            total += 1;
            // Exact match: the card is painted with a solid brush, so its
            // canvas survives capture bit for bit.
            if chunk[0] == CANVAS[0] && chunk[1] == CANVAS[1] && chunk[2] == CANVAS[2] {
                hits += 1;
            }
        }
        if total == 0 {
            0.0
        } else {
            hits as f64 / total as f64
        }
    }

    let bounds = RectI::new(ORIGIN.0, ORIGIN.1, SIZE.0, SIZE.1).ok_or("invalid HUD bounds")?;
    let region = CaptureRegion::new(
        ORIGIN.0,
        ORIGIN.1,
        u32::try_from(SIZE.0)?,
        u32::try_from(SIZE.1)?,
    )?;

    // Start visible to capture, so the negative result later is a change and
    // not the initial state.
    let mut hud = HudWindow::create(HudWindowConfig {
        bounds,
        policy: HudWindowPolicy {
            interaction: HudInteractionMode::Passive,
            capture_affinity: CaptureAffinity::Include,
        },
        visible: false,
    })?;
    hud.set_content(HudContent {
        monitoring: true,
        status_text: "CAPTURE PROBE".to_owned(),
        elapsed: "—".to_owned(),
        lines: vec!["this card must not appear in a capture".to_owned()],
    })?;
    hud.show()?;

    let mut capture = GdiScreenCapture::new();
    // The HUD is thread-bound and repaints from WM_PAINT, so it stays blank
    // until this thread dispatches for it.
    pump_thread_messages(std::time::Duration::from_millis(500));
    let included = capture.capture(region)?;
    let included_share = canvas_pixel_share(&included);

    hud.set_capture_affinity(CaptureAffinity::Exclude)?;
    pump_thread_messages(std::time::Duration::from_millis(500));
    let excluded = capture.capture(region)?;
    let excluded_share = canvas_pixel_share(&excluded);

    hud.hide()?;
    drop(hud);

    println!(
        "card pixels with affinity Include: {:.1}%",
        included_share * 100.0
    );
    println!(
        "card pixels with affinity Exclude: {:.1}%",
        excluded_share * 100.0
    );

    // The card is mostly canvas, so a visible one dominates its own rectangle.
    if included_share < 0.5 {
        return Err(format!(
            "control failed: an included HUD covered only {:.1}% of its own rectangle, \
             so this probe cannot detect the card at all and proves nothing",
            included_share * 100.0
        )
        .into());
    }
    if excluded_share > 0.01 {
        return Err(format!(
            "HUD leaked into capture: {:.1}% of the rectangle is still card. \
             The watcher would read its own overlay as market data.",
            excluded_share * 100.0
        )
        .into());
    }

    println!("\nWDA_EXCLUDEFROMCAPTURE verified: visible to the eye, absent from the capture");
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("hud-probe requires Windows");
}
