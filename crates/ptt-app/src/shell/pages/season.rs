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
        self.ensure_season_info();
        let text = self.text();
        let info = self.season_info.clone().unwrap_or_default();

        let mut body = div().p_3().flex().flex_col().gap_2();
        for line in info {
            body = body.child(mono(line).text_size(fs(FS_11_5)));
        }

        body = body.child(
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
                .child(
                    button("season-start", LedgerButton::Primary, text.season_start, cx).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.start_new_season(cx);
                            cx.notify();
                        }),
                    ),
                ),
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

    /// Loads the season line and the storage footprint once per cache miss;
    /// every season action clears the cache so the page redraws the truth.
    #[cfg(windows)]
    fn ensure_season_info(&mut self) {
        if self.season_info.is_some() {
            return;
        }
        let text = self.text();
        let game = self.settings.active_profile.game.as_str();
        let mut lines = Vec::new();
        match ptt_storage::MarketStore::open(ptt_runtime::pipeline::default_database_path()) {
            Ok(store) => {
                let season_line = match store.active_season(game) {
                    Ok(Some(season)) => format!(
                        "{}: {} · {}",
                        text.season_current,
                        season.label,
                        season.started_at.format("%Y-%m-%d"),
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
        self.season_info = Some(lines);
    }

    /// Starts a season now, labelled from the box. Monotonic by storage
    /// contract; a rejection surfaces in the log rather than half-applying.
    #[cfg(windows)]
    fn start_new_season(&mut self, cx: &gpui::App) {
        let label = self.season_input.read(cx).value().trim().to_string();
        if label.is_empty() {
            return;
        }
        let game = self.settings.active_profile.game.as_str().to_owned();
        match ptt_storage::MarketStore::open(ptt_runtime::pipeline::default_database_path()) {
            Ok(mut store) => match store.start_season(&game, &label, chrono::Utc::now()) {
                Ok(season) => {
                    self.push_log(format!("season {} started", season.label));
                    // Every page now reads a clamped window.
                    self.report_stale = true;
                }
                Err(error) => self.push_log(format!("season: {error}")),
            },
            Err(error) => self.push_log(format!("storage: {error}")),
        }
        self.season_info = None;
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
        self.season_info = None;
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
        self.season_info = None;
    }
}

#[cfg(not(windows))]
impl AppShell {
    pub(crate) fn season_panel(&mut self, _cx: &mut Context<Self>) -> gpui::Div {
        div()
    }
}
