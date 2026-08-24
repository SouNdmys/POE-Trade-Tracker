//! Ledger v1 base components (章节 03–08).
//!
//! Everything here draws exclusively from `theme` tokens. Hover only changes
//! background, never size or position. Panels/inputs have 0 radius, buttons
//! and chips 2px.

// Phase 3 will consume the remaining tokens/components; keep the full set now.
#![allow(dead_code)]

use gpui::{AnyElement, App, Div, IntoElement, ParentElement, SharedString, Styled, div, px};
use gpui_component::{
    StyledExt,
    button::{Button, ButtonCustomVariant, ButtonRounded, ButtonVariants},
};

use crate::theme::*;

// ---------------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------------

/// Simulate the spec's `.14em` letter-spacing (gpui has no letter-spacing)
/// by interspersing U+2009 THIN SPACE between characters.
pub fn spaced(text: &str) -> SharedString {
    let mut out = String::with_capacity(text.len() * 2);
    let mut first = true;
    for ch in text.chars() {
        if !first {
            out.push('\u{2009}');
        }
        out.push(ch);
        first = false;
    }
    out.into()
}

/// 微标题:等宽 + 字距,仅用于区块标题。
/// (按用户反馈从 10px 提到 10.5px、meta 色提到次级色,保证可读性。)
pub fn micro_title(text: &str) -> Div {
    div()
        .font_family(FONT_MONO)
        .text_size(fs(FS_10_5))
        .text_color(c(TEXT_SECONDARY))
        .child(spaced(text))
}

/// 窄栏微标题(10px,用户反馈上调)。
pub fn micro_title_sm(text: &str) -> Div {
    div()
        .font_family(FONT_MONO)
        .text_size(fs(FS_10))
        .text_color(c(TEXT_META))
        .child(spaced(text))
}

/// 等宽数据文字。
pub fn mono(text: impl Into<SharedString>) -> Div {
    div().font_family(FONT_MONO).child(text.into())
}

// ---------------------------------------------------------------------------
// Section scaffolding(面板 · 栏头 · 发丝线)
// ---------------------------------------------------------------------------

/// 内容面板:panel 底 + strong 外框(0 圆角、无阴影)。
pub fn panel() -> Div {
    div().bg(c(PANEL)).border_1().border_color(c(HAIRLINE))
}

/// 区块头:rail 底 + 微标题,高度随内容(9px 12px padding)。
pub fn panel_header(title: &str) -> Div {
    div()
        .h_flex()
        .items_center()
        .gap_3()
        .px_3()
        .py_2()
        .bg(c(RAIL))
        .border_b_1()
        .border_color(c(HAIRLINE))
        .child(micro_title(title))
}

/// 水平分隔线(soft)。
pub fn hairline_soft() -> Div {
    div().h(px(1.)).flex_none().bg(c(HAIRLINE_SOFT))
}

/// 章节小标题行:微标题 + 一条 soft 线拉满(设计稿"数值条件"样式)。
pub fn inline_section(title: &str) -> Div {
    div()
        .h_flex()
        .items_center()
        .gap_2()
        .child(micro_title_sm(title))
        .child(div().flex_1().h(px(1.)).bg(c(HAIRLINE_SOFT)))
}

// ---------------------------------------------------------------------------
// Status(章节 04 · 08)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StatusKind {
    /// ready / idle · 中性灰,无底色
    Idle,
    /// 正在监控 · 金(主题色:这是状态,不是数据新鲜度)
    Monitoring,
    /// 数据新鲜 · 语义绿。色块=语义:绿只上圆点,文字保持灰阶。
    Fresh,
    /// 已命中 · 砖红
    Hit,
    /// 需要注意 / 偏旧 · 琥珀
    Warning,
    /// 错误 / 过期 · 砖红
    Error,
    /// 只读 / 不可用
    Disabled,
}

