//! SQLite persistence for market observations.
//!
//! Lean re-baseline of POE1's storage discipline: STRICT tables, WAL, foreign
//! keys, one transaction per accepted book, quote edges stored exactly as the
//! domain built them. The enterprise audit ceremony (record-state audits,
//! review audit tables) is deliberately dropped per the plan; provenance that
//! matters for auto-accept trust (context identity, confidences, capture
//! times, draft JSON) is kept.

use std::path::Path;

use chrono::{DateTime, Utc};
use ptt_trade_domain::{
    Comparator, ConfirmedCapture, ExecutionType, MarketAssetId, MarketEdgeObservation, QuoteEdge,
    QuoteEdgeRole, QuoteSide, Ratio, SnapshotRecordStatus,
};
use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("stored value invalid: {0}")]
    Corrupt(String),
}

const BASELINE_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS market_contexts (
    context_key TEXT PRIMARY KEY,
    context_json TEXT NOT NULL,
    created_at TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS market_snapshots (
    snapshot_id TEXT PRIMARY KEY,
    capture_id TEXT NOT NULL,
    context_key TEXT NOT NULL REFERENCES market_contexts(context_key),
    need_asset_id TEXT NOT NULL,
    have_asset_id TEXT NOT NULL,
    captured_at TEXT NOT NULL,
    confirmed_at TEXT NOT NULL,
    machine_draft_json TEXT NOT NULL,
    confirmation_mode TEXT NOT NULL DEFAULT 'automatic_consensus',
    frame_hash_first TEXT NOT NULL DEFAULT '',
    frame_hash_second TEXT NOT NULL DEFAULT '',
    provider_id TEXT NOT NULL DEFAULT '',
    model_sha256 TEXT NOT NULL DEFAULT '',
    review_json TEXT NOT NULL DEFAULT '',
    CHECK (need_asset_id <> have_asset_id)
) STRICT;

CREATE TABLE IF NOT EXISTS quote_edges (
    edge_id TEXT PRIMARY KEY,
    snapshot_id TEXT NOT NULL REFERENCES market_snapshots(snapshot_id),
    quote_id TEXT NOT NULL,
    context_key TEXT NOT NULL,
    from_asset_id TEXT NOT NULL,
    to_asset_id TEXT NOT NULL,
    rate_text TEXT NOT NULL,
    rate_numerator INTEGER NOT NULL CHECK (rate_numerator > 0),
    rate_denominator INTEGER NOT NULL CHECK (rate_denominator > 0),
    source_side TEXT NOT NULL CHECK (source_side IN ('available', 'competing')),
    execution_type TEXT NOT NULL CHECK (execution_type IN ('taker', 'maker_reference')),
    role TEXT NOT NULL CHECK (role IN (
        'available_taker', 'available_reverse_maker_reference',
        'competing_maker_reference', 'competing_reverse_taker')),
    stock INTEGER NOT NULL CHECK (stock > 0),
    original_need_asset_id TEXT NOT NULL,
    original_have_asset_id TEXT NOT NULL,
    original_row_index INTEGER NOT NULL CHECK (original_row_index >= 0),
    comparator TEXT NOT NULL CHECK (comparator IN ('exact', 'less_than', 'greater_than')),
    user_edited INTEGER NOT NULL CHECK (user_edited IN (0, 1)),
    machine_confidence_ppm INTEGER,
    captured_at TEXT NOT NULL,
    confirmed_at TEXT NOT NULL
) STRICT;

CREATE INDEX IF NOT EXISTS quote_edges_by_context ON quote_edges(context_key);
CREATE INDEX IF NOT EXISTS quote_edges_by_snapshot ON quote_edges(snapshot_id);
CREATE INDEX IF NOT EXISTS snapshots_by_pair
    ON market_snapshots(context_key, need_asset_id, have_asset_id);
CREATE INDEX IF NOT EXISTS quote_edges_by_context_time
    ON quote_edges(context_key, captured_at);
"#;

/// Columns added after the first shipped baseline; applied idempotently so
/// databases created before them keep working.
const SNAPSHOT_UPGRADE_COLUMNS: [(&str, &str); 6] = [
    (
        "confirmation_mode",
        "TEXT NOT NULL DEFAULT 'automatic_consensus'",
    ),
    ("frame_hash_first", "TEXT NOT NULL DEFAULT ''"),
    ("frame_hash_second", "TEXT NOT NULL DEFAULT ''"),
    ("provider_id", "TEXT NOT NULL DEFAULT ''"),
    ("model_sha256", "TEXT NOT NULL DEFAULT ''"),
    ("review_json", "TEXT NOT NULL DEFAULT ''"),
];

pub struct MarketStore {
    connection: Connection,
}

impl MarketStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        Self::initialize(connection)
    }

    pub fn open_in_memory() -> Result<Self, StorageError> {
        Self::initialize(Connection::open_in_memory()?)
    }

    fn initialize(connection: Connection) -> Result<Self, StorageError> {
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        // Without a busy timeout the first contended access fails instantly
        // (SQLITE_BUSY) and the book is lost; 5s rides out a concurrent
        // probe/app sharing the file.
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(BASELINE_SCHEMA)?;
        Self::apply_upgrades(&connection)?;
        Ok(Self { connection })
    }

    fn apply_upgrades(connection: &Connection) -> Result<(), StorageError> {
        let mut existing = std::collections::BTreeSet::new();
        {
            let mut statement = connection.prepare("PRAGMA table_info(market_snapshots)")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                existing.insert(row.get::<_, String>(1)?);
            }
        }
        for (name, definition) in SNAPSHOT_UPGRADE_COLUMNS {
            if !existing.contains(name) {
                connection.execute_batch(&format!(
                    "ALTER TABLE market_snapshots ADD COLUMN {name} {definition};"
                ))?;
            }
        }
        Ok(())
    }

    /// Persists one accepted book atomically: context (idempotent), snapshot,
    /// and every quote edge exactly as the domain constructed them.
    pub fn persist_capture(&mut self, capture: &ConfirmedCapture) -> Result<(), StorageError> {
        // Immediate: take the write lock up front instead of upgrading a
        // deferred transaction mid-way (the classic contention failure).
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT OR IGNORE INTO market_contexts (context_key, context_json, created_at)
             VALUES (?1, ?2, ?3)",
            params![
                capture.context.stable_key(),
                serde_json::to_string(&capture.context)
                    .map_err(|error| StorageError::Corrupt(error.to_string()))?,
                capture.confirmed_at.to_rfc3339(),
            ],
        )?;
        transaction.execute(
            "INSERT INTO market_snapshots (snapshot_id, capture_id, context_key,
                 need_asset_id, have_asset_id, captured_at, confirmed_at, machine_draft_json,
                 confirmation_mode, frame_hash_first, frame_hash_second, provider_id,
                 model_sha256, review_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                capture.snapshot_id,
                capture.capture_id,
                capture.context.stable_key(),
                capture.need_asset_id.as_str(),
                capture.have_asset_id.as_str(),
                capture.captured_at.to_rfc3339(),
                capture.confirmed_at.to_rfc3339(),
                capture.machine_draft_json,
                enum_key(&capture.provenance.confirmation_mode)?,
                capture
                    .provenance
                    .frame_hashes
                    .first()
                    .cloned()
                    .unwrap_or_default(),
                capture
                    .provenance
                    .frame_hashes
                    .get(1)
                    .cloned()
                    .unwrap_or_default(),
                capture.provenance.provider_id,
                capture.provenance.model_sha256,
                capture.review_json,
            ],
        )?;
        for edge in &capture.quote_edges {
            transaction.execute(
                "INSERT INTO quote_edges (edge_id, snapshot_id, quote_id, context_key,
                     from_asset_id, to_asset_id, rate_text, rate_numerator, rate_denominator,
                     source_side, execution_type, role, stock,
                     original_need_asset_id, original_have_asset_id, original_row_index,
                     comparator, user_edited, machine_confidence_ppm, captured_at, confirmed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                     ?16, ?17, ?18, ?19, ?20, ?21)",
                params![
                    edge.edge_id,
                    edge.snapshot_id,
                    edge.quote_id,
                    edge.context_key,
                    edge.from_asset_id.as_str(),
                    edge.to_asset_id.as_str(),
                    edge.rate.text,
                    i64::try_from(edge.rate.numerator)
                        .map_err(|_| StorageError::Corrupt("rate numerator".into()))?,
                    i64::try_from(edge.rate.denominator)
                        .map_err(|_| StorageError::Corrupt("rate denominator".into()))?,
                    enum_key(&edge.source_side)?,
                    enum_key(&edge.execution_type)?,
                    enum_key(&edge.role)?,
                    i64::try_from(edge.stock).map_err(|_| StorageError::Corrupt("stock".into()))?,
                    edge.original_need_asset_id.as_str(),
                    edge.original_have_asset_id.as_str(),
                    i64::from(edge.original_row_index),
                    enum_key(&edge.comparator)?,
                    i64::from(edge.user_edited),
                    edge.machine_confidence_ppm.map(i64::from),
                    edge.captured_at.to_rfc3339(),
                    edge.confirmed_at.to_rfc3339(),
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Loads every stored edge for a context as engine-ready observations.
    /// All stored snapshots are complete and active by construction in the
    /// auto-accept flow; the coherent-book layer still reduces to the newest
    /// snapshot per pair.
    /// Loads stored edges for a context as engine-ready observations.
    /// `since` bounds the read to edges captured at or after the cutoff --
    /// analysis only consumes the freshness window, and an unbounded read
    /// grows O(season) on the accept path. Pass `None` for full history.
    pub fn load_observations(
        &self,
        context_key: &str,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<MarketEdgeObservation>, StorageError> {
        // RFC3339 with a fixed offset compares lexicographically in time
        // order, so the TEXT column filter is exact.
        let cutoff = since.map(|value| value.to_rfc3339()).unwrap_or_default();
        let mut statement = self.connection.prepare_cached(
            "SELECT edge_id, snapshot_id, quote_id, context_key, from_asset_id, to_asset_id,
                 rate_text, rate_numerator, rate_denominator, source_side, execution_type, role,
                 stock, original_need_asset_id, original_have_asset_id, original_row_index,
                 comparator, user_edited, machine_confidence_ppm, captured_at, confirmed_at
             FROM quote_edges WHERE context_key = ?1 AND captured_at >= ?2
             ORDER BY snapshot_id, original_row_index, role",
        )?;
        let rows = statement.query_map(params![context_key, cutoff], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, i64>(12)?,
                (
                    row.get::<_, String>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, i64>(15)?,
                    row.get::<_, String>(16)?,
                    row.get::<_, i64>(17)?,
                    row.get::<_, Option<i64>>(18)?,
                    row.get::<_, String>(19)?,
                    row.get::<_, String>(20)?,
                ),
            ))
        })?;

        let mut observations = Vec::new();
        for row in rows {
            let (
                edge_id,
                snapshot_id,
                quote_id,
                stored_context,
                from_asset,
                to_asset,
                rate_text,
                rate_numerator,
                rate_denominator,
                source_side,
                execution_type,
                role,
                stock,
                (
                    original_need,
                    original_have,
                    original_row_index,
                    comparator,
                    user_edited,
                    confidence,
                    captured_at,
                    confirmed_at,
                ),
            ) = row?;
            let edge = QuoteEdge {
                edge_id,
                snapshot_id,
                quote_id,
                context_key: stored_context,
                from_asset_id: asset(&from_asset)?,
                to_asset_id: asset(&to_asset)?,
                rate: Ratio {
                    text: rate_text,
                    numerator: u64::try_from(rate_numerator)
                        .map_err(|_| StorageError::Corrupt("rate numerator".into()))?,
                    denominator: u64::try_from(rate_denominator)
                        .map_err(|_| StorageError::Corrupt("rate denominator".into()))?,
                },
                source_side: enum_value::<QuoteSide>(&source_side)?,
                execution_type: enum_value::<ExecutionType>(&execution_type)?,
                role: enum_value::<QuoteEdgeRole>(&role)?,
                stock: u64::try_from(stock).map_err(|_| StorageError::Corrupt("stock".into()))?,
                original_need_asset_id: asset(&original_need)?,
                original_have_asset_id: asset(&original_have)?,
                original_row_index: u8::try_from(original_row_index)
                    .map_err(|_| StorageError::Corrupt("row index".into()))?,
                comparator: enum_value::<Comparator>(&comparator)?,
                user_edited: user_edited != 0,
                machine_confidence_ppm: confidence
                    .map(u32::try_from)
                    .transpose()
                    .map_err(|_| StorageError::Corrupt("confidence".into()))?,
                captured_at: timestamp(&captured_at)?,
                confirmed_at: timestamp(&confirmed_at)?,
            };
            observations.push(MarketEdgeObservation {
                edge,
                snapshot_complete: true,
                record_status: SnapshotRecordStatus::Active,
                record_revision: 1,
                record_reason: None,
            });
        }
        Ok(observations)
    }
}

fn asset(value: &str) -> Result<MarketAssetId, StorageError> {
    MarketAssetId::try_new(value).map_err(|error| StorageError::Corrupt(error.to_string()))
}

fn timestamp(value: &str) -> Result<DateTime<Utc>, StorageError> {
    DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|error| StorageError::Corrupt(error.to_string()))
}

/// Serializes a domain enum through its serde snake_case representation so
/// storage and domain can never disagree on spelling.
fn enum_key<T: serde::Serialize>(value: &T) -> Result<String, StorageError> {
    match serde_json::to_value(value).map_err(|error| StorageError::Corrupt(error.to_string()))? {
        serde_json::Value::String(text) => Ok(text),
        other => Err(StorageError::Corrupt(format!("non-string enum: {other}"))),
    }
}

fn enum_value<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, StorageError> {
    serde_json::from_value(serde_json::Value::String(text.to_owned()))
        .map_err(|error| StorageError::Corrupt(error.to_string()))
}
