//! The settings page: which game is being watched, in which language, and
//! with which hotkeys.
//!
//! It used to sit in a column on the monitor, underneath lists that grow with
//! the market, so a session with enough missing pairs pushed it off the
//! bottom of the screen.

use gpui::{Context, ParentElement, Styled, div, px};

use crate::shell::AppShell;
use crate::theme::*;
use crate::ui::{LedgerButton, button, inline_section, kv_row, mono, panel, panel_header};

/// `CARGO_PKG_AUTHORS` is one `Name <mail>` string; the about panel wants the
/// two halves on their own rows.
///
/// Split here rather than written out twice in the catalogue: `Cargo.toml`
/// already owns this fact, and a second copy is how an interface ends up
/// showing a mail address nobody reads any more.
#[cfg(windows)]
fn author_and_email() -> (&'static str, &'static str) {
    let authors = env!("CARGO_PKG_AUTHORS");
    match authors.split_once('<') {
        Some((name, mail)) => (name.trim(), mail.trim_end_matches('>').trim()),
        None => (authors, ""),
    }
}

impl AppShell {
    /// The settings page (§10 定稿 = 13a):顶部通栏 + 左侧 132px 分段栏 +
    /// 当前分段的面板。前四段:基本 / 浮窗 / 赛季与存储 / 算法参数——原来是
    /// 一条要滚三屏的长列;后两段只读:使用说明 / 关于。
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
            SettingsSegment::Guide => self.guide_panel(),
            SettingsSegment::About => self.about_panel(cx),
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
                )),
        )
    }

    /// 使用说明段:第一次怎么走通、每一页答什么、热键,以及不对劲时先看哪。
    ///
    /// 正文是 i18n 里的整块字符串,一行一条,行内用 `  ·  ` 把标签和说明分开
    /// (`roles_legend` 的老办法)。一行一个字段的话,想补一句就得在四个地方
    /// 各改一处——那样这块文字注定会停在写完的那天。
    #[cfg(windows)]
    fn guide_panel(&self) -> gpui::Div {
        use gpui_component::StyledExt as _;
        let text = self.text();

        // 标签列在一节之内定宽、各节自己定宽度:同一节里的标签必须对齐,
        // 但四节的标签长度差着一个数量级(「1」和「a hotkey is dead」),
        // 全局共用一个宽度不是把长的挤掉行,就是让短的离正文半屏远。
        let line = |raw: &'static str, label_width: f32| {
            let (label, body) = raw.split_once("  ·  ").unwrap_or(("", raw));
            div()
                .flex()
                .items_start()
                .gap_2()
                .py(px(2.))
                .text_size(fs(FS_11_5))
                .child(
                    div()
                        .w(px(label_width))
                        .flex_none()
                        .text_color(c(TEXT_META))
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .text_color(c(TEXT_SECONDARY))
                        .child(body),
                )
        };
        let section = |title: &'static str, block: &'static str, label_width: f32| {
            let mut body = div().flex().flex_col().pt(px(SP_4));
            for raw in block.lines() {
                body = body.child(line(raw, label_width));
            }
            div()
                .flex()
                .flex_col()
                .child(inline_section(title))
                .child(body)
        };

        // 三条命令单独一节,而不是嵌在正文里。
        //
        // 界面上的字一个都选不中——gpui 的普通文本没有选区,而这几条恰恰是
        // 说明书里唯一必须一字不差敲进终端的东西。所以它们从正文里搬出来,
        // 一条一行,右边挂一个复制按钮,点一下就在剪贴板里。
        //
        // 命令是这里的常量而不是 i18n 字段:它们是 PowerShell 字面量,翻译
        // 只会译坏,两份译文各存一份也迟早会漂。
        let command_row = |id: &'static str, label: &'static str, command: &'static str| {
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .py(px(2.))
                .child(
                    div()
                        .w(px(132.))
                        .flex_none()
                        .text_size(fs(FS_11_5))
                        .text_color(c(TEXT_META))
                        .child(label),
                )
                .child(
                    crate::ui::mono(command)
                        .flex_1()
                        .min_w(px(0.))
                        .text_size(fs(FS_11))
                        .text_color(c(TEXT_SECONDARY)),
                )
                .child(gpui_component::clipboard::Clipboard::new(id).value(command))
        };
        let commands = div()
            .flex()
            .flex_col()
            .child(inline_section(text.guide_cmd_header))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .pt(px(SP_4))
                    .child(command_row(
                        "guide-cmd-check",
                        text.guide_cmd_check,
                        "[Windows.Media.Ocr.OcrEngine]::AvailableRecognizerLanguages",
                    ))
                    .child(command_row(
                        "guide-cmd-list",
                        text.guide_cmd_list,
                        "Get-WindowsCapability -Online -Name Language.OCR*",
                    ))
                    .child(command_row(
                        "guide-cmd-add",
                        text.guide_cmd_add,
                        "Add-WindowsCapability -Online -Name Language.OCR~~~zh-TW~0.0.1.0",
                    )),
            );

        // 只列真正注册了的两个:写进说明的键必须是按下去会响的键。
        let hotkey_row = |label: &'static str, key: &str| {
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .py(px(2.))
                .child(
                    div()
                        .w(px(96.))
                        .flex_none()
                        .text_size(fs(FS_11_5))
                        .text_color(c(TEXT_META))
                        .child(label),
                )
                .child(crate::ui::hotkey_chip(key))
        };
        let hotkeys = div()
            .flex()
            .flex_col()
            .child(inline_section(text.guide_hotkeys_header))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .pt(px(SP_4))
                    // 键值取自设置本身,不是抄一份默认值:改过之后说明书跟着变。
                    .child(hotkey_row(
                        text.hud_hotkey_watch,
                        &self.settings.hotkeys.toggle_watch,
                    ))
                    .child(hotkey_row(
                        text.hud_hotkey_toggle,
                        &self.settings.hotkeys.toggle_hud,
                    ))
                    .child(
                        div()
                            .pt(px(SP_4))
                            .text_size(fs(FS_10_5))
                            .text_color(c(TEXT_META))
                            .child(text.guide_hotkeys_note),
                    ),
            );

        panel().child(panel_header(text.seg_guide)).child(
            div()
                .p_3()
                .flex()
                .flex_col()
                .gap(px(SP_12))
                // 识别器排在"第一次用"前面,不是排进"看着不对的时候"。
                // 缺一个识别器不会在启动时报错,只会让每一帧都被丢掉——症状
                // 和"没校准好"一模一样,而这一节要在他去框区域之前就读到,
                // 不是等他卡住了再去翻。
                .child(section(text.guide_ocr_header, text.guide_ocr, 132.))
                .child(commands)
                // 四个宽度是量出来的:识别器那节最长 "traditional chinese"、
                // 序号一位、页名最长 "analytics"、症状最长 "a hotkey is dead"。
                .child(section(
                    text.guide_first_run_header,
                    text.guide_first_run,
                    16.,
                ))
                .child(section(text.guide_pages_header, text.guide_pages, 76.))
                .child(hotkeys)
                .child(section(text.guide_trouble_header, text.guide_trouble, 116.)),
        )
    }

    /// 关于段:名字、版本、作者、联系方式、源码、授权,外加更新。
    ///
    /// 值一个都不写死,全部来自 `env!("CARGO_PKG_*")`——版本号在 Cargo.toml
    /// 改一次就够了,界面上不会剩下一份还写着旧号的副本。更新那一段接在版本
    /// 后面,因为它回答的是同一个问题的下半句:"我在跑哪一版"之后是"还有没有
    /// 更新的"。
    #[cfg(windows)]
    fn about_panel(&self, cx: &mut Context<Self>) -> gpui::Div {
        use gpui_component::StyledExt as _;
        let text = self.text();
        let (author, email) = author_and_email();

        let mut rows = div()
            .p_3()
            .flex()
            .flex_col()
            .gap(px(SP_4))
            .child(
                div()
                    .text_size(fs(FS_15))
                    .font_semibold()
                    .text_color(c(TEXT_PRIMARY))
                    .child(text.app_title),
            )
            .child(kv_row(text.about_version, env!("CARGO_PKG_VERSION")));
        // 两行都可能没有值可给:`authors` 是 Cargo.toml 里唯一没有被继承就
        // 会变成空串的那个字段,而没有尖括号就没有邮箱。空标签配空值比这一
        // 行不存在更糟——它看起来像读取失败。
        if !author.is_empty() {
            rows = rows.child(kv_row(text.about_author, author));
        }
        if !email.is_empty() {
            rows = rows.child(kv_row(text.about_contact, email));
        }

        panel().child(panel_header(text.seg_about)).child(
            rows.child(kv_row(text.about_repository, env!("CARGO_PKG_REPOSITORY")))
                .child(kv_row(text.about_license, env!("CARGO_PKG_LICENSE")))
                .child(self.update_section(cx))
                .child(
                    div()
                        .pt(px(SP_8))
                        .text_size(fs(FS_10_5))
                        .text_color(c(TEXT_META))
                        .child(text.about_feedback),
                ),
        )
    }

    /// 关于段里的更新那一小节。
    ///
    /// 三样东西,顺序就是读的人问问题的顺序:现在是什么状况、新版本是哪一个、
    /// 我能按什么。
    ///
    /// 状态那一行永远有话说——包括"还没查过"和查失败。一个还没答话的更新检查
    /// 不该让这一段看起来像坏了,而失败必须留在这里而不是只闪过底部那条流水灯:
    /// 那条只留得住一句,下一条日志一来就没了。
    #[cfg(windows)]
    fn update_section(&self, cx: &mut Context<Self>) -> gpui::Div {
        use crate::shell::updater::{UpdateState, mib_tenths, progress_percent, stage_line};
        use gpui_component::StyledExt as _;

        let text = self.text();
        let state = &self.update_state;
        // 下载这个状态里面还有三段路,状态枚举不分它们(它只需要回答"能不能
        // 再按按钮"),所以那一句话在这里从进度上取。
        let downloading = matches!(state, UpdateState::Downloading(_));
        let progress = downloading.then(|| self.update_progress.snapshot());
        // 三种语气,一眼能分开:出了事是红的,有好消息是墨青的,其余是常规
        // 数据色。字本身已经说清楚了,颜色只是让人不必读完才知道该不该在意。
        let tone = if state.is_failure() {
            DANGER_TEXT
        } else if state.is_good_news() {
            ACCENT_TEXT
        } else {
            TEXT_DATA
        };

        let mut body = div()
            .pt(px(SP_8))
            .flex()
            .flex_col()
            .child(inline_section(text.update_header))
            .child(
                div()
                    .pt(px(SP_4))
                    .flex()
                    .items_start()
                    .gap_2()
                    .py(px(3.))
                    .text_size(fs(FS_11_5))
                    .child(
                        div()
                            .w(px(64.))
                            .flex_none()
                            .text_color(c(TEXT_META))
                            .child(text.update_status),
                    )
                    // `min_w(0)` 和 `kv_row` 里那一处是同一个理由:没有它,
                    // 一条长的失败消息不会换行,只会把面板撑宽然后被窗口切掉。
                    .child(
                        mono(match progress {
                            Some(snapshot) => stage_line(snapshot.stage, text).to_owned(),
                            None => state.line(text),
                        })
                        .flex_1()
                        .min_w(px(0.))
                        .text_color(c(tone)),
                    ),
            );

        // 没有分母就整块不画——条和数字都不画。GitHub 没报大小、或者正停在写盘
        // 那一段时,`progress_percent` 回的是 `None`,而那两句话里都嵌着分母:
        // 「已下载 12.4 / 0.0 MiB」印出来不是"总数还不知道",是"总共 0.0 MiB",
        // 比不画更糟。所以同一个判断同时管住这两样,不给它们分家的机会。
        if let Some(snapshot) = progress {
            if let Some(percent) = progress_percent(snapshot.done, snapshot.total) {
                body = body.child(
                    div().pt(px(SP_4)).child(
                        gpui_component::progress::Progress::new()
                            .value(f32::from(percent))
                            .h(px(3.)),
                    ),
                );
                // 单位跟着阶段走:下载数字节,核对数条目。写盘那一段本来就没有
                // 分母,走不到这里。
                let amount = match snapshot.stage {
                    crate::update::Stage::Downloading => Some(ptt_runtime::report_text::fill(
                        text.update_progress_bytes,
                        &[&mib_tenths(snapshot.done), &mib_tenths(snapshot.total)],
                    )),
                    crate::update::Stage::Checking => Some(ptt_runtime::report_text::fill(
                        text.update_progress_files,
                        &[&snapshot.done.to_string(), &snapshot.total.to_string()],
                    )),
                    crate::update::Stage::Saving => None,
                };
                if let Some(amount) = amount {
                    body = body.child(
                        div()
                            .pt(px(SP_4))
                            .child(mono(amount).text_size(fs(FS_10_5)).text_color(c(TEXT_META))),
                    );
                }
            }
        }

        if let Some(version) = state.new_version_label() {
            body = body.child(kv_row(text.update_new_version, &version));
        }

        let mut actions = div().h_flex().items_center().gap_2().pt(px(SP_4));
        if self.can_check_update() {
            actions = actions.child(
                button(
                    "update-check",
                    LedgerButton::Secondary,
                    text.update_check_now,
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.check_for_update_now(cx);
                    cx.notify();
                })),
            );
        } else if !state.blocks_a_new_check() {
            // 不忙、也没装完,那么按钮消失的唯一原因就是冷却还没过。说出来,
            // 不然一个按钮凭空不见了看着像坏了。跟着 `season_vacuum_blocked`
            // 的做法:不画一个按不动的按钮,画一句为什么。
            actions = actions.child(
                mono(text.update_check_cooldown)
                    .text_size(fs(FS_10_5))
                    .text_color(c(TEXT_DISABLED)),
            );
        }
        if state.offers_an_install() {
            actions = actions.child(
                button(
                    "update-install",
                    LedgerButton::Primary,
                    text.update_install,
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.install_update_now(cx);
                    cx.notify();
                })),
            );
        }
        body = body.child(actions);

        body.child(
            div()
                .pt(px(SP_4))
                .text_size(fs(FS_10_5))
                .text_color(c(TEXT_META))
                .child(if matches!(state, UpdateState::Installed(_)) {
                    text.update_restart_note
                } else {
                    text.update_note
                }),
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
                            (
                                "client-en",
                                ptt_core::ContentLanguage::English,
                                text.client_lang_en,
                            ),
                            (
                                "client-zh",
                                ptt_core::ContentLanguage::TraditionalChinese,
                                text.client_lang_zh_tw,
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
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(90.0))
                                .text_size(fs(FS_12))
                                .text_color(c(TEXT_META))
                                .child(text.theme_label),
                        )
                        .children(
                            [
                                ("theme-dark", ptt_settings::UiTheme::Dark, text.theme_dark),
                                (
                                    "theme-light",
                                    ptt_settings::UiTheme::Light,
                                    text.theme_light,
                                ),
                            ]
                            .map(|(id, theme, label)| {
                                button(
                                    id,
                                    if theme == self.settings.ui_theme {
                                        LedgerButton::Primary
                                    } else {
                                        LedgerButton::Quiet
                                    },
                                    label,
                                    cx,
                                )
                                // 这一行的 `window` 在邻居那里是丢掉的 `_`。
                                // 换肤必须拿到它:见 `set_theme`。
                                .on_click(cx.listener(
                                    move |this, _, window: &mut gpui::Window, cx| {
                                        this.set_theme(theme, window, cx);
                                        cx.notify();
                                    },
                                ))
                            }),
                        ),
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
        self.invalidate_season_info();
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

    /// Switches the interface palette and persists it.
    ///
    /// 换语言只要换一份字符串表,下一帧自然就对了;换配色要走三步,少任何
    /// 一步都是「点了一半生效」:
    ///
    /// 一、`set_palette` 拨我们自己的取色开关,页面代码里那几百处 `c(...)`
    /// 从下一帧起改读新调色板。
    /// 二、`apply_app_theme` 把新色重新抄进 gpui-component 的主题结构体。
    /// 上游组件(输入框、下拉、开关、滚动条、表格)的颜色是启动时拷进去的
    /// 一份**快照**,不重抄就一直是旧色。这里不能用上游的 `Theme::change`:
    /// 它会顺手 `apply_config`,把字体、字号、圆角、阴影连同一百多个颜色
    /// 字段全部推回上游默认值,整套设计系统当场消失。
    /// 三、`window.refresh()` 把整个窗口标脏。`cx.notify()` 顶替不了:它只
    /// 重画外壳,而那二十多个长活的 `Entity<InputState>` 和雷达表各自持有
    /// 渲染,不标脏就还画在上一套皮肤里。
    #[cfg(windows)]
    pub(crate) fn set_theme(
        &mut self,
        theme: ptt_settings::UiTheme,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings.ui_theme == theme {
            return;
        }
        self.settings.ui_theme = theme;
        if let Err(error) = self.settings_store.save(&self.settings) {
            self.push_log(format!("could not save theme: {error}"));
        }
        set_palette(palette_mode_for(theme));
        apply_app_theme(cx);
        window.refresh();
    }

    #[cfg(not(windows))]
    pub(crate) fn settings_panel(&self, _cx: &mut Context<Self>) -> gpui::Div {
        panel().child(panel_header(self.text().panel_settings))
    }
}
