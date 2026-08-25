use std::marker::PhantomData;
use std::mem::size_of;
use std::num::NonZeroIsize;
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_WINDOW, CreateFontW,
    CreateSolidBrush, DEFAULT_CHARSET, DT_END_ELLIPSIS, DT_RIGHT, DT_SINGLELINE, DT_VCENTER,
    DeleteObject, DrawTextW, EndPaint, FW_NORMAL, FW_SEMIBOLD, FillRect, FrameRect,
    GetSysColorBrush, HFONT, HGDIOBJ, InvalidateRect, OUT_DEFAULT_PRECIS, PAINTSTRUCT,
    SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow, GWL_EXSTYLE,
    GetWindowLongPtrW, HTCAPTION, HTTRANSPARENT, HWND_TOPMOST, IsWindow, LWA_ALPHA, MA_NOACTIVATE,
    RegisterClassExW, SET_WINDOW_POS_FLAGS, SW_HIDE, SWP_FRAMECHANGED, SWP_HIDEWINDOW,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW,
    SetLayeredWindowAttributes, SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WINDOW_EX_STYLE, WM_EXITSIZEMOVE,
    WM_MOUSEACTIVATE, WM_NCHITTEST, WM_PAINT, WNDCLASSEXW, WS_POPUP,
};
use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, GetWindowRect};
use windows::core::{PCWSTR, w};

use crate::hud::{CaptureAffinity, HUD_ALPHA, HudContent, HudWindowConfig, HudWindowPolicy};
use crate::{NativeWindowHandle, PlatformError, RectI};

use super::error_from_windows;

const HUD_CLASS: PCWSTR = w!("PoeTradeTrackerPlatformStatusHud");
const PASSIVE_STYLE_BITS: isize = 0x0800_0020; // WS_EX_NOACTIVATE | WS_EX_TRANSPARENT

static HUD_CLASS_READY: OnceLock<Result<(), PlatformError>> = OnceLock::new();

/// 单实例 HUD 的当前内容;wndproc 在 WM_PAINT 里读取。
static HUD_CONTENT: Mutex<Option<HudContent>> = Mutex::new(None);

/// 用户拖动结束后的窗口左上角(屏幕坐标);服务线程轮询取走。
static HUD_USER_MOVE: Mutex<Option<(i32, i32)>> = Mutex::new(None);

const fn hud_rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

fn hud_font(height: i32, weight: i32) -> HFONT {
    // SAFETY: CreateFontW with a fixed family name; caller deletes via DeleteObject.
    unsafe {
        CreateFontW(
            -height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Microsoft YaHei UI"),
        )
    }
}

unsafe fn hud_text(
    dc: windows::Win32::Graphics::Gdi::HDC,
    font: HFONT,
    color: COLORREF,
    text: &str,
    mut rect: RECT,
    flags: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT,
) {
    if text.is_empty() {
        return;
    }
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: dc/font live for this paint; DrawTextW bounds by rect.
    unsafe {
        let previous = SelectObject(dc, HGDIOBJ(font.0));
        SetTextColor(dc, color);
        DrawTextW(dc, &mut wide, &mut rect, flags);
        SelectObject(dc, previous);
    }
}

