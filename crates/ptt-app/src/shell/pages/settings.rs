//! The settings page: which game is being watched, in which language, and
//! with which hotkeys.
//!
//! It used to sit in a column on the monitor, underneath lists that grow with
//! the market, so a session with enough missing pairs pushed it off the
//! bottom of the screen.

use gpui::{Context, ParentElement, Styled, div, px};

use crate::shell::AppShell;
use crate::theme::*;
use crate::ui::{LedgerButton, button, mono, panel, panel_header};

impl AppShell {
    #[cfg(windows)]
    #[cfg(windows)]
    pub(crate) fn settings_panel(&self, cx: &mut Context<Self>) -> gpui::Div {
        // No region rows here. They reported the stored numbers, which do not
        // change until a drawn rectangle is applied, so a person who had just
        // drawn one saw the old values and reasonably concluded that drawing
        // did nothing. The calibration page shows stored and drawn together,
        // which is the only place the two can be compared; a second read-only
        // copy on another page can only disagree with it.
        let profile = self.settings.active_profile;
        let text = self.text();
        let hotkey_line = if self.hotkey_ok.watch {
            format!(
                "{} — {}",
                self.settings.hotkeys.toggle_watch, text.hotkey_ready
            )
        } else {
            format!(
                "{} — {}",
                self.settings.hotkeys.toggle_watch, text.hotkey_unavailable
            )
        };
        panel().child(panel_header(text.panel_settings)).child(
            div()
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    self.profile_row(
                        text.game_label,
                        [
                            ("profile-poe1", ptt_core::Game::Poe1, "PoE 1"),
                            ("profile-poe2", ptt_core::Game::Poe2, "PoE 2"),
                        ]
                        .map(|(id, game, label)| {
                            (
                                id,
                                game == profile.game,
                                label,
                                ptt_core::ProfileId::new(game, profile.language),
                            )
                        })
                        .to_vec(),
                        cx,
                    ),
                )
                .child(
                    self.profile_row(
                        text.client_language_label,
                        [
                            ("client-en", ptt_core::ContentLanguage::English, "EN"),
                            (
                                "client-zh",
                                ptt_core::ContentLanguage::TraditionalChinese,
                                "繁中",
                            ),
                        ]
                        .map(|(id, language, label)| {
                            (
                                id,
                                language == profile.language,
                                label,
                                ptt_core::ProfileId::new(profile.game, language),
                            )
                        })
                        .to_vec(),
                        cx,
                    ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(90.0))
                                .text_size(fs(FS_12))
                                .text_color(c(TEXT_META))
                                .child(text.language_label),
                        )
                        .children(crate::i18n::LANGUAGES.into_iter().map(|language| {
                            let active = language == self.settings.ui_language;
                            button(
                                match language {
                                    ptt_settings::UiLanguage::English => "lang-en",
                                    ptt_settings::UiLanguage::Chinese => "lang-zh",
                                },
                                if active {
                                    LedgerButton::Primary
                                } else {
                                    LedgerButton::Quiet
                                },
                                // Always in its own language: someone who
                                // cannot read the current one still finds
                                // theirs.
                                crate::i18n::native_label(language),
                                cx,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.set_language(language);
                                    cx.notify();
                                },
                            ))
                        })),
                )
                .child(
                    mono(hotkey_line)
                        .text_size(fs(FS_10_5))
                        .text_color(c(TEXT_META)),
                ),
        )
    }

    /// One row of mutually exclusive profile buttons.
    ///
    /// The profile decides which panel geometry the watcher reads and which
    /// catalog language it matches names against, so it is a setting the user
    /// has to be able to reach — POE1 recognition was otherwise only usable
    /// from the probes.
    #[cfg(windows)]
    pub(crate) fn profile_row(
        &self,
        label: &'static str,
        options: Vec<(&'static str, bool, &'static str, ptt_core::ProfileId)>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .w(px(90.0))
                    .text_size(fs(FS_12))
                    .text_color(c(TEXT_META))
                    .child(label),
            )
            .children(
                options
                    .into_iter()
                    .map(|(option_id, active, option_label, profile)| {
                        button(
                            option_id,
                            if active {
                                LedgerButton::Primary
                            } else {
                                LedgerButton::Quiet
                            },
                            option_label,
                            cx,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_profile(profile);
                            cx.notify();
                        }))
                    }),
            )
    }

    /// Switches the watched profile and persists it.
    ///
    /// Takes effect on the next watch start rather than mid-session: the route
    /// holds its layout and its OCR language from construction, and swapping
    /// them under a running capture would mix two panels' rows into one book.
    #[cfg(windows)]
    pub(crate) fn set_profile(&mut self, profile: ptt_core::ProfileId) {
        if self.settings.active_profile == profile {
            return;
        }
        self.settings.active_profile = profile;
        if let Err(error) = self.settings_store.save(&self.settings) {
            self.push_log(format!("could not save profile: {error}"));
            return;
        }
        self.push_log(format!(
            "profile {profile} — {}",
            self.text().restart_watch_to_apply
        ));
    }

    /// Switches the interface language and persists it.
    #[cfg(windows)]
    pub(crate) fn set_language(&mut self, language: ptt_settings::UiLanguage) {
        if self.settings.ui_language == language {
            return;
        }
        self.settings.ui_language = language;
        if let Err(error) = self.settings_store.save(&self.settings) {
            self.push_log(format!("could not save language: {error}"));
        }
    }

    #[cfg(not(windows))]
    pub(crate) fn settings_panel(&self, _cx: &mut Context<Self>) -> gpui::Div {
        panel().child(panel_header(self.text().panel_settings))
    }
}
