use crate::CapturedFrame;

pub const FNV1A_64_OFFSET_BASIS: u64 = 14_695_981_039_346_656_037;
pub const FNV1A_64_PRIME: u64 = 1_099_511_628_211;

/// Controls the grayscale value passed to a later OCR stage.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BlueMaskIntensityMode {
    /// Preserve anti-aliased edges using `72 + 3 * blue_dominance`.
    #[default]
    Dominance,
    BoostedDominance,
    BlueChannel,
    Binary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlueMaskSettings {
    pub minimum_blue: u8,
    pub minimum_blue_dominance: u8,
    pub maximum_warm_channel_difference: u8,
    pub intensity_mode: BlueMaskIntensityMode,
}

impl Default for BlueMaskSettings {
    fn default() -> Self {
        Self {
            minimum_blue: 105,
            minimum_blue_dominance: 18,
            maximum_warm_channel_difference: 72,
            intensity_mode: BlueMaskIntensityMode::Dominance,
        }
    }
}

/// One byte per source pixel; zero is background and non-zero is glyph ink
/// (blue affix text or warm exchange-table text, depending on the builder).
#[derive(Clone, Debug, Default)]
pub struct TextInkMask {
    width: usize,
    height: usize,
    intensities: Vec<u8>,
    x_fingerprint_terms: Vec<usize>,
    source_region: Option<crate::CaptureRegion>,
    fingerprint: u64,
}

impl TextInkMask {
    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn stride(&self) -> usize {
        self.width
    }

    pub fn intensities(&self) -> &[u8] {
        &self.intensities
    }

    pub fn source_region(&self) -> Option<crate::CaptureRegion> {
        self.source_region
    }

    /// A semantic FNV-1a-style hash of blue glyphs plus source ROI dimensions.
    /// The screen-space ROI origin is deliberately excluded.
    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    pub fn intensity_at(&self, x: usize, y: usize) -> Option<u8> {
        if x >= self.width || y >= self.height {
            return None;
        }
        Some(self.intensities[y * self.width + x])
    }
}

pub fn build_blue_mask(frame: &CapturedFrame, settings: BlueMaskSettings) -> TextInkMask {
    let mut output = TextInkMask::default();
    build_blue_mask_into(frame, settings, &mut output);
    output
}

/// Builds a mask while retaining `output`'s allocation between scans.
pub fn build_blue_mask_into(
    frame: &CapturedFrame,
    settings: BlueMaskSettings,
    output: &mut TextInkMask,
) {
    let width = frame.width();
    let height = frame.height();
    let pixel_count = width
        .checked_mul(height)
        .expect("validated capture dimensions must fit memory");
    output.width = width;
    output.height = height;
    output.source_region = Some(frame.region());
    output.intensities.resize(pixel_count, 0);
    output.intensities.fill(0);
    if output.x_fingerprint_terms.len() != width {
        output.x_fingerprint_terms.clear();
        output
            .x_fingerprint_terms
            .extend((0..width).map(|x| x * 397));
    }

    let mut fingerprint = FNV1A_64_OFFSET_BASIS;
    let source = frame.bgra_pixels();
    for y in 0..height {
        let source_row = y * frame.stride();
        let mask_row = y * width;
        let source_pixels = source[source_row..source_row + width * crate::BYTES_PER_PIXEL]
            .chunks_exact(crate::BYTES_PER_PIXEL);
        let mask_pixels = &mut output.intensities[mask_row..mask_row + width];
        for (x, (pixel, mask_pixel)) in source_pixels.zip(mask_pixels).enumerate() {
            let blue = pixel[0];
            let green = pixel[1];
            let red = pixel[2];
            let warm_max = red.max(green);
            let dominance = i16::from(blue) - i16::from(warm_max);
            let warm_difference = red.abs_diff(green);
            if blue < settings.minimum_blue
                || dominance < i16::from(settings.minimum_blue_dominance)
                || warm_difference > settings.maximum_warm_channel_difference
            {
                continue;
            }

            let intensity = match settings.intensity_mode {
                BlueMaskIntensityMode::Dominance => (72 + i32::from(dominance) * 3).clamp(0, 255),
                BlueMaskIntensityMode::BoostedDominance => {
                    (96 + i32::from(dominance) * 4).clamp(0, 255)
                }
                BlueMaskIntensityMode::BlueChannel => i32::from(blue),
                BlueMaskIntensityMode::Binary => 255,
            } as u8;

            *mask_pixel = intensity;
            fingerprint ^= (output.x_fingerprint_terms[x] ^ y ^ usize::from(intensity >> 5)) as u64;
            fingerprint = fingerprint.wrapping_mul(FNV1A_64_PRIME);
        }
    }

    // Match the established POE Trade Tracker fingerprint contract: glyph semantics first,
    // then logical ROI width and height. ROI position is irrelevant to line wrapping.
    fingerprint ^= width as u64;
    fingerprint = fingerprint.wrapping_mul(FNV1A_64_PRIME);
    fingerprint ^= height as u64;
    fingerprint = fingerprint.wrapping_mul(FNV1A_64_PRIME);
    output.fingerprint = fingerprint;
}

