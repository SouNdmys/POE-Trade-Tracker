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
    /// A rollup value exceeded the i64 column range. Hard error by design:
    /// the day is skipped and reported, never silently saturated.
    #[error("rollup value overflow: {0}")]
    RollupOverflow(String),
    /// A caller request violated an invariant (non-monotonic season start,
    /// malformed day key). Typed so the UI can surface it verbatim.
    #[error("request rejected: {0}")]
    Rejected(String),
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

CREATE TABLE IF NOT EXISTS seasons (
    game TEXT NOT NULL CHECK (game IN ('poe1', 'poe2')),
    season_id TEXT NOT NULL,
    label TEXT NOT NULL CHECK (length(label) > 0),
    started_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (game, season_id),
    UNIQUE (game, started_at)
) STRICT;

CREATE TABLE IF NOT EXISTS pair_day_rollups (
    game TEXT NOT NULL CHECK (game IN ('poe1', 'poe2')),
    utc_day TEXT NOT NULL CHECK (length(utc_day) = 10),
    need_asset_id TEXT NOT NULL,
    have_asset_id TEXT NOT NULL,
    snapshot_count INTEGER NOT NULL CHECK (snapshot_count > 0),
    contexts_merged INTEGER NOT NULL CHECK (contexts_merged > 0),
    first_captured_at TEXT NOT NULL,
    last_captured_at TEXT NOT NULL,
    median_available_rows INTEGER NOT NULL CHECK (median_available_rows >= 0),
    median_competing_rows INTEGER NOT NULL CHECK (median_competing_rows >= 0),
    median_available_sum_need_units INTEGER NOT NULL
        CHECK (median_available_sum_need_units >= 0),
    median_available_sum_have_units INTEGER NOT NULL
        CHECK (median_available_sum_have_units >= 0),
    median_competing_sum_have_units INTEGER NOT NULL
        CHECK (median_competing_sum_have_units >= 0),
    median_competing_sum_need_units INTEGER NOT NULL
        CHECK (median_competing_sum_need_units >= 0),
    median_top_taker_rate_numerator INTEGER
        CHECK (median_top_taker_rate_numerator > 0),
    median_top_taker_rate_denominator INTEGER
        CHECK (median_top_taker_rate_denominator > 0),
    computed_at TEXT NOT NULL,
    PRIMARY KEY (game, utc_day, need_asset_id, have_asset_id),
    CHECK (need_asset_id <> have_asset_id),
    CHECK ((median_top_taker_rate_numerator IS NULL)
        = (median_top_taker_rate_denominator IS NULL))
) STRICT;

CREATE TABLE IF NOT EXISTS rollup_marks (
    game TEXT NOT NULL CHECK (game IN ('poe1', 'poe2')),
    utc_day TEXT NOT NULL CHECK (length(utc_day) = 10),
    snapshot_count INTEGER NOT NULL CHECK (snapshot_count >= 0),
    pair_count INTEGER NOT NULL CHECK (pair_count >= 0),
    computed_at TEXT NOT NULL,
    PRIMARY KEY (game, utc_day)
) STRICT;
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

/// One manual season boundary. The active season for a game is the row with
/// the greatest `started_at`; season rollover is always a user action, never
/// inferred by the program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeasonRow {
    pub game: String,
    pub season_id: String,
    pub label: String,
    pub started_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// A stored market context. `context_json` is the serde `MarketContext`;
/// callers parse it to filter by game — the version string participates in
/// the key hash, so season-scale reads must aggregate across every context
/// of the same game or history fragments at each release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextRow {
    pub context_key: String,
    pub context_json: String,
    pub created_at: DateTime<Utc>,
}

/// One (game, UTC day, book orientation) daily summary. Every median is the
/// lower-middle over that day's per-snapshot folds; the rate median is an
/// actually-observed ratio, never averaged. `None` rate = maker-only day.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairDayRollupRow {
    pub game: String,
    pub utc_day: String,
    pub need_asset_id: String,
    pub have_asset_id: String,
    pub snapshot_count: u32,
    pub contexts_merged: u32,
    pub first_captured_at: DateTime<Utc>,
    pub last_captured_at: DateTime<Utc>,
    pub median_available_rows: u32,
    pub median_competing_rows: u32,
    pub median_available_sum_need_units: i64,
    pub median_available_sum_have_units: i64,
    pub median_competing_sum_have_units: i64,
    pub median_competing_sum_need_units: i64,
    pub median_top_taker_rate: Option<(u64, u64)>,
    pub computed_at: DateTime<Utc>,
}