impl StatusKind {
    pub fn dot(self) -> u32 {
        match self {
            StatusKind::Idle => NEUTRAL_DOT,
            StatusKind::Monitoring => ACCENT,
            StatusKind::Fresh => FRESH,
            StatusKind::Hit | StatusKind::Error => DANGER,
            StatusKind::Warning => WARN,
            StatusKind::Disabled => DISABLED_DOT,
        }
    }

    /// 状态行文字。三档语义永远带汉字,颜色只是加速识别——所以 Fresh 的
    /// 文字是灰的:绿点已经说了"新鲜",绿字就是把同一句话喊两遍。
    pub fn text(self) -> u32 {
        match self {
            StatusKind::Idle | StatusKind::Fresh => TEXT_SECONDARY,
            StatusKind::Monitoring => ACCENT_TEXT,
            StatusKind::Hit | StatusKind::Error => DANGER_TEXT,
            StatusKind::Warning => WARN_TEXT,
            StatusKind::Disabled => TEXT_DISABLED,
        }
    }
}

/// 状态点(7px 圆)。
pub fn status_dot(kind: StatusKind) -> Div {
    let base = div()
        .size(px(7.))
        .flex_none()
        .rounded_full()
        .bg(c(kind.dot()));
    if kind == StatusKind::Disabled {
        base.border_1().border_color(c(HAIRLINE))
    } else {
        base
    }
}

/// 状态行:点 + 文字(600 字重 13px)+ 右侧等宽计时。
pub fn status_line(kind: StatusKind, label: &str, elapsed: &str) -> Div {
    div()
        .h_flex()
        .items_center()
        .gap(px(9.))
        .child(status_dot(kind))
        .child(
            div()
                .text_size(fs(FS_13))
                .font_semibold()
                .text_color(c(kind.text()))
                .child(SharedString::from(label.to_string())),
        )
        .child(
            div()
                .ml_auto()
                .font_family(FONT_MONO)
                .text_size(fs(FS_11_5))
                .text_color(c(TEXT_SECONDARY))
                .child(SharedString::from(elapsed.to_string())),
        )
}

/// 琥珀注意条:2px 左边框 + wash 底,就地显示,不弹窗。
pub fn warning_band(tag: &str, text: &str) -> Div {
    div()
        .h_flex()
        .gap(px(9.))
        .px(px(11.))
        .py_2()
        .bg(c(WARN_WASH))
        .border_l_2()
        .border_color(c(WARN))
        .child(
            div()
                .font_family(FONT_MONO)
                .text_size(fs(FS_10))
                .text_color(c(WARN))
                .child(SharedString::from(tag.to_string())),
        )
        .child(
            div()
                .text_size(fs(FS_11_5))
                .line_height(px(FS_11_5 * 1.55))
                .text_color(c(WARN_TEXT))
                .child(SharedString::from(text.to_string())),
        )
}

/// 校验错误行:红点 + 文案,底 DANGER_WASH。占位恒定一行高。
pub fn error_band(text: &str) -> Div {
    div()
        .h_flex()
        .items_center()
        .gap_2()
        .px(px(10.))
        .py(px(6.))
        .bg(c(DANGER_WASH))
        .border_t_1()
        .border_color(c(DANGER_LINE))
        .child(div().size(px(6.)).flex_none().rounded_full().bg(c(DANGER)))
        .child(
            div()
                .text_size(fs(FS_11_5))
                .text_color(c(DANGER_TEXT))
                .child(SharedString::from(text.to_string())),
        )
}

/// 命中 wash 行(日志里的"规则命中"高亮)。
pub fn accent_wash_line(text: &str) -> Div {
    div()
        .px_2()
        .py(px(5.))
        .bg(c(ACCENT_WASH))
        .border_l_2()
        .border_color(c(ACCENT))
        .font_family(FONT_MONO)
        .text_size(fs(FS_10_5))
        .text_color(c(ACCENT_TEXT))
        .child(SharedString::from(text.to_string()))
}

// ---------------------------------------------------------------------------
// Buttons(章节 05 · 四类)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LedgerButton {
    /// 墨青实底 h28,一屏只允许一个
    Primary,
    /// panel 底 + hairline 边 h28
    Secondary,
    /// 无边框 h26,accent 文字
    Quiet,
    /// 红边红字 h26,pressed 变实底
    Destructive,
}

