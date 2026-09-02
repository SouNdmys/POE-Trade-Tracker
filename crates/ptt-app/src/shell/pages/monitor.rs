//! The monitor: is the data alive, what did the watcher just read, and what
//! that book is worth. UI-DESIGN.md §4(监视器), mock `4a`.
//!
//! 三层按用户的三件事排:健康带(数据是不是活的)→ 最近盘口(刚才读到
//! 什么)→ 左「这个盘口能怎么赚」/ 右「下一步去抓」+「跳过原因」。

use gpui::{Context, ParentElement, SharedString, Styled, div, px};
use gpui_component::StyledExt as _;
use ptt_runtime::report_text;

use crate::shell::AppShell;
use crate::state::PageData;
use crate::theme::*;
use crate::ui::{StatusKind, chip_table, mono, panel};

/// 健康带对「最近一帧」的三档判断,秒。
///
/// 这是"循环还活着吗"的界面判断,不是数据新鲜度的算法判断(那个在
/// `MarketTuning` 里,尺度是小时):面板开着时几秒一帧,五分钟没有新帧
/// 说明面板早关了或者循环卡住了。
const BAND_FRESH_SECONDS: u64 = 300;
const BAND_AGING_SECONDS: u64 = 1800;

/// `"1:9.33"` → 每 1 个左边换 9.33 个右边。
///
/// 比率是面板上的原文,聚合行还带着 `<`/`>` 前缀;这里只为了算队首价差,
/// 解析不动原文。
fn rate_value(rate: &str) -> Option<f64> {
    let (left, right) = rate.split_once(':')?;
    let left: f64 = left.trim().trim_start_matches(['<', '>']).parse().ok()?;
    let right: f64 = right.trim().trim_start_matches(['<', '>']).parse().ok()?;
    if left <= 0.0 {
        return None;
    }
    Some(right / left)
}

/// 队首价差:可用队首比竞争队首贵多少(百分比)。
///
/// 这两个数原来隔着六行才能对上;蜡烛图页管它叫价差,这里是同一个数的
/// 现场版。解析不了(空侧、奇怪文本)就不显示——宁缺毋假。
fn front_spread_percent(available: Option<&str>, competing: Option<&str>) -> Option<f64> {
    let ask = rate_value(available?)?;
    let bid = rate_value(competing?)?;
    if bid <= 0.0 {
        return None;
    }
    Some((ask - bid) / bid * 100.0)
}

