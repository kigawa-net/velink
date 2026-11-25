//! Process-wide windowing fixes that must run before the first window exists.

/// Make the process per-monitor DPI aware on Windows.
///
/// minifb never sets a DPI awareness context, so Windows treats the process as
/// DPI unaware and virtualizes window coordinates. Moving the window to a
/// monitor with a different resolution or scale factor then makes Windows snap
/// it back or render it blurry. Setting the context before the first window is
/// created keeps coordinates in real pixels across monitors.
#[cfg(windows)]
pub fn enable_dpi_awareness() {
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    // SAFETY: called once, before any window or GUI toolkit initializes.
    // Failure (for example when the context is already set) is harmless.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

#[cfg(not(windows))]
pub fn enable_dpi_awareness() {}
