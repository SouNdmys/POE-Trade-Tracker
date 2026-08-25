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
use crate::ui::{StatusKind, empty_state, freshness_kind, kv_row, mono, panel, panel_header};

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

        // 58px 指标带(§8):最新价、吃单价(金)、挂单价(灰)、价差。指标带
        // 照常显示——最新价和价差一帧就能算,不等蜡烛攒够。
        let stat = |label: &'static str, value: String, marker: Option<u32>, color: u32| {
            let cell = div()
                .flex_none()
                .flex()
                .flex_col()
                .justify_center()
                .gap(px(2.))
                .px(px(SP_16));
            let mut value_row = div().h_flex().items_baseline().gap(px(6.));
            if let Some(marker) = marker {
                // 图上那条虚线的颜色,在这里当图例块。
                value_row = value_row.child(
                    div()
                        .w(px(14.))
                        .h(px(2.))
                        .flex_none()
                        .my(px(6.))
                        .bg(c(marker)),
                );
            }
            value_row = value_row.child(mono(value).text_size(fs(FS_15)).text_color(c(color)));
            cell.child(value_row).child(
                div()
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_META))
                    .child(label),
            )
        };
        let divider = || {
            div()
                .w(px(1.))
                .flex_none()
                .my(px(SP_10))
                .bg(c(HAIRLINE_SOFT))
        };

        let mut band = div()
            .h(px(58.))
            .flex_none()
            .flex()
            .bg(c(PANEL))
            .border_1()
            .border_color(c(HAIRLINE))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap(px(2.))
                    .px(px(SP_16))
                    .child(div().text_size(fs(FS_12_5)).child(gpui::SharedString::from(
                        self.pair_label(model.have.as_str(), model.need.as_str()),
                    )))
                    .children(model.light.map(|status| {
                        div()
                            .h_flex()
                            .items_center()
                            .gap(px(5.))
                            .child(crate::ui::status_dot(freshness_kind(status)))
                            .child(
                                div()
                                    .text_size(fs(FS_10_5))
                                    .text_color(c(TEXT_SECONDARY))
                                    .child(report_text::freshness_light(language, status)),
                            )
                    })),
            )
            .child(divider())
            .child(stat(
                text.history_latest,
                rate(&summary.latest_rate),
                None,
                TEXT_DATA,
            ))
            .child(divider())
            // 两条虚线是重点:现在的吃单价(金)和挂单价(灰),中间那条缝
            // 就是价差。
            .child(stat(
                text.history_taker,
                rate(&summary.latest_taker_rate),
                Some(ACCENT),
                ACCENT_TEXT,
            ))
            .child(stat(
                text.history_maker,
                rate(&summary.latest_maker_rate),
                Some(NEUTRAL_DOT),
                TEXT_SECONDARY,
            ));
        if let Some(spread) = summary.spread_basis_points {
            band = band.child(divider()).child(stat(
                text.history_spread,
                report_text::percent_from_basis_points(spread),
                None,
                TEXT_DATA,
            ));
        }
        band = band.child(div().flex_1()).child(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .justify_center()
                .gap(px(2.))
                .px(px(SP_16))
                .text_size(fs(FS_10))
                .text_color(c(TEXT_GHOST))
                // 纵轴方向必须标出来,否则"涨"是好是坏说不清(§8)。
                .child(gpui::SharedString::from(report_text::fill(
                    text.history_axis_note,
                    &[
                        &self.display_name(model.have.as_str()),
                        &self.display_name(model.need.as_str()),
                    ],
                )))
                .child(gpui::SharedString::from(
                    text.history_color_legend.to_string(),
                )),
        );

        let mut facts = div()
            .px(px(SP_10))
            .py(px(SP_8))
            .flex()
            .flex_col()
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
        if summary.historical_only {
            facts = facts.child(
                mono(report_text::report(language).nothing_current)
                    .text_size(fs(FS_11))
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
                    .child(crate::ui::chip_table(
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

        // 右栏 300px:摘要事实 + 当前盘口两侧队列(标题写明它对你意味着
        // 什么)。队列取自最近一帧,且只在那一帧就是这一对时才画——别的
        // 对的盘口画在这里就是张冠李戴。
        let mut side = crate::ui::detail_panel(text.page_history).child(facts);
        if let Some(book) = &self.last_book
            && book.have == model.have.as_str()
            && book.need == model.need.as_str()
        {
            for (title, side_key) in [
                (text.history_available_title, "available"),
                (text.history_competing_title, "competing"),
            ] {
                let rows: Vec<_> = book
                    .order_rows
                    .iter()
                    .filter(|row| row.side == side_key)
                    .collect();
                if rows.is_empty() {
                    continue;
                }
                side = side.child(
                    div()
                        .px(px(SP_10))
                        .pt_1()
                        .text_size(fs(FS_10_5))
                        .text_color(c(TEXT_META))
                        .child(gpui::SharedString::from(title.to_string())),
                );
                let mut list = div().px(px(SP_10)).pb_1().flex().flex_col();
                for row in rows {
                    list = list.child(
                        div()
                            .h(px(20.))
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                mono(row.rate.clone())
                                    .flex_1()
                                    .text_size(fs(FS_11))
                                    .text_color(c(if row.aggregate {
                                        TEXT_META
                                    } else {
                                        TEXT_DATA
                                    })),
                            )
                            .child(
                                mono(row.stock.to_string())
                                    .text_size(fs(FS_11))
                                    .text_color(c(TEXT_SECONDARY)),
                            ),
                    );
                }
                side = side.child(list);
            }
        }

        div()
            .flex_grow()
            .flex()
            .flex_col()
            .gap(px(SP_8))
            .p(px(SP_10))
            .overflow_hidden()
            .child(band)
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .gap(px(SP_8))
                    .overflow_hidden()
                    .child(self.candle_panel(&model.candles))
                    .child(side),
            )
    }

    /// The chart.
    ///
    /// 数据不够时不再整页置空(§8):有几根画几根,标题里说清还差多少。
    /// 24 小时 = 288 根五分钟蜡烛。
    fn candle_panel(&self, candles: &[PriceCandle]) -> gpui::Div {
        const FULL_DAY_CANDLES: usize = 288;
        let text = self.text();
        let mut header = div()
            .h(px(H_INPUT))
            .flex_none()
            .h_flex()
            .items_center()
            .px_3()
            .bg(c(RAIL))
            .border_b_1()
            .border_color(c(HAIRLINE))
            .child(crate::ui::micro_title(text.history_chart))
            .child(div().flex_1());
        if candles.len() < FULL_DAY_CANDLES {
            header = header.child(
                mono(report_text::fill(
                    text.history_candle_progress,
                    &[&candles.len().to_string(), &FULL_DAY_CANDLES.to_string()],
                ))
                .text_size(fs(FS_10_5))
                .text_color(c(TEXT_DISABLED)),
            );
        }
        let body = panel()
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(header);
        if candles.len() < 2 {
            // One candle is a dot, not a shape. The facts panel beside this
            // still states the price.
            return body.child(empty_state(text.history_too_short));
        }
        // Oldest first, which is the order the model keeps them in: a chart
        // that runs backwards is a chart that lies about direction.
        let data: Vec<PriceCandle> = candles.to_vec();
        body.child(
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
