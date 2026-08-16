//! Live wiring: a recognized book becomes a domain `ConfirmedCapture`.
//!
//! The mapping is where auto-accept earns its provenance: the context carries
//! the real pinned asset hashes (ONNX model, dictionary, catalog), the
//! confirmation mode is `AutomaticConsensus`, and the two frame hashes encode
//! the double-read agreement that admitted the book. Per-row confidence is
//! the double-read consensus constant — the route has no per-row score, and
//! two independent reads agreeing on the full content signature is stronger
//! evidence than any single OCR confidence.

use chrono::{DateTime, Utc};
use ptt_recognition::profiles::poe2::RecognizedBook;
use ptt_recognition::{Comparator as FieldComparator, Side};
use ptt_trade_domain::{
    CaptureConfirmationMode, CaptureProvenance, ClientLanguage, Comparator, ConfirmedCapture,
    ConfirmedOrderRow, DomainError, Game, MarketAssetId, MarketContext, ObservationIdentity,
    QuoteSide,
};

/// Confidence attributed to a double-read-confirmed row (ppm).
pub const DOUBLE_READ_CONSENSUS_PPM: u32 = 950_000;

/// Live POE2 zh-TW context with the real pinned asset identities.
pub fn poe2_live_context(league: &str) -> Result<MarketContext, DomainError> {
    let identity = ObservationIdentity::try_new(
        "ptt-winocr-ppocr5",
        env!("CARGO_PKG_VERSION"),
        ptt_ocr_onnx::EXPECTED_MODEL_SHA256.to_lowercase(),
        // No separate provider manifest exists; the dictionary pin plays that
        // role — it is verified at session start exactly like a manifest.
        ptt_ocr_onnx::EXPECTED_DICTIONARY_SHA256.to_lowercase(),
        ptt_catalog::POE2_CATALOG_SHA256.to_owned(),
        "poe2-catalog-660",
        ptt_catalog::POE2_CATALOG_SHA256.to_owned(),
        "poe2-zhtw-route-v1",
        ptt_catalog::POE2_CATALOG_SHA256.to_owned(),
        "warm-mask-v1",
    )?;
    MarketContext::try_new_for(
        Game::Poe2,
        ClientLanguage::TraditionalChinese,
        league,
        "live",
        "poe2-zhtw-2560x1440",
        1,
        "poe2-zhtw-route-v1",
        identity,
    )
}

/// Old catalog ids use underscores; the domain grammar wants hyphens.
pub fn domain_asset_id(catalog_id: &str) -> Result<MarketAssetId, DomainError> {
    MarketAssetId::try_new(catalog_id.replace('_', "-"))
}

