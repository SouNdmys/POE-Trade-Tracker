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

    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    use windows::core::{PCWSTR, PWSTR};

    /// Asks the user for an image, returning `None` if they cancelled.
    ///
    /// Blocks on the calling thread, which must therefore not be the UI
    /// thread: the dialog runs its own modal loop and would freeze rendering
    /// for as long as it is open.
    #[must_use]
    pub fn pick_image() -> Option<PathBuf> {
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
