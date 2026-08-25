//! 方案 A「石板灰蓝 + 暖金」design tokens and gpui-component theme override.
//!
//! Single source of truth for every color, font size, spacing step and
//! control height in the GPUI frontend. Components must never introduce
//! values outside this module. Values come from docs/UI-DESIGN.md §2 (方案 A,
//! 定稿) and the pixel prototype `docs/poe2-ui` (`1a`).
//!
//! 总规则「色块＝语义,色字＝主题」:实心色块(圆点/徽章描边/左边框条)只表达
//! 新鲜度三档与注意/错误;彩色文字只表达主题(金)。三档语义永远带汉字,
//! 颜色只是加速识别。

// Later phases will consume the remaining tokens/components; keep the full set.
#![allow(dead_code)]

use gpui::{App, Hsla, Pixels, Rgba, px};

use gpui_component::theme::{Theme, ThemeColor, ThemeMode};

// ---------------------------------------------------------------------------
// Surfaces
// ---------------------------------------------------------------------------

/// canvas · 窗口底
pub const CANVAS: u32 = 0x12151B;
/// panel · 内容面
pub const PANEL: u32 = 0x171B23;
/// rail · 栏 / 表头
pub const RAIL: u32 = 0x1C212B;
/// 比 rail 再深一档的栏底(设置页分段栏、部分页内工具条)
pub const RAIL_DEEP: u32 = 0x141922;
/// 斑马行(表格偶数行)
pub const ZEBRA: u32 = 0x1A1F28;
/// well · 输入位(唯一近黑)
pub const WELL: u32 = 0x0E1116;
/// hover(只改底色;三态取色是待定项,这是临时值:panel 与选中底之间)
pub const HOVER: u32 = 0x1E242E;
/// selected(配 2px 左边框强调色)
pub const SELECTED: u32 = 0x232B3A;
/// 选中底的不透明度上限。上游把选中高亮画在行**上面**(绝对定位块),不是画在
/// 行背后,实底会把整行文字盖掉;上游主题装载器为此自己夹到 0.2,而我们直接写
/// colors 结构体绕过了那道夹取,所以夹取在这里补。选中感交给 *_active_border。
pub const SELECTED_WASH_ALPHA: f32 = 0.2;
/// pressed(临时值:比选中底再亮一档;深色主题里按下向亮走)
pub const PRESSED: u32 = 0x283040;

/// hairline-soft · 行分隔
pub const HAIRLINE_SOFT: u32 = 0x222834;
/// hairline · 区块 / 输入
pub const HAIRLINE: u32 = 0x2B323F;
/// hairline-strong · 窗口 / 表外框 / 顶条下缘
pub const HAIRLINE_STRONG: u32 = 0x39424F;

/// 系统标题栏(原型 1a:比 canvas 更中性的近黑,配纯黑下缘)
pub const TITLEBAR: u32 = 0x1B1B1D;
pub const TITLEBAR_TEXT: u32 = 0xB8BCC4;
pub const TITLEBAR_BORDER: u32 = 0x000000;

// ---------------------------------------------------------------------------
// Text(文字四级 + 数据色)
// ---------------------------------------------------------------------------

/// 主文字 · 值 · 活动项
pub const TEXT_PRIMARY: u32 = 0xE6E9EF;
/// 次级 · 未选中项 · 正文
pub const TEXT_SECONDARY: u32 = 0xA9B1BE;
/// 元数据 · label · 计数
pub const TEXT_META: u32 = 0x78828F;
/// 禁用 · 占位(有意最低可读档)
pub const TEXT_DISABLED: u32 = 0x59616E;
/// 幽灵灰 · 路径箭头 / 占位横杠 / 空档柱(比禁用还低一档,允许读不清——
/// 它标记的是"这里没有信息",读清了反而抢戏)
pub const TEXT_GHOST: u32 = 0x3F4650;
/// 等宽数值
pub const TEXT_DATA: u32 = 0xD9E0EA;

// ---------------------------------------------------------------------------
// 主题强调(暖金)——只作文字与细线,不作大面积填充
// ---------------------------------------------------------------------------