/// Completion marker for one (game, UTC day). `snapshot_count == 0` records a
/// confirmed-empty day so the builder never revisits it. After raw pruning,
/// marks are the only barrier stopping the builder from recomputing a real
/// rollup into emptiness — deleters must never touch this table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RollupMarkRow {
    pub game: String,
    pub utc_day: String,
    pub snapshot_count: u32,
    pub pair_count: u32,
    pub computed_at: DateTime<Utc>,
}

/// What a deletion actually did. `freed_bytes_estimate` is freelist growth ×
/// page size: space SQLite will reuse, not space returned to the OS (that
/// takes the separate `vacuum`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PurgeStats {
    pub edges_deleted: u64,
    pub snapshots_deleted: u64,
    pub freed_bytes_estimate: u64,
}

/// Size report for the Settings page. COUNT(*) scans — call on page open,
/// never per capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatabaseFootprint {
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub table_rows: Vec<(String, u64)>,
}

/// Upper-bound sentinel for unbounded time reads: RFC3339 text sorts
/// lexicographically, and every stored year starts with "2".
const TIME_UNBOUNDED: &str = "9999-12-31T23:59:59+00:00";

/// Tables reported by `database_footprint`, in display order.
const FOOTPRINT_TABLES: [&str; 6] = [
    "market_contexts",
    "market_snapshots",
    "quote_edges",
    "seasons",
    "pair_day_rollups",
    "rollup_marks",
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
        let cutoff = since.map(|value| value.to_rfc3339()).unwrap_or_default();
        self.load_observations_bounded(context_key, &cutoff, TIME_UNBOUNDED)
    }

    /// `load_observations` with an exclusive upper bound, so a rollup builder
    /// can read exactly one UTC day. Same row mapping and ordering.
    pub fn load_observations_between(
        &self,
        context_key: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<MarketEdgeObservation>, StorageError> {
        self.load_observations_bounded(context_key, &from.to_rfc3339(), &to.to_rfc3339())
    }

    fn load_observations_bounded(
        &self,
        context_key: &str,
        from: &str,
        to_exclusive: &str,
    ) -> Result<Vec<MarketEdgeObservation>, StorageError> {
        // RFC3339 with a fixed offset compares lexicographically in time
        // order, so the TEXT column filters are exact.
        let mut statement = self.connection.prepare_cached(
            "SELECT edge_id, snapshot_id, quote_id, context_key, from_asset_id, to_asset_id,
                 rate_text, rate_numerator, rate_denominator, source_side, execution_type, role,
                 stock, original_need_asset_id, original_have_asset_id, original_row_index,
                 comparator, user_edited, machine_confidence_ppm, captured_at, confirmed_at
             FROM quote_edges
             WHERE context_key = ?1 AND captured_at >= ?2 AND captured_at < ?3
             ORDER BY snapshot_id, original_row_index, role",
        )?;
        let rows = statement.query_map(params![context_key, from, to_exclusive], |row| {
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

    // ----- seasons -----

    /// Inserts a season boundary. `started_at` must be strictly after the
    /// game's current active season: manual rollover is monotonic.
    pub fn start_season(
        &mut self,
        game: &str,
        label: &str,
        started_at: DateTime<Utc>,
    ) -> Result<SeasonRow, StorageError> {
        if let Some(active) = self.active_season(game)?
            && started_at <= active.started_at
        {
            return Err(StorageError::Rejected(format!(
                "season start {} is not after the active season start {}",
                started_at.to_rfc3339(),
                active.started_at.to_rfc3339()
            )));
        }
        let row = SeasonRow {
            game: game.to_owned(),
            season_id: started_at.to_rfc3339(),
            label: label.to_owned(),
            started_at,
            created_at: Utc::now(),
        };
        self.connection.execute(
            "INSERT INTO seasons (game, season_id, label, started_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                row.game,
                row.season_id,
                row.label,
                row.started_at.to_rfc3339(),
                row.created_at.to_rfc3339(),
            ],
        )?;
        Ok(row)
    }

    /// Latest `started_at` for the game; `None` when no season row exists.
    /// Callers treat `None` as "no clamp" — today's behavior exactly.
    pub fn active_season(&self, game: &str) -> Result<Option<SeasonRow>, StorageError> {
        Ok(self.season_rows(game)?.into_iter().next())
    }

    /// All seasons for a game, newest first, for the Settings page.
    pub fn list_seasons(&self, game: &str) -> Result<Vec<SeasonRow>, StorageError> {
        self.season_rows(game)
    }

    fn season_rows(&self, game: &str) -> Result<Vec<SeasonRow>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT game, season_id, label, started_at, created_at FROM seasons
             WHERE game = ?1 ORDER BY started_at DESC",
        )?;
        let rows = statement.query_map(params![game], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut seasons = Vec::new();
        for row in rows {
            let (game, season_id, label, started_at, created_at) = row?;
            seasons.push(SeasonRow {
                game,
                season_id,
                label,
                started_at: timestamp(&started_at)?,
                created_at: timestamp(&created_at)?,
            });
        }
        Ok(seasons)
    }

    // ----- contexts (cross-version aggregation support) -----

    /// Every stored context with its JSON. Callers parse `MarketContext` and
    /// filter by game; storage does not interpret the JSON.
    pub fn list_contexts(&self) -> Result<Vec<ContextRow>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT context_key, context_json, created_at FROM market_contexts
             ORDER BY created_at, context_key",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut contexts = Vec::new();
        for row in rows {
            let (context_key, context_json, created_at) = row?;
            contexts.push(ContextRow {
                context_key,
                context_json,
                created_at: timestamp(&created_at)?,
            });
        }
        Ok(contexts)
    }

    // ----- rollup build support -----

    /// First UTC day ("YYYY-MM-DD") with any edge for this context, via the
    /// (context_key, captured_at) index; `None` for an empty context.
    pub fn earliest_capture_day(&self, context_key: &str) -> Result<Option<String>, StorageError> {
        let earliest: Option<String> = self.connection.query_row(
            "SELECT MIN(captured_at) FROM quote_edges WHERE context_key = ?1",
            params![context_key],
            |row| row.get(0),
        )?;
        Ok(earliest.map(|value| value.chars().take(10).collect()))
    }

    /// One transaction: DELETE the day's rollup rows for the game, INSERT the
    /// replacements, upsert the mark — the idempotent recompute-and-replace
    /// unit. Never deletes marks (they must survive raw pruning).
    pub fn replace_day_rollups(
        &mut self,
        game: &str,
        utc_day: &str,
        rows: &[PairDayRollupRow],
        day_snapshot_count: u32,
        computed_at: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        validate_day_key(utc_day)?;
        for row in rows {
            if row.game != game || row.utc_day != utc_day {
                return Err(StorageError::Rejected(format!(
                    "rollup row {}/{} does not match replace target {game}/{utc_day}",
                    row.game, row.utc_day
                )));
            }
        }
        let pair_count = u32::try_from(rows.len())
            .map_err(|_| StorageError::Rejected("too many rollup rows".into()))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM pair_day_rollups WHERE game = ?1 AND utc_day = ?2",
            params![game, utc_day],
        )?;
        for row in rows {
            let rate_numerator = row
                .median_top_taker_rate
                .map(|(numerator, _)| rate_column(numerator))
                .transpose()?;
            let rate_denominator = row
                .median_top_taker_rate
                .map(|(_, denominator)| rate_column(denominator))
                .transpose()?;
            transaction.execute(
                "INSERT INTO pair_day_rollups (game, utc_day, need_asset_id, have_asset_id,
                     snapshot_count, contexts_merged, first_captured_at, last_captured_at,
                     median_available_rows, median_competing_rows,
                     median_available_sum_need_units, median_available_sum_have_units,
                     median_competing_sum_have_units, median_competing_sum_need_units,
                     median_top_taker_rate_numerator, median_top_taker_rate_denominator,
                     computed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17)",
                params![
                    row.game,
                    row.utc_day,
                    row.need_asset_id,
                    row.have_asset_id,
                    i64::from(row.snapshot_count),
                    i64::from(row.contexts_merged),
                    row.first_captured_at.to_rfc3339(),
                    row.last_captured_at.to_rfc3339(),
                    i64::from(row.median_available_rows),
                    i64::from(row.median_competing_rows),
                    row.median_available_sum_need_units,
                    row.median_available_sum_have_units,
                    row.median_competing_sum_have_units,
                    row.median_competing_sum_need_units,
                    rate_numerator,
                    rate_denominator,
                    row.computed_at.to_rfc3339(),
                ],
            )?;
        }
        transaction.execute(
            "INSERT OR REPLACE INTO rollup_marks
                 (game, utc_day, snapshot_count, pair_count, computed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                game,
                utc_day,
                i64::from(day_snapshot_count),
                i64::from(pair_count),
                computed_at.to_rfc3339(),
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// All marks for a game ordered by day, so the builder can diff candidate
    /// days against completed days in one read.
    pub fn list_rollup_marks(&self, game: &str) -> Result<Vec<RollupMarkRow>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT game, utc_day, snapshot_count, pair_count, computed_at
             FROM rollup_marks WHERE game = ?1 ORDER BY utc_day",
        )?;
        let rows = statement.query_map(params![game], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut marks = Vec::new();
        for row in rows {
            let (game, utc_day, snapshot_count, pair_count, computed_at) = row?;
            marks.push(RollupMarkRow {
                game,
                utc_day,
                snapshot_count: u32::try_from(snapshot_count)
                    .map_err(|_| StorageError::Corrupt("mark snapshot count".into()))?,
                pair_count: u32::try_from(pair_count)
                    .map_err(|_| StorageError::Corrupt("mark pair count".into()))?,
                computed_at: timestamp(&computed_at)?,
            });
        }
        Ok(marks)
    }

    /// Rollup rows for (game, from_day..=to_day) ordered by (day, need, have).
    /// A PK prefix range scan, never a table scan.
    pub fn load_rollups(
        &self,
        game: &str,
        from_day: &str,
        to_day: &str,
    ) -> Result<Vec<PairDayRollupRow>, StorageError> {
        validate_day_key(from_day)?;
        validate_day_key(to_day)?;
        let mut statement = self.connection.prepare_cached(
            "SELECT game, utc_day, need_asset_id, have_asset_id, snapshot_count,
                 contexts_merged, first_captured_at, last_captured_at,
                 median_available_rows, median_competing_rows,
                 median_available_sum_need_units, median_available_sum_have_units,
                 median_competing_sum_have_units, median_competing_sum_need_units,
                 median_top_taker_rate_numerator, median_top_taker_rate_denominator,
                 computed_at
             FROM pair_day_rollups
             WHERE game = ?1 AND utc_day >= ?2 AND utc_day <= ?3
             ORDER BY utc_day, need_asset_id, have_asset_id",
        )?;
        let rows = statement.query_map(params![game, from_day, to_day], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, i64>(11)?,
                (
                    row.get::<_, i64>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, Option<i64>>(14)?,
                    row.get::<_, Option<i64>>(15)?,
                    row.get::<_, String>(16)?,
                ),
            ))
        })?;
        let mut rollups = Vec::new();
        for row in rows {
            let (
                game,
                utc_day,
                need_asset_id,
                have_asset_id,
                snapshot_count,
                contexts_merged,
                first_captured_at,
                last_captured_at,
                median_available_rows,
                median_competing_rows,
                median_available_sum_need_units,
                median_available_sum_have_units,
                (
                    median_competing_sum_have_units,
                    median_competing_sum_need_units,
                    rate_numerator,
                    rate_denominator,
                    computed_at,
                ),
            ) = row?;
            let median_top_taker_rate = match (rate_numerator, rate_denominator) {
                (Some(numerator), Some(denominator)) => Some((
                    u64::try_from(numerator)
                        .map_err(|_| StorageError::Corrupt("rollup rate numerator".into()))?,
                    u64::try_from(denominator)
                        .map_err(|_| StorageError::Corrupt("rollup rate denominator".into()))?,
                )),
                (None, None) => None,
                _ => return Err(StorageError::Corrupt("half-null rollup rate".into())),
            };
            rollups.push(PairDayRollupRow {
                game,
                utc_day,
                need_asset_id,
                have_asset_id,
                snapshot_count: u32::try_from(snapshot_count)
                    .map_err(|_| StorageError::Corrupt("rollup snapshot count".into()))?,
                contexts_merged: u32::try_from(contexts_merged)
                    .map_err(|_| StorageError::Corrupt("rollup context count".into()))?,
                first_captured_at: timestamp(&first_captured_at)?,
                last_captured_at: timestamp(&last_captured_at)?,
                median_available_rows: u32::try_from(median_available_rows)
                    .map_err(|_| StorageError::Corrupt("rollup available rows".into()))?,
                median_competing_rows: u32::try_from(median_competing_rows)
                    .map_err(|_| StorageError::Corrupt("rollup competing rows".into()))?,
                median_available_sum_need_units,
                median_available_sum_have_units,
                median_competing_sum_have_units,
                median_competing_sum_need_units,
                median_top_taker_rate,
                computed_at: timestamp(&computed_at)?,
            });
        }
        Ok(rollups)
    }

    // ----- retention / purge -----

    /// Distinct (need, have) book orientations with raw edges on the day
    /// across the given contexts — the pruner's ground truth, read from the
    /// edges themselves, independent of marks.
    pub fn raw_pairs_on_day(
        &self,
        context_keys: &[String],
        utc_day: &str,
    ) -> Result<Vec<(String, String)>, StorageError> {
        let (from, to_exclusive) = day_bounds(utc_day)?;
        let mut pairs = std::collections::BTreeSet::new();
        let mut statement = self.connection.prepare_cached(
            "SELECT DISTINCT original_need_asset_id, original_have_asset_id
             FROM quote_edges
             WHERE context_key = ?1 AND captured_at >= ?2 AND captured_at < ?3",
        )?;
        for key in context_keys {
            let rows = statement.query_map(params![key, from, to_exclusive], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                pairs.insert(row?);
            }
        }
        Ok(pairs.into_iter().collect())
    }

    /// One transaction: delete the day's quote_edges then market_snapshots
    /// for the given contexts. Rollups and marks are untouched.
    pub fn delete_raw_day(
        &mut self,
        context_keys: &[String],
        utc_day: &str,
    ) -> Result<PurgeStats, StorageError> {
        let (from, to_exclusive) = day_bounds(utc_day)?;
        self.delete_raw_window(context_keys, &from, &to_exclusive)
    }

    /// One transaction: delete all quote_edges then market_snapshots strictly
    /// before the cutoff for the given contexts. Contexts, rollups, and marks
    /// are untouched.
    pub fn purge_before(
        &mut self,
        context_keys: &[String],
        cutoff: DateTime<Utc>,
    ) -> Result<PurgeStats, StorageError> {
        self.delete_raw_window(context_keys, "", &cutoff.to_rfc3339())
    }

    fn delete_raw_window(
        &mut self,
        context_keys: &[String],
        from: &str,
        to_exclusive: &str,
    ) -> Result<PurgeStats, StorageError> {
        let page_size = self.pragma_u64("page_size")?;
        let freelist_before = self.pragma_u64("freelist_count")?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut stats = PurgeStats::default();
        for key in context_keys {
            // Edges first: the snapshot foreign key makes the reverse order fail.
            let edges = transaction.execute(
                "DELETE FROM quote_edges
                 WHERE context_key = ?1 AND captured_at >= ?2 AND captured_at < ?3",
                params![key, from, to_exclusive],
            )?;
            let snapshots = transaction.execute(
                "DELETE FROM market_snapshots
                 WHERE context_key = ?1 AND captured_at >= ?2 AND captured_at < ?3",
                params![key, from, to_exclusive],
            )?;
            stats.edges_deleted += u64::try_from(edges)
                .map_err(|_| StorageError::Corrupt("edge delete count".into()))?;
            stats.snapshots_deleted += u64::try_from(snapshots)
                .map_err(|_| StorageError::Corrupt("snapshot delete count".into()))?;
        }
        transaction.commit()?;
        let freelist_after = self.pragma_u64("freelist_count")?;
        stats.freed_bytes_estimate = freelist_after.saturating_sub(freelist_before) * page_size;
        Ok(stats)
    }

    /// Rollup + mark rows strictly before the day — the explicit "also drop
    /// pre-season summaries" Settings action. Never called by `purge_before`.
    pub fn delete_rollups_before(
        &mut self,
        game: &str,
        utc_day: &str,
    ) -> Result<u64, StorageError> {
        validate_day_key(utc_day)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rollups = transaction.execute(
            "DELETE FROM pair_day_rollups WHERE game = ?1 AND utc_day < ?2",
            params![game, utc_day],
        )?;
        let marks = transaction.execute(
            "DELETE FROM rollup_marks WHERE game = ?1 AND utc_day < ?2",
            params![game, utc_day],
        )?;
        transaction.commit()?;
        u64::try_from(rollups + marks)
            .map_err(|_| StorageError::Corrupt("rollup delete count".into()))
    }

    /// Totals and per-table row counts for the Settings page. COUNT(*)
    /// scans — call on page open, never per capture.
    pub fn database_footprint(&self) -> Result<DatabaseFootprint, StorageError> {
        let page_size = self.pragma_u64("page_size")?;
        let total_bytes = self.pragma_u64("page_count")?.saturating_mul(page_size);
        let free_bytes = self.pragma_u64("freelist_count")?.saturating_mul(page_size);
        let mut table_rows = Vec::with_capacity(FOOTPRINT_TABLES.len());
        for table in FOOTPRINT_TABLES {
            // Table names come from the fixed list above, never from input.
            let count: i64 =
                self.connection
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })?;
            table_rows.push((
                table.to_owned(),
                u64::try_from(count).map_err(|_| StorageError::Corrupt("table count".into()))?,
            ));
        }
        Ok(DatabaseFootprint {
            total_bytes,
            free_bytes,
            table_rows,
        })
    }

    /// VACUUM; returns bytes returned to the filesystem. Separate from purge:
    /// it rewrites the whole file, transiently needs ~2x disk, and blocks
    /// writers for its full duration — never run while a watch session is on.
    pub fn vacuum(&mut self) -> Result<u64, StorageError> {
        let page_size = self.pragma_u64("page_size")?;
        let pages_before = self.pragma_u64("page_count")?;
        self.connection.execute_batch("VACUUM")?;
        let pages_after = self.pragma_u64("page_count")?;
        Ok(pages_before
            .saturating_sub(pages_after)
            .saturating_mul(page_size))
    }

    fn pragma_u64(&self, name: &str) -> Result<u64, StorageError> {
        // Pragma names come from call sites above, never from input.
        let value: i64 = self
            .connection
            .query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))?;
        u64::try_from(value).map_err(|_| StorageError::Corrupt(format!("pragma {name}")))
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

