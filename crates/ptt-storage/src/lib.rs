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
    ended_at TEXT,
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

-- 官方通货交易所小时史（P11）。资产列存 GGG 原始 Metadata 路径而不是 catalog id：
-- 映射表会迭代，原始路径让映射每改一版全部历史立刻升级、零重抓。
-- exchange_hours 的一行 = "这一小时抓过了"，它就是抓取水位；market_count=0 是
-- 确认为空的小时（赛季前），和"还没抓"由行的存在与否区分。
CREATE TABLE IF NOT EXISTS exchange_hours (
    game TEXT NOT NULL CHECK (game IN ('poe1', 'poe2')),
    league TEXT NOT NULL CHECK (length(league) > 0),
    hour_ts INTEGER NOT NULL CHECK (hour_ts > 0 AND hour_ts % 3600 = 0),
    market_count INTEGER NOT NULL CHECK (market_count >= 0),
    fetched_at TEXT NOT NULL,
    PRIMARY KEY (game, league, hour_ts)
) STRICT;

CREATE TABLE IF NOT EXISTS exchange_hour_markets (
    game TEXT NOT NULL CHECK (game IN ('poe1', 'poe2')),
    league TEXT NOT NULL CHECK (length(league) > 0),
    hour_ts INTEGER NOT NULL CHECK (hour_ts > 0 AND hour_ts % 3600 = 0),
    asset_a TEXT NOT NULL,
    asset_b TEXT NOT NULL,
    volume_a INTEGER NOT NULL CHECK (volume_a >= 0),
    volume_b INTEGER NOT NULL CHECK (volume_b >= 0),
    lowest_stock_a INTEGER NOT NULL CHECK (lowest_stock_a >= 0),
    lowest_stock_b INTEGER NOT NULL CHECK (lowest_stock_b >= 0),
    highest_stock_a INTEGER NOT NULL CHECK (highest_stock_a >= 0),
    highest_stock_b INTEGER NOT NULL CHECK (highest_stock_b >= 0),
    lowest_ratio_a TEXT NOT NULL,
    lowest_ratio_b TEXT NOT NULL,
    highest_ratio_a TEXT NOT NULL,
    highest_ratio_b TEXT NOT NULL,
    PRIMARY KEY (game, league, hour_ts, asset_a, asset_b),
    CHECK (asset_a < asset_b)
) STRICT;

-- 小时折成的日线，永久保留（一年后 CDN 过期，这里就是唯一副本）。
-- exchange_day_marks 遵守 R4 纪律：任何删除路径都不能碰它；
-- exchange_hours 的抓取 mark 不在 R4 范围（CDN 一年内可重抓，删了能修回来）。
CREATE TABLE IF NOT EXISTS exchange_day_markets (
    game TEXT NOT NULL CHECK (game IN ('poe1', 'poe2')),
    league TEXT NOT NULL CHECK (length(league) > 0),
    utc_day TEXT NOT NULL CHECK (length(utc_day) = 10),
    asset_a TEXT NOT NULL,
    asset_b TEXT NOT NULL,
    volume_a INTEGER NOT NULL CHECK (volume_a >= 0),
    volume_b INTEGER NOT NULL CHECK (volume_b >= 0),
    hours_covered INTEGER NOT NULL CHECK (hours_covered > 0),
    computed_at TEXT NOT NULL,
    PRIMARY KEY (game, league, utc_day, asset_a, asset_b),
    CHECK (asset_a < asset_b)
) STRICT;

