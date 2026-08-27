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
    /// The settings page (§10 定稿 = 13a):顶部通栏 + 左侧 132px 分段栏 +
    /// 当前分段的面板。四段:基本 / 浮窗 / 赛季与存储 / 算法参数——原来是
    /// 一条要滚三屏的长列。
    #[cfg(windows)]
    pub(crate) fn render_settings_page(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        use crate::shell::SettingsSegment;
        use gpui::{InteractiveElement as _, StatefulInteractiveElement as _};
        use gpui_component::StyledExt as _;

        let text = self.text();
        let current = self.settings_segment;

        let mut rail = div()
            .w(px(132.))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(1.))
            .py_2()
            .bg(c(RAIL))
            .border_r_1()
            .border_color(c(HAIRLINE));
        for segment in SettingsSegment::ALL {
            let active = segment == current;
            let row = div()
                .id(segment.element_id())
                .h(px(H_BUTTON))
                .flex_none()
                .flex()
                .items_center()
                .text_size(fs(FS_12))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.settings_segment = segment;
                    cx.notify();
                }));
            let row = if active {
                row.pl(px(12.))
                    .border_l_2()
                    .border_color(c(ACCENT))
                    .bg(c(PANEL))
                    .font_semibold()
                    .text_color(c(ACCENT_TEXT))
            } else {
                row.pl(px(14.))
                    .text_color(c(TEXT_SECONDARY))
                    .hover(|style| style.bg(c(HOVER)))
            };
            rail = rail.child(row.child(gpui::SharedString::from(segment.label(text).to_string())));
        }

        let body = match current {
            SettingsSegment::Basic => self.settings_panel(cx),
            SettingsSegment::Hud => self.hud_settings_panel(cx),
            SettingsSegment::Season => self.season_panel(cx),
            SettingsSegment::Params => self.tuning_panel(cx),
        };

        div().flex_grow().min_h(px(0.)).flex().child(rail).child(
            div()
                .flex_1()
                .min_w(px(0.))
                .min_h(px(0.))
                .flex()
                .flex_col()
                .gap(px(SP_8))
                .p(px(SP_10))
                // 结算通货与「允许路过」在通栏:不属于任何一段,影响
                // 所有页面(§10)。
                .child(self.settings_banner(cx))
                .child(crate::ui::scrollable(
                    div().flex_grow().flex().flex_col().child(body),
                    "settings-scroll",
                )),
        )
    }

    /// 浮窗段(§4/`3c`):档位、不透明度、热键。摆放模式的入口与顶条交互
    /// 是余下的未竟项。
    #[cfg(windows)]
    fn hud_settings_panel(&self, cx: &mut Context<Self>) -> gpui::Div {
        use gpui::{InteractiveElement as _, StatefulInteractiveElement as _};
        use gpui_component::StyledExt as _;
        let text = self.text();
        let label_col = |label: &'static str| {
            div()
                .w(px(150.))
                .flex_none()
                .text_size(fs(FS_11_5))
                .text_color(c(TEXT_META))
                .child(label)
        };
        let hotkey_row = |label: &'static str, key: String| {
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .py(px(3.))
                .child(label_col(label))
                .child(crate::ui::hotkey_chip(&key))
        };

        // 档位:迷你 / 展开,即点即生效。
        let tier = self.settings.hud.tier;
        let mut tier_cells = div()
            .h_flex()
            .items_center()
            .flex_none()
            .border_1()
            .border_color(c(HAIRLINE));
        for (index, (label, value)) in [
            (text.hud_tier_mini, ptt_settings::HudTier::Mini),
            (text.hud_tier_expanded, ptt_settings::HudTier::Expanded),
        ]
        .into_iter()
        .enumerate()
        {
            let mut cell = div()
                .id(("hud-tier", index))
                .h(px(H_ROW))
                .px(px(10.))
                .flex()
                .items_center()
                .text_size(fs(FS_11_5))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_hud_tier(value);
                    cx.notify();
                }));
            if index > 0 {
                cell = cell.border_l_1().border_color(c(HAIRLINE));
            }
            cell = if value == tier {
                cell.bg(c(ACCENT_WASH)).text_color(c(ACCENT_TEXT))
            } else {
                cell.bg(c(PANEL))
                    .text_color(c(TEXT_SECONDARY))
                    .hover(|style| style.bg(c(HOVER)))
            };
            tier_cells = tier_cells.child(cell.child(label));
        }

        // 不透明度:60–100%,步长 5。
        let opacity = self.settings.hud.clamped_opacity();
        let step_button = |id: &'static str, label: &'static str, step: i16| {
            div()
                .id(id)
                .h(px(H_ROW))
                .w(px(26.))
                .flex()
                .items_center()
                .justify_center()
                .border_1()
                .border_color(c(HAIRLINE))
                .text_size(fs(FS_12))
                .text_color(c(TEXT_SECONDARY))
                .cursor_pointer()
                .hover(|style| style.bg(c(HOVER)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.bump_hud_opacity(step);
                    cx.notify();
                }))
                .child(label)
        };
        let opacity_row = div()
            .h_flex()
            .items_center()
            .gap_2()
            .py(px(3.))
            .child(label_col(text.hud_opacity_label))
            .child(step_button("hud-opacity-down", "−", -5))
            .child(
                mono(format!("{opacity}%"))
                    .w(px(48.))
                    .text_center()
                    .text_size(fs(FS_12))
                    .text_color(c(TEXT_DATA)),
            )
            .child(step_button("hud-opacity-up", "+", 5));

        // 摆放:进摆放模式浮窗接住鼠标,拖到位点「完成」(浮窗顶条或这里
        // 都能点)。
        let placing = self.hud_placement;
        let place_row = div()
            .h_flex()
            .items_center()
            .gap_2()
            .py(px(3.))
            .child(label_col(text.hud_place_hint))
            .child(
                button(
                    "hud-place",
                    if placing {
                        LedgerButton::Primary
                    } else {
                        LedgerButton::Secondary
                    },
                    if placing {
                        text.hud_place_done
                    } else {
                        text.hud_place_button
                    },
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    let next = !this.hud_placement;
                    this.set_hud_placement(next);
                    cx.notify();
                })),
            )
            .child(
                div()
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_META))
                    .child(text.hud_place_help),
            );

        panel().child(panel_header(text.seg_hud)).child(
            div()
                .p_3()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .py(px(3.))
                        .child(label_col(text.hud_tier_label))
                        .child(tier_cells),
                )
                .child(opacity_row)
                .child(place_row)
                .child(hotkey_row(
                    text.hud_hotkey_watch,
                    self.settings.hotkeys.toggle_watch.clone(),
                ))
                .child(hotkey_row(
                    text.hud_hotkey_toggle,
                    self.settings.hotkeys.toggle_hud.clone(),
                ))
                .child(hotkey_row(
                    text.hud_hotkey_capture,
                    self.settings.hotkeys.manual_capture.clone(),
                )),
        )
    }

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
        // 换了游戏,报表和赛季面板显示的就都是上一个游戏的了——立刻作废,
        // 不然要等到下一次 stale 触发之前一直张冠李戴。
        self.report_stale = true;
        self.season_info = None;
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
