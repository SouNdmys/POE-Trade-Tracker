//! 官方交易所总览页——ninja 式表格的首版（数值列 + 百分比，迷你走势线后加）。
//!
//! 这页读的是成交量证据域（官方 API 的账），和市场分析页（挂单簿证据域）
//! 并排放着：同一个市场，两本账，各说各的事实，谁也不冒充谁。

use gpui::{Context, ParentElement, Styled, div, px};
use gpui_component::StyledExt as _;

use crate::shell::AppShell;
use crate::state::PageData;
use crate::theme::*;
use crate::ui::{StatusKind, chip, empty_state, mono, panel};

/// 首版列出多少行。按锚计价成交量降序，前 40 已覆盖绝大部分成交额；
/// 全量列表等滚动容器一起来。
const ROW_LIMIT: usize = 40;

/// 走势列宽：最多 14 根柱 × (3px 柱 + 1px 缝) + 余量。
const SPARK_WIDTH: f32 = 70.;
const SPARK_DAYS: usize = 14;

impl AppShell {
    /// The Exchange page.
    #[cfg(windows)]
    pub(crate) fn render_exchange(&mut self, _cx: &mut Context<Self>) -> gpui::Div {
        let PageData::Exchange(model) = &self.report else {
            return div().flex_grow().flex().flex_col().gap_3().p_3().child(
                panel()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(empty_state(&self.report_body().join("  "))),
            );
        };
        let model: ptt_runtime::reports::ExchangeModel = (**model).clone();
        let text = self.text();

        // ---- 页头：联赛、锚、覆盖率、市场漂移 ----
        let mut head = div().p_3().flex().flex_col().gap_1();
        let drift = model
            .market_median_move_bps
            .map_or_else(|| "-".to_owned(), signed_percent);
        head = head.child(
            div()
                .h_flex()
                .items_center()
                .gap_3()
                .child(mono(model.league.clone()).text_size(fs(FS_11_5)))
                .child(
                    mono(format!(
                        "{} {}",
                        text.exchange_col_value,
                        self.display_name(model.anchor_asset_id.as_str())
                    ))
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_META)),
                )
                .child(
                    mono(format!(
                        "{} {}%",
                        text.exchange_coverage, model.coverage_percent
                    ))
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_META)),
                )
                .child(
                    mono(format!("{} {drift}", text.exchange_drift))
                        .text_size(fs(FS_10_5))
                        .text_color(c(TEXT_META)),
                ),
        );

        // ---- 大雷达："新面孔"条 ----
        // 值得看 + 证据，一行一个；没有新面孔就整条不画，不占注意力。
        // 忽略语义与关注列表页共用同一份列表（量翻倍才再提醒），
        // 这里暂不放忽略按钮——去关注列表页忽略同样生效。
        if !model.radar.is_empty() {
            let mut band = div()
                .px_3()
                .pb_1()
                .h_flex()
                .items_center()
                .gap_3()
                .flex_wrap();
            band = band.child(chip(StatusKind::Warning, text.exchange_radar_tag));
            for item in &model.radar {
                let reason = match item.signal {
                    ptt_runtime::reports::ExchangeRadarSignal::VolumeSurge { percent } => {
                        ptt_runtime::report_text::fill(
                            text.exchange_radar_surge,
                            &[&percent.to_string()],
                        )
                    }
                    ptt_runtime::reports::ExchangeRadarSignal::Appreciating { relative_bps } => {
                        ptt_runtime::report_text::fill(
                            text.exchange_radar_rise,
                            &[&signed_percent(relative_bps)],
                        )
                    }
                    ptt_runtime::reports::ExchangeRadarSignal::PriceGap { gap_bps } => {
                        ptt_runtime::report_text::fill(
                            text.exchange_radar_gap,
                            &[&signed_percent(gap_bps)],
                        )
                    }
                };
                band = band.child(
                    mono(format!(
                        "{} · {reason}",
                        self.display_name(item.asset_id.as_str())
                    ))
                    .text_size(fs(FS_10_5)),
                );
            }
            head = head.child(band);
        }

        // ---- 表头 ----
        let header = div()
            .px_3()
            .py_1()
            .h_flex()
            .items_center()
            .gap_2()
            .child(head_cell(text.exchange_col_asset, 210.))
            .child(head_cell(text.exchange_col_value, 110.))
            .child(head_cell(text.exchange_col_trend, 80.))
            .child(head_cell(text.exchange_col_spark, SPARK_WIDTH))
            .child(head_cell(text.exchange_col_volume, 90.))
            .child(head_cell(text.exchange_col_depth, 90.))
            .child(head_cell(text.exchange_surge_tag, 70.))
            .child(
                mono(text.exchange_col_partner)
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_META)),
            );

        let mut rows = div().flex().flex_col().min_h(px(0.));
        if model.rows.is_empty() {
            rows = rows.child(empty_state(text.exchange_no_data));
        }
        for row in model.rows.iter().take(ROW_LIMIT) {
            let name = self.display_name(row.asset_id.as_str());
            let value = row
                .value_in_anchor
                .as_ref()
                .map_or_else(|| "-".to_owned(), ratio_text);
            let trend = row
                .trend_bps_relative
                .map_or_else(|| "-".to_owned(), signed_percent);
            let partner = row
                .top_partner
                .as_ref()
                .map_or_else(String::new, |partner| self.display_name(partner.as_str()));
            // 放量标签只在显著时出现（≥2 倍自身小时中位），别把每行都点亮。
            let surge = row.surge_percent.filter(|percent| *percent >= 200);
            let mut line = div()
                .px_3()
                .py_1()
                .h_flex()
                .items_center()
                .gap_2()
                .child(
                    div().w(px(210.)).flex_none().child(
                        mono(if row.tracked {
                            format!("{name} ·")
                        } else {
                            name
                        })
                        .text_size(fs(FS_11_5)),
                    ),
                )
                .child(data_cell(value, 110.))
                .child(data_cell(trend, 80.))
                .child(
                    div()
                        .w(px(SPARK_WIDTH))
                        .flex_none()
                        .child(spark_bars(&row.value_by_day)),
                )
                .child(data_cell(compact_amount(row.volume_per_hour_anchor), 90.))
                .child(data_cell(
                    row.depth_anchor
                        .map_or_else(|| "-".to_owned(), compact_amount),
                    90.,
                ));
            line = if let Some(percent) = surge {
                line.child(
                    div()
                        .w(px(70.))
                        .flex_none()
                        .child(chip(StatusKind::Warning, &format!("{percent}%"))),
                )
            } else {
                line.child(div().w(px(70.)).flex_none())
            };
            line = line.child(
                mono(partner)
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_META)),
            );
            rows = rows.child(line);
        }

        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .overflow_hidden()
            .child(
                panel()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(head)
                    .child(header)
                    .child(rows),
            )
    }
}