/// 构造 Ledger 风格按钮(基于 gpui-component Button:保留焦点环/键盘/禁用逻辑,
/// 颜色与几何全部覆盖为 token)。
/// Makes a panel body scroll instead of running off the bottom.
///
/// Every list in this app was drawn eagerly into whatever height it happened
/// to get, so a window that was not maximised simply hid the rest with no
/// indication there was more. The id is what gpui keys the scroll position by,
/// so it has to be unique per panel; `min_h(0)` is what lets a flex child be
/// shorter than its content in the first place.
pub fn scrollable(body: gpui::Div, id: &'static str) -> gpui::Stateful<gpui::Div> {
    use gpui::{InteractiveElement as _, StatefulInteractiveElement as _, Styled as _};
    body.flex_1().min_h(px(0.)).id(id).overflow_y_scroll()
}

/// A Ledger button.
///
/// The id takes anything an [`gpui::ElementId`] can be made from, tuples
/// included, because a button drawn inside a loop needs one per row: GPUI
/// keys interaction state by id, so a whole column of buttons sharing one
/// static string leaves only the first row clickable and the rest inert with
/// nothing on screen to say so.
pub fn button(id: impl Into<gpui::ElementId>, kind: LedgerButton, label: &str, cx: &App) -> Button {
    use gpui_component::ActiveTheme as _;
    let _ = cx.theme();
    let (variant, height) = match kind {
        LedgerButton::Primary => (
            ButtonCustomVariant::new(cx)
                .color(hsla_of(ACCENT))
                .foreground(hsla_of(ON_ACCENT))
                .border(hsla_of(ACCENT))
                .hover(hsla_of(ACCENT_HOVER))
                .active(hsla_of(ACCENT_PRESSED)),
            H_BUTTON,
        ),
        LedgerButton::Secondary => (
            // 原型 1a 的「停止」:panel 底 + hairline 边 + 次级灰字。
            ButtonCustomVariant::new(cx)
                .color(hsla_of(PANEL))
                .foreground(hsla_of(TEXT_SECONDARY))
                .border(hsla_of(HAIRLINE))
                .hover(hsla_of(HOVER))
                .active(hsla_of(PRESSED)),
            H_BUTTON,
        ),
        LedgerButton::Quiet => (
            ButtonCustomVariant::new(cx)
                .color(gpui::transparent_black())
                .foreground(hsla_of(ACCENT_TEXT))
                .border(gpui::transparent_black())
                .hover(hsla_of(HOVER))
                .active(hsla_of(PRESSED)),
            H_INPUT,
        ),
        LedgerButton::Destructive => (
            ButtonCustomVariant::new(cx)
                .color(hsla_of(PANEL))
                .foreground(hsla_of(DANGER))
                .border(hsla_of(DANGER_LINE))
                .hover(hsla_of(DANGER_WASH))
                .active(hsla_of(DANGER)),
            H_INPUT,
        ),
    };
    let btn = Button::new(id)
        .custom(variant)
        .rounded(ButtonRounded::Size(px(RADIUS_BUTTON)))
        .label(SharedString::from(label.to_string()))
        .h(px(height));
    // 用户反馈:Primary 底色深,12px 常规字重不够清晰 → 12.5 + 600。
    if kind == LedgerButton::Primary {
        btn.text_size(fs(FS_12_5)).font_semibold()
    } else {
        btn.text_size(fs(FS_12))
    }
}

/// 呼吸状态点:监控中 1.6s 透明度 1 → 0.45(设计允许的三处动效之一)。
pub fn breathing_dot(id: &'static str, kind: StatusKind) -> gpui::AnyElement {
    use gpui::{Animation, AnimationExt as _};
    use std::time::Duration;
    if kind == StatusKind::Monitoring {
        status_dot(kind)
            .with_animation(
                id,
                Animation::new(Duration::from_millis(1600))
                    .repeat()
                    .with_easing(gpui::pulsating_between(0.45, 1.0)),
                |dot, delta| dot.opacity(delta),
            )
            .into_any_element()
    } else {
        status_dot(kind).into_any_element()
    }
}

