//! What is being watched, whether it is healthy, and what to watch next.
//!
//! The focus list decides which pairs the radar scans and which gaps the
//! probe queue reports, so it is edited here, on the page that shows what
//! that choice produced — a settings screen away from the data would make
//! every adjustment a round trip.

use gpui::{
    Context, InteractiveElement as _, ParentElement, StatefulInteractiveElement as _, Styled, div,
    px,
};
use gpui_component::StyledExt as _;
use ptt_runtime::domain::{FocusCoverageStatus, ValuationStatus};
use ptt_runtime::report_text;
use ptt_runtime::reports::{CoverageOutcome, WatchlistModel};

use crate::shell::AppShell;
use crate::state::PageData;
use crate::theme::*;
use crate::ui::{
    LedgerButton, StatusKind, button, chip, empty_state, kv_row, mono, panel, panel_header,
};

/// A currency's place in the focus set.
///
/// Settlement is absent on purpose: it is the numéraire everything else is
/// priced against, so promoting a currency into it changes what every number
/// on every page means. That belongs on the settings page, deliberately.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FocusChoice {
    /// Scanned for opportunities.
    Target,
    /// Allowed as an intermediate, but not a destination.
    Bridge,
    /// Priced and reported, never routed through.
    WatchOnly,
    /// Not in the focus set at all.
    Unlisted,
}

impl FocusChoice {
    const EDITABLE: [Self; 4] = [Self::Target, Self::Bridge, Self::WatchOnly, Self::Unlisted];

    fn label(self, text: &'static crate::i18n::Text) -> &'static str {
        match self {
            Self::Target => text.role_target,
            Self::Bridge => text.role_bridge,
            Self::WatchOnly => text.role_watch_only,
            Self::Unlisted => text.role_unlisted,
        }
    }

    const fn element_id(self) -> &'static str {
        match self {
            Self::Target => "role-target",
            Self::Bridge => "role-bridge",
            Self::WatchOnly => "role-watch-only",
            Self::Unlisted => "role-unlisted",
        }
    }
}

impl AppShell {
    /// Whether a currency is one of the settlement currencies.
    ///
    /// They hold a role no list can change — everything is priced against
    /// them — so the four buttons do not apply, and offering them was worse
    /// than useless: clicking one had no visible effect on the row while
    /// quietly putting an entry in the focus list, which is enough to switch
    /// the whole coverage panel from "everything captured" to "the list".
    #[cfg(windows)]
    pub(crate) fn is_settlement(&self, asset: &str) -> bool {
        self.settings
            .market_tuning(self.settings.active_profile.game)
            .settlement_assets
            .iter()
            .any(|held| held == asset)
    }

    /// Where a currency currently sits.
    #[cfg(windows)]
    pub(crate) fn focus_choice(&self, asset: &str) -> FocusChoice {
        let tuning = self
            .settings
            .market_tuning(self.settings.active_profile.game);
        let held = |list: &[String]| list.iter().any(|entry| entry == asset);
        if held(&tuning.focus_assets) {
            FocusChoice::Target
        } else if held(&tuning.bridge_assets) {
            FocusChoice::Bridge
        } else if held(&tuning.watch_only_assets) {
            FocusChoice::WatchOnly
        } else {
            FocusChoice::Unlisted
        }
    }

    /// Moves a currency between focus lists and writes the settings back.
    ///
    /// Removed from every list before being added to one: the roles are
    /// exclusive, and a currency sitting in two of them would be scanned
    /// under whichever the reader happened to look at first.
    #[cfg(windows)]
    pub(crate) fn set_focus_choice(&mut self, asset: &str, choice: FocusChoice) {
        let game = self.settings.active_profile.game;
        {
            let tuning = self.settings.market_tuning_mut(game);
            for list in [
                &mut tuning.focus_assets,
                &mut tuning.bridge_assets,
                &mut tuning.watch_only_assets,
            ] {
                list.retain(|entry| entry != asset);
            }
            let list = match choice {
                FocusChoice::Target => Some(&mut tuning.focus_assets),
                FocusChoice::Bridge => Some(&mut tuning.bridge_assets),
                FocusChoice::WatchOnly => Some(&mut tuning.watch_only_assets),
                FocusChoice::Unlisted => None,
            };
            if let Some(list) = list {
                list.push(asset.to_owned());
                list.sort();
                list.dedup();
            }
        }
        match self.settings_store.save(&self.settings) {
            // The scan scope just changed, so the answer on screen describes
            // a focus set that no longer exists.
            Ok(()) => self.report_stale = true,
            Err(error) => self.push_log(format!("settings save failed: {error}")),
        }
    }

