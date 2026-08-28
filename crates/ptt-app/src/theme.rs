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

use std::sync::atomic::{AtomicU8, Ordering};

use gpui::{App, Hsla, Pixels, Rgba, px};

use gpui_component::theme::{Theme, ThemeColor, ThemeMode};

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

/// 一个颜色**槽位**,不是一个颜色。
///
/// 名字挂在角色上("面板底"、"次级文字"),值挂在调色板上,深色和浅色各有一份。
/// 页面代码写的是槽位,所以整套界面换肤时页面一行都不用改。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    Canvas,
    Panel,
    Rail,
    RailDeep,
    Zebra,
    Well,
    Hover,
    Selected,
    SelectedWash,
    Pressed,
    HairlineSoft,
    Hairline,
    HairlineStrong,
    Titlebar,
    TitlebarText,
    TitlebarBorder,
    TextPrimary,
    TextSecondary,
    TextMeta,
    TextDisabled,
    TextGhost,
    TextData,
    Accent,
    AccentText,
    AccentLine,
    AccentWash,
    AccentFill,
    TrendFlatFill,
    AccentHover,
    AccentPressed,
    OnAccent,
    AccentChipText,
    Fresh,
    Warn,
    WarnText,
    WarnLine,
    WarnWash,
    Danger,
    DangerText,
    DangerLine,
    DangerWash,
    NeutralDot,
    DisabledDot,
}

// ---------------------------------------------------------------------------
// Surfaces
// ---------------------------------------------------------------------------

/// canvas · 窗口底
pub const CANVAS: Token = Token::Canvas;
/// panel · 内容面
pub const PANEL: Token = Token::Panel;
/// rail · 栏 / 表头
pub const RAIL: Token = Token::Rail;
/// 比 rail 再深一档的栏底(设置页分段栏、部分页内工具条)
pub const RAIL_DEEP: Token = Token::RailDeep;
/// 斑马行(表格偶数行)
pub const ZEBRA: Token = Token::Zebra;
/// well · 输入位(唯一近黑)
pub const WELL: Token = Token::Well;
/// hover(只改底色;三态取色是待定项,这是临时值:panel 与选中底之间)
pub const HOVER: Token = Token::Hover;
/// selected · 选中行的**实底**(配 2px 左边框强调色)。
pub const SELECTED: Token = Token::Selected;
/// selected 的另一半:喂给 `list_active` / `table_active` 的**淡染色**。
///
/// 和 `SELECTED` 分家是因为两个岗位在浅色底下互相排斥:能读黑字的实底
/// 一压到 20% 透明就什么也看不见,而经得起 20% 透明的浓色当实底又会把
/// 文字埋掉。深色底下一个值能同时干两件事,所以深色两个 token 取同一个
/// 十六进制值,渲染结果与拆分前逐字节相同。
pub const SELECTED_WASH: Token = Token::SelectedWash;
/// 选中底的不透明度上限。上游把选中高亮画在行**上面**(绝对定位块),不是画在
/// 行背后,实底会把整行文字盖掉;上游主题装载器为此自己夹到 0.2,而我们直接写
/// colors 结构体绕过了那道夹取,所以夹取在这里补。选中感交给 *_active_border。
pub const SELECTED_WASH_ALPHA: f32 = 0.2;
/// pressed(临时值:比选中底再亮一档;深色主题里按下向亮走)
pub const PRESSED: Token = Token::Pressed;

/// hairline-soft · 行分隔
pub const HAIRLINE_SOFT: Token = Token::HairlineSoft;
/// hairline · 区块 / 输入
pub const HAIRLINE: Token = Token::Hairline;
/// hairline-strong · 窗口 / 表外框 / 顶条下缘
pub const HAIRLINE_STRONG: Token = Token::HairlineStrong;

/// 系统标题栏(原型 1a:比 canvas 更中性的近黑,配纯黑下缘)
pub const TITLEBAR: Token = Token::Titlebar;
pub const TITLEBAR_TEXT: Token = Token::TitlebarText;
pub const TITLEBAR_BORDER: Token = Token::TitlebarBorder;

// ---------------------------------------------------------------------------
// Text(文字四级 + 数据色)
// ---------------------------------------------------------------------------

