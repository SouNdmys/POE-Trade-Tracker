use crate::{NativeWindowHandle, PlatformError, PointI, RectI, SizeI};

const WS_EX_TOPMOST: u32 = 0x0000_0008;
const WS_EX_LAYERED: u32 = 0x0008_0000;
const WS_EX_TRANSPARENT: u32 = 0x0000_0020;
const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
const SWP_NOACTIVATE: u32 = 0x0010;
const SWP_SHOWWINDOW: u32 = 0x0040;

/// Whether a status HUD is passive or temporarily accepts drag interaction.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum HudInteractionMode {
    #[default]
    Passive,
    Placement,
}

/// How opaque the card is painted, 0 (invisible) to 255 (solid).
///
/// Not fully opaque: the card sits over a game someone is reading, and the
/// point of a card that floats is to be glanced at, not to punch a hole in
/// what is underneath. Not faint either — these are prices being read at a
/// glance, and the panel behind is busy.
pub const HUD_ALPHA: u8 = 216;

/// Whether Windows capture APIs should include the HUD.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CaptureAffinity {
    #[default]
    Include,
    Exclude,
}

impl CaptureAffinity {
    #[must_use]
    pub const fn raw_value(self) -> u32 {
        match self {
            Self::Include => 0x0000_0000, // WDA_NONE
            Self::Exclude => 0x0000_0011, // WDA_EXCLUDEFROMCAPTURE
        }
    }
}

/// Composable native policy for the always-on-top status HUD.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HudWindowPolicy {
    pub interaction: HudInteractionMode,
    pub capture_affinity: CaptureAffinity,
}

impl Default for HudWindowPolicy {
    fn default() -> Self {
        Self {
            interaction: HudInteractionMode::Passive,
            capture_affinity: CaptureAffinity::Include,
        }
    }
}

impl HudWindowPolicy {
    /// Applies only the HUD-owned extended-style bits, preserving all others.
    #[must_use]
    pub const fn compose_extended_style(self, existing: u32) -> u32 {
        // Layered unconditionally: the alpha is set separately, but the style
        // bit has to be present from creation. Toggling it later on a visible
        // window makes it flicker.
        let common = existing | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED;
        match self.interaction {
            HudInteractionMode::Passive => common | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE,
            HudInteractionMode::Placement => common & !WS_EX_TRANSPARENT & !WS_EX_NOACTIVATE,
        }
    }

    /// `SetWindowPos` flags used when showing/reasserting topmost placement.
    #[must_use]
    pub const fn show_position_flags(self) -> u32 {
        match self.interaction {
            HudInteractionMode::Passive => SWP_NOACTIVATE | SWP_SHOWWINDOW,
            HudInteractionMode::Placement => SWP_SHOWWINDOW,
        }
    }

    #[must_use]
    pub const fn is_click_through(self) -> bool {
        matches!(self.interaction, HudInteractionMode::Passive)
    }
}

/// Capture-region-aware persisted HUD placement.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum HudPlacement {
    #[default]
    Automatic,
    Manual {
        relative_x: f64,
        relative_y: f64,
    },
}

impl HudPlacement {
    pub fn manual(relative_x: f64, relative_y: f64) -> Option<Self> {
        (relative_x.is_finite()
            && relative_y.is_finite()
            && (0.0..=1.0).contains(&relative_x)
            && (0.0..=1.0).contains(&relative_y))
        .then_some(Self::Manual {
            relative_x,
            relative_y,
        })
    }
}

