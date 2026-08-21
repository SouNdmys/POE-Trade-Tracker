//! POE Trade Tracker GPUI front end (Ledger design system).
//!
//! The app is a library with a thin binary on top so that the interface can
//! be driven by more than one entry point: the product window, and the
//! component gallery that renders the same widgets against synthetic data.
//! Two binaries each declaring their own copy of the module tree would
//! compile the shared kit twice and report every widget one of them happens
//! not to use as dead code.

pub mod backend;
pub mod calibrate;
pub mod i18n;
pub mod shell;
pub mod theme;
pub mod ui;

use gpui::{
    App, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px, size,
};
use gpui_component::Root;

use shell::AppShell;

pub const WORKBENCH_SIZE: (f32, f32) = (1180.0, 640.0);

/// Opens the product window and runs until it closes.
pub fn run() {
    Application::new().run(move |cx: &mut App| {
        gpui_component::init(cx);
        theme::apply_ledger_theme(cx);

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