/// 热键 chip:等宽 10px,1px 边框,2px 圆角。
pub fn hotkey_chip(key: &str) -> Div {
    div()
        .flex_none()
        .border_1()
        .border_color(c(HAIRLINE))
        .bg(c(PANEL))
        .rounded(px(RADIUS_BUTTON))
        .px(px(6.))
        .py(px(3.))
        .font_family(FONT_MONO)
        .text_size(fs(FS_10))
        .text_color(c(TEXT_SECONDARY))
        .child(SharedString::from(key.to_string()))
}

/// 一组热键 chip。
pub fn hotkey_chips(keys: &[&str]) -> Div {
    let mut row = div().h_flex().gap_1();
    for k in keys {
        row = row.child(hotkey_chip(k));
    }
    row
}

// ---------------------------------------------------------------------------
// Segmented control(章节 06 ·「任意 / 全部 / 指定条数」)
// ---------------------------------------------------------------------------

/// 分段控件(静态渲染;交互接线在 Phase 3 由调用方包 on_click)。
pub fn segmented(items: &[&str], selected: usize, height: f32) -> Div {
    let mut row = div()
        .h_flex()
        .flex_none()
        .border_1()
        .border_color(c(HAIRLINE));
    for (i, item) in items.iter().enumerate() {
        let mut cell = div()
            .h(px(height - 2.))
            .flex_none()
            .px(px(10.))
            .flex()
            .items_center()
            .text_size(fs(FS_11_5))
            .whitespace_nowrap();
        if i > 0 {
            cell = cell.border_l_1().border_color(c(HAIRLINE));
        }
        cell = if i == selected {
            cell.bg(c(ACCENT_WASH)).text_color(c(ACCENT_TEXT))
        } else {
            cell.bg(c(PANEL)).text_color(c(TEXT_SECONDARY))
        };
        row = row.child(cell.child(SharedString::from(item.to_string())));
    }
    row
}

// ---------------------------------------------------------------------------
// Tree rows(章节 07 · 六态)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TreeState {
    Active,
    Selected,
    Hover,
    Default,
    Warning,
    Disabled,
}

pub struct TreeRowSpec<'a> {
    /// 0/1/2 → 缩进 11/24/40
    pub depth: usize,
    pub state: TreeState,
    /// Some(expanded) 渲染 ▾/▸;None 渲染 4px 方点
    pub expander: Option<bool>,
    pub label: &'a str,
    /// 右侧等宽计数
    pub trailing: &'a str,
    /// 计数颜色覆盖(默认 meta)
    pub trailing_color: Option<u32>,
}

/// 树行:行高 26,右侧计数等宽右对齐。
pub fn tree_row(spec: TreeRowSpec) -> Div {
    let indent = TREE_INDENT[spec.depth.min(2)];
    let mut row = div()
        .h(px(H_INPUT))
        .flex_none()
        .h_flex()
        .items_center()
        .gap(px(7.))
        .pr(px(11.))
        .pl(px(indent));

    row = match spec.state {
        TreeState::Active => row.bg(c(PANEL)).border_l_2().border_color(c(ACCENT)),
        TreeState::Selected => row.bg(c(SELECTED)).border_l_2().border_color(c(ACCENT)),
        TreeState::Hover => row.bg(c(HOVER)),
        _ => row,
    };

    let (label_color, weight_semibold) = match spec.state {
        TreeState::Active => (TEXT_PRIMARY, true),
        TreeState::Selected => (TEXT_PRIMARY, false),
        TreeState::Disabled => (TEXT_DISABLED, false),
        TreeState::Warning => (TEXT_SECONDARY, false),
        _ => (TEXT_SECONDARY, false),
    };

    // leading marker
    row = match spec.expander {
        Some(expanded) => row.child(
            div()
                .text_size(px(8.))
                .text_color(c(TEXT_META))
                .child(if expanded { "▾" } else { "▸" }),
        ),
        None => {
            let dot_color = match spec.state {
                TreeState::Active | TreeState::Selected => ACCENT,
                TreeState::Warning => WARN,
                TreeState::Disabled => DISABLED_DOT,
                _ => NEUTRAL_DOT,
            };
            row.child(div().size(px(4.)).flex_none().bg(c(dot_color)))
        }
    };

    let mut label = div()
        .text_size(fs(FS_12))
        .text_color(c(label_color))
        .whitespace_nowrap()
        .overflow_hidden()
        .child(SharedString::from(spec.label.to_string()));
    if weight_semibold {
        label = label.font_semibold();
    }
    row = row.child(label);

    row.child(
        div()
            .ml_auto()
            .font_family(FONT_MONO)
            .text_size(fs(FS_9_5))
            .text_color(c(spec.trailing_color.unwrap_or(TEXT_META)))
            .whitespace_nowrap()
            .child(SharedString::from(spec.trailing.to_string())),
    )
}