/// Resolves physical HUD coordinates using the stable ROI-avoidance order.
#[must_use]
pub fn resolve_hud_position(
    work_area: RectI,
    hud_size: SizeI,
    placement: HudPlacement,
    anchor_region: Option<RectI>,
) -> PointI {
    let available_width = (work_area.width - hud_size.width).max(0);
    let available_height = (work_area.height - hud_size.height).max(0);
    if let HudPlacement::Manual {
        relative_x,
        relative_y,
    } = placement
    {
        return PointI::new(
            work_area.x + (f64::from(available_width) * relative_x).round() as i32,
            work_area.y + (f64::from(available_height) * relative_y).round() as i32,
        );
    }

    const GAP: i32 = 12;
    const EDGE_MARGIN: i32 = 14;
    let min_x = work_area.x.saturating_add(EDGE_MARGIN);
    let min_y = work_area.y.saturating_add(EDGE_MARGIN);
    let max_x = min_x.max(
        work_area
            .right()
            .saturating_sub(hud_size.width)
            .saturating_sub(EDGE_MARGIN),
    );
    let max_y = min_y.max(
        work_area
            .bottom()
            .saturating_sub(hud_size.height)
            .saturating_sub(EDGE_MARGIN),
    );

    if let Some(anchor) = anchor_region {
        let candidates = [
            PointI::new(
                anchor.right().saturating_add(GAP),
                anchor.y.clamp(min_y, max_y),
            ),
            PointI::new(
                anchor.x.saturating_sub(hud_size.width).saturating_sub(GAP),
                anchor.y.clamp(min_y, max_y),
            ),
            PointI::new(
                anchor.x.clamp(min_x, max_x),
                anchor.y.saturating_sub(hud_size.height).saturating_sub(GAP),
            ),
            PointI::new(
                anchor.x.clamp(min_x, max_x),
                anchor.bottom().saturating_add(GAP),
            ),
        ];
        for candidate in candidates {
            if let Some(rectangle) =
                RectI::new(candidate.x, candidate.y, hud_size.width, hud_size.height)
                && contains_rect(work_area, rectangle)
                && !rectangle.intersects(anchor)
            {
                return candidate;
            }
        }
    }

    let corners = [
        PointI::new(max_x, min_y),
        PointI::new(max_x, max_y),
        PointI::new(min_x, min_y),
        PointI::new(min_x, max_y),
    ];
    let Some(anchor) = anchor_region else {
        return corners[0];
    };
    corners
        .into_iter()
        .find(|corner| {
            RectI::new(corner.x, corner.y, hud_size.width, hud_size.height)
                .is_some_and(|rectangle| !rectangle.intersects(anchor))
        })
        .unwrap_or(corners[0])
}

const fn contains_rect(outer: RectI, inner: RectI) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.right() <= outer.right()
        && inner.bottom() <= outer.bottom()
}

/// One order row on the card: 序号 | 比率 | 库存(面板原词)。
#[derive(Clone, Debug, Default)]
pub struct HudQuoteRow {
    pub index: String,
    pub rate: String,
    pub stock: String,
    /// 聚合行(「这一档及更差」):上方一条 hairline,整行降灰,文本原样
    /// 保留 `<1:75` / `>1:60`。
    pub aggregate: bool,
}

/// 结论行的三档:绿「全部读到」/ 黄「跳过」/ 红「不可用」。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum HudTone {
    #[default]
    Ok,
    Warn,
    Err,
}

/// 摆放模式顶条(22px,只此模式可见)的文案。
///
/// 布局(矩形几何)由绘制侧写死,这里只带字:命中测试跑在 wndproc 里,
/// 它和画笔必须对同一套矩形,所以矩形不走内容通道。
#[derive(Clone, Debug, Default)]
pub struct HudPlacementBar {
    /// 左侧提示:「拖动摆放」。
    pub hint: String,
    /// 不透明度当前值,如 `85%`。
    pub opacity_text: String,
    /// 档位切换钮的字:显示**要切去**的那一档(现在是展开就写「迷你」)。
    pub tier_label: String,
    /// 「完成」。
    pub done_label: String,
}

/// 摆放顶条上的按钮点击,由服务线程轮询取走(和 `take_user_move` 同型:
/// wndproc 无法直接调用外壳,只能把事件放进槽里等人来拿)。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HudCommand {
    /// 「完成」:写回设置并回到点击穿透。
    PlacementDone,
    /// 「迷你 / 展开」档位切换。
    ToggleTier,
    OpacityDown,
    OpacityUp,
}

