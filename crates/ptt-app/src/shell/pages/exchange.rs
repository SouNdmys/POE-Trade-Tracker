//! 官方交易所总览页——ninja 式表格的首版（数值列 + 百分比，迷你走势线后加）。
//!
//! 这页读的是成交量证据域（官方 API 的账），和市场分析页（挂单簿证据域）
//! 并排放着：同一个市场，两本账，各说各的事实，谁也不冒充谁。

use gpui::{
    Context, InteractiveElement as _, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{Sizable, Size, StyledExt as _, input::Input, select::Select};

use super::convert::{AssetChoice, AssetList};
use crate::shell::{AppShell, ExchangeRange};
use crate::state::PageData;
use crate::theme::*;
use crate::ui::{
    LedgerButton, StatusKind, bucket_points, bucket_size, button, chip, detail_panel, empty_state,
    hour_bars, kv_headline, kv_row, mean_points, mono, panel, scrollable, slot_at, sparkline_sized,
    sum_points,
};

/// 滚动列表也设个上限：尾巴里是每小时几笔的冷门，画六百行 div 换不来信息。
const ROW_LIMIT: usize = 200;

// （曾经是一张 1/2/3/…/17/21/25/45 的档位表——六测吐槽档位缝像抽奖，
// 而下拉自带搜索，长列表不碍事，于是换成 1..=数据天数 的完整列表。）

/// 走势列宽。柱宽随天数自适应，7 天到 60 天都塞得进这一格。
const SPARK_WIDTH: f32 = 110.;

/// 面板核对最多列几对：越界率最高的几对就是要看的，长尾是噪音。
const RECONCILE_LIMIT: usize = 6;

/// ±2% 以内算持平（与市场分析页同值）：涨跌列和曲线在这条带里不上色。
const FLAT_BAND_BASIS_POINTS: i64 = 200;

/// 柱图画多少天：跟涨跌选择器同一个 N（二测反馈：两处不同步看着像坏了），
/// 但至少 7 根——一两根柱子摆不出"走势"这个词。
fn spark_days(model: &ptt_runtime::reports::ExchangeModel) -> usize {
    (model.trend_days as usize).clamp(7, 60)
}

impl AppShell {
    /// The Exchange page.
    #[cfg(windows)]
    pub(crate) fn render_exchange(&mut self, cx: &mut Context<Self>) -> gpui::Div {
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
                    // 手上真实有几天日线——"拉没拉到"从此有地方看。
                    mono(ptt_runtime::report_text::fill(
                        text.exchange_data_days,
                        &[&model.data_days.to_string()],
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

        // ---- 同步进度行 + 手动同步 ----
        // 首测教训：回补进行中页面空白又不报进度，看起来像卡死。
        // 水位、欠账、按钮放在一行，"到哪了"和"推一把"都有地方。
        let (sync_line, sync_is_fault) = sync_status_line(
            text,
            model.synced_through,
            model.hours_behind,
            self.exchange_sync_error.as_deref(),
        );
        head = head.child(
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .child(
                    mono(sync_line)
                        .text_size(fs(FS_10_5))
                        .text_color(if sync_is_fault {
                            c(DANGER_TEXT)
                        } else if model.hours_behind > 1 {
                            c(TEXT_DATA)
                        } else {
                            c(TEXT_META)
                        }),
                )
                .child(
                    button(
                        "exchange-sync-now",
                        LedgerButton::Secondary,
                        text.exchange_sync_now,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.restart_exchange_sync(cx);
                        cx.notify();
                    })),
                )
                .child(
                    button(
                        "exchange-export",
                        LedgerButton::Quiet,
                        text.exchange_export,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.export_exchange(cx);
                        cx.notify();
                    })),
                )
                .child(
                    div()
                        .w(px(96.))
                        .flex_none()
                        .child(Select::new(&self.exchange_trend_select).with_size(Size::Small)),
                )
                // ---- 截至哪天看 ----
                // 四测提出的"从赛季初看到 6-30"：终点钉在一天，涨跌与走势
                // 从那天往回算；小时级的列诚实留空。
                .child(
                    mono(text.exchange_as_of_label.to_owned())
                        .text_size(fs(FS_10_5))
                        .text_color(c(TEXT_META)),
                )
                .child(
                    div()
                        .w(px(110.))
                        .flex_none()
                        .child(Input::new(&self.exchange_as_of_input).with_size(Size::Small)),
                )
                .child(
                    button(
                        "exchange-as-of-apply",
                        LedgerButton::Quiet,
                        text.exchange_as_of_apply,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.apply_exchange_as_of(cx);
                        cx.notify();
                    })),
                )
                .when(model.historical, |line| {
                    line.child(
                        button(
                            "exchange-as-of-now",
                            LedgerButton::Secondary,
                            text.exchange_as_of_now,
                            cx,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.set_exchange_as_of(String::new(), cx);
                            cx.notify();
                        })),
                    )
                }),
        );
        if let Some(as_of) = model.as_of_day.as_deref().filter(|_| model.historical) {
            head = head.child(div().h_flex().items_center().child(chip(
                StatusKind::Warning,
                &ptt_runtime::report_text::fill(text.exchange_historical_tag, &[as_of]),
            )));
        }

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
                            &[&format!("{:.1}", percent as f64 / 100.0)],
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
                // 升值那条的证据是"涨了多少"，走表内同一套三态色；放量与
                // 价差的证据是倍数和偏离，不是涨跌，保持灰阶。
                let reason_color = match item.signal {
                    ptt_runtime::reports::ExchangeRadarSignal::Appreciating { relative_bps } => {
                        trend_tones(Some(relative_bps)).2
                    }
                    _ => TEXT_SECONDARY,
                };
                band = band.child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_1()
                        .child(
                            mono(format!("{} ·", self.display_name(item.asset_id.as_str())))
                                .text_size(fs(FS_10_5)),
                        )
                        .child(
                            mono(reason)
                                .text_size(fs(FS_10_5))
                                .text_color(c(reason_color)),
                        ),
                );
            }
            head = head.child(band);
        }

        // ---- 面板核对：两本账碰头 ----
        // 面板抓到的最优价对官方同小时实际成交区间。总览一行常驻（"你看到
        // 的价是不是真价"要有地方看），越界的对才逐行列出，每行带故事：
        // 更差 = 别吃单，更好 = 多半误读，几乎全越界 = 先查映射。
        if let Some(reconcile) = &model.reconcile {
            use ptt_runtime::report_text::fill;
            use ptt_runtime::reports::ExchangeReconcileReading;
            let days = reconcile.window_days.to_string();
            let summary = if reconcile.samples == 0 && reconcile.unmatched == 0 {
                fill(text.exchange_reconcile_none, &[&days])
            } else {
                let mut line = fill(
                    text.exchange_reconcile_summary,
                    &[
                        &days,
                        &reconcile.hits.to_string(),
                        &reconcile.samples.to_string(),
                    ],
                );
                if reconcile.unmatched > 0 {
                    line.push(' ');
                    line.push_str(&fill(
                        text.exchange_reconcile_unmatched,
                        &[&reconcile.unmatched.to_string()],
                    ));
                }
                line
            };
            let suspect = reconcile
                .pairs
                .iter()
                .any(|pair| pair.reading == ExchangeReconcileReading::SuspectMapping);
            let weak = reconcile.samples > 0 && reconcile.hits * 100 < reconcile.samples * 80;
            let kind = if suspect || weak {
                StatusKind::Warning
            } else {
                StatusKind::Idle
            };
            let mut band = div().px_3().pb_1().flex().flex_col().gap_1();
            band = band.child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(chip(kind, text.exchange_reconcile_tag))
                    .child(
                        mono(summary)
                            .text_size(fs(FS_10_5))
                            .text_color(c(TEXT_META)),
                    ),
            );
            for pair in reconcile.pairs.iter().take(RECONCILE_LIMIT) {
                let deviation = signed_percent(pair.deviation_bps);
                let reason = match pair.reading {
                    ExchangeReconcileReading::SuspectMapping => {
                        text.exchange_reconcile_mapping.to_owned()
                    }
                    ExchangeReconcileReading::PanelWorse => {
                        fill(text.exchange_reconcile_worse, &[&deviation])
                    }
                    ExchangeReconcileReading::PanelBetter => {
                        fill(text.exchange_reconcile_better, &[&deviation])
                    }
                    ExchangeReconcileReading::Sporadic => {
                        fill(text.exchange_reconcile_sporadic, &[&deviation])
                    }
                };
                let head_line = fill(
                    text.exchange_reconcile_pair,
                    &[
                        &self.display_name(pair.from_asset_id.as_str()),
                        &self.display_name(pair.to_asset_id.as_str()),
                        &pair.misses.to_string(),
                        &pair.samples.to_string(),
                    ],
                );
                band = band.child(
                    mono(format!("{head_line} · {reason}"))
                        .text_size(fs(FS_10_5))
                        .text_color(c(TEXT_DATA)),
                );
            }
            head = head.child(band);
        }

        // ---- 表头 ----
        // 成交列的表头带上当前档位：列里的数是按它算的，不标就是哑谜。
        let volume_label = if model.window_hours.is_some() {
            format!(
                "{} · {}",
                text.exchange_col_volume,
                self.exchange_range.label(text)
            )
        } else {
            text.exchange_col_volume.to_owned()
        };
        let range_row = div().px_3().pb_1().child(self.exchange_range_row(cx));
        let header = div()
            .px_3()
            .py_1()
            .h_flex()
            .items_center()
            .gap_2()
            .child(head_cell(text.exchange_col_asset, 210.))
            .child(head_cell(text.exchange_col_value, 110.))
            .child(head_cell(
                &ptt_runtime::report_text::fill(
                    text.exchange_col_trend_days,
                    &[&model.trend_days.to_string()],
                ),
                80.,
            ))
            .child(head_cell(
                &ptt_runtime::report_text::fill(
                    text.exchange_col_trend_days,
                    &[&spark_days(&model).to_string()],
                ),
                SPARK_WIDTH,
            ))
            .child(head_cell(&volume_label, 90.))
            .child(head_cell(text.exchange_col_depth, 90.))
            .child(head_cell(text.exchange_surge_tag, 70.))
            .child(
                mono(text.exchange_col_partner)
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_META)),
            );

        let mut rows = div().flex().flex_col().min_h(px(0.));
        if model.rows.is_empty() {
            // 在回补和真没数据是两句话：前者是"等一下"，后者是"去检查"。
            rows = rows.child(empty_state(if model.hours_behind > 1 {
                text.exchange_backfilling
            } else {
                text.exchange_no_data
            }));
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
            // 放量标签只在显著时出现（≥2 倍自身常态），别把每行都点亮。
            // 展示成倍数（×4.8）而不是百分比：二测反馈百分比读不懂。
            let surge = row.surge_percent.filter(|percent| *percent >= 200);
            // 涨跌列与曲线同一套三态色（八测反馈：满屏灰抓不到重点）。
            let (spark_line, spark_fill, trend_color) = trend_tones(row.trend_bps_relative);
            // 行可选中：点一行，右侧明细栏画它的小时账本（行高固定是硬约束，
            // 细节不在行内展开）。
            let click_id = row.asset_id.as_str().to_owned();
            let is_selected = self.exchange_selected.as_deref() == Some(row.asset_id.as_str());
            let mut line = div()
                .id(SharedString::from(format!(
                    "exchange-{}",
                    row.asset_id.as_str()
                )))
                .h(px(H_TABLE_ROW))
                .flex_none()
                .px_3()
                .h_flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.exchange_selected = Some(click_id.clone());
                        cx.notify();
                    }),
                )
                .when(is_selected, |line| line.bg(c(SELECTED)))
                .child(
                    div()
                        .w(px(210.))
                        .flex_none()
                        .h_flex()
                        .items_center()
                        .gap_1()
                        .child(mono(name).text_size(fs(FS_11_5)))
                        // 关注中的通货：金点——一眼看出哪些是自己的。
                        .when(row.tracked, |cell| {
                            cell.child(
                                mono("·".to_owned())
                                    .text_size(fs(FS_11_5))
                                    .text_color(c(ACCENT_TEXT)),
                            )
                        }),
                )
                .child(data_cell(value, 110.))
                .child(tinted_cell(trend, 80., trend_color))
                .child(div().w(px(SPARK_WIDTH)).flex_none().child(spark_curve(
                    &row.value_by_day,
                    spark_days(&model),
                    spark_line,
                    spark_fill,
                )))
                .child(data_cell(
                    if model.historical {
                        "-".to_owned()
                    } else {
                        compact_amount(row.volume_per_hour_anchor)
                    },
                    90.,
                ))
                .child(data_cell(
                    row.depth_anchor
                        .map_or_else(|| "-".to_owned(), compact_amount),
                    90.,
                ));
            line = if let Some(percent) = surge {
                line.child(div().w(px(70.)).flex_none().child(chip(
                    StatusKind::Warning,
                    &format!("×{:.1}", percent as f64 / 100.0),
                )))
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

        let detail = self.exchange_detail(&model, cx);
        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .gap_3()
            .p_3()
            .overflow_hidden()
            .child(
                panel()
                    .flex_1()
                    .min_w(px(0.))
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(head)
                    .child(range_row)
                    .child(header)
                    .child(scrollable(rows, "exchange-rows")),
            )
            .child(detail)
    }

    /// 时段档位条：24h / 3d / 7d / 全部保留。当前档金字 + 2px 金下划线，
    /// 同雷达页页签的语汇。切档标脏页面：账本已按水位缓存，重算只是重排。
    fn exchange_range_row(&self, cx: &mut Context<Self>) -> gpui::Div {
        let text = self.text();
        let current = self.exchange_range;
        let mut row = div().h_flex().items_center().gap(px(SP_8));
        for range in ExchangeRange::ALL {
            let active = range == current;
            let chip = div()
                .id(range.element_id())
                .h(px(H_INPUT))
                .px(px(SP_8))
                .flex()
                .items_center()
                .text_size(fs(FS_12))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.exchange_range = range;
                    this.report_stale = true;
                    cx.notify();
                }));
            let chip = if active {
                chip.border_b_2()
                    .border_color(c(ACCENT))
                    .font_semibold()
                    .text_color(c(ACCENT_TEXT))
            } else {
                chip.text_color(c(TEXT_SECONDARY))
                    .hover(|style| style.bg(c(HOVER)))
            };
            row = row.child(chip.child(SharedString::from(range.label(text).to_string())));
        }
        row
    }

    /// 右侧明细栏：选中通货的小时账本——最新小时价、窗口成交额、价格线、
    /// 成交柱，或者按一天里的时段汇总。行高固定、不做行内展开是硬约束，
    /// 细节只能在这里。
    fn exchange_detail(
        &self,
        model: &ptt_runtime::reports::ExchangeModel,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let text = self.text();
        let Some(selected) = self.exchange_selected.as_deref() else {
            return div().w(px(0.)).flex_none();
        };
        let Some(row) = model
            .rows
            .iter()
            .find(|row| row.asset_id.as_str() == selected)
        else {
            return div().w(px(0.)).flex_none();
        };
        let title = self.display_name(selected);
        let Some(ledger_model) = &model.ledger else {
            return detail_panel(&title)
                .child(div().p_3().child(empty_state(text.exchange_detail_none)));
        };
        let ledger = &ledger_model.ledger;
        let hours = self.exchange_range.hours();
        let asset = &row.asset_id;
        let points = ledger.points_in(asset, hours);
        let (total, _) = ledger.window_volume(asset, hours);
        let mean = ledger.window_mean_per_hour(asset, hours);
        let missing = ledger.missing_hours(asset, hours);
        let (line_color, fill_color, value_color) = trend_tones(row.trend_bps_relative);
        let latest = points
            .last()
            .map_or_else(|| "-".to_owned(), |point| ratio_text(&point.value));

        let mut body = div().p_3().flex().flex_col().gap_2();
        body = body
            .child(kv_headline(
                text.exchange_detail_latest,
                &latest,
                value_color,
            ))
            .child(kv_headline(
                text.exchange_detail_window_volume,
                &compact_amount(total),
                TEXT_DATA,
            ))
            .child(kv_row(text.exchange_detail_mean, &compact_amount(mean)))
            .child(self.exchange_range_row(cx));

        if let Some((start, end)) = ledger.window(hours) {
            // 本地时区只在这条绘制边界上出现；账本本身是 UTC 整点。
            let offset = chrono::Local::now().offset().local_minus_utc();
            // 悬停的格号只在格数范围内才算数：切档位、切视图后旧格号可能越界。
            let hover_of = |slots: usize| self.exchange_hover.filter(|slot| *slot < slots);
            let mut chart = div().flex().flex_col();
            let slot_count;
            let mut peak_line = None;
            if self.exchange_hour_of_day {
                let profile = ledger.hour_of_day_profile(asset, hours, offset);
                #[allow(clippy::cast_precision_loss)]
                let bars: Vec<Option<f32>> = profile.iter().map(|v| Some(*v as f32)).collect();
                slot_count = bars.len();
                let hover = hover_of(slot_count);
                chart = chart
                    .child(hour_bars(
                        bars,
                        CHART_WIDTH,
                        CHART_BARS_HEIGHT,
                        TEXT_GHOST,
                        hover.map(|slot| (slot, TEXT_META)),
                    ))
                    .child(match hover {
                        Some(slot) => readout_row(ptt_runtime::report_text::fill(
                            text.exchange_hover_hour_of_day,
                            &[
                                &format!("{slot:02}"),
                                &format!("{:02}", (slot + 1) % 24),
                                &compact_amount(profile[slot]),
                            ],
                        )),
                        None => axis_row("00:00", "23:00"),
                    });
                if let Some((from, to)) = ptt_runtime::domain::peak_window(&profile) {
                    peak_line = Some(
                        mono(ptt_runtime::report_text::fill(
                            text.exchange_detail_peak,
                            &[&format!("{from:02}"), &format!("{to:02}")],
                        ))
                        .text_size(fs(FS_11))
                        .text_color(c(ACCENT_TEXT)),
                    );
                }
            } else {
                // 按小时格铺开：没成交的小时留 None，曲线在那里断开。
                let slots = usize::try_from((end - start) / 3600 + 1).unwrap_or(1);
                let mut values: Vec<Option<f32>> = vec![None; slots];
                let mut volumes: Vec<Option<f32>> = vec![None; slots];
                for point in points {
                    let index = usize::try_from((point.hour_ts - start) / 3600).unwrap_or(0);
                    if index < slots {
                        #[allow(clippy::cast_precision_loss)]
                        let value = point.value.numerator as f32
                            / (point.value.denominator as f32).max(1.0);
                        values[index] = Some(value);
                        #[allow(clippy::cast_precision_loss)]
                        let volume = point.anchor_volume as f32;
                        volumes[index] = Some(volume);
                    }
                }
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let max_slots = (CHART_WIDTH / 2.0) as usize;
                let per_slot = bucket_size(slots, max_slots);
                let values = bucket_points(&values, max_slots, mean_points);
                let volumes = bucket_points(&volumes, max_slots, sum_points);
                slot_count = volumes.len();
                let hover = hover_of(slot_count);
                let readout = hover.map(|slot| {
                    hover_readout(
                        text,
                        start,
                        end,
                        per_slot,
                        slot,
                        values[slot],
                        volumes[slot],
                    )
                });
                chart = chart
                    .child(sparkline_sized(
                        values,
                        CHART_WIDTH,
                        CHART_LINE_HEIGHT,
                        line_color,
                        fill_color,
                        hover,
                    ))
                    .child(hour_bars(
                        volumes,
                        CHART_WIDTH,
                        CHART_BARS_HEIGHT,
                        TEXT_DISABLED,
                        hover.map(|slot| (slot, TEXT_META)),
                    ))
                    .child(match readout {
                        Some(readout) => readout_row(readout),
                        None => axis_row(&local_hour(start), &local_hour(end)),
                    });
            }
            // 悬停读数而不是跟着鼠标走的浮框：细节走右栏、不做悬浮卡片（硬约束 #2），
            // 固定一行也不会挡住图。看不见的画布只负责把图表的位置记下来，
            // 鼠标事件拿它把横坐标换算成格号——和校准页的画布同一套写法。
            let bounds_slot = self.exchange_chart_bounds.clone();
            body = body.child(
                chart
                    .id("exchange-chart")
                    .relative()
                    .w(px(CHART_WIDTH))
                    .child(
                        gpui::canvas(
                            move |bounds, _, _| bounds_slot.set(Some(bounds)),
                            |_, (), _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .on_mouse_move(
                        cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                            let Some(bounds) = this.exchange_chart_bounds.get() else {
                                return;
                            };
                            let x = f32::from(event.position.x - bounds.origin.x);
                            let slot = slot_at(x, f32::from(bounds.size.width), slot_count);
                            if slot != this.exchange_hover {
                                this.exchange_hover = slot;
                                cx.notify();
                            }
                        }),
                    )
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        if !*hovered && this.exchange_hover.is_some() {
                            this.exchange_hover = None;
                            cx.notify();
                        }
                    })),
            );
            if let Some(line) = peak_line {
                body = body.child(line);
            }
        }

        let toggle_label = if self.exchange_hour_of_day {
            text.exchange_detail_by_hours
        } else {
            text.exchange_detail_hour_of_day
        };
        body = body.child(
            div().h_flex().items_center().gap_2().child(
                button(
                    "exchange-hour-of-day",
                    LedgerButton::Quiet,
                    toggle_label,
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.exchange_hour_of_day = !this.exchange_hour_of_day;
                    cx.notify();
                })),
            ),
        );
        if missing > 0 {
            body = body.child(
                mono(ptt_runtime::report_text::fill(
                    text.exchange_detail_missing,
                    &[&missing.to_string()],
                ))
                .text_size(fs(FS_10_5))
                .text_color(c(TEXT_META)),
            );
        }
        detail_panel(&title).child(scrollable(body, "exchange-detail"))
    }

    /// 把这个联赛的全部日线导成 CSV + JSON——先弹系统的选文件夹对话框
    /// （八测反馈：写死在 C 盘占空间），选好再写；取消就什么都不做。
    /// 对话框是异步的（oneshot），所以写文件放在等到结果之后；几万行几秒钟，
    /// 一个手动按钮等得起，路径写进日志，用户从那里找文件。
    #[cfg(windows)]
    pub(crate) fn export_exchange(&mut self, cx: &mut Context<Self>) {
        let game = self.settings.active_profile.game;
        let league = self
            .settings
            .market_tuning(game)
            .exchange
            .league
            .trim()
            .to_owned();
        if league.is_empty() {
            self.push_log("exchange: set the league in Settings before exporting".to_owned());
            return;
        }
        // 成交额按当前锚折算：锚和交易所页同一条选法。
        let anchor = match ptt_runtime::reports::exchange_anchor(&self.settings.market_tuning(game))
        {
            Ok(anchor) => anchor,
            Err(error) => {
                self.push_log(format!("exchange: export: {error}"));
                return;
            }
        };
        // Windows 的对话框只能"只选文件"或"只选文件夹"二选一，这里要文件夹。
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(self.text().exchange_export_prompt.into()),
        });
        cx.spawn(async move |this, cx| {
            let outcome = match picked.await {
                // 读整张日线表再写两个文件要几秒:放后台线程,窗口不冻住。
                Ok(Ok(Some(paths))) => match paths.into_iter().next() {
                    Some(directory) => Some(
                        cx.background_executor()
                            .spawn(async move {
                                ptt_runtime::exchange_export::write_exchange_export(
                                    game, &league, &anchor, &directory,
                                )
                                .map(|outcome| (outcome.base, outcome.rows.len()))
                            })
                            .await,
                    ),
                    None => None,
                },
                Ok(Ok(None)) => None,
                Ok(Err(error)) => Some(Err(format!("dialog: {error}"))),
                Err(_) => Some(Err("dialog closed without an answer".to_owned())),
            };
            this.update(cx, |this: &mut AppShell, cx| {
                match outcome {
                    None => this.push_log("exchange: export cancelled".to_owned()),
                    Some(Ok((base, count))) => this.push_log(format!(
                        "exchange: exported {count} rows to {}.csv / .json",
                        base.display()
                    )),
                    Some(Err(error)) => {
                        this.push_log(format!("exchange: export failed: {error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 日期框里的那天成为"截至哪天看"。空 = 回到现在。格式不对只写日志，
    /// 设置不动——写进去一个坏日期页面会静默回到实时视角，比报错更迷惑。
    #[cfg(windows)]
    pub(crate) fn apply_exchange_as_of(&mut self, cx: &mut Context<Self>) {
        let raw = self.exchange_as_of_input.read(cx).value().trim().to_owned();
        let value = if raw.is_empty() {
            String::new()
        } else {
            match chrono::NaiveDate::parse_from_str(&raw, "%Y-%m-%d") {
                Ok(day) => day.to_string(),
                Err(_) => {
                    self.push_log(format!(
                        "exchange: as-of date must be YYYY-MM-DD, got {raw:?}"
                    ));
                    return;
                }
            }
        };
        self.set_exchange_as_of(value, cx);
    }

    /// 截至日期是设置的一部分（页面数据在后台按设置算），改了就保存并让
    /// 页面重算。
    #[cfg(windows)]
    pub(crate) fn set_exchange_as_of(&mut self, value: String, _cx: &mut Context<Self>) {
        let game = self.settings.active_profile.game;
        if self.settings.market_tuning(game).exchange.as_of_day == value {
            return;
        }
        self.settings.market_tuning_mut(game).exchange.as_of_day = value;
        match self.settings_store.save(&self.settings) {
            Ok(()) => self.report_stale = true,
            Err(error) => self.push_log(format!("settings save failed: {error}")),
        }
    }

    /// 把涨跌天数下拉的选项对齐到"手上真实有几天数据"。
    ///
    /// 重建选项需要 window，后台答案带不动它，所以在每帧 render 里装配，
    /// 靠 (数据天数, 选中值) 的签名挡住重复重建——和兑换页选择器同一个套路。
    /// settings.json 里手填的任意天数不在档位表里也会被塞进选项，
    /// 免得选单显示空白把人吓一跳。
    #[cfg(windows)]
    pub(crate) fn sync_exchange_trend_select(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let PageData::Exchange(model) = &self.report else {
            return;
        };
        let data_days = model.data_days.max(1);
        let selected = self
            .settings
            .market_tuning(self.settings.active_profile.game)
            .exchange
            .trend_days;
        if self.exchange_trend_synced == (data_days, selected) {
            return;
        }
        self.exchange_trend_synced = (data_days, selected);

        let text = self.text();
        // 1 到"手上真实有几天"的完整整数列表——这才是当初说好的"任意天"。
        let mut days: Vec<u64> = (1..=u64::from(data_days).min(120)).collect();
        if !days.contains(&selected) {
            days.push(selected);
            days.sort_unstable();
        }
        let choices: Vec<AssetChoice> = days
            .iter()
            .map(|day| {
                AssetChoice::new(
                    day.to_string(),
                    ptt_runtime::report_text::fill(
                        text.exchange_col_trend_days,
                        &[&day.to_string()],
                    ),
                    vec![day.to_string()],
                )
            })
            .collect();
        let select = self.exchange_trend_select.clone();
        let value = gpui::SharedString::from(selected.to_string());
        select.update(cx, |state, cx| {
            state.set_items(AssetList::new(choices), window, cx);
            state.set_selected_value(&value, window, cx);
        });
    }
}

/// 明细栏图表的宽度：明细栏 300 减两边内边距。高度分线和柱两档。
const CHART_WIDTH: f32 = 270.;
const CHART_LINE_HEIGHT: f32 = 56.;
const CHART_BARS_HEIGHT: f32 = 36.;

/// 图下面的两端时刻：左起点、右终点，中间不标——300px 里标不下更多。
fn axis_row(left: &str, right: &str) -> gpui::Div {
    div()
        .w(px(CHART_WIDTH))
        .h_flex()
        .justify_between()
        .child(
            mono(left.to_owned())
                .text_size(fs(FS_10))
                .text_color(c(TEXT_META)),
        )
        .child(
            mono(right.to_owned())
                .text_size(fs(FS_10))
                .text_color(c(TEXT_META)),
        )
}

/// 悬停时顶替两端时刻的那一行：同一行高、同一字号，图不会跳。
fn readout_row(text: String) -> gpui::Div {
    div()
        .w(px(CHART_WIDTH))
        .h_flex()
        .child(mono(text).text_size(fs(FS_10)).text_color(c(TEXT_DATA)))
}

/// 悬停读数：一格一小时就报那一小时的数；像素不够并了格，就报区间与
/// 合计成交、均价——并出来的数就说是并的，不冒充某一小时。
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn hover_readout(
    text: &crate::i18n::Text,
    start: i64,
    end: i64,
    per_slot: usize,
    slot: usize,
    value: Option<f32>,
    volume: Option<f32>,
) -> String {
    let first = start + i64::try_from(slot * per_slot).unwrap_or(0) * 3600;
    let (when, template) = if per_slot == 1 {
        (local_hour(first), text.exchange_hover_point)
    } else {
        let last = (first + i64::try_from(per_slot - 1).unwrap_or(0) * 3600).min(end);
        (
            format!("{}–{}", local_hour(first), local_clock(last)),
            text.exchange_hover_bucket,
        )
    };
    match (value, volume) {
        (Some(value), Some(volume)) => ptt_runtime::report_text::fill(
            template,
            &[&when, &compact_amount(volume as u64), &price_text(value)],
        ),
        _ => ptt_runtime::report_text::fill(text.exchange_hover_gap, &[&when]),
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

/// 水位时间戳按本地时区显示——个人工具，读表的人在哪个时区一目了然。
/// 同步进度行的文字与"这是不是故障"。失败原因优先于"几分钟内会自己补齐"——
/// 联赛名拼错、限速、断网在页面上必须长得和"数据在路上"不一样；
/// 水位还在时保留它，用户既知道停在哪也知道为什么。
fn sync_status_line(
    text: &crate::i18n::Text,
    synced_through: Option<i64>,
    hours_behind: i64,
    sync_error: Option<&str>,
) -> (String, bool) {
    let progress = synced_through.map(|mark| {
        ptt_runtime::report_text::fill(
            text.exchange_synced_through,
            &[&local_hour(mark), &hours_behind.to_string()],
        )
    });
    match (sync_error, progress) {
        (Some(error), Some(progress)) => (
            format!(
                "{progress} · {}",
                ptt_runtime::report_text::fill(text.exchange_sync_failed, &[error])
            ),
            true,
        ),
        (Some(error), None) => (
            ptt_runtime::report_text::fill(text.exchange_sync_failed, &[error]),
            true,
        ),
        (None, Some(progress)) => (progress, false),
        (None, None) => (text.exchange_no_data.to_owned(), false),
    }
}

fn local_hour(hour_ts: i64) -> String {
    chrono::DateTime::from_timestamp(hour_ts, 0).map_or_else(
        || "?".to_owned(),
        |ts| {
            ts.with_timezone(&chrono::Local)
                .format("%m-%d %H:00")
                .to_string()
        },
    )
}

/// 只有钟点的本地时刻，给区间读数的右端用（左端已经带了日期）。
fn local_clock(hour_ts: i64) -> String {
    chrono::DateTime::from_timestamp(hour_ts, 0).map_or_else(
        || "?".to_owned(),
        |ts| ts.with_timezone(&chrono::Local).format("%H:00").to_string(),
    )
}

/// 走势列：最近 `span_days` 天的日 VWAP 画成市场分析页同款的面积曲线；
/// 不够两天就画柱，不画假折线（§6 既定裁定）。f32 只出现在这条绘制边界上。
fn spark_curve(
    values: &[ptt_trade_domain::Ratio],
    span_days: usize,
    line: Token,
    fill: Token,
) -> gpui::AnyElement {
    let recent = values.len().saturating_sub(span_days);
    #[allow(clippy::cast_precision_loss)]
    let points: Vec<f32> = values[recent..]
        .iter()
        .map(|rate| rate.numerator as f32 / (rate.denominator as f32).max(1.0))
        .collect();
    if points.len() < 2 {
        return crate::ui::day_bars(&points).into_any_element();
    }
    crate::ui::sparkline(points, line, fill).into_any_element()
}

/// 涨跌的三态色（线色, 填充, 字色），与市场分析页同一条规矩：涨=金、跌=砖红
/// （绿留给新鲜度），±2% 以内持平灰——一半的行都在持平带里，全上色就没重点。
fn trend_tones(relative_bps: Option<i64>) -> (Token, Token, Token) {
    match relative_bps {
        Some(bps) if bps >= FLAT_BAND_BASIS_POINTS => (ACCENT, ACCENT_FILL, ACCENT_TEXT),
        Some(bps) if bps <= -FLAT_BAND_BASIS_POINTS => (DANGER, DANGER_WASH, DANGER_TEXT),
        _ => (TEXT_DISABLED, TREND_FLAT_FILL, TEXT_META),
    }
}

/// 带字色的数值格：只给涨跌列用，其余数值列保持"色字=主题"的灰阶。
fn tinted_cell(value: String, width: f32, color: Token) -> gpui::Div {
    div()
        .w(px(width))
        .flex_none()
        .child(mono(value).text_size(fs(FS_11_5)).text_color(c(color)))
}

/// `+50.00%` / `-26.00%`：与监视页同款，正负一眼可辨。
/// 超过 ±999% 只显示 `>+999%` / `<-999%`：开服头几天以稀缺锚计价的涨幅能到
/// 几千万个百分点，数字没错但一格只放得下一行（28px 硬约束），方向比位数重要。
fn signed_percent(basis_points: i64) -> String {
    const CAP_BASIS_POINTS: i64 = 999 * 100;
    if basis_points > CAP_BASIS_POINTS {
        return ">+999%".to_owned();
    }
    if basis_points < -CAP_BASIS_POINTS {
        return "<-999%".to_owned();
    }
    let text = ptt_runtime::report_text::percent_from_basis_points(basis_points);
    if basis_points >= 0 && !text.starts_with('+') {
        format!("+{text}")
    } else {
        text
    }
}

/// Ratio → 展示小数。f64 只在这条绘制边界上出现（既有裁定）。
fn ratio_text(ratio: &ptt_trade_domain::Ratio) -> String {
    price_text(ratio.numerator as f64 / (ratio.denominator as f64).max(1.0))
}

/// 价格的位数跟着数量级走：几百的整数、几块的两位、几分的三位。
fn price_text(value: impl Into<f64>) -> String {
    let value = value.into();
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

#[cfg(test)]
mod exchange_page_tests {
    use super::*;

    #[test]
    fn a_sync_failure_shows_on_the_progress_line_and_reads_as_a_fault() {
        // 联赛拼错、429、断网以前都只在会被盖掉的日志行里闪一下，
        // 页面永远说"几分钟内会自己补齐"。错误必须落在页面上。
        let text = crate::i18n::text(ptt_settings::UiLanguage::English);
        let (line, is_fault) = sync_status_line(text, Some(1_700_000_000), 2, Some("boom"));
        assert!(line.contains("boom"), "{line}");
        assert!(
            line.contains("2 h behind"),
            "the watermark must survive: {line}"
        );
        assert!(is_fault);
        let (line, is_fault) = sync_status_line(text, None, 0, Some("boom"));
        assert!(line.contains("boom"), "{line}");
        assert!(!line.contains(text.exchange_no_data), "{line}");
        assert!(is_fault);
        let (line, is_fault) = sync_status_line(text, None, 0, None);
        assert_eq!(line, text.exchange_no_data);
        assert!(!is_fault);
    }

    #[test]
    fn signed_percent_caps_runaway_values_to_one_cell() {
        // 开服头几天的卡牌以神圣计价能涨几千万个百分点；数字本身没错，
        // 但一格只放得下一行（28px 硬约束）。封顶显示，方向还在。
        assert_eq!(signed_percent(782), "+7.82%");
        assert_eq!(signed_percent(-369), "-3.69%");
        assert_eq!(signed_percent(99_900), "+999.00%");
        assert_eq!(signed_percent(99_901), ">+999%");
        assert_eq!(signed_percent(4_844_094_326), ">+999%");
        assert_eq!(signed_percent(-99_901), "<-999%");
    }
}