/// Maps an accepted (double-read-confirmed) book into a `ConfirmedCapture`,
/// running the full domain validation on the way.
pub fn capture_from_book(
    book: &RecognizedBook,
    context: &MarketContext,
    captured_at: DateTime<Utc>,
    frame_hashes: [String; 2],
    capture_sequence: u64,
) -> Result<ConfirmedCapture, DomainError> {
    let signature = book.observation.signature.0;
    let provenance = CaptureProvenance {
        draft_id: format!("live-{capture_sequence}"),
        capture_job_id: format!("watch-{capture_sequence}"),
        review_revision: 1,
        confirmation_mode: CaptureConfirmationMode::AutomaticConsensus,
        source: "live_watch_double_read".to_owned(),
        evidence_id: format!("sig-{signature:016x}"),
        evidence_removed: true,
        // Real SHA-256 digests of the two independently captured frames
        // whose recognitions agreed (POE1's two-frame stability contract).
        frame_hashes: frame_hashes.into(),
        profile_sha256: ptt_catalog::POE2_CATALOG_SHA256.to_owned(),
        provider_id: context.observation_identity.ocr_provider_id.clone(),
        provider_version: context.observation_identity.ocr_provider_version.clone(),
        model_sha256: context.observation_identity.ocr_model_sha256.clone(),
        provider_manifest_sha256: context
            .observation_identity
            .ocr_provider_manifest_sha256
            .clone(),
        parser_assets_sha256: context.observation_identity.parser_assets_sha256.clone(),
    };

    let mut rows = Vec::with_capacity(book.observation.rows.len());
    for row in &book.observation.rows {
        let side = match row.side {
            Side::Available => QuoteSide::Available,
            Side::Competing => QuoteSide::Competing,
        };
        let comparator = match row.ratio.comparator {
            FieldComparator::Exact => Comparator::Exact,
            FieldComparator::LessThan => Comparator::LessThan,
            FieldComparator::GreaterThan => Comparator::GreaterThan,
        };
        let ratio_text = row
            .ratio
            .normalized
            .trim_start_matches(['<', '>'])
            .to_owned();
        rows.push(ConfirmedOrderRow::try_new(
            side,
            row.row_index,
            comparator,
            &ratio_text,
            &row.stock.to_string(),
            false,
            Some(DOUBLE_READ_CONSENSUS_PPM),
        )?);
    }

    ConfirmedCapture::try_new(
        captured_at,
        context.clone(),
        domain_asset_id(&book.observation.identity.need_asset_id)?,
        domain_asset_id(&book.observation.identity.have_asset_id)?,
        rows,
        provenance,
        serde_json::json!({
            "signature": format!("{signature:016x}"),
            "need_text": book.need_text,
            "have_text": book.have_text,
            "row_skips": book.skipped_rows.len(),
        })
        .to_string(),
        "{\"mode\":\"automatic_consensus\"}".to_owned(),
        vec![ptt_trade_domain::ReviewAuditEntry {
            field_path: "book".to_owned(),
            before: serde_json::json!(null),
            after: serde_json::json!(format!("{signature:016x}")),
            kind: ptt_trade_domain::ReviewAuditKind::AcceptedMachineValue,
        }],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptt_core::CaptureTimestamp;
    use ptt_recognition::{BookIdentity, BookObservation, RowObservation, parse_ratio};

    #[test]
    fn accepted_book_maps_to_a_valid_capture_with_stripped_comparators() {
        let rows = vec![
            RowObservation {
                side: Side::Available,
                row_index: 0,
                ratio: parse_ratio("1:9.80").expect("ratio"),
                stock: 920,
                band_fingerprint: 1,
            },
            RowObservation {
                side: Side::Competing,
                row_index: 5,
                ratio: parse_ratio(">1:9.75").expect("ratio"),
                stock: 611_620,
                band_fingerprint: 2,
            },
        ];
        let observation = BookObservation::assemble(
            BookIdentity {
                need_asset_id: "divine_orb".to_owned(),
                have_asset_id: "chaos_orb".to_owned(),
            },
            rows,
            CaptureTimestamp {
                wall_unix_ms: 0,
                mono_ms: 0,
            },
        );
        let book = RecognizedBook {
            observation,
            skipped_rows: Vec::new(),
            need_text: "神聖石".to_owned(),
            have_text: "混沌石".to_owned(),
        };
        let context = poe2_live_context("test-league").expect("context");
        let capture = capture_from_book(
            &book,
            &context,
            Utc::now(),
            ["a".repeat(64), "b".repeat(64)],
            1,
        )
        .expect("capture");

        assert_eq!(capture.need_asset_id.as_str(), "divine-orb");
        assert_eq!(capture.have_asset_id.as_str(), "chaos-orb");
        assert_eq!(capture.rows.len(), 2);
        assert_eq!(capture.quote_edges.len(), 4);
        assert_eq!(capture.rows[1].comparator, Comparator::GreaterThan);
        assert_eq!(capture.rows[1].ratio.text, "1:9.75", "prefix stripped");
        assert_eq!(
            capture.provenance.confirmation_mode,
            CaptureConfirmationMode::AutomaticConsensus
        );
    }
}
