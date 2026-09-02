//! 官方交易所总览页——ninja 式表格的首版（数值列 + 百分比，迷你走势线后加）。
//!
//! 这页读的是成交量证据域（官方 API 的账），和市场分析页（挂单簿证据域）
//! 并排放着：同一个市场，两本账，各说各的事实，谁也不冒充谁。

use gpui::{Context, IntoElement, ParentElement, Styled, div, prelude::FluentBuilder as _, px};
use gpui_component::{Sizable, Size, StyledExt as _, input::Input, select::Select};

use super::convert::{AssetChoice, AssetList};
use crate::shell::AppShell;
use crate::state::PageData;
use crate::theme::*;
use crate::ui::{LedgerButton, StatusKind, button, chip, empty_state, mono, panel, scrollable};

/// 滚动列表也设个上限：尾巴里是每小时几笔的冷门，画六百行 div 换不来信息。
const ROW_LIMIT: usize = 200;

// （曾经是一张 1/2/3/…/17/21/25/45 的档位表——六测吐槽档位缝像抽奖，
// 而下拉自带搜索，长列表不碍事，于是换成 1..=数据天数 的完整列表。）

/// 走势列宽。柱宽随天数自适应，7 天到 60 天都塞得进这一格。
const SPARK_WIDTH: f32 = 110.;

/// 面板核对最多列几对：越界率最高的几对就是要看的，长尾是噪音。
const RECONCILE_LIMIT: usize = 6;

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
        let sync_line = model.synced_through.map_or_else(
            || text.exchange_no_data.to_owned(),
            |mark| {
                ptt_runtime::report_text::fill(
                    text.exchange_synced_through,
                    &[&local_hour(mark), &model.hours_behind.to_string()],
                )
            },
        );
        head = head.child(
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .child(mono(sync_line).text_size(fs(FS_10_5)).text_color(
                    if model.hours_behind > 1 {
                        c(TEXT_DATA)
                    } else {
                        c(TEXT_META)
                    },
                ))
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
                        .child(spark_curve(&row.value_by_day, spark_days(&model))),
                )
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
                    .child(scrollable(rows, "exchange-rows")),
            )
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
        // Windows 的对话框只能"只选文件"或"只选文件夹"二选一，这里要文件夹。
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(self.text().exchange_export_prompt.into()),
        });
        cx.spawn(async move |this, cx| {
            let outcome = match picked.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next().map(|directory| {
                    ptt_runtime::exchange_export::write_exchange_export(
                        game.as_str(),
                        &league,
                        &directory,
                    )
                    .map(|outcome| (outcome.base, outcome.rows.len()))
                }),
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

/// 走势列：最近 `span_days` 天的日 VWAP 画成市场分析页同款的面积曲线；
/// 不够两天就画柱，不画假折线（§6 既定裁定）。f32 只出现在这条绘制边界上。
fn spark_curve(values: &[ptt_trade_domain::Ratio], span_days: usize) -> gpui::AnyElement {
    let recent = values.len().saturating_sub(span_days);
    #[allow(clippy::cast_precision_loss)]
    let points: Vec<f32> = values[recent..]
        .iter()
        .map(|rate| rate.numerator as f32 / (rate.denominator as f32).max(1.0))
        .collect();
    if points.len() < 2 {
        return crate::ui::day_bars(&points).into_any_element();
    }
    crate::ui::sparkline(points, TEXT_DISABLED, TREND_FLAT_FILL).into_any_element()
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