/// Validates a "YYYY-MM-DD" day key and returns the [start, next-day-start)
/// RFC3339 bounds for TEXT comparison against stored `captured_at`.
fn day_bounds(utc_day: &str) -> Result<(String, String), StorageError> {
    if utc_day.len() != 10 {
        return Err(StorageError::Rejected(format!("bad day key: {utc_day}")));
    }
    let date = chrono::NaiveDate::parse_from_str(utc_day, "%Y-%m-%d")
        .map_err(|error| StorageError::Rejected(format!("bad day key {utc_day}: {error}")))?;
    let start = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| StorageError::Rejected(format!("bad day key: {utc_day}")))?
        .and_utc();
    let next = date
        .succ_opt()
        .and_then(|day| day.and_hms_opt(0, 0, 0))
        .ok_or_else(|| StorageError::Rejected(format!("day out of range: {utc_day}")))?
        .and_utc();
    Ok((start.to_rfc3339(), next.to_rfc3339()))
}

fn validate_day_key(utc_day: &str) -> Result<(), StorageError> {
    day_bounds(utc_day).map(|_| ())
}

/// A rollup rate half on its way into an INTEGER column. Rates come from
/// observed panel ratios (u64 well under i64::MAX); failure means corruption.
fn rate_column(value: u64) -> Result<i64, StorageError> {
    i64::try_from(value).map_err(|_| StorageError::Corrupt("rollup rate".into()))
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