/// Settings for the currency-exchange text mask: bright warm-to-neutral glyphs
/// (ratio/stock digits ≈ (204,185,143), slot names near-white) on the panel's
/// dark-to-mid-gray ground. Calibrated on the 2560×1440 zh-TW corpus — see
/// `docs/P1-CALIBRATION-NOTES.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WarmMaskSettings {
    /// Minimum Rec.601 luminance for a pixel to count as ink. Table body
    /// background sits at L≈60–95 and glyphs at L≈180–205; the brighter
    /// header strips (L≈140–150) deliberately become oversized ink blobs that
    /// the band layer treats as separators, so 150 keeps data rows crisp
    /// without trying to suppress headers here.
    pub minimum_luminance: u8,
    /// Rejects cool/blue pixels: ink requires `blue <= red + tolerance`.
    /// Keeps blue UI accents and blue affix text out of exchange masks.
    pub maximum_blue_over_red: u8,
}

impl Default for WarmMaskSettings {
    fn default() -> Self {
        Self {
            minimum_luminance: 150,
            maximum_blue_over_red: 12,
        }
    }
}

pub fn build_warm_mask(frame: &CapturedFrame, settings: WarmMaskSettings) -> TextInkMask {
    let mut output = TextInkMask::default();
    build_warm_mask_into(frame, settings, &mut output);
    output
}

/// Builds a warm-text mask while retaining `output`'s allocation between scans.
/// Shares the [`TextInkMask`] contract with the blue builder: same fingerprint
/// semantics (glyph positions + ROI dimensions, origin excluded) and the same
/// anti-aliasing-preserving intensity ramp, so band detection, crop buffers,
/// and caches work unchanged on either mask.
pub fn build_warm_mask_into(
    frame: &CapturedFrame,
    settings: WarmMaskSettings,
    output: &mut TextInkMask,
) {
    let width = frame.width();
    let height = frame.height();
    let pixel_count = width
        .checked_mul(height)
        .expect("validated capture dimensions must fit memory");
    output.width = width;
    output.height = height;
    output.source_region = Some(frame.region());
    output.intensities.resize(pixel_count, 0);
    output.intensities.fill(0);
    if output.x_fingerprint_terms.len() != width {
        output.x_fingerprint_terms.clear();
        output
            .x_fingerprint_terms
            .extend((0..width).map(|x| x * 397));
    }

    let mut fingerprint = FNV1A_64_OFFSET_BASIS;
    let source = frame.bgra_pixels();
    for y in 0..height {
        let source_row = y * frame.stride();
        let mask_row = y * width;
        let source_pixels = source[source_row..source_row + width * crate::BYTES_PER_PIXEL]
            .chunks_exact(crate::BYTES_PER_PIXEL);
        let mask_pixels = &mut output.intensities[mask_row..mask_row + width];
        for (x, (pixel, mask_pixel)) in source_pixels.zip(mask_pixels).enumerate() {
            let blue = pixel[0];
            let green = pixel[1];
            let red = pixel[2];
            let luminance =
                ((u32::from(red) * 299 + u32::from(green) * 587 + u32::from(blue) * 114) / 1000)
                    as u8;
            if luminance < settings.minimum_luminance
                || blue > red.saturating_add(settings.maximum_blue_over_red)
            {
                continue;
            }

            // Anti-aliasing-preserving ramp above the threshold, mirroring the
            // blue builder's dominance ramp so OCR sees comparable contrast.
            let head_room = i32::from(luminance) - i32::from(settings.minimum_luminance);
            let intensity = (72 + head_room * 3).clamp(0, 255) as u8;

            *mask_pixel = intensity;
            fingerprint ^= (output.x_fingerprint_terms[x] ^ y ^ usize::from(intensity >> 5)) as u64;
            fingerprint = fingerprint.wrapping_mul(FNV1A_64_PRIME);
        }
    }

    fingerprint ^= width as u64;
    fingerprint = fingerprint.wrapping_mul(FNV1A_64_PRIME);
    fingerprint ^= height as u64;
    fingerprint = fingerprint.wrapping_mul(FNV1A_64_PRIME);
    output.fingerprint = fingerprint;
}

/// Standard FNV-1a 64-bit hash, useful for fixtures and cross-language probes.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = FNV1A_64_OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV1A_64_PRIME);
    }
    hash
}
