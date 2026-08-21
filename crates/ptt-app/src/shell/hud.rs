//! The overlay card: the same book, readable without leaving the game.

use super::{AppShell, HUD_ORIGIN, HUD_SIZE, hud_lines, skip_label};

impl AppShell {
    /// Shows or hides the overlay card, creating it on first use.
    ///
    /// The card is created excluded from capture and click-through from the
    /// moment it exists: a HUD that appears in a screenshot would be read
    /// back as part of the panel it is describing, and one that takes clicks
    /// would steal them from the game.
    #[cfg(windows)]
    /// Whether the card should hide itself from Windows capture.
    ///
    /// It hides only where it would otherwise be read as part of the panel —
    /// that is, where it actually covers one of the calibrated regions. The
    /// flag used to be permanent, which solved that problem and created
    /// another: `WDA_EXCLUDEFROMCAPTURE` hides the window from *every* capture
    /// API, so the person running this could not screenshot their own HUD to
    /// show anyone what it said.
    ///
    /// Deciding it from geometry means neither goal has to be traded for the
    /// other, and neither has to be a setting: a card parked away from the
    /// panel is photographable, and a card dragged over the tables protects
    /// the read without being asked.
    #[cfg(windows)]
    pub(crate) fn hud_capture_affinity(
        &self,
        bounds: ptt_platform_win::RectI,
    ) -> ptt_platform_win::CaptureAffinity {
        use crate::calibrate::Target;

        let profile = self.settings.active_profile;
        let (layout, language) = ptt_runtime::pipeline::route_for(profile);
        let covers = Target::ALL.into_iter().any(|target| {
            let (x, y, width, height) = match target {
                Target::Need => layout.need_name,
                Target::Have => layout.have_name,
                Target::Tables => layout.tables_for(language),
            };
            // The saved rectangle wins where there is one: that is what the
            // watcher will actually capture.
            let (x, y, width, height) = self
                .saved_rect(profile, target)
                .map_or((x, y, width, height), |rect| {
                    (rect.x, rect.y, rect.width, rect.height)
                });
            ptt_platform_win::RectI::new(
                x,
                y,
                i32::try_from(width).unwrap_or(i32::MAX),
                i32::try_from(height).unwrap_or(i32::MAX),
            )
            .is_some_and(|region| region.intersects(bounds))
        });
        if covers {
            ptt_platform_win::CaptureAffinity::Exclude
        } else {
            ptt_platform_win::CaptureAffinity::Include
        }
    }

    pub(crate) fn toggle_hud(&mut self) {
        use ptt_platform_win::{
            HudInteractionMode, HudWindow, HudWindowConfig, HudWindowPolicy, RectI,
        };

        if self.hud.is_none() {
            let Some(bounds) = RectI::new(HUD_ORIGIN.0, HUD_ORIGIN.1, HUD_SIZE.0, HUD_SIZE.1)
            else {
                self.push_log("HUD bounds are invalid".to_owned());
                return;
            };
            let config = HudWindowConfig {
                bounds,
                policy: HudWindowPolicy {
                    interaction: HudInteractionMode::Passive,
                    capture_affinity: self.hud_capture_affinity(bounds),
                },
                visible: false,
            };
            match HudWindow::create(config) {
                Ok(hud) => self.hud = Some(hud),
                Err(error) => {
                    self.push_log(format!("HUD unavailable: {error}"));
                    return;
                }
            }
        }
        let Some(hud) = self.hud.as_mut() else {
            return;
        };
        let outcome = if self.hud_visible {
            hud.hide()
        } else {
            hud.show()
        };
        match outcome {
            Ok(()) => {
                self.hud_visible = !self.hud_visible;
                self.refresh_hud();
            }
            Err(error) => self.push_log(format!("HUD toggle failed: {error}")),
        }
    }

    /// Pushes the current state onto the card. Cheap and idempotent, so it
    /// can run on the tick without a dirty flag.
    #[cfg(windows)]
    pub(crate) fn refresh_hud(&mut self) {
        use ptt_platform_win::HudContent;

        if !self.hud_visible {
            return;
        }
        let text = self.text();
        let status = if self.fault.is_some() {
            text.state_fault
        } else if self.watching {
            text.state_watching
        } else {
            text.state_idle
        };
        // The card mirrors the panel the user is looking at: which pair, the
        // rows read off it, and whether the last frame landed. Anything else
        // belongs in the window, which they can alt-tab to; this is for the
        // moment when they cannot.
        let pair = self.report_pair.as_ref().map_or_else(
            || self.text().no_pair_yet.to_owned(),
            |(have, need)| format!("{} -> {}", self.display_name(have), self.display_name(need)),
        );
        // Stated either way — "nothing since the last book" and "the watcher
        // died" look identical on a card that only reports success.
        let verdict = match (&self.fault, &self.last_skip) {
            (Some(fault), _) => format!("{}: {fault}", self.text().fault_prefix),
            (None, Some(reason)) => format!(
                "{} {}",
                text.skips_label,
                skip_label(reason, self.settings.ui_language)
            ),
            (None, None) if self.accepted > 0 => {
                format!("{} {}", self.text().accepted_label, self.accepted)
            }
            (None, None) => self.text().nothing_yet.to_owned(),
        };
        let lines = hud_lines(
            &pair,
            &self.last_rows,
            self.text().waiting_for_book,
            &verdict,
        );
        let content = HudContent {
            monitoring: self.watching,
            status_text: status.to_owned(),
            elapsed: ptt_runtime::report_text::fill(
                text.hud_accepted_count,
                &[&self.accepted.to_string()],
            ),
            lines,
        };
        if let Some(hud) = self.hud.as_mut()
            && let Err(error) = hud.set_content(content)
        {
            self.push_log(format!("HUD update failed: {error}"));
        }
    }

    #[cfg(not(windows))]
    pub(crate) fn toggle_hud(&mut self) {
        self.hud_visible = !self.hud_visible;
    }

    #[cfg(not(windows))]
    pub(crate) fn refresh_hud(&mut self) {}
}