impl AppShell {
    /// The monitor page.
    pub(crate) fn render_monitor(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex_grow()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .gap(px(SP_8))
            .p(px(SP_10))
            .child(self.health_band())
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .gap(px(SP_8))
                    .child(
                        // 左栏:最近盘口 + 这个盘口能怎么赚。
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap(px(SP_8))
                            .child(self.last_book_panel())
                            .child(self.earn_panel()),
                    )
                    .child(
                        // 右栏:下一步去抓 + 跳过原因。
                        div()
                            .w(px(360.))
                            .flex_none()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap(px(SP_8))
                            .child(self.probe_panel(cx))
                            .child(self.skips_panel()),
                    ),
            )
    }

    /// 56px 健康带:数据是不是活的,一眼一个答案。
    fn health_band(&self) -> gpui::Div {
        let text = self.text();
        let age = self
            .last_book
            .as_ref()
            .map(|book| book.received_at.elapsed().as_secs());
        let (kind, label) = match age {
            None => (StatusKind::Idle, text.monitor_health_waiting),
            Some(seconds) if seconds < BAND_FRESH_SECONDS => {
                (StatusKind::Fresh, text.monitor_health_fresh)
            }
            Some(seconds) if seconds < BAND_AGING_SECONDS => {
                (StatusKind::Warning, text.monitor_health_aging)
            }
            Some(_) => (StatusKind::Error, text.monitor_health_stale),
        };
        let (real_skips, standby) = super::super::standby_skip_split(&self.skips);

        // 一个竖直居中的统计格:15px 主数字 + 10.5px 说明。
        let stat = |value: gpui::Div, label: &'static str| {
            div()
                .flex_none()
                .flex()
                .flex_col()
                .justify_center()
                .gap(px(3.))
                .px(px(14.))
                .child(value)
                .child(
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
            .h(px(56.))
            .flex_none()
            .flex()
            .bg(c(PANEL))
            .border_1()
            .border_color(c(HAIRLINE))
            // 4px 左色条:全带唯一的大色块,新鲜度三色。
            .child(div().w(px(4.)).flex_none().bg(c(kind.dot())))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap(px(3.))
                    .px(px(14.))
                    .min_w(px(170.))
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap(px(6.))
                            .child(crate::ui::status_dot(kind))
                            .child(
                                div()
                                    .text_size(fs(FS_13))
                                    .font_semibold()
                                    .child(SharedString::from(label.to_string())),
                            ),
                    )
                    .children(age.map(|seconds| {
                        mono(report_text::fill(
                            text.monitor_last_frame_ago,
                            &[&seconds.to_string()],
                        ))
                        .text_size(fs(FS_11))
                        .text_color(c(TEXT_META))
                    })),
            )
            .child(divider())
            .child(stat(
                mono(self.accepted.to_string())
                    .text_size(fs(FS_15))
                    .text_color(c(ACCENT_TEXT)),
                text.monitor_accepted_stat,
            ))
            .child(divider());
        if let Some(book) = &self.last_book {
            band = band
                .child(stat(
                    mono(format!("{}ms", book.elapsed_ms))
                        .text_size(fs(FS_15))
                        .text_color(c(TEXT_DATA)),
                    text.monitor_frame_ms_stat,
                ))
                .child(divider());
        }

        // 跳过统计:只把真正的问题当数,待机帧只配一行小灰字。
        let mut skip_cell = div()
            .flex_none()
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(3.))
            .px(px(14.))
            .child(
                div()
                    .h_flex()
                    .items_baseline()
                    .gap(px(6.))
                    .child(
                        mono(real_skips.to_string())
                            .text_size(fs(FS_15))
                            .text_color(c(TEXT_DATA)),
                    )
                    .child(
                        div()
                            .text_size(fs(FS_10_5))
                            .text_color(c(TEXT_META))
                            .child(text.monitor_skipped_stat),
                    )
                    .children((standby > 0).then(|| {
                        div()
                            .text_size(fs(FS_10_5))
                            .text_color(c(TEXT_DISABLED))
                            .child(SharedString::from(report_text::fill(
                                text.monitor_standby_note,
                                &[&standby.to_string()],
                            )))
                    })),
            );
        // 第二行:头两条真正的跳过原因,琥珀点提醒但不惊扰。
        let mut ranked: Vec<(&String, &u64)> = self
            .skips
            .iter()
            .filter(|(key, _)| key.as_str() != "not-book-view")
            .collect();
        ranked.sort_by(|left, right| right.1.cmp(left.1).then_with(|| left.0.cmp(right.0)));
        if !ranked.is_empty() {
            let summary = ranked
                .iter()
                .take(2)
                .map(|(key, count)| {
                    report_text::fill(
                        text.monitor_skip_times,
                        &[
                            &count.to_string(),
                            &super::super::skip_label(key, self.language()),
                        ],
                    )
                })
                .collect::<Vec<_>>()
                .join(" · ");
            skip_cell = skip_cell.child(
                div()
                    .h_flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().size(px(6.)).flex_none().rounded_full().bg(c(WARN)))
                    .child(
                        div()
                            .text_size(fs(FS_10_5))
                            .text_color(c(WARN_TEXT))
                            .child(SharedString::from(summary)),
                    ),
            );
        }
        band.child(skip_cell)
    }

    /// 最近盘口:通货对 + 队首价差 + 可用/竞争左右并排(和浮窗同一套结构)。
    fn last_book_panel(&self) -> gpui::Div {
        let text = self.text();
        let Some(book) = &self.last_book else {
            return panel()
                .flex_none()
                .child(crate::ui::panel_header(text.panel_last_book))
                .child(
                    div()
                        .p_3()
                        .child(mono(text.waiting_for_book).text_size(fs(FS_12))),
                );
        };

        let available: Vec<_> = book
            .order_rows
            .iter()
            .filter(|row| row.side == "available")
            .collect();
        let competing: Vec<_> = book
            .order_rows
            .iter()
            .filter(|row| row.side == "competing")
            .collect();
        let spread = front_spread_percent(
            available
                .iter()
                .find(|row| !row.aggregate)
                .map(|row| row.rate.as_str()),
            competing
                .iter()
                .find(|row| !row.aggregate)
                .map(|row| row.rate.as_str()),
        );

        // 一侧六行:栏头(侧名 + 比率/库存)+ 行(序号 | 比率 | 库存)。
        let column = |title: &'static str, rows: &[&ptt_runtime::pipeline::BookRow]| {
            let mut col = div().flex_1().min_w(px(0.)).flex().flex_col().child(
                div()
                    .h(px(22.))
                    .h_flex()
                    .items_center()
                    .child(
                        div()
                            .text_size(fs(FS_11))
                            .text_color(c(TEXT_SECONDARY))
                            .child(title),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(fs(FS_10))
                            .text_color(c(TEXT_DISABLED))
                            .child(text.monitor_col_rate),
                    )
                    .child(
                        div()
                            .w(px(88.))
                            .flex_none()
                            .text_size(fs(FS_10))
                            .text_color(c(TEXT_DISABLED))
                            .text_right()
                            .child(text.monitor_col_stock),
                    ),
            );
            for (index, row) in rows.iter().enumerate() {
                let last = index + 1 == rows.len();
                // 聚合行不是一条挂单,是"这一档及更差"的总括——整行降灰,
                // 文本原样保留 </> 前缀。
                let rate_color = if row.aggregate {
                    TEXT_META
                } else if index == 0 {
                    TEXT_PRIMARY
                } else {
                    TEXT_DATA
                };
                let mut line = div()
                    .h(px(H_TABLE_ROW))
                    .flex_none()
                    .h_flex()
                    .items_center()
                    .gap(px(SP_8))
                    .child(
                        mono(row.row_index.to_string())
                            .w(px(22.))
                            .flex_none()
                            .text_size(fs(FS_10_5))
                            .text_color(c(if row.aggregate {
                                TEXT_GHOST
                            } else {
                                TEXT_DISABLED
                            })),
                    )
                    .child(
                        mono(row.rate.clone())
                            .flex_1()
                            .text_size(fs(FS_12_5))
                            .text_color(c(rate_color)),
                    )
                    .child(
                        mono(row.stock.to_string())
                            .w(px(88.))
                            .flex_none()
                            .text_right()
                            .text_size(fs(FS_12))
                            .text_color(c(if row.aggregate {
                                TEXT_META
                            } else {
                                TEXT_SECONDARY
                            })),
                    );
                if !last {
                    line = line.border_b_1().border_color(c(HAIRLINE_SOFT));
                }
                col = col.child(line);
            }
            col
        };

        panel()
            .flex_none()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(H_INPUT))
                    .flex_none()
                    .h_flex()
                    .items_center()
                    .px_3()
                    .bg(c(RAIL))
                    .border_b_1()
                    .border_color(c(HAIRLINE))
                    .child(crate::ui::micro_title(text.panel_last_book))
                    .child(div().flex_1())
                    .child(
                        mono(format!(
                            "#{} · {}ms · {}s",
                            book.sequence,
                            book.elapsed_ms,
                            book.received_at.elapsed().as_secs()
                        ))
                        .text_size(fs(FS_10_5))
                        .text_color(c(TEXT_DISABLED)),
                    ),
            )
            .child(
                div()
                    .h(px(30.))
                    .flex_none()
                    .h_flex()
                    .items_center()
                    .gap(px(SP_8))
                    .px_3()
                    .border_b_1()
                    .border_color(c(HAIRLINE_SOFT))
                    .child(
                        div()
                            .text_size(fs(FS_13))
                            .child(SharedString::from(self.pair_label(&book.have, &book.need))),
                    )
                    .child(chip_table(
                        StatusKind::Idle,
                        &report_text::fill(text.book_rows, &[&book.order_rows.len().to_string()]),
                    ))
                    .child(div().flex_1())
                    .children(spread.map(|percent| {
                        div()
                            .h_flex()
                            .items_baseline()
                            .gap(px(6.))
                            .child(
                                div()
                                    .text_size(fs(FS_10_5))
                                    .text_color(c(TEXT_META))
                                    .child(text.monitor_front_spread),
                            )
                            .child(
                                mono(format!("{percent:.2}%"))
                                    .text_size(fs(FS_12))
                                    .text_color(c(ACCENT_TEXT)),
                            )
                    })),
            )
            .child(
                div()
                    .flex()
                    .gap(px(SP_16))
                    .px_3()
                    .py(px(SP_8))
                    .child(column(text.monitor_col_available, &available))
                    .child(div().w(px(1.)).bg(c(HAIRLINE_SOFT)))
                    .child(column(text.monitor_col_competing, &competing)),
            )
    }

    /// 「这个盘口能怎么赚」:种类 / 路径 / 直兑 / 最优 / 收益 的小表格。
    ///
    /// 原来是四行英文流水账;句子拼好就拆不回来,所以数据侧已经改成
    /// typed 的 [`ptt_runtime::analysis::PairAnalysis`],这里只管排版。
    fn earn_panel(&self) -> gpui::Div {
        let text = self.text();
        let language = self.language();

        let header = |basis: Option<String>| {
            let mut row = div()
                .h(px(H_INPUT))
                .flex_none()
                .h_flex()
                .items_center()
                .px_3()
                .bg(c(RAIL))
                .border_b_1()
                .border_color(c(HAIRLINE))
                .child(crate::ui::micro_title(text.monitor_earn_header))
                .child(div().flex_1());
            if let Some(basis) = basis {
                row = row.child(
                    div()
                        .text_size(fs(FS_10_5))
                        .text_color(c(TEXT_DISABLED))
                        .child(SharedString::from(basis)),
                );
            }
            row
        };

        let Some(book) = &self.last_book else {
            return panel()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .flex_col()
                .child(header(None))
                .child(crate::ui::empty_state(text.nothing_yet));
        };
        let analysis = &book.analysis;

        let basis = report_text::fill(
            text.monitor_earn_basis,
            &[&self.display_name(&analysis.have_asset_id)],
        );
        let mut body = panel()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .child(header(Some(basis)));

        if let Some(error) = &analysis.error {
            return body.child(
                div().p_3().child(
                    mono(error.clone())
                        .text_size(fs(FS_11))
                        .text_color(c(DANGER_TEXT)),
                ),
            );
        }
        if analysis.conversion.is_none() && analysis.cycles.is_empty() {
            return body.child(crate::ui::empty_state(text.nothing_yet));
        }

        // 列:种类 52 | 路径 1fr | 直兑 74 | 最优 74 | 收益 62。
        let grid_row = || {
            div()
                .h(px(H_TABLE_ROW))
                .flex_none()
                .h_flex()
                .items_center()
                .px_3()
        };
        let num_cell = |value: Option<String>, color: Token, width: f32| {
            div()
                .w(px(width))
                .flex_none()
                .text_right()
                .font_family(FONT_MONO)
                .text_size(fs(FS_12))
                .text_color(c(if value.is_some() {
                    color
                } else {
                    TEXT_DISABLED
                }))
                .child(SharedString::from(value.unwrap_or_else(|| "—".to_owned())))
        };
        let kind_cell = |label: &'static str| {
            div()
                .w(px(52.))
                .flex_none()
                .text_size(fs(FS_11))
                .text_color(c(TEXT_SECONDARY))
                .child(label)
        };
        let path_cell = |names: Vec<String>, suffix: Option<String>| {
            let mut path = div()
                .flex_1()
                .min_w(px(0.))
                .h_flex()
                .items_center()
                .gap(px(4.))
                .overflow_hidden()
                .text_size(fs(FS_12))
                .text_color(c(TEXT_PRIMARY));
            for (index, name) in names.into_iter().enumerate() {
                if index > 0 {
                    path = path.child(
                        div()
                            .flex_none()
                            .text_color(c(TEXT_GHOST))
                            .child(SharedString::from("→")),
                    );
                }
                path = path.child(div().whitespace_nowrap().child(SharedString::from(name)));
            }
            if let Some(suffix) = suffix {
                path = path.child(
                    div()
                        .flex_none()
                        .text_color(c(TEXT_DISABLED))
                        .child(SharedString::from(suffix)),
                );
            }
            path
        };

        body = body.child(
            grid_row()
                .h(px(H_ROW))
                .border_b_1()
                .border_color(c(HAIRLINE_SOFT))
                .text_size(fs(FS_10_5))
                .text_color(c(TEXT_META))
                .child(div().w(px(52.)).flex_none().child(text.radar_column_kind))
                .child(div().flex_1().child(text.radar_column_route))
                .child(
                    div()
                        .w(px(74.))
                        .flex_none()
                        .text_right()
                        .child(text.monitor_col_direct),
                )
                .child(
                    div()
                        .w(px(74.))
                        .flex_none()
                        .text_right()
                        .child(text.monitor_col_best),
                )
                .child(
                    div()
                        .w(px(62.))
                        .flex_none()
                        .text_right()
                        .child(text.radar_column_edge),
                ),
        );

        if let Some(conversion) = &analysis.conversion {
            let names: Vec<String> = conversion
                .path_asset_ids
                .iter()
                .map(|asset| self.display_name(asset.as_str()))
                .collect();
            let suffix = (conversion.hops() > 1).then(|| {
                report_text::fill(text.monitor_steps_suffix, &[&conversion.hops().to_string()])
            });
            let gain = conversion.gain_basis_points();
            body = body.child(
                grid_row()
                    .border_b_1()
                    .border_color(c(HAIRLINE_SOFT))
                    .child(kind_cell(text.radar_kind_conversion))
                    .child(path_cell(names, suffix))
                    .child(num_cell(
                        conversion.direct_out.map(|out| out.to_string()),
                        TEXT_SECONDARY,
                        74.,
                    ))
                    .child(num_cell(
                        conversion.best_out.map(|out| out.to_string()),
                        TEXT_PRIMARY,
                        74.,
                    ))
                    .child(num_cell(
                        gain.map(signed_percent),
                        if gain.unwrap_or(0) >= 0 {
                            ACCENT_TEXT
                        } else {
                            DANGER_TEXT
                        },
                        62.,
                    )),
            );
            // 风险徽章行:超过 3 条折成「还有 N 条」,完整明细在雷达页。
            if !conversion.risk_flags.is_empty() {
                let labels: Vec<String> = conversion
                    .risk_flags
                    .iter()
                    .map(|flag| report_text::execution_risk_flag(language, *flag).to_owned())
                    .collect();
                let mut risk_row = div()
                    .h(px(H_INPUT))
                    .flex_none()
                    .h_flex()
                    .items_center()
                    .gap(px(6.))
                    .pl(px(64.))
                    .pr_3()
                    .border_b_1()
                    .border_color(c(HAIRLINE_SOFT))
                    .child(crate::ui::chips_table(StatusKind::Warning, &labels, 3));
                if labels.len() > 3 {
                    risk_row = risk_row.child(chip_table(
                        StatusKind::Idle,
                        &report_text::fill(
                            text.monitor_more_risks,
                            &[&(labels.len() - 3).to_string()],
                        ),
                    ));
                }
                risk_row = risk_row.child(div().flex_1()).child(
                    div()
                        .text_size(fs(FS_10_5))
                        .text_color(c(TEXT_DISABLED))
                        .child(text.monitor_see_radar),
                );
                body = body.child(risk_row);
            }
        }

        for (index, cycle) in analysis.cycles.iter().enumerate() {
            let names: Vec<String> = cycle
                .cycle_asset_ids
                .iter()
                .map(|asset| self.display_name(asset.as_str()))
                .collect();
            let profit = cycle.profit_basis_points;
            let mut row = grid_row()
                .child(kind_cell(text.radar_kind_loop))
                .child(path_cell(names, None))
                .child(num_cell(None, TEXT_DISABLED, 74.))
                .child(num_cell(None, TEXT_DISABLED, 74.))
                .child(num_cell(
                    profit.map(signed_percent),
                    if profit.unwrap_or(0) >= 0 {
                        ACCENT_TEXT
                    } else {
                        // 负数用砖红文字:已批准的例外。
                        DANGER_TEXT
                    },
                    62.,
                ));
            if index + 1 != analysis.cycles.len() {
                row = row.border_b_1().border_color(c(HAIRLINE_SOFT));
            }
            body = body.child(row);
        }

        // 收口一句人话:三条闭环都在亏时,替读者把结论说出来。
        let all_losing = !analysis.cycles.is_empty()
            && analysis
                .cycles
                .iter()
                .all(|cycle| cycle.profit_basis_points.unwrap_or(0) < 0);
        if all_losing {
            body = body.child(div().flex_1()).child(
                div()
                    .h(px(H_ROW))
                    .flex_none()
                    .h_flex()
                    .items_center()
                    .px_3()
                    .border_t_1()
                    .border_color(c(HAIRLINE_SOFT))
                    .child(
                        div()
                            .text_size(fs(FS_10_5))
                            .text_color(c(TEXT_DISABLED))
                            .child(text.monitor_cycles_losing),
                    ),
            );
        }
        body
    }

    /// 「下一步去抓」:排队的在前(金色左条),建议在后;和浮窗底条打通。
    fn probe_panel(&self, cx: &mut Context<Self>) -> gpui::Div {
        use gpui::{InteractiveElement as _, StatefulInteractiveElement as _};

        let text = self.text();
        let language = self.language();

        // (通货对, 理由, 是否已排队)——先排队的,后建议的。
        let mut entries: Vec<(String, String, String, bool)> = self
            .probe_queue
            .entries()
            .iter()
            .map(|entry| {
                (
                    entry.from_asset_id.clone(),
                    entry.to_asset_id.clone(),
                    entry.reason.clone(),
                    true,
                )
            })
            .collect();
        if let PageData::Probes(model) = &self.report {
            entries.extend(
                model
                    .candidates
                    .iter()
                    .filter(|candidate| {
                        !self.probe_queue.is_pinned(
                            candidate.from_asset_id.as_str(),
                            candidate.to_asset_id.as_str(),
                        )
                    })
                    .take(6)
                    .map(|candidate| {
                        (
                            candidate.from_asset_id.as_str().to_owned(),
                            candidate.to_asset_id.as_str().to_owned(),
                            report_text::probe_reason(language, candidate.reason).to_owned(),
                            false,
                        )
                    }),
            );
        }

        let mut body = panel().flex_1().min_h(px(0.)).flex().flex_col().child(
            div()
                .h(px(H_INPUT))
                .flex_none()
                .h_flex()
                .items_center()
                .px_3()
                .bg(c(RAIL))
                .border_b_1()
                .border_color(c(HAIRLINE))
                .child(crate::ui::micro_title(text.panel_probe_queue))
                .child(div().flex_1())
                .child(
                    mono(entries.len().to_string())
                        .text_size(fs(FS_10_5))
                        .text_color(c(TEXT_DISABLED)),
                ),
        );

        if entries.is_empty() {
            return body.child(crate::ui::empty_state(text.nothing_yet));
        }

        for (index, (from, to, reason, pinned)) in entries.into_iter().enumerate() {
            let action = div()
                .id(("probe-toggle", index))
                .h(px(H_BADGE_TABLE))
                .flex_none()
                .h_flex()
                .items_center()
                .px(px(6.))
                .rounded(px(RADIUS_BUTTON))
                .border_1()
                .cursor_pointer()
                .text_size(fs(FS_10_5));
            let action = if pinned {
                action
                    .border_color(c(ACCENT_LINE))
                    .bg(c(ACCENT_WASH))
                    .text_color(c(ACCENT_TEXT))
                    .child(SharedString::from(text.pinned_label.to_string()))
            } else {
                action
                    .border_color(c(HAIRLINE))
                    .text_color(c(TEXT_SECONDARY))
                    .hover(|style| style.bg(c(HOVER)))
                    .child(SharedString::from(text.pin_label.to_string()))
            };
            let (click_from, click_to, click_reason) = (from.clone(), to.clone(), reason.clone());
            let action = action.on_click(cx.listener(move |this, _, _, cx| {
                if this.probe_queue.is_pinned(&click_from, &click_to) {
                    this.unpin_probe(&click_from, &click_to);
                } else {
                    this.pin_probe(&click_from, &click_to, &click_reason, false);
                }
                cx.notify();
            }));

            body = body.child(
                div()
                    .h(px(H_TABLE_ROW))
                    .flex_none()
                    .h_flex()
                    .items_center()
                    .gap(px(SP_8))
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
                            .child(SharedString::from(self.pair_label(&from, &to))),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(fs(FS_10_5))
                            .text_color(c(TEXT_META))
                            .child(SharedString::from(reason)),
                    )
                    .child(action),
            );
        }

        body.child(div().flex_1()).child(
            div()
                .h(px(H_INPUT))
                .flex_none()
                .h_flex()
                .items_center()
                .gap(px(SP_8))
                .px_3()
                .border_t_1()
                .border_color(c(HAIRLINE_SOFT))
                .child(div().w(px(3.)).h(px(10.)).flex_none().bg(c(ACCENT)))
                .child(
                    div()
                        .text_size(fs(FS_10_5))
                        .text_color(c(TEXT_DISABLED))
                        .child(text.monitor_queue_legend),
                ),
        )
    }

    /// 跳过原因:计数条形 + 人话标签;待机帧标「正常」。
    fn skips_panel(&self) -> gpui::Div {
        let text = self.text();
        let total: u64 = self.skips.values().sum();

        let mut ranked: Vec<(&String, &u64)> = self.skips.iter().collect();
        ranked.sort_by(|left, right| right.1.cmp(left.1).then_with(|| left.0.cmp(right.0)));
        let max = ranked.first().map_or(1, |(_, count)| **count).max(1);

        let body = panel().flex_none().flex().flex_col().child(
            div()
                .h(px(H_INPUT))
                .flex_none()
                .h_flex()
                .items_center()
                .px_3()
                .bg(c(RAIL))
                .border_b_1()
                .border_color(c(HAIRLINE))
                .child(crate::ui::micro_title(text.panel_skips))
                .child(div().flex_1())
                .child(
                    mono(report_text::fill(
                        text.monitor_skip_frames,
                        &[&total.to_string()],
                    ))
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_DISABLED)),
                ),
        );

        if ranked.is_empty() {
            return body.child(
                div()
                    .p_3()
                    .child(mono(text.nothing_yet).text_size(fs(FS_12))),
            );
        }

        let mut list = div().flex().flex_col().px_3().py(px(6.));
        for (key, count) in ranked.into_iter().take(3) {
            let standby = key.as_str() == "not-book-view";
            #[allow(clippy::cast_precision_loss)]
            let fill_width = (120.0 * (*count as f32 / max as f32)).max(2.0);
            list = list.child(
                div()
                    .h(px(H_INPUT))
                    .flex_none()
                    .h_flex()
                    .items_center()
                    .gap(px(SP_8))
                    .child(
                        mono(count.to_string())
                            .w(px(44.))
                            .flex_none()
                            .text_right()
                            .text_size(fs(FS_12))
                            .text_color(c(if standby { TEXT_META } else { TEXT_PRIMARY })),
                    )
                    .child(
                        div()
                            .w(px(120.))
                            .h(px(4.))
                            .flex_none()
                            .bg(c(HAIRLINE))
                            .child(div().w(px(fill_width)).h(px(4.)).bg(c(if standby {
                                NEUTRAL_DOT
                            } else {
                                WARN
                            }))),
                    )
                    .child(
                        div()
                            .text_size(fs(FS_11_5))
                            .text_color(c(if standby { TEXT_META } else { TEXT_PRIMARY }))
                            .child(SharedString::from(super::super::skip_label(
                                key,
                                self.language(),
                            ))),
                    )
                    .child(div().flex_1())
                    .children(standby.then(|| {
                        div()
                            .text_size(fs(FS_10))
                            .text_color(c(TEXT_DISABLED))
                            .child(text.skip_standby_tag)
                    })),
            );
        }
        body.child(list)
    }
}

/// `+50.00%` / `-26.00%`:收益列带符号,正负都一眼可辨。
fn signed_percent(basis_points: i64) -> String {
    let text = report_text::percent_from_basis_points(basis_points);
    if basis_points >= 0 && !text.starts_with('+') {
        format!("+{text}")
    } else {
        text
    }
}

#[cfg(test)]
mod monitor_tests {
    use super::*;

    /// 队首价差就是"可用队首比竞争队首贵多少":1:9.33 对 1:9.10 是 2.5%。
    #[test]
    fn front_spread_reads_the_two_front_rates() {
        let spread = front_spread_percent(Some("1:9.33"), Some("1:9.10")).expect("both parse");
        assert!((spread - 2.527).abs() < 0.01, "got {spread}");
    }

    /// 聚合行带比较符,解析要把它当数字读;一侧缺失就不给数。
    #[test]
    fn spread_survives_comparators_and_missing_sides() {
        assert!(front_spread_percent(Some("<1:9.60"), Some(">1:8.50")).is_some());
        assert_eq!(front_spread_percent(None, Some("1:9")), None);
        assert_eq!(front_spread_percent(Some("nonsense"), Some("1:9")), None);
    }
}