/// 金 · 状态点 / 2px 左边框 / Primary 底
pub const ACCENT: u32 = 0xD9B978;
/// 金文字(数值高亮、当前页签、导航激活项)
pub const ACCENT_TEXT: u32 = 0xE7C88C;
/// 金描边(选中徽章、摆放模式外框)
pub const ACCENT_LINE: u32 = 0x6B5A34;
/// 金底(命中行 wash)
pub const ACCENT_WASH: u32 = 0x211D13;
/// 趋势曲线的金填充(市场分析迷你曲线、历史页)
pub const ACCENT_FILL: u32 = 0x241F14;
/// Primary hover / pressed(临时值:hover 提亮、pressed 压暗)
pub const ACCENT_HOVER: u32 = 0xE3C98F;
pub const ACCENT_PRESSED: u32 = 0xC7A863;
/// 金底上的深色文字(Primary 按钮字色)
pub const ON_ACCENT: u32 = 0x12151B;
/// Primary 上的热键 chip 文字
pub const ACCENT_CHIP_TEXT: u32 = 0x211D13;

// ---------------------------------------------------------------------------
// 语义三色(红黄绿被数据新鲜度占用,主题不得挪用;永远配汉字)
// ---------------------------------------------------------------------------

/// 绿 · 新鲜(只作 6px 圆点等色块,文字保持灰阶)
pub const FRESH: u32 = 0x45A96B;

/// 琥珀 · 偏旧 / 需要注意(色块:圆点、2px 左条)
pub const WARN: u32 = 0xE08A3C;
/// 琥珀徽章文字
pub const WARN_TEXT: u32 = 0xE5A24E;
/// 琥珀徽章描边(徽章=透明底+描边+色字)
pub const WARN_LINE: u32 = 0x6B4A20;
/// 琥珀 wash(注意条底;临时值,原型徽章全部透明底,wash 只剩注意条在用)
pub const WARN_WASH: u32 = 0x221A0E;

/// 砖红 · 过期 / 错误 / 命中(色块)
pub const DANGER: u32 = 0xD0564B;
/// 砖红文字(负收益是「色字=主题」的唯一批准例外)
pub const DANGER_TEXT: u32 = 0xE0705F;
/// 砖红描边(临时值,按琥珀描边的明度比例配;原型里红没有徽章形态)
pub const DANGER_LINE: u32 = 0x6B2A24;
/// 砖红 wash · 兼趋势曲线的红填充
pub const DANGER_WASH: u32 = 0x241614;

/// 中性状态点(ready / idle)
pub const NEUTRAL_DOT: u32 = 0x59616E;
/// disabled 圆点
pub const DISABLED_DOT: u32 = 0x2B323F;

// ---------------------------------------------------------------------------
// Typography
// ---------------------------------------------------------------------------

pub const FONT_UI: &str = "Microsoft YaHei UI";
/// 数据字族:Cascadia Mono 优先(Windows 自带),Consolas 兜底。
pub const FONT_MONO: &str = "Cascadia Mono";

/// 字号阶:10 · 10.5 · 11 · 11.5 · 12 · 12.5 · 13 · 15 · 20(CJK 不低于 11)
pub const FS_10: f32 = 10.0;
pub const FS_10_5: f32 = 10.5;
pub const FS_11: f32 = 11.0;
pub const FS_11_5: f32 = 11.5;
pub const FS_12: f32 = 12.0;
pub const FS_12_5: f32 = 12.5;
pub const FS_13: f32 = 13.0;
pub const FS_15: f32 = 15.0;
pub const FS_20: f32 = 20.0;
/// 微标题专用 9.5/10(窄栏)
pub const FS_9_5: f32 = 9.5;

// ---------------------------------------------------------------------------
// Spacing / sizes
// ---------------------------------------------------------------------------

/// 间距阶(基数 4):4 6 8 10 12 16 18 24
pub const SP_4: f32 = 4.0;
pub const SP_6: f32 = 6.0;
pub const SP_8: f32 = 8.0;
pub const SP_10: f32 = 10.0;
pub const SP_12: f32 = 12.0;
pub const SP_16: f32 = 16.0;
pub const SP_18: f32 = 18.0;
pub const SP_24: f32 = 24.0;