// ---------------------------------------------------------------------------
// Metric / summary rows(右栏 24px 指标行 · Dock 32px 摘要行)
// ---------------------------------------------------------------------------

/// 指标行:label(meta) + 右侧等宽值,h24,底部 soft 分隔。
pub fn metric_row(label: &str, value: &str, last: bool) -> Div {
    let mut row = div()
        .h(px(H_ROW))
        .flex_none()
        .h_flex()
        .items_center()
        .text_size(fs(FS_11_5));
    if !last {
        row = row.border_b_1().border_color(c(HAIRLINE_SOFT));
    }
    row.child(
        div()
            .text_color(c(TEXT_META))
            .child(SharedString::from(label.to_string())),
    )
    .child(
        div()
            .ml_auto()
            .font_family(FONT_MONO)
            .text_color(c(TEXT_DATA))
            .child(SharedString::from(value.to_string())),
    )
}

// ---------------------------------------------------------------------------
// Status bar(24px)
// ---------------------------------------------------------------------------

pub struct StatusSegment<'a> {
    pub text: &'a str,
    pub color: Option<u32>,
}

/// 状态栏:rail 底,10px 等宽,段间 soft 竖线;最后一个右对齐段自动推到最右。
pub fn status_bar(segments: &[StatusSegment], trailing: Option<&str>) -> Div {
    let mut bar = div()
        .h(px(H_STATUS_BOTTOM))
        .flex_none()
        .h_flex()
        .items_center()
        .bg(c(RAIL))
        .border_t_1()
        .border_color(c(HAIRLINE))
        .font_family(FONT_MONO)
        .text_size(fs(FS_10))
        .text_color(c(TEXT_SECONDARY));
    for (i, seg) in segments.iter().enumerate() {
        let mut cell = div()
            .px(px(11.))
            .h_full()
            .flex()
            .items_center()
            .whitespace_nowrap();
        if i < segments.len() {
            cell = cell.border_r_1().border_color(c(HAIRLINE_SOFT));
        }
        if let Some(color) = seg.color {
            cell = cell.text_color(c(color));
        }
        bar = bar.child(cell.child(SharedString::from(seg.text.to_string())));
    }
    if let Some(t) = trailing {
        bar = bar.child(
            div()
                .ml_auto()
                .px(px(11.))
                .h_full()
                .flex()
                .items_center()
                .border_l_1()
                .border_color(c(HAIRLINE_SOFT))
                .text_color(c(TEXT_META))
                .whitespace_nowrap()
                .child(SharedString::from(t.to_string())),
        );
    }
    bar
}

// ---------------------------------------------------------------------------
// Log pane(右栏日志 · 10.5px 等宽 · 行高 1.5)
// ---------------------------------------------------------------------------

