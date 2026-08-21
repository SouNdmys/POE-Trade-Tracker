//! The product window. Everything it does lives in the library beside it, so
//! the gallery binary can drive the same widgets.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    ptt_app::run();
}