    /// Stops a currency being suggested again.
    ///
    /// Its own list rather than watch-only: "do not ask me about this" is not
    /// the same statement as "never route through this", and folding the first
    /// into the second would quietly change what the engine may do.
    #[cfg(windows)]
    pub(crate) fn ignore_suggestion(&mut self, asset: &str, snapshots: u64) {
        let game = self.settings.active_profile.game;
        {
            let tuning = self.settings.market_tuning_mut(game);
            tuning
                .ignored_suggestions
                .retain(|held| held.asset_id != asset);
            tuning
                .ignored_suggestions
                .push(ptt_settings::IgnoredSuggestion {
                    asset_id: asset.to_owned(),
                    snapshots_when_ignored: snapshots,
                });
            tuning
                .ignored_suggestions
                .sort_by(|left, right| left.asset_id.cmp(&right.asset_id));
        }
        match self.settings_store.save(&self.settings) {
            Ok(()) => self.report_stale = true,
            Err(error) => self.push_log(format!("settings save failed: {error}")),
        }
    }

    /// The watchlist page.
    pub(crate) fn render_watchlist(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let language = self.language();

        let PageData::Watchlist(model) = &self.report else {
            return div().flex_grow().flex().flex_col().gap_3().p_3().child(
                panel()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(empty_state(&self.report_body().join("  "))),
            );
        };
        let model: WatchlistModel = (**model).clone();

        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .gap_3()
            .p_3()
            .overflow_hidden()
            .child(self.focus_panel(&model, cx))
            .child(self.coverage_panel(&model, language, cx))
    }

