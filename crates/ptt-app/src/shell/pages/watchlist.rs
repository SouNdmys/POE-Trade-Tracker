//! What is being watched, whether it is healthy, and what to watch next.
//!
//! The focus list decides which pairs the radar scans and which gaps the
//! probe queue reports, so it is edited here, on the page that shows what
//! that choice produced — a settings screen away from the data would make
//! every adjustment a round trip.

use gpui::{Context, IntoElement as _, ParentElement, Styled, div, px};
use gpui_component::{Sizable as _, StyledExt as _};
use ptt_runtime::domain::{FocusCoverageStatus, ValuationStatus};
use ptt_runtime::report_text;
use ptt_runtime::reports::{CoverageOutcome, WatchlistModel};

use crate::shell::AppShell;
use crate::state::PageData;
use crate::theme::*;
use crate::ui::{
    LedgerButton, StatusKind, button, empty_state, error_band, kv_row, mono, panel, warning_band,
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

    /// Stops drawing a currency's valuation row. Display only.
    #[cfg(windows)]
    pub(crate) fn hide_asset(&mut self, asset: &str) {
        let game = self.settings.active_profile.game;
        {
            let tuning = self.settings.market_tuning_mut(game);
            if tuning.hidden_assets.iter().any(|held| held == asset) {
                return;
            }
            tuning.hidden_assets.push(asset.to_owned());
            tuning.hidden_assets.sort();
        }
        match self.settings_store.save(&self.settings) {
            Ok(()) => self.report_stale = true,
            Err(error) => self.push_log(format!("settings save failed: {error}")),
        }
    }

    /// Brings every hidden row back.
    #[cfg(windows)]
    pub(crate) fn unhide_all_assets(&mut self) {
        let game = self.settings.active_profile.game;
        self.settings.market_tuning_mut(game).hidden_assets.clear();
        match self.settings_store.save(&self.settings) {
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

    /// Refuses a pair permanently: it leaves the watchlist, the monitor's
    /// "next to capture" and the HUD reminder in one motion, because they all
    /// read models the report layer already filtered.
    #[cfg(windows)]
    pub(crate) fn ignore_probe(&mut self, from: &str, to: &str) {
        let game = self.settings.active_profile.game;
        {
            let tuning = self.settings.market_tuning_mut(game);
            if tuning.is_probe_ignored(from, to) {
                return;
            }
            tuning.ignored_probes.push(ptt_settings::IgnoredProbe {
                from_asset_id: from.to_owned(),
                to_asset_id: to.to_owned(),
            });
            tuning.ignored_probes.sort_by(|left, right| {
                left.from_asset_id
                    .cmp(&right.from_asset_id)
                    .then_with(|| left.to_asset_id.cmp(&right.to_asset_id))
            });
        }
        // A pair can be queued and ignored in the same breath; the session
        // queue is display-side of the filter, so it has to let go itself.
        self.probe_queue.unpin(from, to);
        match self.settings_store.save(&self.settings) {
            Ok(()) => self.report_stale = true,
            Err(error) => self.push_log(format!("settings save failed: {error}")),
        }
    }

    /// The regret path: puts one refused pair back into circulation.
    #[cfg(windows)]
    pub(crate) fn restore_ignored_probe(&mut self, from: &str, to: &str) {
        let game = self.settings.active_profile.game;
        self.settings
            .market_tuning_mut(game)
            .ignored_probes
            .retain(|held| !(held.from_asset_id == from && held.to_asset_id == to));
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
                    .child(self.report_fallback()),
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
    ///
    /// §5 定稿:六列 28px 固定行(通货 220 | 每个约 116 | 计价 76 | 依据 60 |
    /// 角色 92 | 操作 1fr),角色从四个按钮铺满一行改成一个下拉——原来
    /// 22 行 × 4 = 88 个按钮,眼睛没有落点。
    fn focus_panel(&self, model: &WatchlistModel, cx: &mut Context<Self>) -> gpui::Div {
        let text = self.text();
        let language = self.language();

        // 头:标题 + 右侧结算通货(金字,「色字=主题」)。
        let header = div()
            .h(px(H_INPUT))
            .flex_none()
            .h_flex()
            .items_center()
            .px_3()
            .bg(c(RAIL))
            .border_b_1()
            .border_color(c(HAIRLINE))
            .child(crate::ui::micro_title(text.page_watchlist))
            .child(div().flex_1())
            .child(
                div()
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_DISABLED))
                    .child(text.settlement_label),
            )
            .child(
                div()
                    .pl_2()
                    .text_size(fs(FS_11_5))
                    .text_color(c(ACCENT_TEXT))
                    .child(gpui::SharedString::from(
                        model
                            .core_liquidity
                            .iter()
                            .map(|asset| self.display_name(asset.as_str()))
                            .collect::<Vec<_>>()
                            .join(" · "),
                    )),
            );

        // 四行角色说明压成一行(§5)。
        let legend = div()
            .h(px(H_ROW))
            .flex_none()
            .h_flex()
            .items_center()
            .px_3()
            .border_b_1()
            .border_color(c(HAIRLINE_SOFT))
            .text_size(fs(FS_10_5))
            .text_color(c(TEXT_DISABLED))
            .overflow_hidden()
            .whitespace_nowrap()
            .child(gpui::SharedString::from(
                text.roles_legend.lines().collect::<Vec<_>>().join(" · "),
            ));

        // 列头。
        let heading = |label: &'static str| {
            div()
                .text_size(fs(FS_10_5))
                .text_color(c(TEXT_META))
                .child(label)
        };
        let columns = div()
            .h(px(H_ROW))
            .flex_none()
            .h_flex()
            .items_center()
            .px_3()
            .border_b_1()
            .border_color(c(HAIRLINE_SOFT))
            .child(heading(text.watch_col_asset).w(px(220.)).flex_none())
            .child(
                heading(text.watch_col_per_unit)
                    .w(px(116.))
                    .flex_none()
                    .text_right()
                    .pr_2(),
            )
            .child(heading(text.watch_col_anchor).w(px(76.)).flex_none())
            .child(heading(text.watch_col_basis).w(px(60.)).flex_none())
            .child(heading(text.watch_col_role).w(px(92.)).flex_none())
            .child(heading(text.watch_col_actions).flex_1().text_right());

        let mut body = div().flex().flex_col();
        for note in &model.notes {
            // 注意条,不是一段琥珀色的字:块色(左边那条 2px)才是"这里要留意"
            // 的载体,光把整段话染成琥珀是「色块＝语义」的反面。而且第一次
            // 打开程序时这块是屏幕上唯一的东西——一屏琥珀色段落会让人以为
            // 出了故障,其实只是还没抓过数据。
            body = body.child(warning_band(text.note_band_tag, note));
        }
        if model.valuations.is_empty() {
            body = body.child(empty_state(report_text::report(language).no_price_capture));
        }

        #[cfg(windows)]
        let hidden: std::collections::BTreeSet<String> = self
            .settings_tuning()
            .hidden_assets
            .iter()
            .cloned()
            .collect();
        #[cfg(not(windows))]
        let hidden: std::collections::BTreeSet<String> = Default::default();
        let mut hidden_count = 0usize;
        let mut zebra = false;

        for (row, entry) in model.valuations.iter().enumerate() {
            let asset = entry.asset_id.as_str().to_owned();
            if hidden.contains(&asset) {
                hidden_count += 1;
                continue;
            }
            #[cfg(windows)]
            let choice = self.focus_choice(&asset);
            #[cfg(not(windows))]
            let choice = FocusChoice::Unlisted;
            #[cfg(windows)]
            let settlement = self.is_settlement(&asset);
            #[cfg(not(windows))]
            let settlement = false;

            // "91:2" is exact and unreadable; "45.50" is what a person wants.
            // Integer arithmetic rounded to two places — a display projection
            // of the rational value, never fed back into anything.
            let (per_unit, anchor, basis): (String, String, &str) =
                match (&entry.valuation.value, entry.valuation.status) {
                    (Some(value), status) => (
                        per_unit_text(value),
                        self.display_name(entry.valuation.anchor_asset_id.as_str()),
                        if status == ValuationStatus::TwoSided {
                            text.valuation_two_sided
                        } else {
                            text.valuation_one_sided
                        },
                    ),
                    (None, _) => ("—".to_owned(), "—".to_owned(), text.watch_basis_none),
                };
            let unpriced = entry.valuation.value.is_none();

            // 角色:结算通货固定为金徽章;其余一个下拉,点开才是四选一。
            let role_cell: gpui::AnyElement = if settlement {
                crate::ui::chip_table(StatusKind::Monitoring, text.settlement_label)
                    .into_any_element()
            } else {
                let shell = cx.entity();
                let menu_asset = asset.clone();
                // 默认 variant 的底色就是 panel 色,按钮和行底在深色主题下
                // 融成一块、箭头也看不见——描边和字色必须自己给。
                let role_variant = gpui_component::button::ButtonCustomVariant::new(cx)
                    .color(hsla_of(PANEL))
                    .foreground(hsla_of(TEXT_SECONDARY))
                    .border(hsla_of(HAIRLINE_STRONG))
                    .hover(hsla_of(HOVER))
                    .active(hsla_of(PRESSED));
                use gpui_component::button::ButtonVariants as _;
                gpui_component::button::DropdownButton::new(("role-dropdown", row))
                    .custom(role_variant)
                    .button(
                        gpui_component::button::Button::new(("role-current", row))
                            .label(choice.label(text))
                            .with_size(gpui_component::Size::XSmall),
                    )
                    .dropdown_menu(move |mut menu, _, _| {
                        for option in FocusChoice::EDITABLE {
                            let shell = shell.clone();
                            let target = menu_asset.clone();
                            menu = menu.item(
                                gpui_component::menu::PopupMenuItem::new(option.label(text))
                                    .on_click(move |_, _, app| {
                                        shell.update(app, |this, cx| {
                                            #[cfg(windows)]
                                            this.set_focus_choice(&target, option);
                                            #[cfg(not(windows))]
                                            let _ = &target;
                                            cx.notify();
                                        });
                                    }),
                            );
                        }
                        menu
                    })
                    .into_any_element()
            };

            let hide_target = asset.clone();
            let hideable = !settlement && choice == FocusChoice::Unlisted;
            let mut line = div()
                .h(px(H_TABLE_ROW))
                .flex_none()
                .h_flex()
                .items_center()
                .px_3()
                .border_b_1()
                .border_color(c(HAIRLINE_SOFT))
                .text_size(fs(FS_12));
            if zebra {
                line = line.bg(c(ZEBRA));
            }
            zebra = !zebra;
            body = body.child(
                line.child(
                    div()
                        .w(px(220.))
                        .flex_none()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_color(c(if settlement {
                            ACCENT_TEXT
                        } else if unpriced {
                            TEXT_META
                        } else {
                            TEXT_PRIMARY
                        }))
                        .child(gpui::SharedString::from(self.display_name(&asset))),
                )
                .child(
                    mono(per_unit)
                        .w(px(116.))
                        .flex_none()
                        .text_right()
                        .pr_2()
                        .text_color(c(if unpriced {
                            TEXT_DISABLED
                        } else {
                            TEXT_PRIMARY
                        })),
                )
                .child(
                    div()
                        .w(px(76.))
                        .flex_none()
                        .text_size(fs(FS_11))
                        .text_color(c(if unpriced { TEXT_GHOST } else { TEXT_META }))
                        .child(gpui::SharedString::from(anchor)),
                )
                .child(
                    div()
                        .w(px(60.))
                        .flex_none()
                        .text_size(fs(FS_10_5))
                        .text_color(c(TEXT_META))
                        .child(basis),
                )
                .child(div().w(px(92.)).flex_none().child(role_cell))
                .child(
                    div()
                        .flex_1()
                        .h_flex()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .children(hideable.then(|| {
                            button(("row-hide", row), LedgerButton::Quiet, text.hide_label, cx)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    #[cfg(windows)]
                                    this.hide_asset(&hide_target);
                                    #[cfg(not(windows))]
                                    let _ = &hide_target;
                                    cx.notify();
                                }))
                        })),
                ),
            );
        }
        if hidden_count > 0 {
            body = body.child(
                div()
                    .h(px(H_INPUT))
                    .flex_none()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .child(
                        div()
                            .text_size(fs(FS_10_5))
                            .text_color(c(TEXT_DISABLED))
                            .child(gpui::SharedString::from(report_text::fill(
                                text.hidden_count,
                                &[&hidden_count.to_string()],
                            ))),
                    )
                    .child(div().flex_1())
                    .child(
                        button("rows-unhide", LedgerButton::Quiet, text.unhide_all, cx).on_click(
                            cx.listener(|this, _, _, cx| {
                                #[cfg(windows)]
                                this.unhide_all_assets();
                                cx.notify();
                            }),
                        ),
                    ),
            );
        }

        // 底部建议区(深一档的底):买压明显高于在售的通货,值得盯。
        let mut suggestions = div().flex_none().flex().flex_col();
        if !model.suggestions.is_empty() {
            suggestions = suggestions
                .bg(c(RAIL_DEEP))
                .border_t_1()
                .border_color(c(HAIRLINE))
                .child(
                    div()
                        .h(px(H_ROW))
                        .flex_none()
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .child(crate::ui::micro_title_sm(text.suggestion_label))
                        .child(
                            div()
                                .text_size(fs(FS_10_5))
                                .text_color(c(TEXT_DISABLED))
                                .child(text.suggestion_hint),
                        ),
                );
            for (row, suggestion) in model.suggestions.iter().enumerate() {
                let asset = suggestion.asset_id.as_str().to_owned();
                let adopt = asset.clone();
                let ignore = asset.clone();
                let seen_count = suggestion.demand_anchor;
                suggestions = suggestions.child(
                    div()
                        .h(px(H_TABLE_ROW))
                        .flex_none()
                        .h_flex()
                        .items_center()
                        .gap(px(SP_10))
                        .px_3()
                        .child(crate::ui::chip_table(
                            StatusKind::Warning,
                            text.suggestion_label,
                        ))
                        .child(
                            div()
                                .w(px(150.))
                                .flex_none()
                                .text_size(fs(FS_12))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(gpui::SharedString::from(self.display_name(&asset))),
                        )
                        .child(
                            div()
                                .text_size(fs(FS_10_5))
                                .text_color(c(TEXT_META))
                                .child(text.suggestion_demand_label),
                        )
                        .child(
                            mono(suggestion.demand_anchor.to_string())
                                .text_size(fs(FS_12))
                                .text_color(c(TEXT_PRIMARY)),
                        )
                        .child(
                            div()
                                .text_size(fs(FS_10_5))
                                .text_color(c(TEXT_META))
                                .child(text.suggestion_supply_label),
                        )
                        .child(
                            mono(suggestion.supply_anchor.to_string())
                                .text_size(fs(FS_12))
                                .text_color(c(TEXT_SECONDARY)),
                        )
                        .child(div().flex_1())
                        .child(
                            button(
                                ("focus-adopt", row),
                                LedgerButton::Secondary,
                                text.adopt_label,
                                cx,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    #[cfg(windows)]
                                    this.set_focus_choice(&adopt, FocusChoice::Target);
                                    #[cfg(not(windows))]
                                    let _ = &adopt;
                                    cx.notify();
                                },
                            )),
                        )
                        .child(
                            button(
                                ("focus-ignore", row),
                                LedgerButton::Quiet,
                                text.ignore_label,
                                cx,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    #[cfg(windows)]
                                    this.ignore_suggestion(&ignore, seen_count);
                                    #[cfg(not(windows))]
                                    let _ = (&ignore, seen_count);
                                    cx.notify();
                                },
                            )),
                        ),
                );
            }
        }

        panel()
            .w(px(716.))
            .flex_none()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(header)
            .child(legend)
            .child(columns)
            .child(crate::ui::scrollable(body, "watchlist-focus"))
            .child(suggestions)
    }

    /// The gaps, and the probes that would close them.
    fn coverage_panel(
        &self,
        model: &WatchlistModel,
        language: ptt_settings::UiLanguage,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let text = self.text();
        let mut body = div().flex().flex_col();

        // 头右侧的 62/74:整页唯一的总量指标,进度条是它的图形版。
        let totals = if let CoverageOutcome::Ready(coverage) = &model.coverage {
            let incomplete = coverage
                .entries
                .iter()
                .filter(|entry| entry.status != FocusCoverageStatus::Complete)
                .count();
            Some((coverage.entries.len() - incomplete, coverage.entries.len()))
        } else {
            None
        };

        match &model.coverage {
            CoverageOutcome::NotComputed => {
                body = body.child(empty_state(report_text::report(language).no_core_currency));
            }
            CoverageOutcome::Failed(reason) => {
                // `error_band` 而不是一行 `c(DANGER)` 的字:砖红的块色和字色
                // 是配对的两半(§11.7),块色 6px 的点画在左边,字用 DANGER_TEXT。
                // 拿块色直接当字色是这条规矩里点名过的那个错。
                body = body.child(error_band(&report_text::fill(
                    report_text::report(language).coverage_unavailable,
                    &[reason],
                )));
            }
            CoverageOutcome::Ready(coverage) => {
                // A list that names only settlement currencies leaves nothing
                // to measure, and the two pairs it does produce look exactly
                // like a market nobody has captured.
                if coverage.status == ptt_runtime::domain::FocusScopeStatus::MissingTarget {
                    body = body.child(warning_band(
                        text.note_band_tag,
                        report_text::report(language).focus_has_no_targets,
                    ));
                }
                let incomplete: Vec<_> = coverage
                    .entries
                    .iter()
                    .filter(|entry| entry.status != FocusCoverageStatus::Complete)
                    .collect();
                let complete = coverage.entries.len() - incomplete.len();

                // 覆盖度进度条:这一页唯一的总量指标(§5)。
                if !coverage.entries.is_empty() {
                    #[allow(clippy::cast_precision_loss)]
                    let share = complete as f32 / coverage.entries.len() as f32;
                    body = body.child(
                        div()
                            .h(px(34.))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .justify_center()
                            .gap(px(6.))
                            .px_3()
                            .border_b_1()
                            .border_color(c(HAIRLINE_SOFT))
                            .child(
                                div()
                                    .h(px(4.))
                                    .bg(c(HAIRLINE))
                                    .child(div().h(px(4.)).w(gpui::relative(share)).bg(c(ACCENT))),
                            )
                            .child(
                                div()
                                    .h_flex()
                                    .items_center()
                                    .text_size(fs(FS_10_5))
                                    .child(div().text_color(c(TEXT_DISABLED)).child(
                                        gpui::SharedString::from(report_text::fill(
                                            text.coverage_complete_pairs,
                                            &[&complete.to_string()],
                                        )),
                                    ))
                                    .child(div().flex_1())
                                    .child(div().text_color(c(WARN_TEXT)).child(
                                        gpui::SharedString::from(report_text::fill(
                                            text.coverage_gap_pairs,
                                            &[&incomplete.len().to_string()],
                                        )),
                                    )),
                            ),
                    );
                }

                // 小节头:深一档的底 + 微标题 + 计数。
                let section = |title: &'static str, count: String, hint: Option<&'static str>| {
                    let mut row = div()
                        .h(px(H_ROW))
                        .flex_none()
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .bg(c(RAIL_DEEP))
                        .border_b_1()
                        .border_color(c(HAIRLINE_SOFT))
                        .child(crate::ui::micro_title_sm(title))
                        .child(
                            mono(count)
                                .text_size(fs(FS_10_5))
                                .text_color(c(TEXT_DISABLED)),
                        );
                    if let Some(hint) = hint {
                        row = row.child(div().flex_1()).child(
                            div()
                                .text_size(fs(FS_10))
                                .text_color(c(TEXT_GHOST))
                                .child(hint),
                        );
                    }
                    row
                };

                // 下一步去抓:排队的会进浮窗;每行可排队、可忽略。
                body = body.child(section(
                    text.panel_probe_queue,
                    coverage.candidates.len().to_string(),
                    Some(text.probe_hud_hint),
                ));
                for (row, candidate) in coverage.candidates.iter().take(8).enumerate() {
                    let from = candidate.from_asset_id.as_str().to_owned();
                    let to = candidate.to_asset_id.as_str().to_owned();
                    let reason = report_text::probe_reason(language, candidate.reason).to_owned();
                    let pinned = self.probe_queue.is_pinned(&from, &to);
                    let (ignore_from, ignore_to) = (from.clone(), to.clone());
                    body = body.child(
                        div()
                            .h(px(H_TABLE_ROW))
                            .flex_none()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .border_b_1()
                            .border_color(c(HAIRLINE_SOFT))
                            // 金色左条 = 已排队 = 浮窗底条正在提醒的那种。
                            .child(div().w(px(3.)).h(px(14.)).flex_none().bg(c(if pinned {
                                ACCENT
                            } else {
                                DISABLED_DOT
                            })))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_size(fs(FS_12))
                                    .child(gpui::SharedString::from(self.pair_label(&from, &to))),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(fs(FS_10_5))
                                    .text_color(c(TEXT_META))
                                    .child(gpui::SharedString::from(reason.clone())),
                            )
                            .child(if pinned {
                                crate::ui::chip_table(StatusKind::Monitoring, text.pinned_label)
                                    .into_any_element()
                            } else {
                                button(
                                    ("watch-probe-pin", row),
                                    LedgerButton::Quiet,
                                    text.pin_label,
                                    cx,
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.pin_probe(&from, &to, &reason, false);
                                    cx.notify();
                                }))
                                .into_any_element()
                            })
                            // 忽略去抓:这一对我不抓。点下去同时从三处消失。
                            .child(
                                button(
                                    ("watch-probe-ignore", row),
                                    LedgerButton::Quiet,
                                    text.ignore_label,
                                    cx,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        #[cfg(windows)]
                                        this.ignore_probe(&ignore_from, &ignore_to);
                                        #[cfg(not(windows))]
                                        let _ = (&ignore_from, &ignore_to);
                                        cx.notify();
                                    },
                                )),
                            ),
                    );
                }
                body = body.child(self.ignored_probes_footer(cx));

                // 缺口明细:哪一对缺、缺在哪一侧。
                body = body.child(section(
                    text.coverage_gaps_header,
                    incomplete.len().to_string(),
                    None,
                ));
                for entry in incomplete.iter().take(10) {
                    body = body.child(
                        div()
                            .h(px(H_TABLE_ROW))
                            .flex_none()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .border_b_1()
                            .border_color(c(HAIRLINE_SOFT))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_size(fs(FS_12))
                                    .text_color(c(TEXT_SECONDARY))
                                    .child(gpui::SharedString::from(self.pair_label(
                                        entry.from_asset_id.as_str(),
                                        entry.to_asset_id.as_str(),
                                    ))),
                            )
                            .child(crate::ui::chip_table(
                                StatusKind::Warning,
                                report_text::focus_coverage_status(language, entry.status),
                            )),
                    );
                }

                // 「继续观察」折成一句话:它们本来就是"没事"(§5)。
                if complete > 0 {
                    body = body.child(
                        div()
                            .h(px(H_INPUT))
                            .flex_none()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .child(div().size(px(6.)).flex_none().rounded_full().bg(c(FRESH)))
                            .child(div().text_size(fs(FS_10_5)).text_color(c(TEXT_META)).child(
                                gpui::SharedString::from(report_text::fill(
                                    text.coverage_rest_fine,
                                    &[&complete.to_string()],
                                )),
                            )),
                    );
                }
            }
        }

        for recommendation in &model.anchors {
            body = body.child(div().px_3().child(kv_row(
                report_text::anchor_action(language, recommendation.action),
                &format!(
                    "{}   {}.{}   {} / {}",
                    self.display_name(recommendation.asset_id.as_str()),
                    recommendation.score_tenths / 10,
                    recommendation.score_tenths % 10,
                    recommendation.pair_coverage_count,
                    recommendation.bidirectional_pair_count,
                ),
            )));
        }

        let mut header = div()
            .h(px(H_INPUT))
            .flex_none()
            .h_flex()
            .items_center()
            .px_3()
            .bg(c(RAIL))
            .border_b_1()
            .border_color(c(HAIRLINE))
            .child(crate::ui::micro_title(text.coverage_header))
            .child(div().flex_1());
        if let Some((complete, total)) = totals {
            header = header.child(
                mono(format!("{complete} / {total}"))
                    .text_size(fs(FS_11))
                    .text_color(c(TEXT_DATA)),
            );
        }

        panel()
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(header)
            .child(crate::ui::scrollable(body, "watchlist-coverage"))
    }
}