/// HUD card content(§4 浮窗定稿:两档,左右两栏,结论一句人话,待抓底条)。
///
/// Typed 字段而不是一列行:左右两栏把两侧队首价放到相邻位置,价差一眼
/// 就出来——一列文本做不到这件事。
#[derive(Clone, Debug, Default)]
pub struct HudContent {
    /// 左侧 2px 竖条:金 = 在跑,红 = 停了。全卡唯一会变色的大色块。
    pub monitoring: bool,
    /// 迷你档(260×88)只画状态/通货对/结论/待抓四行。
    pub mini: bool,
    /// 头行:● 监视中。
    pub status_text: String,
    /// 头行中段:混沌石 → 削切之兆。
    pub pair_text: String,
    /// 头行右侧:#41。
    pub sequence_text: String,
    /// 结论行。
    pub tone: HudTone,
    pub verdict_text: String,
    /// 结论行右侧:106ms · 已接受 41 · 跳过 566。
    pub verdict_meta: String,
    /// 跳过/故障时整体降一档灰:数字不抹掉(可能正需要它),但不允许它
    /// 装成刚读到的。
    pub dimmed: bool,
    /// 迷你档降灰时右侧的「8s 前」。
    pub dimmed_note: String,
    /// 栏头:可用 / 竞争(游戏面板原词)。
    pub column_titles: (String, String),
    /// 列名:比率 / 库存。
    pub header_titles: (String, String),
    pub available: Vec<HudQuoteRow>,
    pub competing: Vec<HudQuoteRow>,
    /// 待抓底条:`待抓  混沌石 → 高階混沌石  缺正向报价`。空则整条不画,
    /// 窗口应当矮 20px(由外壳算尺寸)。
    pub probe_text: String,
    /// 折叠计数,如 `+2`;空则不画。
    pub probe_more: String,
    /// 摆放模式顶条;Some 时卡顶多 22px、外框变金,窗口应当高 22px
    /// (由外壳算尺寸)。
    pub placement: Option<HudPlacementBar>,
}

/// Native HUD construction parameters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HudWindowConfig {
    pub bounds: RectI,
    pub policy: HudWindowPolicy,
    pub visible: bool,
}

/// Native topmost HUD shell with safe style and display-affinity transitions.
#[derive(Debug)]
pub struct HudWindow {
    #[cfg(windows)]
    native: crate::win32::NativeHudWindow,
    #[cfg(not(windows))]
    native: crate::non_windows::NativeHudWindow,
    policy: HudWindowPolicy,
}

impl HudWindow {
    pub fn create(config: HudWindowConfig) -> Result<Self, PlatformError> {
        let native = platform_create_hud(config)?;
        Ok(Self {
            native,
            policy: config.policy,
        })
    }

    #[must_use]
    pub const fn policy(&self) -> HudWindowPolicy {
        self.policy
    }

    #[must_use]
    pub fn window_handle(&self) -> NativeWindowHandle {
        self.native.window_handle()
    }

    pub fn set_interaction_mode(
        &mut self,
        interaction: HudInteractionMode,
    ) -> Result<(), PlatformError> {
        let mut policy = self.policy;
        policy.interaction = interaction;
        self.native.apply_policy(policy)?;
        self.policy = policy;
        Ok(())
    }

    pub fn set_capture_affinity(&mut self, affinity: CaptureAffinity) -> Result<(), PlatformError> {
        self.native.set_capture_affinity(affinity)?;
        self.policy.capture_affinity = affinity;
        Ok(())
    }

    pub fn set_bounds(&mut self, bounds: RectI) -> Result<(), PlatformError> {
        self.native.set_bounds(bounds, self.policy)
    }

    pub fn set_content(&mut self, content: HudContent) -> Result<(), PlatformError> {
        self.native.set_content(content)
    }

    pub fn show(&mut self) -> Result<(), PlatformError> {
        self.native.show(self.policy)
    }