/// 方案 A 状态卡绘制(§4 浮窗定稿)。
///
/// 全部由实心矩形 + 文字构成:`FillRect`/`FrameRect`/`DrawTextW` 直接能画,
/// 不需要圆角、阴影、渐变。
unsafe fn paint_hud(hwnd: HWND) {
    let mut paint = PAINTSTRUCT::default();
    // SAFETY: balanced Begin/EndPaint on our HWND.
    let dc = unsafe { BeginPaint(hwnd, &mut paint) };
    let mut client = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut client) };
    let content = HUD_CONTENT
        .lock()
        .map(|c| c.clone())
        .unwrap_or_default()
        .unwrap_or_default();

    // 方案 A 深色板(theme.rs 的同名 token)。
    let panel = hud_rgb(0x17, 0x1B, 0x23);
    let border = hud_rgb(0x39, 0x42, 0x4F);
    let hairline = hud_rgb(0x22, 0x28, 0x34);
    let gold = hud_rgb(0xD9, 0xB9, 0x78);
    let red = hud_rgb(0xD0, 0x56, 0x4B);
    let green = hud_rgb(0x45, 0xA9, 0x6B);
    let amber = hud_rgb(0xE0, 0x8A, 0x3C);
    let text_primary = hud_rgb(0xE6, 0xE9, 0xEF);
    let text_secondary = hud_rgb(0xA9, 0xB1, 0xBE);
    let text_meta = hud_rgb(0x78, 0x82, 0x8F);
    let text_disabled = hud_rgb(0x59, 0x61, 0x6E);
    let text_ghost = hud_rgb(0x3F, 0x46, 0x50);
    let text_data = hud_rgb(0xD9, 0xE0, 0xEA);
    let amber_text = hud_rgb(0xE5, 0xA2, 0x4E);
    let red_text = hud_rgb(0xE0, 0x70, 0x5F);

    // 左侧 2px 竖条:金 = 在跑,红 = 停了。
    let bar_color = if content.monitoring { gold } else { red };
    let (tone_dot, tone_text) = match content.tone {
        crate::hud::HudTone::Ok => (green, text_secondary),
        crate::hud::HudTone::Warn => (amber, amber_text),
        crate::hud::HudTone::Err => (red, red_text),
    };
    // 跳过时数字不抹掉,但不允许它装成刚读到的:整体降一档灰。
    let (row_rate, row_rate_front, row_stock) = if content.dimmed {
        (text_meta, text_meta, text_meta)
    } else {
        (text_data, text_primary, text_secondary)
    };

    // SAFETY: brushes/fonts created and released within this scope.
    unsafe {
        let panel_brush = CreateSolidBrush(panel);
        let border_brush = CreateSolidBrush(border);
        let bar_brush = CreateSolidBrush(bar_color);
        let hairline_brush = CreateSolidBrush(hairline);
        FillRect(dc, &client, panel_brush);
        FrameRect(dc, &client, border_brush);
        let left_bar = RECT {
            left: client.left,
            top: client.top,
            right: client.left + 2,
            bottom: client.bottom,
        };
        FillRect(dc, &left_bar, bar_brush);
        SetBkMode(dc, TRANSPARENT);

        let status_font = hud_font(15, FW_SEMIBOLD.0 as i32);
        let body_font = hud_font(14, FW_NORMAL.0 as i32);
        let small_font = hud_font(12, FW_NORMAL.0 as i32);

        let dot = |dc, x: i32, y: i32, size: i32, color| {
            let brush = CreateSolidBrush(color);
            let rect = RECT {
                left: x,
                top: y,
                right: x + size,
                bottom: y + size,
            };
            FillRect(dc, &rect, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
        };
        let hline = |dc, y: i32| {
            let rect = RECT {
                left: client.left + 2,
                top: y,
                right: client.right - 1,
                bottom: y + 1,
            };
            FillRect(dc, &rect, hairline_brush);
        };
        let single = DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS;

        if content.mini {
            // 迷你档 88 = 内边距 5+5 + 状态 20 + 通货对 20 + 结论 20 + 待抓 18。
            let left = client.left + 11;
            let right = client.right - 8;
            let mut top = client.top + 5;
            dot(dc, left, top + 7, 6, bar_color);
            hud_text(
                dc,
                status_font,
                text_primary,
                &content.status_text,
                RECT {
                    left: left + 11,
                    top,
                    right: right - 44,
                    bottom: top + 20,
                },
                single,
            );
            hud_text(
                dc,
                small_font,
                text_disabled,
                &content.sequence_text,
                RECT {
                    left: right - 44,
                    top,
                    right,
                    bottom: top + 20,
                },
                DT_SINGLELINE | DT_VCENTER | DT_RIGHT,
            );
            top += 20;
            // 通货对:降灰时右侧标「8s 前」。
            let pair_color = if content.dimmed { text_meta } else { text_data };
            hud_text(
                dc,
                body_font,
                pair_color,
                &content.pair_text,
                RECT {
                    left,
                    top,
                    right: right - 50,
                    bottom: top + 20,
                },
                single,
            );
            if content.dimmed {
                hud_text(
                    dc,
                    small_font,
                    text_disabled,
                    &content.dimmed_note,
                    RECT {
                        left: right - 50,
                        top,
                        right,
                        bottom: top + 20,
                    },
                    DT_SINGLELINE | DT_VCENTER | DT_RIGHT,
                );
            }
            top += 20;
            dot(dc, left, top + 7, 6, tone_dot);
            hud_text(
                dc,
                small_font,
                tone_text,
                &content.verdict_text,
                RECT {
                    left: left + 11,
                    top,
                    right,
                    bottom: top + 20,
                },
                single,
            );
            top += 20;
            if !content.probe_text.is_empty() {
                hud_text(
                    dc,
                    small_font,
                    text_meta,
                    &content.probe_text,
                    RECT {
                        left,
                        top,
                        right: right - 26,
                        bottom: top + 18,
                    },
                    single,
                );
                hud_text(
                    dc,
                    small_font,
                    text_ghost,
                    &content.probe_more,
                    RECT {
                        left: right - 26,
                        top,
                        right,
                        bottom: top + 18,
                    },
                    DT_SINGLELINE | DT_VCENTER | DT_RIGHT,
                );
            }
        } else {
            // 展开档:26 头行 | 137 两栏 | 24 结论 | 20 待抓,段间 hairline。
            let left = client.left + 11;
            let right = client.right - 10;
            let header_bottom = client.top + 26;
            dot(dc, left, client.top + 10, 6, bar_color);
            hud_text(
                dc,
                status_font,
                text_primary,
                &content.status_text,
                RECT {
                    left: left + 11,
                    top: client.top,
                    right: left + 75,
                    bottom: header_bottom,
                },
                single,
            );
            hud_text(
                dc,
                body_font,
                text_data,
                &content.pair_text,
                RECT {
                    left: left + 80,
                    top: client.top,
                    right: right - 40,
                    bottom: header_bottom,
                },
                single,
            );
            hud_text(
                dc,
                small_font,
                text_disabled,
                &content.sequence_text,
                RECT {
                    left: right - 40,
                    top: client.top,
                    right,
                    bottom: header_bottom,
                },
                DT_SINGLELINE | DT_VCENTER | DT_RIGHT,
            );
            hline(dc, header_bottom);

            // 两栏:上边距 6 + 栏头 16 + 6×18 行 + 下边距 4 = 137。
            let body_top = header_bottom + 1;
            let column_width = (right - left - 12) / 2;
            let columns = [
                (left, &content.column_titles.0, &content.available),
                (
                    left + column_width + 12,
                    &content.column_titles.1,
                    &content.competing,
                ),
            ];
            for (x, title, rows) in columns {
                let col_right = x + column_width;
                let head_top = body_top + 6;
                hud_text(
                    dc,
                    small_font,
                    text_secondary,
                    title,
                    RECT {
                        left: x,
                        top: head_top,
                        right: x + 60,
                        bottom: head_top + 16,
                    },
                    single,
                );
                hud_text(
                    dc,
                    small_font,
                    text_ghost,
                    &content.header_titles.0,
                    RECT {
                        left: col_right - 110,
                        top: head_top,
                        right: col_right - 56,
                        bottom: head_top + 16,
                    },
                    DT_SINGLELINE | DT_VCENTER | DT_RIGHT,
                );
                hud_text(
                    dc,
                    small_font,
                    text_ghost,
                    &content.header_titles.1,
                    RECT {
                        left: col_right - 54,
                        top: head_top,
                        right: col_right,
                        bottom: head_top + 16,
                    },
                    DT_SINGLELINE | DT_VCENTER | DT_RIGHT,
                );
                let mut row_top = head_top + 16;
                for (index, row) in rows.iter().take(6).enumerate() {
                    if row.aggregate {
                        // 第 6 行(及更差)上方一条 hairline。
                        hline(dc, row_top);
                    }
                    let idx_color = if row.aggregate {
                        text_ghost
                    } else {
                        text_disabled
                    };
                    let rate_color = if row.aggregate {
                        text_meta
                    } else if index == 0 {
                        row_rate_front
                    } else {
                        row_rate
                    };
                    let stock_color = if row.aggregate { text_meta } else { row_stock };
                    hud_text(
                        dc,
                        small_font,
                        idx_color,
                        &row.index,
                        RECT {
                            left: x,
                            top: row_top,
                            right: x + 14,
                            bottom: row_top + 18,
                        },
                        single,
                    );
                    hud_text(
                        dc,
                        body_font,
                        rate_color,
                        &row.rate,
                        RECT {
                            left: x + 18,
                            top: row_top,
                            right: col_right - 58,
                            bottom: row_top + 18,
                        },
                        single,
                    );
                    hud_text(
                        dc,
                        body_font,
                        stock_color,
                        &row.stock,
                        RECT {
                            left: col_right - 56,
                            top: row_top,
                            right: col_right,
                            bottom: row_top + 18,
                        },
                        DT_SINGLELINE | DT_VCENTER | DT_RIGHT,
                    );
                    row_top += 18;
                }
            }
            let body_bottom = body_top + 137;
            hline(dc, body_bottom);

            // 结论:● 一句人话 + 右侧计数。跳过时加 2px 琥珀左条。
            let verdict_top = body_bottom + 1;
            let verdict_bottom = verdict_top + 24;
            if content.tone == crate::hud::HudTone::Warn {
                let strip = RECT {
                    left: client.left + 2,
                    top: verdict_top,
                    right: client.left + 4,
                    bottom: verdict_bottom,
                };
                let amber_brush = CreateSolidBrush(amber);
                FillRect(dc, &strip, amber_brush);
                let _ = DeleteObject(HGDIOBJ(amber_brush.0));
            }
            dot(dc, left, verdict_top + 9, 6, tone_dot);
            hud_text(
                dc,
                body_font,
                tone_text,
                &content.verdict_text,
                RECT {
                    left: left + 11,
                    top: verdict_top,
                    right: right - 150,
                    bottom: verdict_bottom,
                },
                single,
            );
            hud_text(
                dc,
                small_font,
                text_disabled,
                &content.verdict_meta,
                RECT {
                    left: right - 150,
                    top: verdict_top,
                    right,
                    bottom: verdict_bottom,
                },
                DT_SINGLELINE | DT_VCENTER | DT_RIGHT,
            );

            // 待抓底条:固定最底,只显示最紧的 1 条;不给颜色、不给按钮。
            if !content.probe_text.is_empty() {
                hline(dc, verdict_bottom);
                let probe_top = verdict_bottom + 1;
                hud_text(
                    dc,
                    small_font,
                    text_meta,
                    &content.probe_text,
                    RECT {
                        left,
                        top: probe_top,
                        right: right - 26,
                        bottom: probe_top + 19,
                    },
                    single,
                );
                hud_text(
                    dc,
                    small_font,
                    text_ghost,
                    &content.probe_more,
                    RECT {
                        left: right - 26,
                        top: probe_top,
                        right,
                        bottom: probe_top + 19,
                    },
                    DT_SINGLELINE | DT_VCENTER | DT_RIGHT,
                );
            }
        }

        let _ = DeleteObject(HGDIOBJ(status_font.0));
        let _ = DeleteObject(HGDIOBJ(body_font.0));
        let _ = DeleteObject(HGDIOBJ(small_font.0));
        let _ = DeleteObject(HGDIOBJ(panel_brush.0));
        let _ = DeleteObject(HGDIOBJ(border_brush.0));
        let _ = DeleteObject(HGDIOBJ(bar_brush.0));
        let _ = DeleteObject(HGDIOBJ(hairline_brush.0));
        let _ = EndPaint(hwnd, &paint);
    }
}

