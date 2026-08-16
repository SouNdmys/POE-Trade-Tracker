//! POE Trade Tracker GPUI front end (Ledger design system).
//!
//! P3 skeleton: one window hosting the watch loop — status strip, last
//! accepted book, live opportunities. Release builds are GUI-subsystem.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod backend;
mod i18n;
mod shell;
mod theme;
mod ui;

use gpui::{
    App, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px, size,
};
use gpui_component::Root;

use shell::AppShell;

pub const WORKBENCH_SIZE: (f32, f32) = (1180.0, 640.0);

fn main() {
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