/// 主文字 · 值 · 活动项
pub const TEXT_PRIMARY: Token = Token::TextPrimary;
/// 次级 · 未选中项 · 正文
pub const TEXT_SECONDARY: Token = Token::TextSecondary;
/// 元数据 · label · 计数
pub const TEXT_META: Token = Token::TextMeta;
/// 禁用 · 占位(有意最低可读档)
pub const TEXT_DISABLED: Token = Token::TextDisabled;
/// 幽灵灰 · 路径箭头 / 占位横杠 / 空档柱(比禁用还低一档,允许读不清——
/// 它标记的是"这里没有信息",读清了反而抢戏)
pub const TEXT_GHOST: Token = Token::TextGhost;
/// 等宽数值
pub const TEXT_DATA: Token = Token::TextData;

// ---------------------------------------------------------------------------
// 主题强调(暖金)——只作文字与细线,不作大面积填充
// ---------------------------------------------------------------------------

/// 金 · 状态点 / 2px 左边框 / Primary 底
pub const ACCENT: Token = Token::Accent;
/// 金文字(数值高亮、当前页签、导航激活项)
pub const ACCENT_TEXT: Token = Token::AccentText;
/// 金描边(选中徽章、摆放模式外框)
pub const ACCENT_LINE: Token = Token::AccentLine;
/// 金底(命中行 wash)
pub const ACCENT_WASH: Token = Token::AccentWash;
/// 趋势曲线的金填充(市场分析迷你曲线、历史页)
pub const ACCENT_FILL: Token = Token::AccentFill;
/// 趋势持平时的曲线填充——三种填充里最安静的那个。
///
/// 不能借用 `RAIL_DEEP`:深色里它比面板还暗一点点,几乎看不见,正是
/// 「一半的行都持平,全上色就没重点了」要的效果;可浅色里同一个值是
/// 白底上的一块中灰,比「涨」和「跌」都响,整列的轻重就颠倒了。持平
/// 要的是「比另外两个都轻」这个相对关系,而相对关系翻不过来,只能
/// 每套皮肤各配一个值。
pub const TREND_FLAT_FILL: Token = Token::TrendFlatFill;
/// Primary hover / pressed(临时值:hover 提亮、pressed 压暗)
pub const ACCENT_HOVER: Token = Token::AccentHover;
pub const ACCENT_PRESSED: Token = Token::AccentPressed;
/// 金底上的深色文字(Primary 按钮字色)
pub const ON_ACCENT: Token = Token::OnAccent;
/// Primary 上的热键 chip 文字
pub const ACCENT_CHIP_TEXT: Token = Token::AccentChipText;

// ---------------------------------------------------------------------------
// 语义三色(红黄绿被数据新鲜度占用,主题不得挪用;永远配汉字)
// ---------------------------------------------------------------------------

/// 绿 · 新鲜(只作 6px 圆点等色块,文字保持灰阶)
pub const FRESH: Token = Token::Fresh;

/// 琥珀 · 偏旧 / 需要注意(色块:圆点、2px 左条)
pub const WARN: Token = Token::Warn;
/// 琥珀徽章文字
pub const WARN_TEXT: Token = Token::WarnText;
/// 琥珀徽章描边(徽章=透明底+描边+色字)
pub const WARN_LINE: Token = Token::WarnLine;
/// 琥珀 wash(注意条底;临时值,原型徽章全部透明底,wash 只剩注意条在用)
pub const WARN_WASH: Token = Token::WarnWash;

/// 砖红 · 过期 / 错误 / 命中(色块)
pub const DANGER: Token = Token::Danger;
/// 砖红文字(负收益是「色字=主题」的唯一批准例外)
pub const DANGER_TEXT: Token = Token::DangerText;
/// 砖红描边(临时值,按琥珀描边的明度比例配;原型里红没有徽章形态)
pub const DANGER_LINE: Token = Token::DangerLine;
/// 砖红 wash · 兼趋势曲线的红填充
pub const DANGER_WASH: Token = Token::DangerWash;

/// 中性状态点(ready / idle)
pub const NEUTRAL_DOT: Token = Token::NeutralDot;
/// disabled 圆点
pub const DISABLED_DOT: Token = Token::DisabledDot;

// ---------------------------------------------------------------------------
// Palettes
// ---------------------------------------------------------------------------

