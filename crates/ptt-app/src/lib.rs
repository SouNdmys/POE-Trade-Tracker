//! POE Trade Tracker GPUI front end (Ledger design system).
//!
//! The app is a library with a thin binary on top so that the interface can
//! be driven by more than one entry point: the product window, and the
//! component gallery that renders the same widgets against synthetic data.
//! Two binaries each declaring their own copy of the module tree would
//! compile the shared kit twice and report every widget one of them happens
//! not to use as dead code.

pub mod assets;
pub mod backend;
pub mod calibrate;
pub mod crashlog;
pub mod i18n;
pub mod names;
pub mod shell;
pub mod state;
pub mod theme;
pub mod ui;
// 自更新整条路都是 Windows 才有的东西:安装目录布局、被进程占住的 DLL、
// `%LOCALAPPDATA%`。依赖也全挂在 `cfg(windows)` 上,模块跟着一起门控。
#[cfg(windows)]
pub mod update;

use gpui::{
    App, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px, size,
};
use gpui_component::Root;

use shell::AppShell;

pub const WORKBENCH_SIZE: (f32, f32) = (1180.0, 640.0);

/// 开窗之前把用户存的配色装上。
///
/// 设置本来是 `AppShell::new` 读的,而那是开窗回调里的事——主题在那之前就
/// 已经装好了,于是选了浅色的人每次启动都会先看见一帧深色再跳过去。多读一次
/// 磁盘换掉那一帧;读的路径和外壳里那份一样(`analytics_probe` 也是这么绕过
/// 外壳读设置的)。读不到就保持默认深色,启动不因为设置文件坏了而失败。
#[cfg(windows)]
fn restore_saved_palette() {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let store = ptt_settings::SettingsStore::release_default_from(std::path::Path::new(&local));
    theme::set_palette(theme::palette_mode_for(store.load().settings.ui_theme));
}

/// 非 Windows 上没有设置存储可读,保持默认调色板。
#[cfg(not(windows))]
fn restore_saved_palette() {}

/// Opens the product window and runs until it closes.
pub fn run() {
    // release 是 panic = "abort" + 无控制台:先装 hook,否则任何 panic 都是
    // 窗口无声消失,连"哪个版本、哪一行"都留不下。
    crashlog::install();
    // 不注册资源源,gpui-component 的 SVG 图标(下拉箭头、菜单勾)会静默
    // 画成空白。
    Application::new()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            // 顺序是有意的:先定哪一套颜色,再把它装进 gpui-component 的主题
            // 结构体。反过来的话装进去的是深色,窗口开出来才改。
            restore_saved_palette();
            theme::apply_app_theme(cx);

            let (width, height) = WORKBENCH_SIZE;
            let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("POE Trade Tracker".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    let view = cx.new(|cx| AppShell::new(window, cx));
                    let focus = view.read(cx).focus_handle.clone();
                    window.focus(&focus);
                    cx.new(|cx| Root::new(view, window, cx))
                },
            )
            .expect("failed to open window");
            cx.activate(true);
        });
}