CREATE TABLE IF NOT EXISTS exchange_day_marks (
    game TEXT NOT NULL CHECK (game IN ('poe1', 'poe2')),
    league TEXT NOT NULL CHECK (length(league) > 0),
    utc_day TEXT NOT NULL CHECK (length(utc_day) = 10),
    hour_count INTEGER NOT NULL CHECK (hour_count >= 0),
    market_count INTEGER NOT NULL CHECK (market_count >= 0),
    computed_at TEXT NOT NULL,
    PRIMARY KEY (game, league, utc_day)
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
    /// `None` while the season is still running. An ended season caps every
    /// reading window the way `started_at` floors it — statistics stop at
    /// the boundary, capture itself is never blocked.
    pub ended_at: Option<DateTime<Utc>>,
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

/// 交易所小时数据的一条市场行。`hour_ts` 随行携带，范围读取保持扁平；
/// 写入时它必须等于 replace 目标，防止一批行悄悄写进错的小时。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExchangeHourMarketRow {
    pub hour_ts: i64,
    pub asset_a: String,
    pub asset_b: String,
    pub volume_a: u64,
    pub volume_b: u64,
    pub lowest_stock_a: u64,
    pub lowest_stock_b: u64,
    pub highest_stock_a: u64,
    pub highest_stock_b: u64,
    pub lowest_ratio_a: String,
    pub lowest_ratio_b: String,
    pub highest_ratio_a: String,
    pub highest_ratio_b: String,
}

/// "这一小时抓过了"的水位记录。market_count=0 = 确认为空（赛季前）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExchangeHourMark {
    pub hour_ts: i64,
    pub market_count: u32,
    pub fetched_at: DateTime<Utc>,
}