/// 一整套颜色值,每个槽位一格。
pub struct Palette {
    pub canvas: u32,
    pub panel: u32,
    pub rail: u32,
    pub rail_deep: u32,
    pub zebra: u32,
    pub well: u32,
    pub hover: u32,
    pub selected: u32,
    pub selected_wash: u32,
    pub pressed: u32,
    pub hairline_soft: u32,
    pub hairline: u32,
    pub hairline_strong: u32,
    pub titlebar: u32,
    pub titlebar_text: u32,
    pub titlebar_border: u32,
    pub text_primary: u32,
    pub text_secondary: u32,
    pub text_meta: u32,
    pub text_disabled: u32,
    pub text_ghost: u32,
    pub text_data: u32,
    pub accent: u32,
    pub accent_text: u32,
    pub accent_line: u32,
    pub accent_wash: u32,
    pub accent_fill: u32,
    pub trend_flat_fill: u32,
    pub accent_hover: u32,
    pub accent_pressed: u32,
    pub on_accent: u32,
    pub accent_chip_text: u32,
    pub fresh: u32,
    pub warn: u32,
    pub warn_text: u32,
    pub warn_line: u32,
    pub warn_wash: u32,
    pub danger: u32,
    pub danger_text: u32,
    pub danger_line: u32,
    pub danger_wash: u32,
    pub neutral_dot: u32,
    pub disabled_dot: u32,
}

impl Palette {
    /// 槽位 → 十六进制。
    ///
    /// 写成穷尽 `match` 而不是数组下标,是为了让"加了槽位却忘了配值"变成
    /// 编译错误。数组的话少填一格只会静默用错颜色,那种 bug 得靠眼睛发现。
    pub fn hex(&self, token: Token) -> u32 {
        match token {
            Token::Canvas => self.canvas,
            Token::Panel => self.panel,
            Token::Rail => self.rail,
            Token::RailDeep => self.rail_deep,
            Token::Zebra => self.zebra,
            Token::Well => self.well,
            Token::Hover => self.hover,
            Token::Selected => self.selected,
            Token::SelectedWash => self.selected_wash,
            Token::Pressed => self.pressed,
            Token::HairlineSoft => self.hairline_soft,
            Token::Hairline => self.hairline,
            Token::HairlineStrong => self.hairline_strong,
            Token::Titlebar => self.titlebar,
            Token::TitlebarText => self.titlebar_text,
            Token::TitlebarBorder => self.titlebar_border,
            Token::TextPrimary => self.text_primary,
            Token::TextSecondary => self.text_secondary,
            Token::TextMeta => self.text_meta,
            Token::TextDisabled => self.text_disabled,
            Token::TextGhost => self.text_ghost,
            Token::TextData => self.text_data,
            Token::Accent => self.accent,
            Token::AccentText => self.accent_text,
            Token::AccentLine => self.accent_line,
            Token::AccentWash => self.accent_wash,
            Token::AccentFill => self.accent_fill,
            Token::TrendFlatFill => self.trend_flat_fill,
            Token::AccentHover => self.accent_hover,
            Token::AccentPressed => self.accent_pressed,
            Token::OnAccent => self.on_accent,
            Token::AccentChipText => self.accent_chip_text,
            Token::Fresh => self.fresh,
            Token::Warn => self.warn,
            Token::WarnText => self.warn_text,
            Token::WarnLine => self.warn_line,
            Token::WarnWash => self.warn_wash,
            Token::Danger => self.danger,
            Token::DangerText => self.danger_text,
            Token::DangerLine => self.danger_line,
            Token::DangerWash => self.danger_wash,
            Token::NeutralDot => self.neutral_dot,
            Token::DisabledDot => self.disabled_dot,
        }
    }
}