/// label 列固定宽
pub const LABEL_COL: f32 = 96.0;

/// 控件高度(UI-DESIGN.md §1.2)
pub const H_BADGE_TABLE: f32 = 18.0; // 表内徽章
pub const H_CHIP: f32 = 22.0; // 面板内徽章 · 底部状态栏
pub const H_ROW: f32 = 24.0; // 指标行 · 次级按钮下限
pub const H_INPUT: f32 = 26.0; // 输入 · 下拉 · 小按钮 · 表头行
pub const H_TABLE_ROW: f32 = 28.0; // 表格行(固定,不做行内展开)
pub const H_BUTTON: f32 = 28.0; // 次级按钮上限 · 导航条目
pub const H_TITLEBAR: f32 = 30.0; // 系统标题栏
pub const H_STATUS_TOP: f32 = 36.0; // 顶部状态条
pub const H_STATUS_BOTTOM: f32 = 22.0; // 底部状态栏
pub const H_DOCK_ROW: f32 = 32.0; // Dock 摘要行
pub const H_DOCK_PRIMARY: f32 = 40.0; // Dock 主操作
pub const H_CONFIRM: f32 = 46.0; // 命中锁定卡确认键(唯一大控件)

/// 左导航:栏宽 108,条目 28
pub const W_NAV: f32 = 108.0;
/// 明细栏宽(1280 窗口下)
pub const W_DETAIL: f32 = 300.0;

/// 圆角:面板/表格/输入 = 0,按钮/徽章 = 2
pub const RADIUS_BUTTON: f32 = 2.0;

/// 树缩进:11 / 24 / 40
pub const TREE_INDENT: [f32; 3] = [11.0, 24.0, 40.0];

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Hex → Hsla(gpui-component 主题需要 Hsla)。
pub fn hsla_of(hex: u32) -> Hsla {
    gpui::rgb(hex).into()
}

/// Convenience:hex → gpui color for direct styling.
pub fn c(hex: u32) -> Rgba {
    gpui::rgb(hex)
}

/// 全局字号缩放。
///
/// 用户实机反馈:设计稿的 12px 正文在 2560×1440 上看着吃力。字号整体放大
/// 一成、布局常量一律不动——28px 的行放 13px 的字没有问题,列宽靠截断
/// 兜底(截断优于换行是既定规矩)。调回 1.0 就是设计稿原始字号。
pub const UI_SCALE: f32 = 1.1;

/// 字号 → 像素,带全局缩放,取整到半像素保持清晰。
pub fn fs(v: f32) -> Pixels {
    px((v * UI_SCALE * 2.0).round() / 2.0)
}

// ---------------------------------------------------------------------------
// gpui-component theme override
// ---------------------------------------------------------------------------

/// Apply the 方案 A token set on top of gpui-component's dark theme so every
/// stock component (Input, Dropdown, Checkbox, Switch, scrollbars, ...)
/// renders in our colors. Call once at startup, before any window opens.
pub fn apply_app_theme(cx: &mut App) {
    let theme = Theme::global_mut(cx);

    theme.mode = ThemeMode::Dark;
    theme.font_family = FONT_UI.into();
    theme.font_size = fs(FS_12);
    theme.mono_font_family = FONT_MONO.into();
    theme.mono_font_size = fs(FS_10_5);
    // 面板/输入 0 圆角;按钮/徽章的 2px 由组件层自己画。
    theme.radius = px(0.);
    theme.radius_lg = px(0.);
    theme.shadow = false;
    theme.tile_shadow = false;
    theme.tile_radius = px(0.);

    apply_app_colors(&mut theme.colors);
}

