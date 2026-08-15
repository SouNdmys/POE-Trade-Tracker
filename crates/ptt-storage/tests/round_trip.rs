//! Round-trip contract: a confirmed capture persists atomically and loads
//! back as byte-equal engine observations.

use chrono::Utc;
use ptt_storage::MarketStore;
use ptt_trade_domain::{
    CaptureConfirmationMode, CaptureProvenance, ClientLanguage, Comparator, ConfirmedCapture,
    ConfirmedOrderRow, Game, MarketAssetId, MarketContext, ObservationIdentity, QuoteSide,
    ReviewAuditEntry, ReviewAuditKind,
};
use serde_json::json;

fn sha() -> String {
    "a".repeat(64)
}

fn capture() -> ConfirmedCapture {
    let identity = ObservationIdentity::try_new(
        "ptt-windows-ocr",
        "1.0.0",
        sha(),
        sha(),
        sha(),
        "poe2-catalog-v1",
        sha(),
        "poe2-recognition-v1",
        sha(),
        "warm-mask-v1",
    )
    .expect("identity");
    let context = MarketContext::try_new_for(
        Game::Poe2,
        ClientLanguage::TraditionalChinese,
        "rise-of-the-abyssal",
        "0.10.3",
        "poe2-zhtw-2560x1440",
        1,
        "poe2-zhtw-route-v1",
        identity.clone(),
    )
    .expect("context");
    let provenance = CaptureProvenance {
        draft_id: "draft-1".to_owned(),
        capture_job_id: "job-1".to_owned(),
        review_revision: 1,
        confirmation_mode: CaptureConfirmationMode::AutomaticConsensus,
        source: "live_watch_double_read".to_owned(),
        evidence_id: "evidence-1".to_owned(),
        evidence_removed: false,
        frame_hashes: vec![sha(), sha()],
        profile_sha256: sha(),
        provider_id: identity.ocr_provider_id.clone(),
        provider_version: identity.ocr_provider_version.clone(),
        model_sha256: identity.ocr_model_sha256.clone(),
        provider_manifest_sha256: identity.ocr_provider_manifest_sha256.clone(),
        parser_assets_sha256: identity.parser_assets_sha256.clone(),
    };
    let rows = vec![
        ConfirmedOrderRow::try_new(
            QuoteSide::Available,
            0,
            Comparator::Exact,
            "1:9.80",
            "920",
            false,
            Some(990_000),
        )
        .expect("row"),
        ConfirmedOrderRow::try_new(
            QuoteSide::Competing,
            5,
            Comparator::GreaterThan,
            "1:9.75",
            "611,620",
            false,
            Some(980_000),
        )
        .expect("row"),
    ];
    ConfirmedCapture::try_new(
        Utc::now(),
        context,
        MarketAssetId::try_new("divine-orb").expect("need"),
        MarketAssetId::try_new("chaos-orb").expect("have"),
        rows,
        provenance,
        "{}".to_owned(),
        "{}".to_owned(),
        vec![ReviewAuditEntry {
            field_path: "need".to_owned(),
            before: json!("divine-orb"),
            after: json!("divine-orb"),
            kind: ReviewAuditKind::AcceptedMachineValue,
        }],
    )
    .expect("capture")
}

#[test]
fn capture_round_trips_to_identical_observations() {
    let mut store = MarketStore::open_in_memory().expect("store");
    let capture = capture();
    store.persist_capture(&capture).expect("persist");

    let observations = store
        .load_observations(&capture.context.stable_key())
        .expect("load");
    assert_eq!(observations.len(), 4, "two rows -> four edges");
    let stored: Vec<_> = observations.iter().map(|o| &o.edge).collect();
    for edge in &capture.quote_edges {
        assert!(
            stored.contains(&edge),
            "edge {} must round-trip unchanged",
            edge.edge_id
        );
    }
    assert!(observations.iter().all(|o| o.snapshot_complete));

    assert!(
        store
            .load_observations("other-context")
            .expect("load")
            .is_empty(),
        "context isolation"
    );
}

#[test]
fn duplicate_snapshot_insert_fails_atomically() {
    let mut store = MarketStore::open_in_memory().expect("store");
    let capture = capture();
    store.persist_capture(&capture).expect("persist");
    assert!(
        store.persist_capture(&capture).is_err(),
        "same snapshot id must not double-insert"
    );
    let observations = store
        .load_observations(&capture.context.stable_key())
        .expect("load");
    assert_eq!(observations.len(), 4, "failed insert left no partial rows");
}