/// 深色「石板灰蓝 + 暖金」,方案 A 定稿值。
pub static DARK: Palette = Palette {
    canvas: 0x12151B,
    panel: 0x171B23,
    rail: 0x1C212B,
    rail_deep: 0x141922,
    zebra: 0x1A1F28,
    well: 0x0E1116,
    hover: 0x1E242E,
    selected: 0x232B3A,
    // 深色下实底与淡染是同一个值:拆分不改深色一个像素。
    selected_wash: 0x232B3A,
    pressed: 0x283040,
    hairline_soft: 0x222834,
    hairline: 0x2B323F,
    hairline_strong: 0x39424F,
    titlebar: 0x1B1B1D,
    titlebar_text: 0xB8BCC4,
    titlebar_border: 0x000000,
    text_primary: 0xE6E9EF,
    text_secondary: 0xA9B1BE,
    text_meta: 0x78828F,
    text_disabled: 0x59616E,
    text_ghost: 0x3F4650,
    text_data: 0xD9E0EA,
    accent: 0xD9B978,
    accent_text: 0xE7C88C,
    accent_line: 0x6B5A34,
    accent_wash: 0x211D13,
    accent_fill: 0x241F14,
    trend_flat_fill: 0x141922,
    accent_hover: 0xE3C98F,
    accent_pressed: 0xC7A863,
    on_accent: 0x12151B,
    accent_chip_text: 0x211D13,
    fresh: 0x45A96B,
    warn: 0xE08A3C,
    warn_text: 0xE5A24E,
    warn_line: 0x6B4A20,
    warn_wash: 0x221A0E,
    danger: 0xD0564B,
    danger_text: 0xE0705F,
    danger_line: 0x6B2A24,
    danger_wash: 0x241614,
    neutral_dot: 0x59616E,
    disabled_dot: 0x2B323F,
};

/// 浅色对位。同一套语义、同一套色相家族,明暗关系整个翻过来。
///
/// 每个值都过了两道尺:正文对面板的对比度,以及金与琥珀的色相距离——
/// 记在 docs/UI-DESIGN.md,守在本文件底部的测试里。
// 浅色的强调色不是金,是石板墨蓝——用户在四个候选里拍板的(对比页:现状
// 青铜 / 墨蓝 / 黛青 / 石墨)。黄色系在白底上试了两轮:中段(#AC821F)是
// 芥末,压到深青铜(#7E5D0F)实机看还是"屎色"——黄这个色相在浅色底上就
// 没有让人舒服的档位。墨蓝和整套浅色底色同族(石板灰蓝),是"纸上蓝墨";
// 附带的红利是金/琥珀相撞的老问题在浅色下彻底消失。深色主题的金不动:
// 深底上的金是发光感,从来没被抱怨过。
//
// 实机又调淡一档(#2E5A8F → #3D6CA5,用户嫌字看不清):白字余量从 7.1 降到
// 5.4,还够。为了让填充能淡,hover/按下改成**往深走**——浅色主题按下去
// 变实是符合直觉的方向,也让三个状态的最低对比就是静止态本身。
pub static LIGHT: Palette = Palette {
    canvas: 0xE4E9F0,
    panel: 0xFFFFFF,
    rail: 0xEFF3F8,
    rail_deep: 0xDCE2EA,
    zebra: 0xF5F8FB,
    well: 0xEDF2F8,
    hover: 0xE7EDF5,
    selected: 0xD8E6F8,
    // 浅色下两个岗位必须分家:能读黑字的浅蓝实底压到 20% 就消失了,
    // 所以淡染另配一个深得多的蓝。
    selected_wash: 0x5A8BC8,
    pressed: 0xDCE4EE,
    hairline_soft: 0xE7EBF1,
    hairline: 0xD5DCE6,
    hairline_strong: 0xB6C0CD,
    titlebar: 0xF3F3F5,
    titlebar_text: 0x3A3D42,
    titlebar_border: 0xD6D8DC,
    text_primary: 0x171C25,
    text_secondary: 0x49515F,
    text_meta: 0x5A6472,
    text_disabled: 0x8B95A4,
    text_ghost: 0xC6CDD7,
    text_data: 0x232A34,
    accent: 0x3D6CA5,
    accent_text: 0x2E5A8F,
    accent_line: 0xAFC6DE,
    accent_wash: 0xE9F1FA,
    accent_fill: 0xDFEAF7,
    trend_flat_fill: 0xF4F6FA,
    accent_hover: 0x366094,
    accent_pressed: 0x2C5180,
    on_accent: 0xFFFFFF,
    accent_chip_text: 0xE7F0FA,
    fresh: 0x2E8B52,
    warn: 0xC06515,
    warn_text: 0x8F5410,
    warn_line: 0xD9A45E,
    warn_wash: 0xFDF3E3,
    danger: 0xB93A2E,
    danger_text: 0xA32E22,
    danger_line: 0xE2A79F,
    danger_wash: 0xFCEDEA,
    neutral_dot: 0x8A94A3,
    disabled_dot: 0xCFD6DF,
};