/// Thread-bound owner of the native status HUD window.
pub(crate) struct NativeHudWindow {
    hwnd: HWND,
    _thread_bound: PhantomData<Rc<()>>,
}

impl std::fmt::Debug for NativeHudWindow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeHudWindow")
            .field("hwnd", &(self.hwnd.0 as usize))
            .finish()
    }
}

impl NativeHudWindow {
    pub(crate) fn create(config: HudWindowConfig) -> Result<Self, PlatformError> {
        ensure_class()?;
        let module = unsafe { GetModuleHandleW(None) }
            .map_err(|error| error_from_windows("GetModuleHandleW", error))?;
        let extended = WINDOW_EX_STYLE(config.policy.compose_extended_style(0));
        // SAFETY: The registered class and all pointer parameters are valid;
        // this owner destroys the returned top-level HWND on the same thread.
        let hwnd = unsafe {
            CreateWindowExW(
                extended,
                HUD_CLASS,
                w!("POE Trade Tracker status"),
                WS_POPUP,
                config.bounds.x,
                config.bounds.y,
                config.bounds.width,
                config.bounds.height,
                None,
                None,
                Some(module.into()),
                None,
            )
        }
        .map_err(|error| error_from_windows("CreateWindowExW(status HUD)", error))?;
        let mut window = Self {
            hwnd,
            _thread_bound: PhantomData,
        };
        // Uniformly translucent, set once. `LWA_ALPHA` is what a card wants:
        // per-pixel alpha would mean painting through `UpdateLayeredWindow`
        // and giving up the ordinary `WM_PAINT` path this window uses.
        // SAFETY: `hwnd` is this owner's live layered top-level window.
        if let Err(error) =
            unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), HUD_ALPHA, LWA_ALPHA) }
        {
            drop(window);
            return Err(error_from_windows("SetLayeredWindowAttributes", error));
        }
        if let Err(error) = window.set_capture_affinity(config.policy.capture_affinity) {
            drop(window);
            return Err(error);
        }
        window.set_bounds(config.bounds, config.policy)?;
        if config.visible {
            window.show(config.policy)?;
        }
        Ok(window)
    }

    pub(crate) fn window_handle(&self) -> NativeWindowHandle {
        let raw = self.hwnd.0 as isize;
        NativeWindowHandle::from_known_valid(
            NonZeroIsize::new(raw).expect("CreateWindowExW returned a non-null HWND"),
        )
    }

    pub(crate) fn apply_policy(&mut self, policy: HudWindowPolicy) -> Result<(), PlatformError> {
        // SAFETY: hwnd is live and thread-owned by self.
        let existing = unsafe { GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) } as u32;
        let composed = policy.compose_extended_style(existing);
        // SAFETY: Only extended style bits are replaced on our live HWND.
        unsafe { SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, composed as isize) };
        // SetWindowLongPtrW can return zero both on success and failure, so
        // verify the resulting value instead of interpreting its return value.
        let observed = unsafe { GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) } as u32;
        if observed != composed {
            return Err(PlatformError::Win32 {
                operation: "SetWindowLongPtrW(status HUD policy)",
                code: 0,
                message: format!(
                    "extended style verification failed (wanted 0x{composed:08X}, got 0x{observed:08X})"
                ),
            });
        }
        // SAFETY: Flags request a non-moving/non-sizing frame refresh.
        unsafe {
            SetWindowPos(
                self.hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            )
        }
        .map_err(|error| error_from_windows("SetWindowPos(apply HUD policy)", error))
    }

    pub(crate) fn set_capture_affinity(
        &mut self,
        affinity: CaptureAffinity,
    ) -> Result<(), PlatformError> {
        let native = match affinity {
            CaptureAffinity::Include => WDA_NONE,
            CaptureAffinity::Exclude => WDA_EXCLUDEFROMCAPTURE,
        };
        // SAFETY: SetWindowDisplayAffinity accepts this live top-level window.
        unsafe { SetWindowDisplayAffinity(self.hwnd, native) }
            .map_err(|error| error_from_windows("SetWindowDisplayAffinity", error))
    }

    pub(crate) fn set_bounds(
        &mut self,
        bounds: RectI,
        policy: HudWindowPolicy,
    ) -> Result<(), PlatformError> {
        let flags = SET_WINDOW_POS_FLAGS(policy.show_position_flags() & !SWP_SHOWWINDOW.0);
        // SAFETY: hwnd and HWND_TOPMOST are valid; RectI proves positive dimensions.
        unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                bounds.x,
                bounds.y,
                bounds.width,
                bounds.height,
                flags,
            )
        }
        .map_err(|error| error_from_windows("SetWindowPos(position HUD)", error))
    }

    pub(crate) fn show(&mut self, policy: HudWindowPolicy) -> Result<(), PlatformError> {
        let flags =
            SET_WINDOW_POS_FLAGS(policy.show_position_flags() | SWP_NOMOVE.0 | SWP_NOSIZE.0);
        // SAFETY: hwnd is live; this reasserts topmost without changing geometry.
        unsafe { SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags) }
            .map_err(|error| error_from_windows("SetWindowPos(show HUD)", error))
    }

    pub(crate) fn set_content(&mut self, content: HudContent) -> Result<(), PlatformError> {
        if let Ok(mut slot) = HUD_CONTENT.lock() {
            *slot = Some(content);
        }
        // SAFETY: hwnd is live; invalidation schedules WM_PAINT on its thread.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
        Ok(())
    }

    pub(crate) fn hide(&mut self) -> Result<(), PlatformError> {
        // SAFETY: hwnd is live and SW_HIDE does not activate it.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
            SetWindowPos(
                self.hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_HIDEWINDOW,
            )
        }
        .map_err(|error| error_from_windows("SetWindowPos(hide HUD)", error))
    }

    /// 取走用户拖动结束后的窗口左上角(无新拖动时为 None)。
    pub(crate) fn set_opacity(&mut self, alpha: u8) -> Result<(), PlatformError> {
        // SAFETY: `hwnd` is this owner's live layered top-level window.
        unsafe { SetLayeredWindowAttributes(self.hwnd, COLORREF(0), alpha, LWA_ALPHA) }
            .map_err(|error| error_from_windows("SetLayeredWindowAttributes", error))
    }

    pub(crate) fn take_user_move(&mut self) -> Option<(i32, i32)> {
        HUD_USER_MOVE.lock().ok().and_then(|mut slot| slot.take())
    }

    pub(super) fn raw_handle(&self) -> HWND {
        self.hwnd
    }

    pub(super) fn extended_style(&self) -> u32 {
        // SAFETY: self owns a live HWND.
        unsafe { GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) as u32 }
    }
}