    pub fn hide(&mut self) -> Result<(), PlatformError> {
        self.native.hide()
    }

    /// 取走最近一次用户拖动结束后的窗口左上角(屏幕坐标)。
    pub fn take_user_move(&mut self) -> Option<PointI> {
        self.native.take_user_move().map(|(x, y)| PointI::new(x, y))
    }

    /// 取走摆放顶条上最近一次按钮点击(无点击为 None)。
    pub fn take_user_command(&mut self) -> Option<HudCommand> {
        self.native.take_user_command()
    }

    /// 窗口当前所在显示器的工作区。摆放坐标存相对比例,换算基准就是它。
    #[must_use]
    pub fn work_area(&self) -> Option<RectI> {
        self.native.work_area()
    }

    /// 整窗不透明度,0–255。改完立即生效,不需重建窗口(`LWA_ALPHA`)。
    pub fn set_opacity(&mut self, alpha: u8) -> Result<(), PlatformError> {
        self.native.set_opacity(alpha)
    }
}

fn platform_create_hud(config: HudWindowConfig) -> Result<PlatformHudWindow, PlatformError> {
    #[cfg(windows)]
    {
        crate::win32::NativeHudWindow::create(config)
    }
    #[cfg(not(windows))]
    {
        crate::non_windows::NativeHudWindow::create(config)
    }
}

#[cfg(windows)]
type PlatformHudWindow = crate::win32::NativeHudWindow;
#[cfg(not(windows))]
type PlatformHudWindow = crate::non_windows::NativeHudWindow;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passive_policy_is_topmost_no_activate_and_click_through() {
        let policy = HudWindowPolicy::default();
        let style = policy.compose_extended_style(0x0008_0000);
        assert_eq!(style & WS_EX_TOPMOST, WS_EX_TOPMOST);
        assert_eq!(style & WS_EX_TOOLWINDOW, WS_EX_TOOLWINDOW);
        assert_eq!(style & WS_EX_NOACTIVATE, WS_EX_NOACTIVATE);
        assert_eq!(style & WS_EX_TRANSPARENT, WS_EX_TRANSPARENT);
        assert_eq!(style & 0x0008_0000, 0x0008_0000);
        assert_eq!(
            policy.show_position_flags(),
            SWP_NOACTIVATE | SWP_SHOWWINDOW
        );
    }

    #[test]
    fn placement_mode_removes_only_passive_bits() {
        let policy = HudWindowPolicy {
            interaction: HudInteractionMode::Placement,
            ..HudWindowPolicy::default()
        };
        let style = policy.compose_extended_style(WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | 0x400);
        assert_eq!(style & WS_EX_TRANSPARENT, 0);
        assert_eq!(style & WS_EX_NOACTIVATE, 0);
        assert_eq!(style & WS_EX_TOPMOST, WS_EX_TOPMOST);
        assert_eq!(style & WS_EX_TOOLWINDOW, WS_EX_TOOLWINDOW);
        assert_eq!(style & 0x400, 0x400);
    }

    #[test]
    fn automatic_position_prefers_right_of_capture_region() {
        let work = RectI::new(0, 0, 1920, 1040).unwrap();
        let anchor = RectI::new(500, 200, 400, 500).unwrap();
        let size = SizeI::new(336, 150).unwrap();
        assert_eq!(
            resolve_hud_position(work, size, HudPlacement::Automatic, Some(anchor)),
            PointI::new(912, 200)
        );
    }

    #[test]
    fn manual_position_uses_normalized_available_work_area() {
        let work = RectI::new(-1920, 0, 1920, 1040).unwrap();
        let size = SizeI::new(320, 140).unwrap();
        let placement = HudPlacement::manual(0.25, 1.0).unwrap();
        assert_eq!(
            resolve_hud_position(work, size, placement, None),
            PointI::new(-1520, 900)
        );
        assert_eq!(HudPlacement::manual(f64::NAN, 0.0), None);
    }
}