/// 深色还是浅色。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PaletteMode {
    #[default]
    Dark,
    Light,
}

/// 当前生效的调色板,存成一个原子字节。
///
/// 不放进 GPUI 的全局状态,是因为 `c()` 被夹在成百上千条 builder 链中间调用,
/// 那些位置手上没有 `&App` 可拿——为了换肤给每个组件函数加一个参数,代价是
/// 五百多个调用点全要改写。一个字节在两个 `&'static` 之间二选一,没有第二个
/// 变量需要跟它保持先后关系,所以 `Relaxed` 就够。
static ACTIVE_MODE: AtomicU8 = AtomicU8::new(PaletteMode::Dark as u8);

/// 换肤。下一帧起 `c()` / `hsla_of()` 全部改读新调色板。
pub fn set_palette(mode: PaletteMode) {
    ACTIVE_MODE.store(mode as u8, Ordering::Relaxed);
}

/// 现在是哪一套。
pub fn palette_mode() -> PaletteMode {
    if ACTIVE_MODE.load(Ordering::Relaxed) == PaletteMode::Light as u8 {
        PaletteMode::Light
    } else {
        PaletteMode::Dark
    }
}

/// 存盘里的那个选项 → 本模块认识的调色板。
///
/// 翻译放在这里而不是让调用方各写各的 `match`:启动时读盘的地方和设置页
/// 点击的地方各有一份的话,某天加第三套皮肤就会漏掉其中一份,表现是"设置
/// 里选了、重启又变回去"。`ptt-settings` 只在 Windows 上是依赖,所以门控。
#[cfg(windows)]
pub fn palette_mode_for(theme: ptt_settings::UiTheme) -> PaletteMode {
    match theme {
        ptt_settings::UiTheme::Dark => PaletteMode::Dark,
        ptt_settings::UiTheme::Light => PaletteMode::Light,
    }
}

/// 现在这套的色值表。
pub fn active_palette() -> &'static Palette {
    match palette_mode() {
        PaletteMode::Dark => &DARK,
        PaletteMode::Light => &LIGHT,
    }
}

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

/// 槽位 → Hsla(gpui-component 主题需要 Hsla)。
pub fn hsla_of(token: Token) -> Hsla {
    gpui::rgb(active_palette().hex(token)).into()
}

/// Convenience:槽位 → gpui color for direct styling.
pub fn c(token: Token) -> Rgba {
    gpui::rgb(active_palette().hex(token))
}

