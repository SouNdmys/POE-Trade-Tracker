//! The calibration page: a screenshot, and three rectangles drawn on it.
//!
//! Split out of the shell because it is the one page that draws pixels rather
//! than data — a canvas, a loupe, drag handling and coordinate conversion,
//! none of which the other pages have any use for.

use gpui::{Context, InteractiveElement as _, ParentElement, Styled, div, px};

use crate::backend::{Backend, RegionSlot, ShellMsg};
use crate::shell::AppShell;
use crate::theme::*;
use crate::ui::{LedgerButton, button, mono};

impl AppShell {
    #[cfg(windows)]
    pub(crate) fn apply_calibration(
        &mut self,
        slot: RegionSlot,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    ) {
        let region = ptt_settings::Region {
            x,
            y,
            width,
            height,
        };
        let profile = self.settings.active_profile;
        let entry = self.settings.profile_mut(profile);
        match slot {
            RegionSlot::Need => entry.need_name_region = Some(region),
            RegionSlot::Have => entry.have_name_region = Some(region),
            RegionSlot::Tables => entry.tables_region = Some(region),
        }
        // Keyed by the profile's own panel. Storing a POE1 rectangle under
        // POE2's prefix leaves the route reading its factory preset while the
        // interface shows the region as calibrated.
        ptt_recognition::route::set_region_override(
            ptt_runtime::pipeline::route_for(profile).0.key_prefix,
            slot.override_name(),
            (x, y, width, height),
        );
        match self.settings_store.save(&self.settings) {
            Ok(()) => self.push_log(format!(
                "calibrated {}: {x},{y} {width}x{height}",
                slot.label(self.text())
            )),
            Err(error) => self.push_log(format!("settings save failed: {error}")),
        }
        // A running session captured its regions at start; restart it so the
        // new geometry takes effect immediately.
        if self.watching {
            if let Some(mut backend) = self.backend.take() {
                backend.stop();
            }
            self.backend = Some(Backend::start());
        }
    }