pub enum LogLine {
    /// 前缀行(meta 色):`band 01 · 29 ms`
    Meta(SharedString),
    /// 内容行(次级色,左缩进 8)
    Text(SharedString),
    /// 命中行(accent 色,左缩进 8)
    Match(SharedString),
    /// 命中 wash 块
    Hit(SharedString),
}

pub fn log_pane(lines: &[LogLine]) -> Div {
    let mut pane = div()
        .v_flex()
        .gap(px(5.))
        .p(px(9.))
        .px_3()
        .bg(c(PANEL))
        .font_family(FONT_MONO)
        .text_size(fs(FS_10_5))
        .line_height(px(FS_10_5 * 1.5));
    for line in lines {
        pane = match line {
            LogLine::Meta(t) => pane.child(div().text_color(c(TEXT_META)).child(t.clone())),
            LogLine::Text(t) => {
                pane.child(div().pl_2().text_color(c(TEXT_SECONDARY)).child(t.clone()))
            }
            LogLine::Match(t) => {
                pane.child(div().pl_2().text_color(c(ACCENT_TEXT)).child(t.clone()))
            }
            LogLine::Hit(t) => pane.child(accent_wash_line(t)),
        };
    }
    pane
}

/// helper: erase Div type for heterogeneous children lists.
pub fn el(d: Div) -> AnyElement {
    d.into_any_element()
}

// ---------------------------------------------------------------------------
// Data-page kit(P8:表格页的徽章、详情面板与键值行)
// ---------------------------------------------------------------------------

/// 新鲜度 → 状态三色。
///
/// 红黄绿只在这里定义一次。四档语义(Fresh 可执行、Usable 建议核对、
/// Stale/Archived 默认禁入执行)是算法层的判断,这里只把它翻成颜色——
/// 每个页面各自决定"多旧算旧"会让同一份数据在两页显示成两种颜色。
pub fn freshness_kind(status: ptt_runtime::domain::FreshnessStatus) -> StatusKind {
    use ptt_runtime::domain::FreshnessStatus;
    match status {
        FreshnessStatus::Fresh => StatusKind::Fresh,
        FreshnessStatus::Usable => StatusKind::Warning,
        FreshnessStatus::Stale | FreshnessStatus::Archived => StatusKind::Error,
    }
}

/// 新鲜度单元格:6px 色点 + 11px 灰字(新鲜 / 偏旧 / 过期)。
///
/// 表格第一列的标准形态。文字永远是次级灰——色弱、以及缩到迷你浮窗时,
/// 汉字仍然读得出来,颜色只是加速识别。
pub fn freshness_cell(kind: StatusKind, label: &str) -> Div {
    div()
        .h_flex()
        .items_center()
        .gap(px(5.))
        .child(
            div()
                .size(px(6.))
                .flex_none()
                .rounded_full()
                .bg(c(kind.dot())),
        )
        .child(
            div()
                .text_size(fs(FS_11))
                .text_color(c(TEXT_SECONDARY))
                .whitespace_nowrap()
                .child(SharedString::from(label.to_string())),
        )
}

/// 徽章底色与描边:透明底 + 1px 语义描边 + 语义色字(原型 1a 定稿)。
///
/// 旧版给徽章铺 wash 底;新设计里徽章是「描边=色块」规则的载体,透明底
/// 让它在斑马行和 panel 上都不用配第二套底色。
fn chip_stroke(kind: StatusKind) -> u32 {
    match kind {
        StatusKind::Monitoring => ACCENT_LINE,
        StatusKind::Warning => WARN_LINE,
        StatusKind::Hit | StatusKind::Error => DANGER_LINE,
        StatusKind::Idle | StatusKind::Fresh | StatusKind::Disabled => HAIRLINE,
    }
}

fn chip_sized(kind: StatusKind, label: &str, height: f32) -> Div {
    div()
        .h(px(height))
        .flex_none()
        .h_flex()
        .items_center()
        .px(px(SP_6))
        .rounded(px(RADIUS_BUTTON))
        .border_1()
        .border_color(c(chip_stroke(kind)))
        .text_size(fs(FS_10_5))
        .text_color(c(kind.text()))
        .whitespace_nowrap()
        .child(SharedString::from(label.to_string()))
}