/// Write the 方案 A palette onto a `ThemeColor`.
///
/// Split out of `apply_app_theme` because `Theme::global_mut` needs a live
/// `App`: the palette itself has invariants worth asserting (see
/// `theme_tests`), and a plain unit test can only reach them through a
/// function that takes the struct.
fn apply_app_colors(colors: &mut ThemeColor) {
    // Surfaces
    colors.background = hsla_of(CANVAS);
    colors.foreground = hsla_of(TEXT_PRIMARY);
    colors.popover = hsla_of(RAIL);
    colors.popover_foreground = hsla_of(TEXT_PRIMARY);
    colors.title_bar = hsla_of(TITLEBAR);
    colors.title_bar_border = hsla_of(TITLEBAR_BORDER);
    colors.window_border = hsla_of(HAIRLINE_STRONG);
    colors.tiles = hsla_of(CANVAS);
    colors.overlay = gpui::hsla(0., 0., 0., 0.45);

    // Muted / secondary text
    colors.muted = hsla_of(RAIL);
    colors.muted_foreground = hsla_of(TEXT_META);
    colors.secondary = hsla_of(PANEL);
    colors.secondary_foreground = hsla_of(TEXT_PRIMARY);
    colors.secondary_hover = hsla_of(HOVER);
    colors.secondary_active = hsla_of(PRESSED);

    // Borders / inputs
    colors.border = hsla_of(HAIRLINE);
    colors.input = hsla_of(HAIRLINE);
    colors.ring = hsla_of(ACCENT);
    colors.caret = hsla_of(ACCENT);
    // 文字选区:金 wash 在深底上几乎不可见,用选中行底。
    colors.selection = hsla_of(SELECTED);

    // Primary(金)——金底上必须配深字,浅字对不齐对比度。
    colors.primary = hsla_of(ACCENT);
    colors.primary_foreground = hsla_of(ON_ACCENT);
    colors.primary_hover = hsla_of(ACCENT_HOVER);
    colors.primary_active = hsla_of(ACCENT_PRESSED);
    colors.accent = hsla_of(SELECTED);
    colors.accent_foreground = hsla_of(TEXT_PRIMARY);

    // Danger(砖红)
    colors.danger = hsla_of(DANGER);
    colors.danger_foreground = hsla_of(ON_ACCENT);
    colors.danger_hover = hsla_of(DANGER_TEXT);
    colors.danger_active = hsla_of(DANGER_TEXT);

    // Warning(琥珀)
    colors.warning = hsla_of(WARN);
    colors.warning_foreground = hsla_of(ON_ACCENT);
    colors.warning_hover = hsla_of(WARN_TEXT);
    colors.warning_active = hsla_of(WARN_TEXT);

    // Success = 语义绿(新鲜)。旧版收敛到主题色;现在绿有自己的语义位。
    colors.success = hsla_of(FRESH);
    colors.success_foreground = hsla_of(ON_ACCENT);
    colors.success_hover = hsla_of(FRESH);
    colors.success_active = hsla_of(FRESH);

    // Info 收敛到金体系
    colors.info = hsla_of(ACCENT_WASH);
    colors.info_foreground = hsla_of(ACCENT_TEXT);
    colors.info_hover = hsla_of(ACCENT_LINE);
    colors.info_active = hsla_of(ACCENT_LINE);

    // List / tree
    colors.list = hsla_of(PANEL);
    colors.list_active = hsla_of(SELECTED).alpha(SELECTED_WASH_ALPHA);
    colors.list_active_border = hsla_of(ACCENT);
    colors.list_even = hsla_of(ZEBRA);
    colors.list_head = hsla_of(RAIL);
    colors.list_hover = hsla_of(HOVER);

    // Table(斑马行 = ZEBRA)
    colors.table = hsla_of(PANEL);
    colors.table_active = hsla_of(SELECTED).alpha(SELECTED_WASH_ALPHA);
    colors.table_active_border = hsla_of(ACCENT);
    colors.table_even = hsla_of(ZEBRA);
    colors.table_head = hsla_of(RAIL);
    colors.table_head_foreground = hsla_of(TEXT_META);
    colors.table_hover = hsla_of(HOVER);
    colors.table_row_border = hsla_of(HAIRLINE_SOFT);

    // Tabs(当前页签用金字——「色字=主题」)
    colors.tab = gpui::transparent_black();
    colors.tab_active = hsla_of(PANEL);
    colors.tab_active_foreground = hsla_of(ACCENT_TEXT);
    colors.tab_bar = hsla_of(RAIL);
    colors.tab_bar_segmented = hsla_of(RAIL);
    colors.tab_foreground = hsla_of(TEXT_META);

    // Sidebar(左导航:激活 = 金字 + panel 底,2px 金左条由外壳画)
    colors.sidebar = hsla_of(RAIL);
    colors.sidebar_accent = hsla_of(PANEL);
    colors.sidebar_accent_foreground = hsla_of(ACCENT_TEXT);
    colors.sidebar_border = hsla_of(HAIRLINE);
    colors.sidebar_foreground = hsla_of(TEXT_SECONDARY);
    colors.sidebar_primary = hsla_of(ACCENT);
    colors.sidebar_primary_foreground = hsla_of(ON_ACCENT);

    // Scrollbar:低存在感
    colors.scrollbar = gpui::transparent_black();
    colors.scrollbar_thumb = hsla_of(HAIRLINE);
    colors.scrollbar_thumb_hover = hsla_of(HAIRLINE_STRONG);

    // 蜡烛图(§8):金 = 变贵、砖红 = 变便宜。绿留给数据新鲜度,上游默认的
    // 绿涨红跌在这套语义里会把"涨"读成"新鲜"。
    colors.bullish = hsla_of(ACCENT);
    colors.bearish = hsla_of(DANGER);

    // Misc
    colors.link = hsla_of(ACCENT_TEXT);
    colors.link_hover = hsla_of(ACCENT);
    colors.link_active = hsla_of(ACCENT_PRESSED);
    colors.drag_border = hsla_of(ACCENT);
    colors.drop_target = hsla_of(ACCENT_WASH);
    colors.progress_bar = hsla_of(ACCENT);
    colors.skeleton = hsla_of(RAIL);
    colors.switch = hsla_of(HAIRLINE);
    colors.switch_thumb = hsla_of(TEXT_PRIMARY);
    colors.slider_bar = hsla_of(ACCENT);
    colors.slider_thumb = hsla_of(TEXT_PRIMARY);
    colors.accordion = hsla_of(PANEL);
    colors.accordion_hover = hsla_of(HOVER);
    colors.group_box = hsla_of(PANEL);
    colors.group_box_foreground = hsla_of(TEXT_PRIMARY);
    colors.description_list_label = hsla_of(RAIL);
    colors.description_list_label_foreground = hsla_of(TEXT_META);
}

