//! Season lifecycle and storage management on the settings page.
//!
//! The economy wipes each season, so history must stop counting at the
//! boundary. The rollover is always a user action — the program never infers
//! one — and rolling over archives by clamping: old rows stay on disk,
//! outside every window, until the explicit purge below removes them. The
//! purge is two-click and reports what it did; VACUUM is its own button
//! because it rewrites the whole file and blocks the capture writer, so it
//! is disabled while a watch session runs.

use gpui::{Context, ParentElement, Styled, div, px};
use gpui_component::{Sizable, Size, StyledExt as _, input::Input};

use crate::shell::AppShell;
use crate::theme::*;
use crate::ui::{LedgerButton, button, mono, panel, panel_header};

impl AppShell {
    /// Season banner + storage report + the three actions.
    #[cfg(windows)]
    pub(crate) fn season_panel(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        self.ensure_season_info(cx);
        let text = self.text();
        // 还没数完就先画一行省略号:空白看着像坏了。
        let info = self
            .season_info
            .clone()
            .unwrap_or_else(|| vec!["…".to_owned()]);

        let mut body = div().p_3().flex().flex_col().gap_2();
        for line in info {
            body = body.child(mono(line).text_size(fs(FS_11_5)));
        }

        body =
            body.child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(150.))
                            .flex_none()
                            .text_size(fs(FS_11_5))
                            .text_color(c(TEXT_META))
                            .child(text.season_start),
                    )
                    .child(
                        div()
                            .w(px(120.))
                            .flex_none()
                            .child(Input::new(&self.season_input).with_size(Size::Small)),
                    )
                    // 边界日期两个动作共用:开赛用它当开始时间,结束用它当
                    // 结束时间。留空 = 现在,后端本来就收任意时间戳。
                    .child(
                        div()
                            .text_size(fs(FS_11_5))
                            .text_color(c(TEXT_META))
                            .child(text.season_date_label),
                    )
                    .child(
                        div()
                            .w(px(120.))
                            .flex_none()
                            .child(Input::new(&self.season_date_input).with_size(Size::Small)),
                    )
                    .child(
                        button("season-start", LedgerButton::Primary, text.season_start, cx)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.start_new_season(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        button("season-end", LedgerButton::Secondary, text.season_end, cx)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.end_active_season(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        button("season-amend", LedgerButton::Quiet, text.season_amend, cx)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.amend_season_start(cx);
                                cx.notify();
                            })),
                    ),
            );

        body =
            body.child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(150.))
                            .flex_none()
                            .text_size(fs(FS_11_5))
                            .text_color(c(TEXT_META))
                            .child(text.exchange_league_label),
                    )
                    .child(
                        div()
                            .w(px(200.))
                            .flex_none()
                            .child(Input::new(&self.exchange_league_input).with_size(Size::Small)),
                    )
                    .child(
                        div()
                            .text_size(fs(FS_11_5))
                            .text_color(c(TEXT_META))
                            .child(text.exchange_backfill_label),
                    )
                    .child(
                        div().w(px(56.)).flex_none().child(
                            Input::new(&self.exchange_backfill_input).with_size(Size::Small),
                        ),
                    )
                    .child(
                        div()
                            .text_size(fs(FS_11_5))
                            .text_color(c(TEXT_META))
                            .child(text.exchange_retention_label),
                    )
                    .child(
                        div().w(px(56.)).flex_none().child(
                            Input::new(&self.exchange_retention_input).with_size(Size::Small),
                        ),
                    )
                    .child(
                        button(
                            "exchange-league-save",
                            LedgerButton::Secondary,
                            text.exchange_league_save,
                            cx,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.save_exchange_league(cx);
                            cx.notify();
                        })),
                    ),
            );
        // 二测反馈：两个数字框光有名字看不懂，给一行人话。
        body = body.child(
            mono(text.exchange_settings_hint)
                .text_size(fs(FS_10_5))
                .text_color(c(TEXT_META)),
        );

        let purge_label = if self.purge_armed {
            text.season_purge_confirm
        } else {
            text.season_purge
        };
        let mut actions = div().h_flex().items_center().gap_2().pt_1().child(
            button(
                "season-purge",
                if self.purge_armed {
                    LedgerButton::Primary
                } else {
                    LedgerButton::Secondary
                },
                purge_label,
                cx,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.purge_old_season();
                cx.notify();
            })),
        );
        if self.watching {
            actions = actions.child(
                mono(text.season_vacuum_blocked)
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_DISABLED)),
            );
        } else {
            actions = actions.child(
                button("season-vacuum", LedgerButton::Quiet, text.season_vacuum, cx).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.vacuum_store();
                        cx.notify();
                    }),
                ),
            );
        }
        body = body.child(actions);

        panel().child(panel_header(text.season_header)).child(body)
    }

    /// Kicks off the season line and the storage footprint on a cache miss,
    /// off the UI thread; every season action clears the cache so the page
    /// redraws the truth.
    ///
    /// 八测反馈：切到这一段会卡一下。原因是逐表 `COUNT(*)`——小时表几十万行，
    /// 全在 UI 线程上数。现在后台数，数完再落回来；数的期间画一行「…」。
    /// 代次号挡住过期结果：数到一半用户点了清理，旧数字回来也不上屏。
    #[cfg(windows)]
    fn ensure_season_info(&mut self, cx: &mut Context<Self>) {
        if self.season_info.is_some() || self.season_info_loading {
            return;
        }
        self.season_info_loading = true;
        let generation = self.season_info_generation;
        let text = self.text();
        let game = self.settings.active_profile.game.as_str().to_owned();
        cx.spawn(async move |this, cx| {
            let lines = cx
                .background_executor()
                .spawn(async move { season_info_lines(&game, text) })
                .await;
            this.update(cx, |this: &mut AppShell, cx| {
                this.season_info_loading = false;
                if this.season_info_generation == generation {
                    this.season_info = Some(lines);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Forgets the cached season/storage lines and retires any count still
    /// in flight, so the next draw shows what the action just did.
    #[cfg(windows)]
    pub(crate) fn invalidate_season_info(&mut self) {
        self.season_info = None;
        self.season_info_generation = self.season_info_generation.wrapping_add(1);
    }

    /// The boundary box as a moment: empty means right now, a date means
    /// that UTC day's midnight, anything else is a refusal (logged), never a
    /// silent "now" — a mistyped date silently becoming "now" would clamp a
    /// whole season to the wrong day.
    #[cfg(windows)]
    fn season_boundary(&mut self, cx: &gpui::App) -> Option<chrono::DateTime<chrono::Utc>> {
        let raw = self.season_date_input.read(cx).value().trim().to_string();
        if raw.is_empty() {
            return Some(chrono::Utc::now());
        }
        match chrono::NaiveDate::parse_from_str(&raw, "%Y-%m-%d") {
            Ok(date) => Some(chrono::DateTime::from_naive_utc_and_offset(
                date.and_hms_opt(0, 0, 0)?,
                chrono::Utc,
            )),
            Err(_) => {
                self.push_log(format!("season date {raw:?} is not YYYY-MM-DD"));
                None
            }
        }
    }

    /// 保存交易所三项设置（联赛/回补天数/保留天数）并立刻按新配置开一轮。
    ///
    /// 换联赛 = 换一本账：新 (game, league) 的水位从零回补，旧联赛的数据
    /// 原地保留。旧同步链靠代次作废，不会出现两条链同时抓。
    /// 数字框留空或写不成数就保持原值——静默吞掉一个坏输入比报错更糟，
    /// 所以保持原值也要在日志里说一声。
    #[cfg(windows)]
    fn save_exchange_league(&mut self, cx: &mut Context<Self>) {
        let league = self
            .exchange_league_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        let game = self.settings.active_profile.game;
        let backfill_raw = self
            .exchange_backfill_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        let retention_raw = self
            .exchange_retention_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        let exchange = self.settings.market_tuning(game).exchange.clone();
        // 上限 365：三测提出"一次拉 30000 天怎么办"。回补真正的硬下限是
        // 赛季起点（配置了就到赛季为止），这里的钳位只是把荒谬值挡在门口
        // 并说一声，而不是静默接受或崩掉。
        let mut parse_days = |raw: &str, label: &str, current: u64| -> u64 {
            match raw.parse::<u64>() {
                Ok(days) if days > 365 => {
                    self.push_log(format!("exchange: {label} {days} clamped to 365"));
                    365
                }
                Ok(days) => days,
                Err(_) => {
                    self.push_log(format!(
                        "exchange: {label} \"{raw}\" ignored, kept {current}"
                    ));
                    current
                }
            }
        };
        let backfill_days = parse_days(&backfill_raw, "backfill", exchange.backfill_days);
        let retention_days = parse_days(&retention_raw, "retention", exchange.hour_retention_days);
        if exchange.league == league
            && exchange.backfill_days == backfill_days
            && exchange.hour_retention_days == retention_days
        {
            return;
        }
        {
            let tuning = self.settings.market_tuning_mut(game);
            tuning.exchange.league = league.clone();
            tuning.exchange.backfill_days = backfill_days;
            tuning.exchange.hour_retention_days = retention_days;
        }
        match self.settings_store.save(&self.settings) {
            Ok(()) => {
                self.push_log(if league.is_empty() {
                    "exchange: league cleared, sync off".to_owned()
                } else {
                    format!("exchange: league {league}, backfill {backfill_days}d, keep {retention_days}d")
                });
                self.restart_exchange_sync(cx);
            }
            Err(error) => self.push_log(format!("settings save failed: {error}")),
        }
    }

    /// Starts a season at the boundary date (or now), labelled from the box.
    /// Monotonic by storage contract; a rejection surfaces in the log rather
    /// than half-applying.
    #[cfg(windows)]
    fn start_new_season(&mut self, cx: &gpui::App) {
        let label = self.season_input.read(cx).value().trim().to_string();
        if label.is_empty() {
            return;
        }
        let Some(started_at) = self.season_boundary(cx) else {
            return;
        };
        let game = self.settings.active_profile.game.as_str().to_owned();
        match ptt_storage::MarketStore::open(ptt_runtime::pipeline::default_database_path()) {
            Ok(mut store) => match store.start_season(&game, &label, started_at) {
                Ok(season) => {
                    self.push_log(format!(
                        "season {} started at {}",
                        season.label,
                        season.started_at.format("%Y-%m-%d")
                    ));
                    // Every page now reads a clamped window.
                    self.report_stale = true;
                }
                Err(error) => self.push_log(format!("season: {error}")),
            },
            Err(error) => self.push_log(format!("storage: {error}")),
        }
        self.invalidate_season_info();
        self.purge_armed = false;
    }

    /// 修正当前赛季的开始日期（日期框必填——"修正到现在"没有意义）。
    /// 成功后立刻重启交易所同步：回补下限变了，历史从新起点长出来。
    #[cfg(windows)]
    fn amend_season_start(&mut self, cx: &mut Context<Self>) {
        if self.season_date_input.read(cx).value().trim().is_empty() {
            self.push_log("season: give the corrected date in the date box first".to_owned());
            return;
        }
        let Some(started_at) = self.season_boundary(cx) else {
            return;
        };
        let game = self.settings.active_profile.game.as_str().to_owned();
        match ptt_storage::MarketStore::open(ptt_runtime::pipeline::default_database_path()) {
            Ok(mut store) => match store.amend_season_start(&game, started_at) {
                Ok(season) => {
                    self.push_log(format!(
                        "season {} start corrected to {}",
                        season.label,
                        season.started_at.format("%Y-%m-%d")
                    ));
                    self.report_stale = true;
                    self.restart_exchange_sync(cx);
                }
                Err(error) => self.push_log(format!("season: {error}")),
            },
            Err(error) => self.push_log(format!("storage: {error}")),
        }
        self.invalidate_season_info();
        self.purge_armed = false;
    }

    /// Records when the active season ended (boundary date, or now).
    /// Statistics stop counting there; capturing itself is never blocked.
    #[cfg(windows)]
    fn end_active_season(&mut self, cx: &gpui::App) {
        let Some(ended_at) = self.season_boundary(cx) else {
            return;
        };
        let game = self.settings.active_profile.game.as_str().to_owned();
        match ptt_storage::MarketStore::open(ptt_runtime::pipeline::default_database_path()) {
            Ok(mut store) => match store.end_season(&game, ended_at) {
                Ok(season) => {
                    self.push_log(format!(
                        "season {} ended at {}",
                        season.label,
                        ended_at.format("%Y-%m-%d")
                    ));
                    self.report_stale = true;
                }
                Err(error) => self.push_log(format!("season: {error}")),
            },
            Err(error) => self.push_log(format!("storage: {error}")),
        }
        self.invalidate_season_info();
        self.purge_armed = false;
    }

    /// First click arms, second click deletes raw rows strictly before the
    /// active season. Rollups, marks, and contexts survive by design.
    #[cfg(windows)]
    fn purge_old_season(&mut self) {
        if !self.purge_armed {
            self.purge_armed = true;
            return;
        }
        self.purge_armed = false;
        let game = self.settings.active_profile.game.as_str().to_owned();
        match ptt_storage::MarketStore::open(ptt_runtime::pipeline::default_database_path()) {
            Ok(mut store) => {
                match ptt_runtime::rollup::purge_before_active_season(&mut store, &game) {
                    Ok((season, stats)) => {
                        self.push_log(format!(
                            "purged before {}: {} edges, {} snapshots (~{} KiB reusable)",
                            season.label,
                            stats.edges_deleted,
                            stats.snapshots_deleted,
                            stats.freed_bytes_estimate / 1024,
                        ));
                        self.report_stale = true;
                    }
                    Err(error) => self.push_log(format!("purge: {error}")),
                }
            }
            Err(error) => self.push_log(format!("storage: {error}")),
        }
        self.invalidate_season_info();
    }

    /// VACUUM, only while not watching: it blocks the capture writer for its
    /// whole duration and would burn through the busy timeout.
    #[cfg(windows)]
    fn vacuum_store(&mut self) {
        if self.watching {
            return;
        }
        match ptt_storage::MarketStore::open(ptt_runtime::pipeline::default_database_path()) {
            Ok(mut store) => match store.vacuum() {
                Ok(reclaimed) => {
                    self.push_log(format!("vacuum reclaimed {} KiB", reclaimed / 1024));
                }
                Err(error) => self.push_log(format!("vacuum: {error}")),
            },
            Err(error) => self.push_log(format!("storage: {error}")),
        }
        self.invalidate_season_info();
    }
}

#[cfg(not(windows))]
impl AppShell {
    pub(crate) fn season_panel(&mut self, _cx: &mut Context<Self>) -> gpui::Div {
        div()
    }
}

/// The season line plus the storage footprint, as display lines. Pure and
/// thread-free so the shell can run it on the background executor.
#[cfg(windows)]
fn season_info_lines(game: &str, text: &'static crate::i18n::Text) -> Vec<String> {
    let mut lines = Vec::new();
    match ptt_storage::MarketStore::open(ptt_runtime::pipeline::default_database_path()) {
        Ok(store) => {
            let season_line = match store.active_season(game) {
                Ok(Some(season)) => format!(
                    "{}: {} · {} ~ {}",
                    text.season_current,
                    season.label,
                    season.started_at.format("%Y-%m-%d"),
                    season
                        .ended_at
                        .map_or_else(String::new, |end| end.format("%Y-%m-%d").to_string()),
                ),
                Ok(None) => format!("{}: {}", text.season_current, text.season_none),
                Err(error) => format!("{}: {error}", text.season_current),
            };
            lines.push(season_line);
            if let Ok(footprint) = store.database_footprint() {
                lines.push(format!(
                    "{}: {} MiB ({} MiB free)",
                    text.season_db,
                    footprint.total_bytes / (1024 * 1024),
                    footprint.free_bytes / (1024 * 1024),
                ));
                for (table, rows) in footprint.table_rows {
                    lines.push(format!("  {table:<20} {rows}"));
                }
            }
        }
        Err(error) => lines.push(format!("storage: {error}")),
    }
    lines
}