impl AppShell {
    /// 「已忽略 N 对 · 查看并恢复」——唯一的后悔药,不藏进设置页。
    ///
    /// 收起时只占一行;点开列出每一对,各带一个「恢复」。列表为空时整段
    /// 消失,不占位置。
    fn ignored_probes_footer(&self, cx: &mut Context<Self>) -> gpui::Div {
        let text = self.text();
        #[cfg(windows)]
        let ignored: Vec<(String, String)> = self
            .settings_tuning()
            .ignored_probes
            .iter()
            .map(|pair| (pair.from_asset_id.clone(), pair.to_asset_id.clone()))
            .collect();
        #[cfg(not(windows))]
        let ignored: Vec<(String, String)> = Vec::new();

        if ignored.is_empty() {
            return div();
        }
        let mut footer = div().flex().flex_col().pt_1().child(
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(fs(FS_10_5))
                        .text_color(c(TEXT_DISABLED))
                        .child(gpui::SharedString::from(report_text::fill(
                            text.ignored_probes_count,
                            &[&ignored.len().to_string()],
                        ))),
                )
                .child(div().flex_1())
                .child(
                    button(
                        "ignored-probes-toggle",
                        LedgerButton::Quiet,
                        text.ignored_probes_review,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_ignored_probes = !this.show_ignored_probes;
                        cx.notify();
                    })),
                ),
        );
        if self.show_ignored_probes {
            for (row, (from, to)) in ignored.into_iter().enumerate() {
                let label = self.pair_label(&from, &to);
                footer = footer.child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .text_size(fs(FS_10_5))
                        .child(mono(label).text_color(c(TEXT_META)).flex_grow())
                        .child(
                            button(
                                ("ignored-probe-restore", row),
                                LedgerButton::Quiet,
                                text.ignored_probes_restore,
                                cx,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    #[cfg(windows)]
                                    this.restore_ignored_probe(&from, &to);
                                    #[cfg(not(windows))]
                                    let _ = (&from, &to);
                                    cx.notify();
                                },
                            )),
                        ),
                );
            }
        }
        footer
    }
}

/// A ratio of anchor-per-unit, as a two-decimal reading.
///
/// Integer arithmetic with explicit rounding — `91:2` becomes `45.50`, and
/// `58469137:200000` becomes `292.35` instead of a wall of digits. Display
/// only; every decision still runs on the exact ratio.
pub(crate) fn per_unit_text(value: &ptt_trade_domain::Ratio) -> String {
    if value.denominator == 0 {
        return value.text.clone();
    }
    let numerator = u128::from(value.numerator);
    let denominator = u128::from(value.denominator);
    let scaled = (numerator * 100 + denominator / 2) / denominator;
    let whole = scaled / 100;
    let cents = scaled % 100;
    if cents == 0 {
        format!("{whole}")
    } else if cents % 10 == 0 {
        format!("{whole}.{}", cents / 10)
    } else {
        format!("{whole}.{cents:02}")
    }
}