    /// Every currency the book has priced, and what the user has made of it.
    fn focus_panel(&self, model: &WatchlistModel, cx: &mut Context<Self>) -> gpui::Div {
        let text = self.text();
        let language = self.language();
        let mut body = div().p_3().flex().flex_col().gap_1();

        for note in &model.notes {
            body = body.child(
                mono(note.clone())
                    .text_size(fs(FS_11_5))
                    .text_color(c(WARN_TEXT)),
            );
        }
        body = body.child(kv_row(
            text.settlement_label,
            &model
                .core_liquidity
                .iter()
                .map(|asset| self.display_name(asset.as_str()))
                .collect::<Vec<_>>()
                .join(", "),
        ));

        // Said here rather than in a document nobody opens: this is the
        // screen where the choice is made, and the choice reads as being about
        // how much the reader cares while it acts on what the engine may do.
        body = body
            .child(
                div()
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_DISABLED))
                    .child(text.settlement_hint),
            )
            .child(
                div()
                    .pt_1()
                    .pb_1()
                    .flex()
                    .flex_col()
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_DISABLED))
                    .children(
                        text.roles_legend
                            .lines()
                            .map(|line| div().child(gpui::SharedString::from(line))),
                    ),
            );

        if model.valuations.is_empty() {
            body = body.child(empty_state(report_text::report(language).no_price_capture));
        }
        for (row, entry) in model.valuations.iter().enumerate() {
            let asset = entry.asset_id.as_str().to_owned();
            let value = match (&entry.valuation.value, entry.valuation.status) {
                (Some(value), ValuationStatus::TwoSided) => {
                    format!("{} ({})", value.text, text.valuation_two_sided)
                }
                (Some(value), _) => format!("{} ({})", value.text, text.valuation_one_sided),
                (None, _) => report_text::report(language).no_price_capture.to_owned(),
            };
            #[cfg(windows)]
            let choice = self.focus_choice(&asset);
            #[cfg(not(windows))]
            let choice = FocusChoice::Unlisted;

            #[cfg(windows)]
            let settlement = self.is_settlement(&asset);
            #[cfg(not(windows))]
            let settlement = false;

            let mut roles = div().h_flex().items_center().flex_none();
            for (index, option) in FocusChoice::EDITABLE.into_iter().enumerate() {
                let target = asset.clone();
                let active = option == choice;
                let mut cell = div()
                    // Per row, not per option: keyed only by which of the
                    // four it is, every row after the first was inert.
                    .id((option.element_id(), row))
                    .h(px(H_ROW))
                    .px(px(SP_6))
                    .flex()
                    .items_center()
                    .text_size(fs(FS_10_5))
                    .whitespace_nowrap()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        #[cfg(windows)]
                        this.set_focus_choice(&target, option);
                        cx.notify();
                    }));
                if index > 0 {
                    cell = cell.border_l_1().border_color(c(HAIRLINE));
                }
                cell = if active {
                    cell.bg(c(ACCENT_WASH)).text_color(c(ACCENT_TEXT))
                } else {
                    cell.bg(c(PANEL))
                        .text_color(c(TEXT_SECONDARY))
                        .hover(|style| style.bg(c(HOVER)))
                };
                roles = roles.child(cell.child(option.label(text)));
            }

            body = body.child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        mono(self.display_name(&asset))
                            .text_size(fs(FS_11_5))
                            .w(px(180.)),
                    )
                    .child(
                        mono(value)
                            .text_size(fs(FS_11_5))
                            .text_color(c(TEXT_SECONDARY))
                            .flex_grow(),
                    )
                    .child(if settlement {
                        div().child(chip(StatusKind::Monitoring, text.settlement_label))
                    } else {
                        div().border_1().border_color(c(HAIRLINE)).child(roles)
                    }),
            );
        }

        // Currencies the user keeps flipping but has never put in the list.
        for (row, suggestion) in model.suggestions.iter().enumerate() {
            let asset = suggestion.asset_id.as_str().to_owned();
            let adopt = asset.clone();
            let ignore = asset.clone();
            let seen_count = u64::try_from(suggestion.snapshot_count).unwrap_or(u64::MAX);
            body = body.child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(chip(StatusKind::Warning, text.suggestion_label))
                    .child(
                        mono(format!(
                            "{}   ×{}",
                            self.display_name(&asset),
                            suggestion.snapshot_count
                        ))
                        .text_size(fs(FS_11_5))
                        .flex_grow(),
                    )
                    .child(
                        button(
                            ("focus-adopt", row),
                            LedgerButton::Secondary,
                            text.adopt_label,
                            cx,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            #[cfg(windows)]
                            this.set_focus_choice(&adopt, FocusChoice::Target);
                            cx.notify();
                        })),
                    )
                    .child(
                        button(
                            ("focus-ignore", row),
                            LedgerButton::Quiet,
                            text.ignore_label,
                            cx,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            #[cfg(windows)]
                            this.ignore_suggestion(&ignore, seen_count);
                            cx.notify();
                        })),
                    ),
            );
        }

        panel()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(panel_header(text.page_watchlist))
            .child(crate::ui::scrollable(body, "watchlist-focus"))
    }

    /// The gaps, and the probes that would close them.
    fn coverage_panel(
        &self,
        model: &WatchlistModel,
        language: ptt_settings::UiLanguage,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let text = self.text();
        let mut body = div().p_3().flex().flex_col().gap_1();

        match &model.coverage {
            CoverageOutcome::NotComputed => {
                body = body.child(empty_state(report_text::report(language).no_core_currency));
            }
            CoverageOutcome::Failed(reason) => {
                body = body.child(
                    mono(report_text::fill(
                        report_text::report(language).coverage_unavailable,
                        &[reason],
                    ))
                    .text_size(fs(FS_11_5))
                    .text_color(c(DANGER)),
                );
            }
            CoverageOutcome::Ready(coverage) => {
                // A list that names only settlement currencies leaves nothing
                // to measure, and the two pairs it does produce look exactly
                // like a market nobody has captured.
                if coverage.status == ptt_runtime::domain::FocusScopeStatus::MissingTarget {
                    body = body.child(
                        mono(report_text::report(language).focus_has_no_targets)
                            .text_size(fs(FS_11_5))
                            .text_color(c(WARN_TEXT)),
                    );
                }
                let incomplete: Vec<_> = coverage
                    .entries
                    .iter()
                    .filter(|entry| entry.status != FocusCoverageStatus::Complete)
                    .collect();
                body = body.child(kv_row(
                    text.coverage_label,
                    &format!(
                        "{} / {}",
                        coverage.entries.len() - incomplete.len(),
                        coverage.entries.len()
                    ),
                ));
                for entry in incomplete.into_iter().take(10) {
                    body = body.child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .text_size(fs(FS_10_5))
                            .child(
                                mono(self.pair_label(
                                    entry.from_asset_id.as_str(),
                                    entry.to_asset_id.as_str(),
                                ))
                                .flex_grow(),
                            )
                            .child(chip(
                                StatusKind::Warning,
                                report_text::focus_coverage_status(language, entry.status),
                            )),
                    );
                }
                for (row, candidate) in coverage.candidates.iter().take(8).enumerate() {
                    let from = candidate.from_asset_id.as_str().to_owned();
                    let to = candidate.to_asset_id.as_str().to_owned();
                    let reason = report_text::probe_reason(language, candidate.reason).to_owned();
                    let pinned = self.probe_queue.is_pinned(&from, &to);
                    body = body.child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .text_size(fs(FS_10_5))
                            .child(mono(self.pair_label(&from, &to)).flex_grow())
                            .child(mono(reason.clone()).text_color(c(TEXT_META)))
                            .child(if pinned {
                                chip(StatusKind::Monitoring, text.pinned_label)
                            } else {
                                div().child(
                                    button(
                                        ("watch-probe-pin", row),
                                        LedgerButton::Quiet,
                                        text.pin_label,
                                        cx,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.pin_probe(&from, &to, &reason);
                                            cx.notify();
                                        },
                                    )),
                                )
                            }),
                    );
                }
            }
        }

        for recommendation in &model.anchors {
            body = body.child(kv_row(
                report_text::anchor_action(language, recommendation.action),
                &format!(
                    "{}   {}.{}   {} / {}",
                    self.display_name(recommendation.asset_id.as_str()),
                    recommendation.score_tenths / 10,
                    recommendation.score_tenths % 10,
                    recommendation.pair_coverage_count,
                    recommendation.bidirectional_pair_count,
                ),
            ));
        }

        panel()
            .w(px(440.))
            .flex_none()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(panel_header(text.coverage_header))
            .child(crate::ui::scrollable(body, "watchlist-coverage"))
    }
}
