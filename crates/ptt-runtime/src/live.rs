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
use ptt_core::{ContentLanguage, Game as ProfileGame, ProfileId};
use ptt_recognition::route::RecognizedBook;
use ptt_recognition::{Comparator as FieldComparator, Side};
use ptt_trade_domain::{
    CaptureConfirmationMode, CaptureProvenance, ClientLanguage, Comparator, ConfirmedCapture,
    ConfirmedOrderRow, DomainError, Game, MarketAssetId, MarketContext, ObservationIdentity,
    QuoteSide,
};

/// Confidence attributed to a double-read-confirmed row (ppm).
pub const DOUBLE_READ_CONSENSUS_PPM: u32 = 950_000;

/// The live context for a profile, with the real pinned asset identities.
///
/// The catalog hash is part of the observation identity, so it must be the
/// hash of the catalog this profile actually matches names against — feeding
/// POE2's pin to a POE1 session would label the rows with provenance that
/// does not describe them.
pub fn live_context(profile: ProfileId, league: &str) -> Result<MarketContext, DomainError> {
    // Two parallel `Game` enums exist, one in `ptt-core` for profile identity
    // and one in `ptt-trade-domain` for provenance. Mapped explicitly here
    // rather than papered over with a blanket `From`, so adding a third game
    // has to visit this function.
    let (catalog_sha, catalog_id, domain_game) = match profile.game {
        ProfileGame::Poe1 => (
            ptt_catalog::POE1_CATALOG_SHA256,
            "poe1-catalog-1047",
            Game::Poe1,
        ),
        ProfileGame::Poe2 => (
            ptt_catalog::POE2_CATALOG_SHA256,
            "poe2-catalog-691",
            Game::Poe2,
        ),
    };
    let client_language = match profile.language {
        ContentLanguage::TraditionalChinese => ClientLanguage::TraditionalChinese,
        ContentLanguage::English => ClientLanguage::English,
    };
    let (route_id, geometry): (&str, &str) = match (profile.game, profile.language) {
        (ProfileGame::Poe1, ContentLanguage::English) => ("poe1-en-route-v1", "poe1-en-2560x1440"),
        (ProfileGame::Poe1, ContentLanguage::TraditionalChinese) => {
            ("poe1-zhtw-route-v1", "poe1-zhtw-2560x1440")
        }
        (ProfileGame::Poe2, ContentLanguage::English) => ("poe2-en-route-v1", "poe2-en-2560x1440"),
        (ProfileGame::Poe2, ContentLanguage::TraditionalChinese) => {
            ("poe2-zhtw-route-v1", "poe2-zhtw-2560x1440")
        }
    };
    let identity = ObservationIdentity::try_new(
        "ptt-winocr-ppocr5",
        env!("CARGO_PKG_VERSION"),
        ptt_ocr_onnx::EXPECTED_MODEL_SHA256.to_lowercase(),
        // No separate provider manifest exists; the dictionary pin plays that
        // role — it is verified at session start exactly like a manifest.
        ptt_ocr_onnx::EXPECTED_DICTIONARY_SHA256.to_lowercase(),
        catalog_sha.to_owned(),
        catalog_id,
        catalog_sha.to_owned(),
        route_id,
        catalog_sha.to_owned(),
        "warm-mask-v1",
    )?;
    MarketContext::try_new_for(
        domain_game,
        client_language,
        league,
        "live",
        geometry,
        1,
        route_id,
        identity,
    )
}

/// The POE2 Traditional Chinese context, which the probes and fixtures use.
pub fn poe2_live_context(league: &str) -> Result<MarketContext, DomainError> {
    live_context(
        ProfileId::new(ProfileGame::Poe2, ContentLanguage::TraditionalChinese),
        league,
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
        // 从上下文取,不写死:上下文按 game 选了正确的目录哈希,这里再
        // 硬编码一份就会给 POE1 的抓取盖上 POE2 的印。
        profile_sha256: context.observation_identity.product_catalog_sha256.clone(),
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

    /// The provenance's catalog hash must describe the catalog this profile
    /// actually matched names against — a POE1 capture stamped with POE2's
    /// pin claims evidence that does not exist.
    #[test]
    fn a_poe1_capture_carries_poe1_catalog_provenance() {
        let rows = vec![RowObservation {
            side: Side::Available,
            row_index: 0,
            ratio: parse_ratio("1:2.5").expect("ratio"),
            stock: 100,
            band_fingerprint: 1,
        }];
        let observation = BookObservation::assemble(
            BookIdentity {
                need_asset_id: "divine-orb".to_owned(),
                have_asset_id: "chaos-orb".to_owned(),
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
            need_text: "Divine Orb".to_owned(),
            have_text: "Chaos Orb".to_owned(),
        };
        let context = live_context(
            ProfileId::new(ProfileGame::Poe1, ContentLanguage::English),
            "test-league",
        )
        .expect("context");
        let capture = capture_from_book(
            &book,
            &context,
            Utc::now(),
            ["a".repeat(64), "b".repeat(64)],
            1,
        )
        .expect("capture");

        assert_eq!(
            capture.provenance.profile_sha256,
            ptt_catalog::POE1_CATALOG_SHA256,
            "a POE1 capture must carry the POE1 catalog pin"
        );
    }
}
