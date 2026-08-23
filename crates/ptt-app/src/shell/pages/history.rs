//! What a pair has been doing: a summary, a chart, and what looks off.
//!
//! The only place in the app where an exact rate becomes a floating point
//! number. A chart is pixels, and pixels are approximate; the conversion
//! happens inside the plotting closures and nowhere else, so nothing that
//! decides anything ever sees the result. Every number the page states in
//! words comes from the rational value.

use gpui::{Context, ParentElement, Styled, div, px};
use gpui_component::{StyledExt as _, chart::CandlestickChart};
use ptt_runtime::domain::PriceCandle;
use ptt_runtime::report_text;
use ptt_runtime::reports::HistoryModel;
use ptt_trade_domain::Ratio;

use crate::shell::AppShell;
use crate::state::PageData;
use crate::theme::*;
use crate::ui::{StatusKind, chip, empty_state, freshness_kind, kv_row, mono, panel, panel_header};

/// A rate as a plot coordinate.
///
/// Display only. The rational value is what every stated number comes from;
/// this exists because a chart has to land on a pixel, and a pixel cannot
/// hold a fraction. Nothing derived from it is allowed back into the model.
fn plot_value(rate: &Ratio) -> f64 {
    if rate.denominator == 0 {
        return 0.0;
    }
    rate.numerator as f64 / rate.denominator as f64
}

impl AppShell {
    /// The history page.
    pub(crate) fn render_history(&mut self, _cx: &mut Context<Self>) -> gpui::Div {
        let text = self.text();
        let language = self.language();

        let PageData::History(model) = &self.report else {
            return div().flex_grow().flex().flex_col().gap_3().p_3().child(
                panel()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(empty_state(&self.report_body().join("  "))),
            );
        };
        let model: &HistoryModel = model;

        let Some(summary) = &model.summary else {
            return div().flex_grow().flex().flex_col().gap_3().p_3().child(
                panel()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(panel_header(text.page_history))
                    .child(empty_state(&report_text::fill(
                        report_text::report(language).no_history_yet,
                        &[
                            &self.display_name(model.have.as_str()),
                            &self.display_name(model.need.as_str()),
                        ],
                    ))),
            );
        };

        let rate = |value: &Option<Ratio>| {
            value
                .as_ref()
                .map_or_else(|| "—".to_owned(), |rate| rate.text.clone())
        };
        let mut facts = div()
            .p_3()
            .flex()
            .flex_col()
            .child(kv_row(
                text.history_pair,
                &self.pair_label(model.have.as_str(), model.need.as_str()),
            ))
            .child(kv_row(text.history_latest, &rate(&summary.latest_rate)))
            .child(kv_row(
                text.history_taker,
                &rate(&summary.latest_taker_rate),
            ))
            .child(kv_row(
                text.history_maker,
                &rate(&summary.latest_maker_rate),
            ))
            .child(kv_row(
                text.history_band,
                &format!(
                    "{} · {} · {}",
                    rate(&summary.min_rate),
                    rate(&summary.median_rate),
                    rate(&summary.max_rate)
                ),
            ));
        if let Some(range) = summary.range_basis_points {
            facts = facts.child(kv_row(
                text.history_range,
                &report_text::percent_from_basis_points(range),
            ));
        }
        if let Some(spread) = summary.spread_basis_points {
            facts = facts.child(kv_row(
                text.history_spread,
                &report_text::percent_from_basis_points(spread),
            ));
        }
        // How much of the series is worth acting on, rather than just how
        // much of it there is.
        facts = facts.child(kv_row(
            text.history_points,
            &format!(
                "{} / {} · {}·{}·{}·{}",
                summary.point_count,
                summary.snapshot_count,
                summary.fresh_point_count,
                summary.usable_point_count,
                summary.stale_point_count,
                summary.archived_point_count,
            ),
        ));
        if let Some(status) = model.light {
            facts = facts.child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .py(px(3.))
                    .child(crate::ui::status_dot(freshness_kind(status)))
                    .child(
                        div()
                            .text_size(fs(FS_11_5))
                            .child(report_text::freshness_light(language, status)),
                    ),
            );
        }
        if summary.historical_only {
            facts = facts.child(
                mono(report_text::report(language).nothing_current)
                    .text_size(fs(FS_11_5))
                    .text_color(c(WARN_TEXT)),
            );
        }
        for anomaly in &model.anomalies {
            facts = facts.child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .py(px(3.))
                    .child(chip(
                        StatusKind::Warning,
                        report_text::price_anomaly_kind(language, anomaly.kind),
                    ))
                    .child(
                        mono(format!(
                            "{}{}",
                            report_text::anomaly_severity(language, anomaly.severity),
                            anomaly.basis_points.map_or_else(String::new, |points| {
                                format!("  {}", report_text::percent_from_basis_points(points))
                            })
                        ))
                        .text_size(fs(FS_10_5))
                        .text_color(c(TEXT_META)),
                    ),
            );
        }

        div()
            .flex_grow()
            .flex()
            .gap_3()
            .p_3()
            .overflow_hidden()
            .child(self.candle_panel(&model.candles))
            .child(
                panel()
                    .w(px(360.))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(panel_header(text.page_history))
                    .child(facts),
            )
    }

    /// The chart.
    fn candle_panel(&self, candles: &[PriceCandle]) -> gpui::Div {
        let text = self.text();
        let panel = panel()
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(panel_header(text.history_chart));
        if candles.len() < 2 {
            // One candle is a dot, not a shape. The facts panel beside this
            // still states the price.
            return panel.child(empty_state(text.history_too_short));
        }
        // Oldest first, which is the order the model keeps them in: a chart
        // that runs backwards is a chart that lies about direction.
        let data: Vec<PriceCandle> = candles.to_vec();
        panel.child(
            div().flex_1().p_3().child(
                CandlestickChart::new(data)
                    .x(|candle: &PriceCandle| candle.bucket_start.format("%H:%M").to_string())
                    .open(|candle: &PriceCandle| plot_value(&candle.open))
                    .high(|candle: &PriceCandle| plot_value(&candle.high))
                    .low(|candle: &PriceCandle| plot_value(&candle.low))
                    .close(|candle: &PriceCandle| plot_value(&candle.close)),
            ),
        )
    }
}
