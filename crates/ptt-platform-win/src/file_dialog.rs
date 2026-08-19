//! The system's own "open file" dialog.
//!
//! Calibration needs a screenshot of the exchange panel, and the panel only
//! appears while the game holds focus — so it cannot be drawn on directly, and
//! the picture has to come from a file the user already has. Anything less than
//! the shell's dialog (a path typed into a text field, a watched folder) puts
//! the burden back on them for no gain.

#[cfg(windows)]
mod windows_impl {
    use std::path::PathBuf;

    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    use windows::core::{PCWSTR, PWSTR};

    /// Asks the user for an image, returning `None` if they cancelled.
    ///
    /// Must not run on the UI thread, and not merely because it blocks. The
    /// dialog pumps messages while it is open, which re-enters the interface
    /// framework's event handling — from inside an event handler, where the
    /// view is already mutably borrowed. The process does not hang, it dies.
    /// [`spawn_pick_image`] is the safe way in; this stays public for callers
    /// that already own a worker thread.
    #[must_use]
    pub fn pick_image() -> Option<PathBuf> {
        // The common dialog reaches into the shell, which wants an apartment.
        // Failure here is not fatal — the thread may already be initialised,
        // and the dialog still opens — so the result is deliberately ignored.
        // SAFETY: paired with no uninitialise; the thread ends after the call.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        // Wide, NUL-separated, double-NUL-terminated — the Win32 filter shape.
        let mut filter: Vec<u16> = Vec::new();
        for part in ["Screenshots (*.png;*.jpg;*.jpeg)", "*.png;*.jpg;*.jpeg"] {
            filter.extend(part.encode_utf16());
            filter.push(0);
        }
        filter.push(0);

        // The dialog writes the chosen path back into this buffer, so it has
        // to be long enough for one and owned for the call's duration.
        let mut buffer = vec![0u16; 1024];
        let mut options = OPENFILENAMEW {
            lStructSize: u32::try_from(size_of::<OPENFILENAMEW>()).ok()?,
            lpstrFilter: PCWSTR(filter.as_ptr()),
            lpstrFile: PWSTR(buffer.as_mut_ptr()),
            nMaxFile: u32::try_from(buffer.len()).ok()?,
            // NOCHANGEDIR matters: without it the dialog moves the process's
            // working directory to wherever the user browsed, and every later
            // relative path in the program resolves somewhere else.
            Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
            ..Default::default()
        };

        // SAFETY: both pointers address buffers that outlive the call, and the
        // struct's size field matches the type being passed.
        let chosen = unsafe { GetOpenFileNameW(&raw mut options) };
        if !chosen.as_bool() {
            // Cancel and failure are indistinguishable here by design: the
            // caller's response to both is the same, and CommDlgExtendedError
            // reports per-thread state this function does not own.
            return None;
        }
        let end = buffer.iter().position(|unit| *unit == 0)?;
        if end == 0 {
            return None;
        }
        Some(PathBuf::from(String::from_utf16_lossy(&buffer[..end])))
    }
}

#[cfg(windows)]
pub use windows_impl::pick_image;

/// Off Windows there is no dialog; the caller falls back to its own path.
#[cfg(not(windows))]
#[must_use]
pub fn pick_image() -> Option<std::path::PathBuf> {
    None
}

/// Runs the picker on its own thread and hands the result to `deliver`.
///
/// The callback runs on that thread, so it must do nothing but pass the path
/// along — a channel send, typically.
#[cfg(windows)]
pub fn spawn_pick_image(deliver: impl FnOnce(Option<std::path::PathBuf>) + Send + 'static) {
    std::thread::Builder::new()
        .name("ptt-file-dialog".to_owned())
        .spawn(move || deliver(pick_image()))
        .expect("spawn file dialog thread");
}

#[cfg(not(windows))]
pub fn spawn_pick_image(deliver: impl FnOnce(Option<std::path::PathBuf>) + Send + 'static) {
    deliver(None);
}
