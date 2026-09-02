//! The market pulse: what every currency is worth, who wants it, and
//! whether the settlement anchor itself is drifting.
//!
//! The page answers the three questions the user specified as one feature:
//! value via settlement cross-rates (with the anchor-drift decomposition —
//! a market-wide "rise" is the anchor falling), supply and demand pressure
//! from the two listing sides (TASK-50 payout attribution), and the
//! scarce-vs-oversupplied discrimination that tells a mirror from junk.
//!
//! Quantities are drawn as proportional bars scaled within each asset's own
//! day range: display geometry only, nothing flows back into a model.

use gpui::{Context, InteractiveElement as _, IntoElement, ParentElement, Styled, div, px};
use gpui_component::StyledExt as _;
use ptt_runtime::report_text;
use ptt_runtime::reports::AnalyticsModel;
use ptt_settings::UiLanguage;

use crate::shell::AppShell;
use crate::state::PageData;
use crate::theme::*;
use crate::ui::{
    StatusKind, chip, chip_table, kv_row, mono, panel, panel_header, scrollable, warning_band,
};

/// Liquidity class → chip colour: scarce is the interesting gold state,
/// oversupplied and quiet are warnings, balanced is neutral.
fn class_kind(class: ptt_runtime::domain::LiquidityClass) -> StatusKind {
    match class {
        ptt_runtime::domain::LiquidityClass::Scarce => StatusKind::Monitoring,
        ptt_runtime::domain::LiquidityClass::Oversupplied
        | ptt_runtime::domain::LiquidityClass::Quiet => StatusKind::Warning,
        ptt_runtime::domain::LiquidityClass::Balanced => StatusKind::Idle,
    }
}

/// A drift column reads as a signed percentage.
///
/// The rule itself lives in `report_text` rather than here: this page and
/// `analytics_report_lines` print the same drifts, and a sign rule owned by
/// one of them is a sign rule the other will drift away from.
fn signed_percent(value: i64) -> String {
    report_text::signed_percent_from_basis_points(value)
}

/// 大数字用万(§6):12141911 → 1214万。十万以下保持原样——18531 还读得动,
/// 精确值永远在明细栏。
fn wan(units: u128) -> String {
    if units >= 100_000 {
        format!("{}万", units / 10_000)
    } else {
        units.to_string()
    }
}