fn head_cell(label: &str, width: f32) -> gpui::Div {
    div().w(px(width)).flex_none().child(
        mono(label.to_owned())
            .text_size(fs(FS_10_5))
            .text_color(c(TEXT_META)),
    )
}

fn data_cell(value: String, width: f32) -> gpui::Div {
    div()
        .w(px(width))
        .flex_none()
        .child(mono(value).text_size(fs(FS_11_5)))
}

/// 迷你日柱：有几天画几根，不画假折线（Analytics 页的既定裁定 §6）。
/// min-max 归一化只决定柱高，f32 只在这条绘制边界上出现。
fn spark_bars(values: &[ptt_trade_domain::Ratio]) -> gpui::Div {
    let recent = values.len().saturating_sub(SPARK_DAYS);
    let points: Vec<f32> = values[recent..]
        .iter()
        .map(|rate| rate.numerator as f32 / (rate.denominator as f32).max(1.0))
        .collect();
    let mut row = div().h(px(16.)).h_flex().items_end().gap(px(1.));
    if points.len() < 2 {
        // 一天以内说不出"走势"，留白比一根孤柱诚实。
        return row;
    }
    let min = points.iter().copied().fold(f32::INFINITY, f32::min);
    let max = points.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let span = (max - min).max(f32::EPSILON);
    for value in &points {
        let height = 4.0 + 12.0 * ((value - min) / span);
        row = row.child(div().w(px(3.)).h(px(height)).bg(c(TEXT_GHOST)));
    }
    row
}

/// `+50.00%` / `-26.00%`：与监视页同款，正负一眼可辨。
fn signed_percent(basis_points: i64) -> String {
    let text = ptt_runtime::report_text::percent_from_basis_points(basis_points);
    if basis_points >= 0 && !text.starts_with('+') {
        format!("+{text}")
    } else {
        text
    }
}

/// Ratio → 展示小数。f64 只在这条绘制边界上出现（既有裁定）。
fn ratio_text(ratio: &ptt_trade_domain::Ratio) -> String {
    let value = ratio.numerator as f64 / (ratio.denominator as f64).max(1.0);
    if value >= 100.0 {
        format!("{value:.0}")
    } else if value >= 1.0 {
        format!("{value:.2}")
    } else {
        format!("{value:.3}")
    }
}

/// 8_500_000 → "8.5M"，读表扫得快比位数精确重要。
fn compact_amount(value: u64) -> String {
    if value >= 10_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 10_000 {
        format!("{:.0}k", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}