/// 日折行：一天内两侧成交量的合计。比值即该日 VWAP，永久保留。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExchangeDayMarketRow {
    pub utc_day: String,
    pub asset_a: String,
    pub asset_b: String,
    pub volume_a: u64,
    pub volume_b: u64,
    pub hours_covered: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExchangeDayMark {
    pub utc_day: String,
    pub hour_count: u32,
    pub market_count: u32,
    pub computed_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ExchangeHourPruneStats {
    pub hours_deleted: u64,
    pub markets_deleted: u64,
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
const FOOTPRINT_TABLES: [&str; 10] = [
    "market_contexts",
    "market_snapshots",
    "quote_edges",
    "seasons",
    "pair_day_rollups",
    "rollup_marks",
    "exchange_hours",
    "exchange_hour_markets",
    "exchange_day_markets",
    "exchange_day_marks",
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
        // 老库的 seasons 表没有结束时间列;同样幂等补上。
        let mut season_columns = std::collections::BTreeSet::new();
        {
            let mut statement = connection.prepare("PRAGMA table_info(seasons)")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                season_columns.insert(row.get::<_, String>(1)?);
            }
        }
        if !season_columns.contains("ended_at") {
            connection.execute_batch("ALTER TABLE seasons ADD COLUMN ended_at TEXT;")?;
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
    ///
    /// 开新赛季会顺手给上一季补一个结束点(若它还没有):时间线上不该有
    /// 一段"两个赛季同时进行"的重叠。
    pub fn start_season(
        &mut self,
        game: &str,
        label: &str,
        started_at: DateTime<Utc>,
    ) -> Result<SeasonRow, StorageError> {
        if let Some(active) = self.active_season(game)? {
            if started_at <= active.started_at {
                return Err(StorageError::Rejected(format!(
                    "season start {} is not after the active season start {}",
                    started_at.to_rfc3339(),
                    active.started_at.to_rfc3339()
                )));
            }
            if active.ended_at.is_none() {
                self.connection.execute(
                    "UPDATE seasons SET ended_at = ?3
                     WHERE game = ?1 AND season_id = ?2 AND ended_at IS NULL",
                    params![active.game, active.season_id, started_at.to_rfc3339()],
                )?;
            }
        }
        let row = SeasonRow {
            game: game.to_owned(),
            season_id: started_at.to_rfc3339(),
            label: label.to_owned(),
            started_at,
            ended_at: None,
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

    /// Records (or corrects) when the active season ended. Statistics stop
    /// counting there; capturing is never blocked by an ended season.
    pub fn end_season(
        &mut self,
        game: &str,
        ended_at: DateTime<Utc>,
    ) -> Result<SeasonRow, StorageError> {
        let Some(mut active) = self.active_season(game)? else {
            return Err(StorageError::Rejected(
                "no season configured for this game".to_owned(),
            ));
        };
        if ended_at <= active.started_at {
            return Err(StorageError::Rejected(format!(
                "season end {} is not after its start {}",
                ended_at.to_rfc3339(),
                active.started_at.to_rfc3339()
            )));
        }
        self.connection.execute(
            "UPDATE seasons SET ended_at = ?3 WHERE game = ?1 AND season_id = ?2",
            params![active.game, active.season_id, ended_at.to_rfc3339()],
        )?;
        active.ended_at = Some(ended_at);
        Ok(active)
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
            "SELECT game, season_id, label, started_at, ended_at, created_at FROM seasons
             WHERE game = ?1 ORDER BY started_at DESC",
        )?;
        let rows = statement.query_map(params![game], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        let mut seasons = Vec::new();
        for row in rows {
            let (game, season_id, label, started_at, ended_at, created_at) = row?;
            seasons.push(SeasonRow {
                game,
                season_id,
                label,
                started_at: timestamp(&started_at)?,
                ended_at: ended_at.as_deref().map(timestamp).transpose()?,
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

    // ----- exchange history (official hourly market feed, P11) -----

    /// 一个事务写完一个 (game, league, hour)：先写小时 mark 再写行。
    /// mark 的 market_count 从 rows 派生——少一个能说谎的地方；确认为空的
    /// 小时就是 rows 为空、mark 记 0，mark 的存在本身就是抓取水位。
    pub fn replace_exchange_hour(
        &mut self,
        game: &str,
        league: &str,
        hour_ts: i64,
        rows: &[ExchangeHourMarketRow],
        fetched_at: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        validate_hour_ts(hour_ts)?;
        for row in rows {
            if row.hour_ts != hour_ts {
                return Err(StorageError::Rejected(format!(
                    "exchange row hour {} does not match replace target {hour_ts}",
                    row.hour_ts
                )));
            }
        }
        let market_count = i64::try_from(rows.len())
            .map_err(|_| StorageError::Rejected("too many exchange rows".into()))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT OR REPLACE INTO exchange_hours
                 (game, league, hour_ts, market_count, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![game, league, hour_ts, market_count, fetched_at.to_rfc3339()],
        )?;
        transaction.execute(
            "DELETE FROM exchange_hour_markets
             WHERE game = ?1 AND league = ?2 AND hour_ts = ?3",
            params![game, league, hour_ts],
        )?;
        for row in rows {
            transaction.execute(
                "INSERT INTO exchange_hour_markets (game, league, hour_ts, asset_a, asset_b,
                     volume_a, volume_b, lowest_stock_a, lowest_stock_b,
                     highest_stock_a, highest_stock_b,
                     lowest_ratio_a, lowest_ratio_b, highest_ratio_a, highest_ratio_b)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    game,
                    league,
                    hour_ts,
                    row.asset_a,
                    row.asset_b,
                    volume_column(row.volume_a)?,
                    volume_column(row.volume_b)?,
                    volume_column(row.lowest_stock_a)?,
                    volume_column(row.lowest_stock_b)?,
                    volume_column(row.highest_stock_a)?,
                    volume_column(row.highest_stock_b)?,
                    row.lowest_ratio_a,
                    row.lowest_ratio_b,
                    row.highest_ratio_a,
                    row.highest_ratio_b,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// 已抓小时的最大 hour_ts。抓取计划从这里 +3600 往前推进；
    /// `None` = 这个 (game, league) 一小时都还没抓过。
    pub fn exchange_watermark(
        &self,
        game: &str,
        league: &str,
    ) -> Result<Option<i64>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT MAX(hour_ts) FROM exchange_hours WHERE game = ?1 AND league = ?2",
        )?;
        let watermark: Option<i64> =
            statement.query_row(params![game, league], |row| row.get(0))?;
        Ok(watermark)
    }

    /// 全部小时 mark 按时间升序，给覆盖审计和空洞检查用。
    pub fn list_exchange_hour_marks(
        &self,
        game: &str,
        league: &str,
    ) -> Result<Vec<ExchangeHourMark>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT hour_ts, market_count, fetched_at FROM exchange_hours
             WHERE game = ?1 AND league = ?2 ORDER BY hour_ts",
        )?;
        let rows = statement.query_map(params![game, league], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut marks = Vec::new();
        for row in rows {
            let (hour_ts, market_count, fetched_at) = row?;
            marks.push(ExchangeHourMark {
                hour_ts,
                market_count: count_u32(market_count)?,
                fetched_at: timestamp(&fetched_at)?,
            });
        }
        Ok(marks)
    }

    /// `[from_ts, to_ts)` 区间的小时市场行，扁平返回、时间升序。
    pub fn load_exchange_hours(
        &self,
        game: &str,
        league: &str,
        from_ts: i64,
        to_ts: i64,
    ) -> Result<Vec<ExchangeHourMarketRow>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT hour_ts, asset_a, asset_b, volume_a, volume_b,
                 lowest_stock_a, lowest_stock_b, highest_stock_a, highest_stock_b,
                 lowest_ratio_a, lowest_ratio_b, highest_ratio_a, highest_ratio_b
             FROM exchange_hour_markets
             WHERE game = ?1 AND league = ?2 AND hour_ts >= ?3 AND hour_ts < ?4
             ORDER BY hour_ts, asset_a, asset_b",
        )?;
        let rows = statement.query_map(params![game, league, from_ts, to_ts], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, String>(12)?,
            ))
        })?;
        let mut markets = Vec::new();
        for row in rows {
            let values = row?;
            markets.push(ExchangeHourMarketRow {
                hour_ts: values.0,
                asset_a: values.1,
                asset_b: values.2,
                volume_a: column_volume(values.3)?,
                volume_b: column_volume(values.4)?,
                lowest_stock_a: column_volume(values.5)?,
                lowest_stock_b: column_volume(values.6)?,
                highest_stock_a: column_volume(values.7)?,
                highest_stock_b: column_volume(values.8)?,
                lowest_ratio_a: values.9,
                lowest_ratio_b: values.10,
                highest_ratio_a: values.11,
                highest_ratio_b: values.12,
            });
        }
        Ok(markets)
    }

    /// 一个事务写完一天的日折：删旧行、写新行、盖 day mark。
    /// day mark 遵守 R4：它是"重折不会把真数据盖成空"的唯一屏障，
    /// 任何删除路径都不能碰它。
    pub fn replace_exchange_day(
        &mut self,
        game: &str,
        league: &str,
        utc_day: &str,
        rows: &[ExchangeDayMarketRow],
        hour_count: u32,
        computed_at: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        validate_day_key(utc_day)?;
        for row in rows {
            if row.utc_day != utc_day {
                return Err(StorageError::Rejected(format!(
                    "exchange day row {} does not match replace target {utc_day}",
                    row.utc_day
                )));
            }
        }
        let market_count = i64::try_from(rows.len())
            .map_err(|_| StorageError::Rejected("too many exchange day rows".into()))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM exchange_day_markets
             WHERE game = ?1 AND league = ?2 AND utc_day = ?3",
            params![game, league, utc_day],
        )?;
        for row in rows {
            transaction.execute(
                "INSERT INTO exchange_day_markets (game, league, utc_day, asset_a, asset_b,
                     volume_a, volume_b, hours_covered, computed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    game,
                    league,
                    utc_day,
                    row.asset_a,
                    row.asset_b,
                    volume_column(row.volume_a)?,
                    volume_column(row.volume_b)?,
                    i64::from(row.hours_covered),
                    computed_at.to_rfc3339(),
                ],
            )?;
        }
        transaction.execute(
            "INSERT OR REPLACE INTO exchange_day_marks
                 (game, league, utc_day, hour_count, market_count, computed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                game,
                league,
                utc_day,
                i64::from(hour_count),
                market_count,
                computed_at.to_rfc3339(),
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// 全部 day mark 按日升序，日折构建器用它跳过已完成的天。
    pub fn list_exchange_day_marks(
        &self,
        game: &str,
        league: &str,
    ) -> Result<Vec<ExchangeDayMark>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT utc_day, hour_count, market_count, computed_at FROM exchange_day_marks
             WHERE game = ?1 AND league = ?2 ORDER BY utc_day",
        )?;
        let rows = statement.query_map(params![game, league], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut marks = Vec::new();
        for row in rows {
            let (utc_day, hour_count, market_count, computed_at) = row?;
            marks.push(ExchangeDayMark {
                utc_day,
                hour_count: count_u32(hour_count)?,
                market_count: count_u32(market_count)?,
                computed_at: timestamp(&computed_at)?,
            });
        }
        Ok(marks)
    }

    /// `[from_day, to_day]`（两端含）的日折行，日升序。
    pub fn load_exchange_days(
        &self,
        game: &str,
        league: &str,
        from_day: &str,
        to_day: &str,
    ) -> Result<Vec<ExchangeDayMarketRow>, StorageError> {
        validate_day_key(from_day)?;
        validate_day_key(to_day)?;
        let mut statement = self.connection.prepare_cached(
            "SELECT utc_day, asset_a, asset_b, volume_a, volume_b, hours_covered
             FROM exchange_day_markets
             WHERE game = ?1 AND league = ?2 AND utc_day >= ?3 AND utc_day <= ?4
             ORDER BY utc_day, asset_a, asset_b",
        )?;
        let rows = statement.query_map(params![game, league, from_day, to_day], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        let mut markets = Vec::new();
        for row in rows {
            let (utc_day, asset_a, asset_b, volume_a, volume_b, hours_covered) = row?;
            markets.push(ExchangeDayMarketRow {
                utc_day,
                asset_a,
                asset_b,
                volume_a: column_volume(volume_a)?,
                volume_b: column_volume(volume_b)?,
                hours_covered: count_u32(hours_covered)?,
            });
        }
        Ok(markets)
    }

    /// 删掉一天的小时行和小时 mark（清理原语，保持 dumb）。调用方必须先做
    /// ground-truth 核对——该天的日折行真实存在——才允许调用，纪律同
    /// `prune_raw_days`。小时 mark 不在 R4 范围：CDN 一年内可重抓。
    pub fn delete_exchange_hours_of_day(
        &mut self,
        game: &str,
        league: &str,
        utc_day: &str,
    ) -> Result<ExchangeHourPruneStats, StorageError> {
        let (day_start, day_end) = day_hour_bounds(utc_day)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let markets_deleted = transaction.execute(
            "DELETE FROM exchange_hour_markets
             WHERE game = ?1 AND league = ?2 AND hour_ts >= ?3 AND hour_ts < ?4",
            params![game, league, day_start, day_end],
        )?;
        let hours_deleted = transaction.execute(
            "DELETE FROM exchange_hours
             WHERE game = ?1 AND league = ?2 AND hour_ts >= ?3 AND hour_ts < ?4",
            params![game, league, day_start, day_end],
        )?;
        transaction.commit()?;
        Ok(ExchangeHourPruneStats {
            hours_deleted: hours_deleted as u64,
            markets_deleted: markets_deleted as u64,
        })
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

/// 交易所成交量/库存进 INTEGER 列。超出 i64 硬错不饱和——
/// 对齐 RollupOverflow 的纪律：宁可整小时跳过并报告，也不写一个错的数。
fn volume_column(value: u64) -> Result<i64, StorageError> {
    i64::try_from(value)
        .map_err(|_| StorageError::RollupOverflow(format!("exchange volume {value}")))
}

/// INTEGER 列读回 u64。CHECK 保证非负，负数出现即库损坏。
fn column_volume(value: i64) -> Result<u64, StorageError> {
    u64::try_from(value)
        .map_err(|_| StorageError::Corrupt(format!("negative exchange volume {value}")))
}

fn count_u32(value: i64) -> Result<u32, StorageError> {
    u32::try_from(value).map_err(|_| StorageError::Corrupt(format!("bad count {value}")))
}

fn validate_hour_ts(hour_ts: i64) -> Result<(), StorageError> {
    if hour_ts > 0 && hour_ts % 3600 == 0 {
        Ok(())
    } else {
        Err(StorageError::Rejected(format!("bad hour ts: {hour_ts}")))
    }
}

/// 一个 UTC 日对应的小时时间戳半开区间 `[00:00, 次日 00:00)`。
fn day_hour_bounds(utc_day: &str) -> Result<(i64, i64), StorageError> {
    let date = chrono::NaiveDate::parse_from_str(utc_day, "%Y-%m-%d")
        .map_err(|error| StorageError::Rejected(format!("bad day key {utc_day}: {error}")))?;
    let start = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| StorageError::Rejected(format!("bad day key: {utc_day}")))?
        .and_utc()
        .timestamp();
    Ok((start, start + 24 * 3600))
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