/// 面板内徽章:22px 高。
///
/// 用来承载 typed 的枚举名(可执行性、风险、覆盖状态),所以只收已经翻译
/// 好的字符串——徽章不认识枚举,免得多出第二处需要双语的地方。
pub fn chip(kind: StatusKind, label: &str) -> Div {
    chip_sized(kind, label, H_CHIP)
}

/// 表内徽章:18px 高,放得进 28px 固定行。
pub fn chip_table(kind: StatusKind, label: &str) -> Div {
    chip_sized(kind, label, H_BADGE_TABLE)
}

/// 一行徽章,超出数量的折成 "+N"。
///
/// 阻断性风险常常一次来好几条,全部铺开会把表格行挤走形;省略的那部分在
/// 详情面板里逐条列出,所以这里折叠不丢信息。
pub fn chips(kind: StatusKind, labels: &[String], limit: usize) -> Div {
    let mut row = div().h_flex().items_center().gap_1();
    for label in labels.iter().take(limit) {
        row = row.child(chip(kind, label));
    }
    if labels.len() > limit {
        row = row.child(chip(
            StatusKind::Idle,
            &format!("+{}", labels.len() - limit),
        ));
    }
    row
}

/// The same row of chips, but silent about what did not fit.
///
/// A bare `+3` says "there is more" and not what, so the only thing a reader
/// can do with it is open the detail panel -- which is where the full list
/// already is, and where they were going anyway. In a table column it spends
/// attention to deliver nothing: the owner's ruling on the radar's risk
/// column, and the same "silence is the all-clear" rule the Convert page's
/// leg chips were cut down by.
pub fn chips_capped(kind: StatusKind, labels: &[String], limit: usize) -> Div {
    let mut row = div().h_flex().items_center().gap_1();
    for label in labels.iter().take(limit) {
        row = row.child(chip(kind, label));
    }
    row
}

/// 详情面板:选中一行以后右侧那一栏。
///
/// 表格行高是固定的(虚拟化的代价),行内展开做不到,所以"看细节"这件事
/// 由它承担。
pub fn detail_panel(title: &str) -> Div {
    panel()
        .w(px(W_DETAIL))
        // 300 是宽度上限,不只是初始宽:没有 min_w(0),一个不换行的长值会把这一栏
        // 顶到 300 以上,溢出到窗口外面去(理由见 kv_row)。
        .min_w(px(0.))
        .flex_none()
        .flex()
        .flex_col()
        .child(panel_header(title))
}

/// 详情面板里的键值行:左标签,右等宽值,值可换行。
///
/// 与 `metric_row` 的区别是这行不定高——路径、风险原因这类值会长到两三行,
/// 定高会把它们裁掉。
///
/// 值容器上的 `min_w(0)` 是换行能不能发生的开关:gpui 量出的文本最小内容宽
/// **就是不换行的整行宽**(它把 MinContent 和 MaxContent 算成同一个数),而 flex
/// 子项默认 min-width:auto = 最小内容宽,于是 `flex_1` 永远压不下去,值永远拿不到
/// 一个比整行窄的确定宽度,也就永远不换行——只会把面板一起撑宽、被窗口边界裁掉。
pub fn kv_row(label: &str, value: &str) -> Div {
    div()
        .flex()
        .items_start()
        .gap_2()
        .py(px(3.))
        .text_size(fs(FS_11_5))
        .child(
            div()
                .w(px(96.))
                .flex_none()
                .text_color(c(TEXT_META))
                .child(SharedString::from(label.to_string())),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .font_family(FONT_MONO)
                .text_color(c(TEXT_DATA))
                .child(SharedString::from(value.to_string())),
        )
}

/// 空态:面板中央一句话,而不是一片空白。
pub fn empty_state(text: &str) -> Div {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .p_4()
        .text_size(fs(FS_12))
        .text_color(c(TEXT_DISABLED))
        .child(SharedString::from(text.to_string()))
}