    /// The calibration screen: a screenshot, and three rectangles drawn on it.
    #[cfg(windows)]
    pub(crate) fn render_calibrate(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        use crate::calibrate::{MAGNIFIER_ZOOM, Target, loupe_origin};

        // Fit here rather than only when the file loads. The canvas reports
        // its bounds while painting, so a load that arrives before the first
        // paint has nothing to fit against; deciding it on the frame that can
        // answer removes the ordering question instead of racing it.
        if self.calibration.view.is_none() {
            self.fit_calibration();
        }
        // Open on the regions that are actually in effect. Drawn rectangles
        // and saved regions used to be two unrelated sets of numbers with no
        // visible link, so applying looked like it did nothing at all.
        let profile = self.settings.active_profile;
        if self.calibration.seeded_for != Some(profile) {
            self.seed_calibration(profile);
        }

        let text = self.text();
        let view = self.calibration.view();
        let size = self.calibration.image_size;
        let image = self.calibration.image.clone();
        let bounds_slot = self.canvas_bounds.clone();
        let active_target = self.calibration.target();
        // Where this region normally sits, drawn as a guide. Answers the only
        // question a blank screenshot raises — which part of the panel am I
        // supposed to be framing — without asserting the answer: the guide is
        // a hint to aim at, and the drawn rectangle is still what gets used.
        let (layout, language) = ptt_runtime::pipeline::route_for(profile);
        let guide = match active_target {
            Target::Need => layout.need_name,
            Target::Have => layout.have_name,
            Target::Tables => layout.tables_for(language),
        };
        let hint = match active_target {
            Target::Need => text.hint_need,
            Target::Have => text.hint_have,
            Target::Tables => text.hint_tables,
        };
        // Only while this slot still holds the factory rectangle. Once a
        // region has been drawn the guide is a second box sitting beside the
        // real one, which reads as a stale leftover rather than as advice.
        let show_guide = self
            .calibration
            .rect(active_target)
            .is_none_or(|rect| (rect.x, rect.y, rect.width, rect.height) == guide);

        let toolbar = div()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .p_3()
            .child(
                button("cal-load", LedgerButton::Primary, text.load_screenshot, cx).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.load_screenshot();
                        cx.notify();
                    }),
                ),
            )
            .children(Target::ALL.into_iter().map(|target| {
                let active = target == active_target;
                button(
                    target.element_id(),
                    if active {
                        LedgerButton::Primary
                    } else {
                        LedgerButton::Quiet
                    },
                    match target {
                        Target::Need => text.slot_need,
                        Target::Have => text.slot_have,
                        Target::Tables => text.slot_tables,
                    },
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.calibration.target = Some(target);
                    cx.notify();
                }))
            }))
            .child(
                button("cal-zoom-in", LedgerButton::Quiet, text.zoom_in, cx).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.zoom_calibration(1.25);
                        cx.notify();
                    },
                )),
            )
            .child(
                button("cal-zoom-out", LedgerButton::Quiet, text.zoom_out, cx).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.zoom_calibration(0.8);
                        cx.notify();
                    }),
                ),
            )
            .child(
                button("cal-fit", LedgerButton::Quiet, text.fit_window, cx).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.fit_calibration();
                        cx.notify();
                    },
                )),
            )
            .child(
                button("cal-actual", LedgerButton::Quiet, text.actual_size, cx).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.zoom_calibration_to(1.0);
                        cx.notify();
                    }),
                ),
            )
            .child(
                button("cal-apply", LedgerButton::Primary, text.apply_regions, cx).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.apply_drawn_regions();
                        cx.notify();
                    }),
                ),
            )
            .child(
                // Beside the drawing tools, because it is one of them: at this
                // resolution it is the accurate way to set these rectangles,
                // and drawing by hand is the fallback for every other one.
                button("cal-preset", LedgerButton::Quiet, text.use_preset, cx).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.apply_preset_regions();
                        cx.notify();
                    }),
                ),
            );

        let hint_row = div()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .pb_2()
            .child(
                mono(hint.to_owned())
                    .text_size(fs(FS_12))
                    .text_color(c(TEXT_SECONDARY)),
            )
            .child(
                mono(text.guide_hint.to_owned())
                    .text_size(fs(FS_12))
                    .text_color(c(WARN_TEXT)),
            );

        let drawn: Vec<(bool, f32, f32, f32, f32)> = Target::ALL
            .into_iter()
            .filter_map(|target| {
                let rect = self.calibration.rect(target)?;
                let (left, top) = view.to_canvas(rect.x as f32, rect.y as f32);
                Some((
                    target == active_target,
                    left,
                    top,
                    rect.width as f32 * view.zoom,
                    rect.height as f32 * view.zoom,
                ))
            })
            .collect();

        // The loupe: the same picture scaled up, shifted so the pixel under
        // the cursor sits under the crosshair. Sampling a decoded image would
        // mean decoding it here; letting the renderer scale it costs nothing
        // and shows exactly the pixels that will be captured.
        //
        // It rides with the cursor rather than parking in a corner. A fixed
        // loupe makes you look away from the thing you are aiming at, which is
        // the one moment aim matters, and it flips to the opposite side near
        // an edge so it never covers the pixel it exists to show.
        let magnifier = self
            .calibration
            .cursor
            .zip(image.clone())
            .map(|(point, path)| {
                let (image_width, image_height) = size.unwrap_or((0, 0));
                let zoom = MAGNIFIER_ZOOM;
                const BOX: f32 = 176.0;
                const GAP: f32 = 20.0;
                let (canvas_w, canvas_h) = self.canvas_bounds.get().map_or((0.0, 0.0), |bounds| {
                    (f32::from(bounds.size.width), f32::from(bounds.size.height))
                });
                let at = view.to_canvas(point.0, point.1);
                let (left, top) = loupe_origin(at, (canvas_w, canvas_h), BOX, GAP);
                // A hairline through the exact source pixel, in both axes. Without
                // it the loupe shows a magnified patch with no indication of which
                // pixel of it the cursor is on, which is the one thing it is for.
                let crosshair = |vertical: bool| {
                    let line = div().absolute().bg(c(DANGER));
                    if vertical {
                        line.left(px(BOX / 2.0)).top_0().w(px(1.0)).h(px(BOX))
                    } else {
                        line.top(px(BOX / 2.0)).left_0().h(px(1.0)).w(px(BOX))
                    }
                };
                div()
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(BOX))
                    .h(px(BOX))
                    .overflow_hidden()
                    .border_2()
                    .border_color(c(ACCENT))
                    .bg(c(WELL))
                    .child(
                        gpui::img(path)
                            .image_cache(&self.image_cache)
                            .absolute()
                            .left(px(BOX / 2.0 - point.0 * zoom))
                            .top(px(BOX / 2.0 - point.1 * zoom))
                            .w(px(image_width as f32 * zoom))
                            .h(px(image_height as f32 * zoom)),
                    )
                    .child(crosshair(false))
                    .child(crosshair(true))
                    .child(
                        // The coordinate under the crosshair, on the loupe rather
                        // than in the status bar: at the moment of aiming, the eye
                        // is here.
                        div()
                            .absolute()
                            .bottom_0()
                            .left_0()
                            .w(px(BOX))
                            .bg(c(WELL))
                            .child(
                                mono(format!("{}, {}", point.0.round(), point.1.round()))
                                    .text_size(fs(FS_12))
                                    .text_color(c(TEXT_DATA)),
                            ),
                    )
            });

        let canvas_area = div()
            .relative()
            .flex_grow()
            .overflow_hidden()
            .bg(c(WELL))
            .border_1()
            .border_color(c(HAIRLINE))
            .child(
                // Sized to fill, or it measures itself — which is nothing.
                // This element exists only to report where the canvas landed,
                // and a zero-sized probe reports a zero-sized canvas: the fit
                // then clamped to the minimum zoom, and every mouse position
                // fell outside the bounds and was discarded. Nothing on the
                // screen worked, and all of it was this.
                gpui::canvas(
                    move |bounds, _, _| bounds_slot.set(Some(bounds)),
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .children(image.clone().map(|path| {
                let (width, height) = size.unwrap_or((0, 0));
                gpui::img(path)
                    .image_cache(&self.image_cache)
                    .absolute()
                    .left(px(view.pan_x))
                    .top(px(view.pan_y))
                    .w(px(width as f32 * view.zoom))
                    .h(px(height as f32 * view.zoom))
            }))
            .children(image.as_ref().filter(|_| show_guide).map(|_| {
                let (left, top) = view.to_canvas(guide.0 as f32, guide.1 as f32);
                div()
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(guide.2 as f32 * view.zoom))
                    .h(px(guide.3 as f32 * view.zoom))
                    .border_1()
                    .border_color(c(WARN))
            }))
            .children(
                // The rectangle as it is being dragged. Needed to draw at all
                // — a selection you cannot see until you release is a guess —
                // and it doubles as the answer to whether the press registered.
                self.calibration
                    .drag_from
                    .zip(self.calibration.cursor)
                    .map(|(from, to)| {
                        let start = view.to_canvas(from.0, from.1);
                        let end = view.to_canvas(to.0, to.1);
                        div()
                            .absolute()
                            .left(px(start.0.min(end.0)))
                            .top(px(start.1.min(end.1)))
                            .w(px((end.0 - start.0).abs()))
                            .h(px((end.1 - start.1).abs()))
                            .border_2()
                            .border_color(c(ACCENT))
                    }),
            )
            .children(drawn.into_iter().map(|(active, left, top, w, h)| {
                div()
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(w))
                    .h(px(h))
                    .border_2()
                    .border_color(c(if active { ACCENT } else { HAIRLINE_STRONG }))
            }))
            .children(magnifier)
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                    if let Some(point) = this.canvas_point(event.position) {
                        this.calibration.drag_from = Some(point);
                        cx.notify();
                    }
                }),
            )
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                    this.calibration.pan_from = this.raw_canvas_point(event.position);
                    cx.notify();
                }),
            )
            .on_mouse_up(
                gpui::MouseButton::Right,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    this.calibration.pan_from = None;
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                // Panning first: while it is happening the transform is
                // moving, so a source-pixel reading taken now would describe
                // the previous frame.
                if let (Some(from), Some(to)) = (
                    this.calibration.pan_from,
                    this.raw_canvas_point(event.position),
                ) {
                    let mut view = this.calibration.view();
                    view.pan_x += to.0 - from.0;
                    view.pan_y += to.1 - from.1;
                    this.calibration.view = Some(view);
                    this.calibration.pan_from = Some(to);
                }
                if let Some(point) = this.canvas_point(event.position) {
                    this.calibration.cursor = Some(point);
                }
                cx.notify();
            }))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseUpEvent, _, cx| {
                    this.finish_drag(event.position);
                    cx.notify();
                }),
            );

        // What each target currently holds, so the page says whether a click
        // did anything. Without it the only feedback was the drawn rectangle,
        // and with nothing rendering there was none at all.
        // Saved, and - when they differ - what applying would change it to.
        // The two used to be reported on different pages with no way to tell
        // whether a click had connected them, which made a working feature
        // indistinguishable from a broken one.
        let saved: Vec<(String, bool)> = Target::ALL
            .into_iter()
            .map(|target| {
                let label = match target {
                    Target::Need => text.slot_need,
                    Target::Have => text.slot_have,
                    Target::Tables => text.slot_tables,
                };
                let show = |rect: crate::calibrate::SourceRect| {
                    format!("{},{} {}x{}", rect.x, rect.y, rect.width, rect.height)
                };
                let stored = self.saved_rect(profile, target);
                let drawn = self.calibration.rect(target);
                match (stored, drawn) {
                    (stored, Some(rect)) if self.calibration.differs(target, stored) => (
                        format!(
                            "{label} {} \u{2192} {}",
                            stored.map_or_else(|| text.nothing_yet.to_owned(), show),
                            show(rect)
                        ),
                        true,
                    ),
                    (Some(rect), _) => (format!("{label} {}", show(rect)), false),
                    // `(None, Some(_))` is already taken by the guard above;
                    // the compiler cannot see that, hence the wildcard.
                    (None, _) => (format!("{label} {}", text.nothing_yet), false),
                }
            })
            .collect();

        let status = self.calibration.message.clone().unwrap_or_else(|| {
            if self.calibration.image.is_none() {
                text.no_screenshot.to_owned()
            } else {
                let cursor = self
                    .calibration
                    .cursor
                    .map(|(x, y)| format!("  {}, {}", x.round(), y.round()))
                    .unwrap_or_default();
                // The canvas size and zoom ride along. Everything on this
                // screen is that transform, and when it is wrong the picture
                // still looks plausible — so the numbers that would say so are
                // on screen rather than reachable only from a debugger.
                let canvas = self.canvas_bounds.get().map_or_else(
                    || "canvas ?".to_owned(),
                    |bounds| {
                        format!(
                            "canvas {}x{}",
                            f32::from(bounds.size.width).round(),
                            f32::from(bounds.size.height).round()
                        )
                    },
                );
                format!(
                    "{}{cursor}   {canvas}  zoom {:.2}",
                    text.drag_to_draw, view.zoom
                )
            }
        });

        div()
            .flex_grow()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(hint_row)
            .child(
                // A flex column, not a bare block. `canvas_area` grows into
                // this, and growth in a non-flex parent is no growth at all:
                // the box collapsed to zero height, so the screenshot was
                // being drawn correctly into nothing. The magnifier, being
                // absolutely positioned, kept rendering — which is why the
                // loupe showed a picture the canvas did not.
                div()
                    .flex_grow()
                    .flex()
                    .flex_col()
                    .relative()
                    .px_3()
                    .pb_2()
                    .child(canvas_area),
            )
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().flex().gap_4().children(saved.into_iter().map(
                        |(line, pending)| {
                            mono(line).text_size(fs(FS_12)).text_color(c(if pending {
                                WARN_TEXT
                            } else {
                                TEXT_DATA
                            }))
                        },
                    )))
                    .child(mono(status).text_size(fs(FS_12)).text_color(c(TEXT_META))),
            )
    }

    /// The profile's stored rectangle for one target, if it has one.
    #[cfg(windows)]
    pub(crate) fn saved_rect(
        &self,
        profile: ptt_core::ProfileId,
        target: crate::calibrate::Target,
    ) -> Option<crate::calibrate::SourceRect> {
        let entry = self.settings.profile(profile)?;
        let region = match target {
            crate::calibrate::Target::Need => entry.need_name_region,
            crate::calibrate::Target::Have => entry.have_name_region,
            crate::calibrate::Target::Tables => entry.tables_region,
        }?;
        Some(crate::calibrate::SourceRect {
            x: region.x,
            y: region.y,
            width: region.width,
            height: region.height,
        })
    }

    /// Loads a profile's stored rectangles onto the drawing surface.
    ///
    /// Without this the page starts blank and every region has to be redrawn
    /// from nothing to change one of them, and - worse - there is no way to
    /// see that applying worked, because the numbers it wrote were only ever
    /// shown on another page.
    #[cfg(windows)]
    pub(crate) fn seed_calibration(&mut self, profile: ptt_core::ProfileId) {
        use crate::calibrate::Target;
        self.calibration.need = self.saved_rect(profile, Target::Need);
        self.calibration.have = self.saved_rect(profile, Target::Have);
        self.calibration.tables = self.saved_rect(profile, Target::Tables);
        self.calibration.seeded_for = Some(profile);
    }

    /// Window position to a position inside the canvas, or `None` outside it.
    #[cfg(windows)]
    pub(crate) fn raw_canvas_point(
        &self,
        position: gpui::Point<gpui::Pixels>,
    ) -> Option<(f32, f32)> {
        let bounds = self.canvas_bounds.get()?;
        let x = f32::from(position.x - bounds.origin.x);
        let y = f32::from(position.y - bounds.origin.y);
        (x >= 0.0
            && y >= 0.0
            && x <= f32::from(bounds.size.width)
            && y <= f32::from(bounds.size.height))
        .then_some((x, y))
    }

    /// Window position to a point in the screenshot, or `None` off-canvas.
    #[cfg(windows)]
    pub(crate) fn canvas_point(&self, position: gpui::Point<gpui::Pixels>) -> Option<(f32, f32)> {
        let (x, y) = self.raw_canvas_point(position)?;
        let (source_x, source_y) = self.calibration.view().to_source(x, y);
        let (width, height) = self.calibration.image_size?;
        // Clamped rather than rejected: a drag that runs past the edge should
        // stop at the edge, not discard the whole rectangle.
        Some((
            source_x.clamp(0.0, width as f32),
            source_y.clamp(0.0, height as f32),
        ))
    }

    #[cfg(windows)]
    pub(crate) fn finish_drag(&mut self, position: gpui::Point<gpui::Pixels>) {
        let Some(from) = self.calibration.drag_from.take() else {
            return;
        };
        let Some(to) = self.canvas_point(position) else {
            return;
        };
        let target = self.calibration.target();
        let label = match target {
            crate::calibrate::Target::Need => self.text().slot_need,
            crate::calibrate::Target::Have => self.text().slot_have,
            crate::calibrate::Target::Tables => self.text().slot_tables,
        };
        match crate::calibrate::SourceRect::from_corners(from, to) {
            Some(rect) => {
                self.calibration.set_rect(target, rect);
                self.calibration.message = Some(format!(
                    "{label}  {},{}  {}x{}",
                    rect.x, rect.y, rect.width, rect.height
                ));
            }
            None => self.calibration.message = None,
        }
    }

    #[cfg(windows)]
    pub(crate) fn zoom_calibration(&mut self, factor: f32) {
        let (anchor_x, anchor_y) = self.canvas_bounds.get().map_or((0.0, 0.0), |bounds| {
            (
                f32::from(bounds.size.width) / 2.0,
                f32::from(bounds.size.height) / 2.0,
            )
        });
        let view = self
            .calibration
            .view()
            .zoomed_about(factor, anchor_x, anchor_y);
        self.calibration.view = Some(view);
    }

    #[cfg(windows)]
    pub(crate) fn zoom_calibration_to(&mut self, zoom: f32) {
        let current = self.calibration.view().zoom;
        if current > 0.0 {
            self.zoom_calibration(zoom / current);
        }
    }

    #[cfg(windows)]
    pub(crate) fn fit_calibration(&mut self) {
        let (Some(size), Some(bounds)) = (self.calibration.image_size, self.canvas_bounds.get())
        else {
            return;
        };
        self.calibration.view = Some(crate::calibrate::View::fitted(
            size,
            (f32::from(bounds.size.width), f32::from(bounds.size.height)),
        ));
    }

    /// Starts the system picker on its own thread.
    ///
    /// Not called inline: the dialog pumps messages while it is open, and from
    /// inside an event handler that re-enters the framework while this view is
    /// already mutably borrowed. The process does not hang, it dies — which is
    /// what clicking this button used to do.
    #[cfg(windows)]
    pub(crate) fn load_screenshot(&mut self) {
        let sender = self.shell_tx.clone();
        ptt_platform_win::spawn_pick_image(move |path| {
            let _ = sender.send(ShellMsg::ScreenshotPicked(path));
        });
    }

    /// Measures whatever the picker returned.
    #[cfg(windows)]
    pub(crate) fn screenshot_picked(&mut self, path: Option<std::path::PathBuf>) {
        let Some(path) = path else {
            return;
        };
        let size = std::fs::read(&path)
            .ok()
            .and_then(|bytes| crate::calibrate::image_size(&bytes));
        match size {
            Some(size) => {
                self.calibration.image = Some(path);
                self.calibration.image_size = Some(size);
                self.calibration.message = None;
                // Cleared so the next paint fits the new picture; fitting here
                // would use whatever bounds the last paint happened to leave.
                self.calibration.view = None;
            }
            None => {
                // Refused rather than shown at a guessed size: every rectangle
                // drawn on a wrongly scaled picture lands somewhere else.
                self.calibration.message =
                    Some(format!("unreadable image header: {}", path.display()));
            }
        }
    }

    /// Writes the drawn rectangles into the active profile.
    #[cfg(windows)]
    pub(crate) fn apply_drawn_regions(&mut self) {
        let profile = self.settings.active_profile;
        // Only what actually differs. Since the page now opens on the stored
        // rectangles, applying would otherwise rewrite all three every time --
        // three log lines and three watcher restarts to change one region.
        let changed: Vec<_> = self
            .calibration
            .completed()
            .into_iter()
            .filter(|(target, _)| {
                self.calibration
                    .differs(*target, self.saved_rect(profile, *target))
            })
            .collect();
        if changed.is_empty() {
            self.calibration.message = Some(self.text().nothing_to_apply.to_owned());
            return;
        }
        let count = changed.len();
        for (target, rect) in changed {
            let slot = match target {
                crate::calibrate::Target::Need => RegionSlot::Need,
                crate::calibrate::Target::Have => RegionSlot::Have,
                crate::calibrate::Target::Tables => RegionSlot::Tables,
            };
            self.apply_calibration(slot, rect.x, rect.y, rect.width, rect.height);
        }
        self.calibration.message = Some(format!("{} {count}", self.text().applied));
    }

    /// Installs the profile's factory rectangles for 2560x1440.
    ///
    /// The panel's geometry is fixed for a given resolution, so the numbers
    /// that the corpus was calibrated against are the right ones — drawing
    /// them by hand can only be less accurate. A hand-drawn region that starts
    /// above the tables puts the panel's title and market ratio inside it, and
    /// the row grid then anchors on furniture and reads two rows of twelve.
    ///
    /// Only useful at 2560x1440; on any other desktop the user still has to
    /// draw, which is why this sits beside the calibrate buttons rather than
    /// replacing them.
    #[cfg(windows)]
    pub(crate) fn apply_preset_regions(&mut self) {
        let profile = self.settings.active_profile;
        let (layout, language) = ptt_runtime::pipeline::route_for(profile);
        for (slot, (x, y, width, height)) in [
            (RegionSlot::Need, layout.need_name),
            (RegionSlot::Have, layout.have_name),
            // Through `tables_for`, because the Traditional Chinese client
            // draws taller table headings and sits the tables lower. Reading
            // the raw constant here would hand a zh-TW profile the English
            // rectangle and call it a preset.
            (RegionSlot::Tables, layout.tables_for(language)),
        ] {
            self.apply_calibration(slot, x, y, width, height);
            // Onto the canvas as well, so the preset is something you can look
            // at and check against the panel rather than three numbers you
            // have to trust. It is also what keeps the page honest: it would
            // otherwise go on showing whatever was drawn before.
            let target = match slot {
                RegionSlot::Need => crate::calibrate::Target::Need,
                RegionSlot::Have => crate::calibrate::Target::Have,
                RegionSlot::Tables => crate::calibrate::Target::Tables,
            };
            self.calibration.set_rect(
                target,
                crate::calibrate::SourceRect {
                    x,
                    y,
                    width,
                    height,
                },
            );
        }
        self.calibration.message = Some(self.text().preset_applied.to_owned());
    }
}