/// 取深色那一套的值,不管用户选的是哪套皮肤。
///
/// 只给一种东西用:画在**游戏画面**上的标注(校准页框在截图上的那几个框、
/// 放大镜十字线)。它们的背景不是我们的面板,是 POE 的近黑客户端——而那个
/// 背景不会因为用户切了浅色就变亮。浅色版的金是压暗过的(白底上要读得清),
/// 拿到近黑背景上亮度只剩深色版的一半,框线反而更难看见。同一条理由让浮窗
/// 整个留在深色(见 docs/UI-DESIGN.md §11.5)。
pub fn c_over_game(token: Token) -> Rgba {
    gpui::rgb(DARK.hex(token))
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

/// Apply the 方案 A token set on top of gpui-component's stock theme so every
/// stock component (Input, Dropdown, Checkbox, Switch, scrollbars, ...)
/// renders in our colors. Call once at startup, before any window opens.
pub fn apply_app_theme(cx: &mut App) {
    let theme = Theme::global_mut(cx);

    // 跟着当前调色板走,不能钉死 Dark:上游的幽灵按钮拿 `mode` 决定 hover
    // 是提亮还是压暗(gpui-component `button.rs`),浅色底下写 Dark 会让
    // 按钮悬停时**变亮**——在一张接近白的底上,那等于按钮消失。
    theme.mode = match palette_mode() {
        PaletteMode::Dark => ThemeMode::Dark,
        PaletteMode::Light => ThemeMode::Light,
    };
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
    // 不走调色板:遮罩的职责是把身后的页面压下去,浅色主题里的模态遮罩
    // 一样是黑的——跟着变浅就遮不住东西了。
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
    colors.list_active = hsla_of(SELECTED_WASH).alpha(SELECTED_WASH_ALPHA);
    colors.list_active_border = hsla_of(ACCENT);
    colors.list_even = hsla_of(ZEBRA);
    colors.list_head = hsla_of(RAIL);
    colors.list_hover = hsla_of(HOVER);

    // Table(斑马行 = ZEBRA)
    colors.table = hsla_of(PANEL);
    colors.table_active = hsla_of(SELECTED_WASH).alpha(SELECTED_WASH_ALPHA);
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

    // 上游 `ThemeColor` 还有 17 个字段这里没写:`chart_1..chart_5`,以及
    // red/green/blue/yellow/magenta/cyan 各自的常规与 `_light` 版本。
    // 它们由 `gpui_component::init` 按**操作系统**的亮暗设置播种,而且
    // 之后没人再碰——也就是说它们的值取决于用户 Windows 的主题,与这里
    // 选的皮肤无关。
    //
    // 今天不影响任何一个像素:app 真正渲染的上游组件(Button、Dropdown、
    // Input、Select、PopupMenu、Table、CandlestickChart、滚动条)一个都
    // 不读它们;读它们的是 Badge、ColorPicker、Avatar、语法高亮和
    // 面积/柱/折线/饼图,这些都没用。
    //
    // 三个有语义对应的还是钉死:哪天真用上了 Badge,一个跟着系统主题走的
    // "红"会和新鲜度的砖红对不上,而那种错只有换台机器才看得见。
    colors.red = hsla_of(DANGER);
    colors.green = hsla_of(FRESH);
    colors.yellow = hsla_of(WARN);
}

#[cfg(test)]
mod theme_tests {
    use std::sync::{Mutex, PoisonError};

    use super::*;

    /// 每条不变量都跑两遍。只测"当前生效的那一套"等于把另一套放生,而放生的
    /// 那套照样会发到用户手上——浅色调色板刚加进来时就是这么无人看守的。
    const PALETTES: [(PaletteMode, &Palette); 2] =
        [(PaletteMode::Dark, &DARK), (PaletteMode::Light, &LIGHT)];

    /// 加了槽位记得加到这里,否则新槽位不进"两套必须不一样"的体检。
    /// (少配一个**值**是编译错误,那道保险在 `Palette::hex` 的穷尽 match 上。)
    const ALL_TOKENS: [Token; 43] = [
        Token::Canvas,
        Token::Panel,
        Token::Rail,
        Token::RailDeep,
        Token::Zebra,
        Token::Well,
        Token::Hover,
        Token::Selected,
        Token::SelectedWash,
        Token::Pressed,
        Token::HairlineSoft,
        Token::Hairline,
        Token::HairlineStrong,
        Token::Titlebar,
        Token::TitlebarText,
        Token::TitlebarBorder,
        Token::TextPrimary,
        Token::TextSecondary,
        Token::TextMeta,
        Token::TextDisabled,
        Token::TextGhost,
        Token::TextData,
        Token::Accent,
        Token::AccentText,
        Token::AccentLine,
        Token::AccentWash,
        Token::AccentFill,
        Token::TrendFlatFill,
        Token::AccentHover,
        Token::AccentPressed,
        Token::OnAccent,
        Token::AccentChipText,
        Token::Fresh,
        Token::Warn,
        Token::WarnText,
        Token::WarnLine,
        Token::WarnWash,
        Token::Danger,
        Token::DangerText,
        Token::DangerLine,
        Token::DangerWash,
        Token::NeutralDot,
        Token::DisabledDot,
    ];

    /// 切到某套皮肤,装出一份 `ThemeColor`,再把全局拨回原位。
    ///
    /// 生效的调色板是进程级的一个字节,而测试是并行跑的:不上锁不还原,
    /// 这个测试就会把别的测试的皮肤换掉。
    fn colors_under(mode: PaletteMode) -> ThemeColor {
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(PoisonError::into_inner);

        let previous = palette_mode();
        set_palette(mode);
        let mut colors = ThemeColor::default();
        apply_app_colors(&mut colors);
        set_palette(previous);
        colors
    }

    fn hsla_from(hex: u32) -> Hsla {
        gpui::rgb(hex).into()
    }

    /// WCAG 2.1 相对亮度:先把每个通道从"显示器上的样子"还原成线性光,
    /// 再按人眼对红绿蓝的敏感度加权(绿占七成)。
    fn relative_luminance(hex: u32) -> f32 {
        let channel = |shift: u32| {
            let raw = f32::from(((hex >> shift) & 0xFF) as u8) / 255.0;
            if raw <= 0.03928 {
                raw / 12.92
            } else {
                ((raw + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
    }

    /// 前景/背景对比度,WCAG 的那条 (L+0.05) 之比。7:1 是正文的 AAA 档,
    /// 4.5:1 是 AA 档。
    fn contrast_ratio(a: u32, b: u32) -> f32 {
        let (mut hi, mut lo) = (relative_luminance(a), relative_luminance(b));
        if hi < lo {
            std::mem::swap(&mut hi, &mut lo);
        }
        (hi + 0.05) / (lo + 0.05)
    }

    /// The selected-row highlight upstream draws is a positioned element on
    /// top of the row, not a background behind it (gpui-component
    /// `table/state.rs`), so an opaque fill hides every cell it covers.
    /// Upstream's own theme loader clamps these two to <= 0.2 alpha
    /// (`theme/schema.rs`); writing the color structs directly skips that
    /// clamp, so the invariant has to be held here instead.
    /// 0.2 在这里必须**写死**,不能引用 `SELECTED_WASH_ALPHA`:那样的话把
    /// 常量调到 0.5,断言也跟着松到 0.5,测试只会证明"常量等于它自己"。
    #[test]
    fn active_row_highlights_stay_translucent_in_both_palettes() {
        for (mode, _) in PALETTES {
            let colors = colors_under(mode);

            assert!(
                colors.table_active.a <= 0.2,
                "{mode:?}: table_active alpha {} would paint over the selected row's text",
                colors.table_active.a
            );
            assert!(
                colors.list_active.a <= 0.2,
                "{mode:?}: list_active alpha {} would paint over the selected item's text",
                colors.list_active.a
            );
        }
    }

    /// 硬约束 #5:红黄绿三个色相归数据新鲜度,主题强调色不得占用。
    /// 金、琥珀、砖红同属暖区(设计文档已知风险),压住的手段是色相拉开。
    /// 这里锁死三对两两之间的距离,防止后续调色时把任意一个往另一个推。
    /// 读的是两个 `Palette` 自己的值,不是当前生效的那套——否则另一套永远
    /// 测不到。
    ///
    /// 浅色的 accent 已经整个离开暖区(墨蓝),它对琥珀/砖红的距离是
    /// 平凡地大;这条测试在浅色下真正看守的是琥珀/砖红那一对,在深色下
    /// 看守全部三对。
    ///
    /// 只量色相,是因为(深色)量不了别的:深色里金还比三个语义色都亮
    /// (相对亮度 0.51 对 0.34/0.20/0.14),明暗本身就是第二道线索;浅色里
    /// 四个颜色都得够暗才能在白底上读出来,于是全被压进同一条窄亮度带,
    /// 金/琥珀的亮度比从深色的 1.48 掉到 1.21。试过挪琥珀和压暗金,
    /// 都是解开一对、撞上另一对(见 docs/UI-DESIGN.md)。所以浅色下这道
    /// 防线只剩色相 + 语义色永远带汉字这两条。
    #[test]
    fn the_warm_hues_stay_apart_in_both_palettes() {
        for (mode, palette) in PALETTES {
            for (left, right, pair) in [
                (palette.accent, palette.warn, "gold/amber"),
                (palette.warn, palette.danger, "amber/brick"),
                (palette.accent, palette.danger, "gold/brick"),
            ] {
                let a: Hsla = hsla_from(left);
                let b: Hsla = hsla_from(right);
                let distance = (a.h - b.h).abs() * 360.0;
                assert!(
                    distance >= 10.0,
                    "{mode:?}: {pair} hues are only {distance:.1} degrees apart; in light mode hue is the only cue left, so this is the whole defense"
                );
            }
        }
    }

    /// 正文必须读得清,而"读得清"不是靠眼睛在自己的显示器上看一眼定的。
    /// 调色板以后再被人动的时候,这条挡住的是"改完看着还行、实际已经糊了"。
    #[test]
    fn body_text_stays_readable_on_panels_in_both_palettes() {
        for (mode, palette) in PALETTES {
            let primary = contrast_ratio(palette.text_primary, palette.panel);
            assert!(
                primary >= 7.0,
                "{mode:?}: primary text on panel is only {primary:.1}:1, below the 7:1 the \
                 值/活动项 tier is supposed to hold"
            );
            let secondary = contrast_ratio(palette.text_secondary, palette.panel);
            assert!(
                secondary >= 4.5,
                "{mode:?}: secondary text on panel is only {secondary:.1}:1, below 4.5:1"
            );
            // TEXT_META 是全 app 出现最多的文字色(标签、计数、微标题),而它
            // 落在三种底上,不只是 panel:微标题就画在 RAIL_DEEP 的分节条上。
            // 只量 panel 会漏掉最暗的那一种组合——浅色版第一版正是在
            // RAIL_DEEP 上掉到 3.9,而深色版同样位置有 4.5。
            //
            // 门槛取 4.0 而不是 AA 的 4.5:深色版在 panel 上本来就是 4.4,
            // 这条守的是"别比现有的更糊",不是给一个从来没达过 AA 的层级
            // 补票。
            for (surface, name) in [
                (palette.panel, "panel"),
                (palette.rail, "rail"),
                (palette.rail_deep, "rail_deep"),
            ] {
                let meta = contrast_ratio(palette.text_meta, surface);
                assert!(
                    meta >= 4.0,
                    "{mode:?}: meta text on {name} is only {meta:.1}:1, below the 4.0 floor the 元数据 tier already holds in the shipped dark palette"
                );
            }
        }
    }

    /// Primary 按钮的字在它的金底上、**三个状态里**都要读得清。
    ///
    /// 只量静止态抓不住浅色第一版的病:芥末填充(#AC821F)配深字静止时有
    /// 5.2:1,丑但及格——真正跌破的是 hover 4.3 和按下 3.5,恰恰是手指
    /// 正按着、最需要确认按对了的那两个瞬间。现在浅色是墨蓝 + 白字,
    /// hover/按下往深走,三态 5.4 / 6.5 / 8.1——最低点就是静止态。这条
    /// 挡的是下次调色把任何一个状态挪进低对比区——那里没有任何字色能救。
    #[test]
    fn the_primary_button_stays_legible_in_all_three_states_in_both_palettes() {
        for (mode, palette) in PALETTES {
            for (fill, state) in [
                (palette.accent, "resting"),
                (palette.accent_hover, "hover"),
                (palette.accent_pressed, "pressed"),
            ] {
                let ratio = contrast_ratio(palette.on_accent, fill);
                assert!(
                    ratio >= 4.5,
                    "{mode:?}: ON_ACCENT on the {state} fill is only {ratio:.1}:1 - the label goes murky exactly while the user is pressing it"
                );
            }
        }
    }

    /// 存盘的选项和调色板之间只隔一个 `match`,而把两条臂写反的表现是
    /// 「选了浅色,界面纹丝不动」——不崩、不报错、日志里也没有一行,
    /// 唯一的线索是眼睛。一条断言就能把它挡在编译之后、发版之前。
    #[cfg(windows)]
    #[test]
    fn the_saved_setting_maps_to_the_palette_of_the_same_name() {
        assert_eq!(
            palette_mode_for(ptt_settings::UiTheme::Dark),
            PaletteMode::Dark
        );
        assert_eq!(
            palette_mode_for(ptt_settings::UiTheme::Light),
            PaletteMode::Light
        );
    }

    /// 一次复制粘贴就能让浅色调色板留着深色的值,而那种错误在深色模式下
    /// 跑测试、看界面都不会露头——只有切到浅色才发现整片是黑的。
    #[test]
    fn the_light_palette_is_not_a_copy_of_the_dark_one() {
        for token in ALL_TOKENS {
            assert_ne!(
                DARK.hex(token),
                LIGHT.hex(token),
                "{token:?} holds the same value in both palettes; \
                 the light entry looks like an un-edited copy"
            );
        }
    }
}