#[cfg(test)]
mod theme_tests {
    use super::*;

    fn app_colors() -> ThemeColor {
        let mut colors = ThemeColor::default();
        apply_app_colors(&mut colors);
        colors
    }

    /// The selected-row highlight upstream draws is a positioned element on
    /// top of the row, not a background behind it (gpui-component
    /// `table/state.rs`), so an opaque fill hides every cell it covers.
    /// Upstream's own theme loader clamps these two to <= 0.2 alpha
    /// (`theme/schema.rs`); writing the color structs directly skips that
    /// clamp, so the invariant has to be held here instead.
    #[test]
    fn active_row_highlights_stay_translucent() {
        let colors = app_colors();

        assert!(
            colors.table_active.a <= 0.2,
            "table_active alpha {} would paint over the selected row's text",
            colors.table_active.a
        );
        assert!(
            colors.list_active.a <= 0.2,
            "list_active alpha {} would paint over the selected item's text",
            colors.list_active.a
        );
    }

    /// 硬约束 #5:红黄绿三个色相归数据新鲜度,主题强调色不得占用。
    /// 金 #D9B978 与琥珀 #E08A3C 同属暖区(设计文档已知风险),压住的手段是
    /// 色相拉开:琥珀刻意偏橙。这里锁死两者的色相距离,防止后续调色时
    /// 把金往橙推、或把琥珀往黄推,重新撞回同一个色相。
    #[test]
    fn accent_gold_keeps_distance_from_semantic_amber() {
        let gold: Hsla = hsla_of(ACCENT);
        let amber: Hsla = hsla_of(WARN);
        let distance = (gold.h - amber.h).abs() * 360.0;
        assert!(
            distance >= 10.0,
            "gold hue and amber hue are only {distance:.1} degrees apart; \
             the two-tier defense (gold=text-only, amber=block-only) needs \
             hue distance too"
        );
    }
}