/// 供需比:买压是在售的几倍,一位小数。标签旁边的依据(§6 规则 3)。
fn demand_supply_ratio(demand: Option<u128>, supply: Option<u128>) -> Option<String> {
    let demand = demand?;
    let supply = supply?;
    if supply == 0 {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    Some(format!("{:.1}×", demand as f64 / supply as f64))
}

/// ±2% 以内算持平(§6):一半的行都在这里,全上色就没重点了。
const FLAT_BAND_BASIS_POINTS: i64 = 200;

impl AppShell {
    /// 趋势曲线/判定要求的基线天数。
    ///
    /// 跟随设置里的「趋势基线(天)」而不是写死 7:页面一边说"要 7 天基线"
    /// 一边放着一个用户已经调到 3 的设置项,等于告诉用户设置不管用。
    /// 夹在 [2, 30]:1 天连线都画不成,30 天之外的曲线在 110px 里全是噪声。
    fn trend_baseline_days(&self) -> u64 {
        let game = self.settings.active_profile.game;
        self.settings
            .market_tuning(game)
            .analytics
            .trend_window_days
            .clamp(2, 30)
    }
}

impl AppShell {
    /// The Analytics page.
    pub(crate) fn render_analytics(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let PageData::Analytics(model) = &self.report else {
            return div().flex_grow().flex().flex_col().gap_3().p_3().child(
                panel()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(self.report_fallback()),
            );
        };
        let model: AnalyticsModel = (**model).clone();
        let language = self.language();

        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .gap_3()
            .p_3()
            .overflow_hidden()
            .child(self.analytics_main(&model, language, cx))
            .child(self.analytics_detail(&model, language))
    }

    /// Season banner, anchor health, and the asset table.
    fn analytics_main(
        &self,
        model: &AnalyticsModel,
        language: UiLanguage,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let text = self.text();

        let mut head = div().p_3().flex().flex_col().gap_1();
        for note in &model.notes {
            // 注意条,不是一段琥珀色的字——理由同关注列表页。
            head = head.child(warning_band(text.note_band_tag, note));
        }
        let season_line = model.season.as_ref().map_or_else(
            || text.analytics_no_season.to_owned(),
            |season| {
                format!(
                    "{} {} · {}",
                    text.analytics_season, season.label, season.started_day
                )
            },
        );
        let as_of = model.pulse.as_of_day.clone().unwrap_or_default();
        head = head.child(
            div()
                .h_flex()
                .items_center()
                .gap_3()
                .child(mono(season_line).text_size(fs(FS_11_5)))
                .child(
                    mono(format!(
                        "{as_of} · {} {}",
                        model.data_days, text.analytics_days
                    ))
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_META)),
                ),
        );
        if let Some(health) = &model.pulse.anchor_health {
            let median = health
                .market_median_move_bps
                .map_or_else(|| "-".to_owned(), signed_percent);
            head = head
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            mono(self.display_name(health.anchor_asset_id.as_str()))
                                .text_size(fs(FS_11_5)),
                        )
                        .child(chip(
                            match health.drift {
                                ptt_runtime::domain::AnchorDrift::Steady => StatusKind::Idle,
                                _ => StatusKind::Warning,
                            },
                            report_text::anchor_drift(language, health.drift),
                        ))
                        .child(
                            mono(format!(
                                "{}: {}↑ {}↓ {}= · {median}",
                                text.analytics_breadth, health.risers, health.fallers, health.flat,
                            ))
                            .text_size(fs(FS_10_5))
                            .text_color(c(TEXT_META)),
                        ),
                )
                .children(health.crosses.iter().map(|cross| {
                    let drift = cross
                        .drift_bps
                        .map_or_else(|| "-".to_owned(), signed_percent);
                    mono(format!(
                        "{} = {} ({drift})",
                        self.display_name(cross.asset_id.as_str()),
                        cross.latest_rate.text,
                    ))
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_META))
                }));
        }

        // 数据天数不够基线时,把「趋势整列都是横杠」的原因写在顶栏,
        // 而不是让人对着 24 个横杠猜(§6)。
        if u64::from(model.data_days) < self.trend_baseline_days() {
            head = head.child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(div().size(px(6.)).flex_none().rounded_full().bg(c(WARN)))
                    .child(div().text_size(fs(FS_10_5)).text_color(c(WARN_TEXT)).child(
                        gpui::SharedString::from(report_text::fill(
                            text.analytics_trend_baseline,
                            &[
                                &self.trend_baseline_days().to_string(),
                                &model.data_days.to_string(),
                            ],
                        )),
                    )),
            );
        }

        // Column headers, then one row per currency, demand-pressure order.
        let header = div()
            .h_flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .text_size(fs(FS_10_5))
            .text_color(c(TEXT_META))
            .child(div().w(px(190.)).child(gpui::SharedString::from("")))
            .child(cell_text(text.analytics_col_value, 96.))
            .child(cell_text(text.analytics_col_demand, 90.))
            .child(cell_text(text.analytics_col_supply, 90.))
            .child(cell_text(text.analytics_col_ratio, 130.))
            .child(cell_text(text.analytics_col_trend, 166.))
            .child(cell_text(text.analytics_col_rows, 60.));

        let anchor_id = model
            .pulse
            .anchor_asset_id
            .as_ref()
            .map(|asset| asset.as_str().to_owned());
        let selected = self.analytics_selected.clone();
        let mut rows = div().flex().flex_col();
        let mut zebra = false;
        for asset in &model.pulse.assets {
            let id = asset.asset_id.as_str().to_owned();
            let is_selected = selected.as_deref() == Some(id.as_str());
            let is_anchor = anchor_id.as_deref() == Some(id.as_str());
            // 价值不写比率(§6 规则 1):「91:2」精确但读不动,「45.50」才是
            // 人要的数。除法只是显示投影,不回写计算。
            let value = asset
                .value_in_anchor
                .as_ref()
                .map_or_else(|| "—".to_owned(), super::watchlist::per_unit_text);
            let demand = asset
                .demand_anchor
                .map_or_else(|| format!("{}?", wan(asset.demand_units)), wan);
            let supply = asset
                .supply_anchor
                .map_or_else(|| format!("{}?", wan(asset.supply_units)), wan);
            let ratio = demand_supply_ratio(asset.demand_anchor, asset.supply_anchor);

            // 默认态不给标签(§6 规则 2):供需均衡就是没事,没事不占眼睛。
            let mut ratio_cell = div()
                .w(px(130.))
                .flex_none()
                .h_flex()
                .items_center()
                .gap(px(6.))
                .child(
                    mono(ratio.unwrap_or_else(|| "—".to_owned()))
                        .text_size(fs(FS_11_5))
                        .text_color(c(TEXT_DATA)),
                );
            if asset.class != ptt_runtime::domain::LiquidityClass::Balanced {
                ratio_cell = ratio_cell.child(chip_table(
                    class_kind(asset.class),
                    report_text::liquidity_class(language, asset.class),
                ));
            }

            rows = rows.child(self.analytics_row(
                asset,
                &id,
                is_selected,
                is_anchor,
                zebra,
                value,
                demand,
                supply,
                ratio_cell,
                language,
                cx,
            ));
            zebra = !zebra;
        }

        panel()
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(panel_header(text.page_analytics))
            .child(head)
            .child(header)
            .child(scrollable(rows, "analytics-rows"))
    }

    /// One pulse row at the fixed 28px height, trend drawn as its shape.
    #[allow(clippy::too_many_arguments)]
    fn analytics_row(
        &self,
        asset: &ptt_runtime::domain::AssetPulse,
        id: &str,
        is_selected: bool,
        is_anchor: bool,
        zebra: bool,
        value: String,
        demand: String,
        supply: String,
        ratio_cell: gpui::Div,
        language: UiLanguage,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let text = self.text();

        // 趋势列(§6 定稿):曲线就是趋势,不再写词。
        let trend_cell: gpui::AnyElement = if is_anchor {
            // 锚对自己恒为 1:画出来是一条直线,是误导不是信息。
            div()
                .text_size(fs(FS_10_5))
                .text_color(c(TEXT_GHOST))
                .child(text.analytics_anchor_constant)
                .into_any_element()
        } else {
            #[allow(clippy::cast_possible_truncation)]
            let baseline_days = self.trend_baseline_days() as usize;
            let points: Vec<f32> = asset
                .value_by_day
                .iter()
                .rev()
                .take(baseline_days)
                .rev()
                .filter_map(|(_, rate)| {
                    if rate.denominator == 0 {
                        None
                    } else {
                        #[allow(clippy::cast_precision_loss)]
                        Some(rate.numerator as f32 / rate.denominator as f32)
                    }
                })
                .collect();
            let missing = baseline_days.saturating_sub(points.len());
            if points.len() < 2 {
                div()
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_DISABLED))
                    .child(gpui::SharedString::from(report_text::fill(
                        text.analytics_days_short,
                        &[&missing.to_string()],
                    )))
                    .into_any_element()
            } else if missing > 0 {
                // 有几天画几根柱 + 还差几天:不画假曲线。
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(crate::ui::day_bars(&points))
                    .child(
                        div()
                            .text_size(fs(FS_10))
                            .text_color(c(TEXT_DISABLED))
                            .child(gpui::SharedString::from(report_text::fill(
                                text.analytics_days_short,
                                &[&missing.to_string()],
                            ))),
                    )
                    .into_any_element()
            } else {
                let relative = asset.trend_bps_relative.unwrap_or(0);
                // 涨用主题强调色(深色=金,浅色=墨蓝;绿被新鲜度占了),
                // 跌用砖红,±2% 灰——一半的行都持平,全上色就没重点了。
                let (line, fill, text_color) = if relative >= FLAT_BAND_BASIS_POINTS {
                    (ACCENT, ACCENT_FILL, ACCENT_TEXT)
                } else if relative <= -FLAT_BAND_BASIS_POINTS {
                    (DANGER, DANGER_WASH, DANGER_TEXT)
                } else {
                    (TEXT_DISABLED, TREND_FLAT_FILL, TEXT_META)
                };
                let delta = asset
                    .trend_bps_relative
                    .map_or_else(|| "—".to_owned(), signed_percent);
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(crate::ui::sparkline(points, line, fill))
                    .child(mono(delta).text_size(fs(FS_10_5)).text_color(c(text_color)))
                    .into_any_element()
            }
        };
        let _ = language;

        let click_id = id.to_owned();
        let mut row = div()
            .id(gpui::SharedString::from(format!("pulse-{id}")))
            .h(px(H_TABLE_ROW))
            .flex_none()
            .h_flex()
            .items_center()
            .gap_2()
            .px_2()
            .border_b_1()
            .border_color(c(HAIRLINE_SOFT))
            .text_size(fs(FS_11_5))
            .cursor_pointer()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.analytics_selected = Some(click_id.clone());
                    cx.notify();
                }),
            )
            .child(
                div()
                    .w(px(190.))
                    .flex_none()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_color(c(if is_anchor { ACCENT_TEXT } else { TEXT_PRIMARY }))
                    .child(gpui::SharedString::from(self.display_name(id))),
            )
            .child(cell_mono(value, 96.))
            .child(cell_mono(demand, 90.))
            .child(cell_mono(supply, 90.))
            .child(ratio_cell)
            .child(div().w(px(166.)).flex_none().child(trend_cell))
            .child(cell_mono(asset.listing_rows.to_string(), 60.));
        if is_selected {
            row = row.bg(c(SELECTED));
        } else if zebra {
            row = row.bg(c(ZEBRA));
        }
        row
    }

    /// The selected currency's day-by-day story.
    fn analytics_detail(&self, model: &AnalyticsModel, language: UiLanguage) -> gpui::Div {
        let text = self.text();
        let report = report_text::report(language);
        let Some(asset) = self.analytics_selected.as_deref().and_then(|id| {
            model
                .pulse
                .assets
                .iter()
                .find(|asset| asset.asset_id.as_str() == id)
        }) else {
            return div().w(px(0.)).flex_none();
        };

        let mut body = div().p_3().flex().flex_col().gap_1();
        let value = asset
            .value_in_anchor
            .as_ref()
            .map_or_else(|| "-".to_owned(), |rate| rate.text.clone());
        let composed = if asset.value_is_composed {
            format!(" ({})", text.analytics_composed)
        } else {
            String::new()
        };
        body = body
            .child(kv_row(
                text.analytics_col_value,
                &format!("{value}{composed}"),
            ))
            .child(kv_row(
                text.analytics_col_class,
                report_text::liquidity_class(language, asset.class),
            ));
        if let Some(raw) = asset.trend_bps_raw {
            let relative = asset
                .trend_bps_relative
                .map_or_else(|| "-".to_owned(), signed_percent);
            body = body.child(kv_row(
                text.analytics_col_trend,
                &format!("{} / {relative}", signed_percent(raw)),
            ));
        }
        if let Some(norm) = asset.circulation_norm_units {
            body = body.child(kv_row(text.analytics_detail_norm, &norm.to_string()));
        }
        body = body.child(kv_row(
            text.analytics_detail_days,
            &asset.days_observed.to_string(),
        ));
        if asset.greedy_candidate {
            body = body.child(chip(StatusKind::Monitoring, report.analytics_marker_greedy));
        }

        // Recent days: value text plus a supply bar scaled to the asset's own
        // maximum. Widths are display geometry only.
        let recent: Vec<_> = asset.supply_by_day.iter().rev().take(14).rev().collect();
        let max_supply = recent.iter().map(|(_, units)| *units).max().unwrap_or(0);
        if !recent.is_empty() {
            body = body.child(
                div()
                    .pt_2()
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_META))
                    .child(gpui::SharedString::from(
                        text.analytics_detail_series.to_string(),
                    )),
            );
            for (day, units) in recent {
                let value_of_day = asset
                    .value_by_day
                    .iter()
                    .find(|(value_day, _)| value_day == day)
                    .map_or_else(|| "-".to_owned(), |(_, rate)| rate.text.clone());
                // Proportional width, 0..=120px, floor — geometry, not data.
                let width = if max_supply == 0 {
                    0.0
                } else {
                    ((units.saturating_mul(120) / max_supply.max(1)) as u32).min(120) as f32
                };
                body = body.child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            mono(day[5..].to_owned())
                                .text_size(fs(FS_10_5))
                                .text_color(c(TEXT_META)),
                        )
                        .child(div().h(px(8.)).w(px(width)).bg(c(ACCENT_LINE)).flex_none())
                        .child(mono(format!("{units} · {value_of_day}")).text_size(fs(FS_10_5))),
                );
            }
        }

        panel()
            .w(px(360.))
            .flex_none()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(panel_header(&self.display_name(asset.asset_id.as_str())))
            .child(scrollable(body, "analytics-detail"))
    }
}

fn cell_text(label: &str, width: f32) -> gpui::Div {
    div()
        .w(px(width))
        .flex_none()
        .child(gpui::SharedString::from(label.to_string()))
}

fn cell_mono(value: String, width: f32) -> gpui::Div {
    div()
        .w(px(width))
        .flex_none()
        .overflow_hidden()
        .child(mono(value).text_size(fs(FS_11_5)))
}