impl Drop for NativeHudWindow {
    fn drop(&mut self) {
        // SAFETY: This type is !Send and destroys its owned HWND at most once.
        if unsafe { IsWindow(Some(self.hwnd)) }.as_bool() {
            let _ = unsafe { DestroyWindow(self.hwnd) };
        }
    }
}

fn ensure_class() -> Result<(), PlatformError> {
    HUD_CLASS_READY
        .get_or_init(|| {
            let module = unsafe { GetModuleHandleW(None) }
                .map_err(|error| error_from_windows("GetModuleHandleW", error))?;
            // SAFETY: System color brushes are process-stable and must not be deleted.
            let background = unsafe { GetSysColorBrush(COLOR_WINDOW) };
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(hud_window_proc),
                hInstance: module.into(),
                hbrBackground: background,
                lpszClassName: HUD_CLASS,
                ..Default::default()
            };
            // SAFETY: WNDCLASSEXW points only to static class data.
            if unsafe { RegisterClassExW(&class) } == 0 {
                return Err(error_from_windows(
                    "RegisterClassExW(status HUD)",
                    windows::core::Error::from_win32(),
                ));
            }
            Ok(())
        })
        .clone()
}

unsafe extern "system" fn hud_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCHITTEST => {
            // SAFETY: Querying styles for the window currently dispatching the message.
            let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
            if style & PASSIVE_STYLE_BITS == PASSIVE_STYLE_BITS {
                LRESULT(HTTRANSPARENT as isize)
            } else {
                // Placement 模式:整张卡都是拖动把手。
                LRESULT(HTCAPTION as isize)
            }
        }
        WM_EXITSIZEMOVE => {
            let mut rect = RECT::default();
            // SAFETY: hwnd is the live HUD window that just finished a user move.
            if unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok()
                && let Ok(mut slot) = HUD_USER_MOVE.lock()
            {
                *slot = Some((rect.left, rect.top));
            }
            // SAFETY: Forward to the system procedure after recording the move.
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_MOUSEACTIVATE => {
            // SAFETY: Querying styles for the window currently dispatching the message.
            let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
            if style & 0x0800_0000 != 0 {
                LRESULT(MA_NOACTIVATE as isize)
            } else {
                // SAFETY: Placement mode deliberately permits ordinary activation behavior.
                unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
            }
        }
        WM_PAINT => {
            // SAFETY: hwnd is the live HUD window being painted.
            unsafe {
                paint_hud(hwnd);
            }
            LRESULT(0)
        }
        _ => {
            // SAFETY: Default handling for all messages not owned by this adapter.
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
    }
}

pub(super) fn is_window(hwnd: HWND) -> bool {
    // SAFETY: IsWindow accepts stale handles for diagnostic probing.
    unsafe { IsWindow(Some(hwnd)) }.as_bool()
}

pub(super) const fn required_passive_style_bits() -> u32 {
    0x0800_00A8 // TOPMOST | TRANSPARENT | TOOLWINDOW | NOACTIVATE
}

/// Dispatches this thread's pending messages for up to `budget`.
///
/// A [`HudWindow`](crate::HudWindow) is thread-bound and repaints from
/// `WM_PAINT`, so it never draws unless the thread that created it pumps.
/// Inside the app that is GPUI's own loop; a console tool has to do it
/// itself, and this exists so such a tool does not have to reach for the
/// Win32 crate directly.
pub fn pump_thread_messages(budget: std::time::Duration) {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
    };

    let deadline = std::time::Instant::now() + budget;
    let mut message = MSG::default();
    while std::time::Instant::now() < deadline {
        // SAFETY: standard non-blocking pump over this thread's own queue.
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
}
