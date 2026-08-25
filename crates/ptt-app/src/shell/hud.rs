//! The overlay card: the same book, readable without leaving the game.

#[cfg(windows)]
use super::HUD_PLACEMENT_BAR;
use super::{AppShell, HUD_ORIGIN, HUD_PROBE_STRIP, HUD_SIZE_EXPANDED, HUD_SIZE_MINI, skip_label};

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

        let covers = Target::ALL
            .into_iter()
            .any(|target| self.calibrated_region(target).intersects(bounds));
        if covers {
            ptt_platform_win::CaptureAffinity::Exclude
        } else {
            ptt_platform_win::CaptureAffinity::Include
        }
    }

    /// 一个校准区域的屏幕矩形:有存的用存的(那才是监视器真正抓的),
    /// 没存的用预设。
    #[cfg(windows)]
    fn calibrated_region(&self, target: crate::calibrate::Target) -> ptt_platform_win::RectI {
        use crate::calibrate::Target;

        let profile = self.settings.active_profile;
        let (layout, language) = ptt_runtime::pipeline::route_for(profile);
        let (x, y, width, height) = match target {
            Target::Need => layout.need_name,
            Target::Have => layout.have_name,
            Target::Tables => layout.tables_for(language),
        };
        let (x, y, width, height) = self
            .saved_rect(profile, target)
            .map_or((x, y, width, height), |rect| {
                (rect.x, rect.y, rect.width, rect.height)
            });
        ptt_platform_win::RectI::new(
            x,
            y,
            i32::try_from(width).unwrap_or(i32::MAX).max(1),
            i32::try_from(height).unwrap_or(i32::MAX).max(1),
        )
        .expect("calibrated regions have positive dimensions")
    }

    /// The card's current size:档位来自设置,待抓条空了矮 20px(§4),
    /// 摆放模式顶上多 22px 顶条。
    #[cfg(windows)]
    pub(crate) fn hud_size(&self) -> (i32, i32) {
        let (width, height) = match self.settings.hud.tier {
            ptt_settings::HudTier::Mini => HUD_SIZE_MINI,
            ptt_settings::HudTier::Expanded => HUD_SIZE_EXPANDED,
        };
        let height = if self.hud_probe_line().is_some() {
            height
        } else {
            height - HUD_PROBE_STRIP
        };
        let height = if self.hud_placement {
            height + HUD_PLACEMENT_BAR
        } else {
            height
        };
        (width, height)
    }

    /// 设置里的摆放,翻成平台类型(千分位 → 0..1 比例)。
    #[cfg(windows)]
    fn hud_placement_setting(&self) -> ptt_platform_win::HudPlacement {
        match self.settings.hud.placement {
            ptt_settings::HudPlacementSetting::Automatic => {
                ptt_platform_win::HudPlacement::Automatic
            }
            ptt_settings::HudPlacementSetting::Manual {
                relative_x_permille,
                relative_y_permille,
            } => ptt_platform_win::HudPlacement::manual(
                f64::from(relative_x_permille.min(1000)) / 1000.0,
                f64::from(relative_y_permille.min(1000)) / 1000.0,
            )
            .unwrap_or(ptt_platform_win::HudPlacement::Automatic),
        }
    }

    /// Where the card goes right now.
    ///
    /// 自动模式避开「交易表格」区域(读回自己的影子最伤);手动模式按存的
    /// 比例换算。窗口还没建、或查不到工作区时退回固定原点。
    #[cfg(windows)]
    fn hud_bounds(&self, size: (i32, i32)) -> Option<ptt_platform_win::RectI> {
        use ptt_platform_win::{RectI, SizeI, resolve_hud_position};

        let fallback = RectI::new(HUD_ORIGIN.0, HUD_ORIGIN.1, size.0, size.1);
        let Some(work) = self.hud.as_ref().and_then(|hud| hud.work_area()) else {
            return fallback;
        };
        let Some(hud_size) = SizeI::new(size.0, size.1) else {
            return fallback;
        };
        let anchor = self.calibrated_region(crate::calibrate::Target::Tables);
        let origin =
            resolve_hud_position(work, hud_size, self.hud_placement_setting(), Some(anchor));
        RectI::new(origin.x, origin.y, size.0, size.1).or(fallback)
    }

    /// The most urgent pair to go capture, and how many more are folded away.
    ///
    /// 数据源就是关注列表 / 监视器那份「待采集队列」:先手动排队的,再
    /// 建议的(升序第一条最紧——`ProbePriority` 声明 High 在前,High 是
    /// 最小值,模型已按它排好)。
    #[cfg(windows)]
    fn hud_probe_line(&self) -> Option<(String, usize)> {
        let text = self.text();
        let language = self.language();
        if let Some(entry) = self.probe_queue.entries().first() {
            let total = self.probe_queue.entries().len();
            return Some((
                format!(
                    "{}  {}  {}",
                    text.hud_probe_label,
                    self.pair_label(&entry.from_asset_id, &entry.to_asset_id),
                    entry.reason
                ),
                total.saturating_sub(1),
            ));
        }
        // 独立缓存,不借当前页面的报表:主窗口停在哪一页不该决定浮窗
        // 有没有待抓条。
        if let Some(model) = self.hud_probes.as_ref()
            && let Some(candidate) = model.candidates.first()
        {
            return Some((
                format!(
                    "{}  {}  {}",
                    text.hud_probe_label,
                    self.pair_label(
                        candidate.from_asset_id.as_str(),
                        candidate.to_asset_id.as_str()
                    ),
                    ptt_runtime::report_text::probe_reason(language, candidate.reason)
                ),
                model.candidates.len().saturating_sub(1),
            ));
        }
        None
    }

    /// 进出摆放模式(§4):进 = 拿掉点击穿透、卡顶出 22px 顶条、外框变金;
    /// 出 = 写回设置、回到点击穿透。
    #[cfg(windows)]
    pub(crate) fn set_hud_placement(&mut self, on: bool) {
        use ptt_platform_win::HudInteractionMode;

        if self.hud_placement == on {
            return;
        }
        // 摆放的前提是浮窗在屏上,否则没东西可拖。
        if on && !self.hud_visible {
            self.toggle_hud();
            if !self.hud_visible {
                return;
            }
        }
        self.hud_placement = on;
        let mode = if on {
            HudInteractionMode::Placement
        } else {
            HudInteractionMode::Passive
        };
        if let Some(hud) = self.hud.as_mut()
            && let Err(error) = hud.set_interaction_mode(mode)
        {
            self.push_log(format!("HUD placement mode failed: {error}"));
        }
        if !on {
            // 「完成」的落盘。拖动本身已边拖边存,这里兜底一次,防止
            // 最后一次拖动与退出之间断电丢位置。
            if let Err(error) = self.settings_store.save(&self.settings) {
                self.push_log(format!("settings save failed: {error}"));
            }
        }
        self.refresh_hud();
    }

    /// 取走 wndproc 留的言:拖动落点存成相对比例,顶条按钮点击照做。
    #[cfg(windows)]
    pub(crate) fn poll_hud_placement(&mut self, cx: &mut gpui::Context<Self>) {
        use ptt_platform_win::HudCommand;

        if self.hud.is_none() {
            return;
        }
        self.absorb_hud_drag();
        let command = self.hud.as_mut().and_then(|hud| hud.take_user_command());
        let Some(command) = command else {
            return;
        };
        match command {
            HudCommand::PlacementDone => self.set_hud_placement(false),
            HudCommand::ToggleTier => {
                let next = match self.settings.hud.tier {
                    ptt_settings::HudTier::Mini => ptt_settings::HudTier::Expanded,
                    ptt_settings::HudTier::Expanded => ptt_settings::HudTier::Mini,
                };
                self.set_hud_tier(next);
            }
            HudCommand::OpacityDown => self.bump_hud_opacity(-5),
            HudCommand::OpacityUp => self.bump_hud_opacity(5),
        }
        self.refresh_hud();
        // 设置页上的摆放按钮/数值要跟着变。
        cx.notify();
    }

    /// 拖完的位置 → 相对比例(千分位)→ 设置,立即落盘。
    ///
    /// 存比例不存像素:换分辨率/换显示器不会飞到屏幕外(§4)。
    #[cfg(windows)]
    fn absorb_hud_drag(&mut self) {
        if !self.hud_placement {
            return;
        }
        let Some(point) = self.hud.as_mut().and_then(|hud| hud.take_user_move()) else {
            return;
        };
        let Some(work) = self.hud.as_ref().and_then(|hud| hud.work_area()) else {
            return;
        };
        let size = self.hud_size();
        let permille = |position: i32, base: i32, free: i32| -> u32 {
            let ratio = f64::from(position - base) / f64::from(free.max(1));
            (ratio.clamp(0.0, 1.0) * 1000.0).round() as u32
        };
        self.settings.hud.placement = ptt_settings::HudPlacementSetting::Manual {
            relative_x_permille: permille(point.x, work.x, work.width - size.0),
            relative_y_permille: permille(point.y, work.y, work.height - size.1),
        };
        if let Err(error) = self.settings_store.save(&self.settings) {
            self.push_log(format!("settings save failed: {error}"));
        }
    }

    /// Switches the card between mini and expanded, live.
    #[cfg(windows)]
    pub(crate) fn set_hud_tier(&mut self, tier: ptt_settings::HudTier) {
        if self.settings.hud.tier == tier {
            return;
        }
        self.settings.hud.tier = tier;
        if let Err(error) = self.settings_store.save(&self.settings) {
            self.push_log(format!("settings save failed: {error}"));
        }
        self.refresh_hud();
    }

    /// Steps the opacity by ±5%, clamped to [60, 100] — 下限 60 是因为再低
    /// 10px 的灰字就糊了(`3b`)。`LWA_ALPHA` 改完立即生效。
    #[cfg(windows)]
    pub(crate) fn bump_hud_opacity(&mut self, step: i16) {
        let current = i16::from(self.settings.hud.clamped_opacity());
        let next = u8::try_from((current + step).clamp(60, 100)).unwrap_or(85);
        if next == self.settings.hud.clamped_opacity() {
            return;
        }
        self.settings.hud.opacity_percent = next;
        if let Err(error) = self.settings_store.save(&self.settings) {
            self.push_log(format!("settings save failed: {error}"));
        }
        if let Some(hud) = self.hud.as_mut() {
            let _ = hud.set_opacity(self.settings.hud.alpha());
        }
    }

    pub(crate) fn toggle_hud(&mut self) {
        use ptt_platform_win::{
            HudInteractionMode, HudWindow, HudWindowConfig, HudWindowPolicy, RectI,
        };

        if self.hud.is_none() {
            let size = self.hud_size();
            let Some(bounds) = RectI::new(HUD_ORIGIN.0, HUD_ORIGIN.1, size.0, size.1) else {
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
                Ok(mut hud) => {
                    // 不透明度是设置项(60–100%,步长 5),改完立即生效。
                    let _ = hud.set_opacity(self.settings.hud.alpha());
                    self.hud = Some(hud);
                    // 创建时查不到所在显示器的工作区,位置只能建完再解析,
                    // 趁窗口还没显示挪过去,免得在角落闪一下。
                    if let Some(resolved) = self.hud_bounds(size)
                        && let Some(hud) = self.hud.as_mut()
                    {
                        let _ = hud.set_bounds(resolved);
                    }
                }
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
        use ptt_platform_win::{HudContent, HudQuoteRow, HudTone};

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
        //
        // Both halves come from the last accepted book. Not `report_pair`:
        // that follows the convert page's pickers, so it would title the panel
        // in front of the user with whichever currency they last asked the
        // report about.
        let pair = self.last_book_pair();

        // 结论一句人话:红「不可用」/ 黄「跳过 · 原因」/ 绿「N 行全部读到」。
        // Stated either way — "nothing since the last book" and "the watcher
        // died" look identical on a card that only reports success.
        let (tone, verdict) = match (&self.fault, &self.last_skip) {
            (Some(fault), _) => (HudTone::Err, format!("{}: {fault}", text.fault_prefix)),
            (None, Some(reason)) => (
                HudTone::Warn,
                format!(
                    "{} · {}",
                    text.skips_label,
                    skip_label(reason, self.settings.ui_language)
                ),
            ),
            (None, None) => match &self.last_book {
                Some(book) => (
                    HudTone::Ok,
                    ptt_runtime::report_text::fill(
                        text.hud_rows_ok,
                        &[&book.order_rows.len().to_string()],
                    ),
                ),
                None => (HudTone::Ok, text.waiting_for_book.to_owned()),
            },
        };
        // 跳过时数字不抹掉(你可能正需要它),但整体降一档灰,不许它装成
        // 刚读到的。
        let dimmed = self.fault.is_some() || self.last_skip.is_some();

        let quote_row = |row: &ptt_runtime::pipeline::BookRow| HudQuoteRow {
            index: row.row_index.to_string(),
            rate: row.rate.clone(),
            stock: row.stock.to_string(),
            aggregate: row.aggregate,
        };
        let side_rows = |side: &str| -> Vec<HudQuoteRow> {
            self.last_book
                .as_ref()
                .map(|book| {
                    book.order_rows
                        .iter()
                        .filter(|row| row.side == side)
                        .take(6)
                        .map(quote_row)
                        .collect()
                })
                .unwrap_or_default()
        };

        let skip_total: u64 = self.skips.values().sum();
        let verdict_meta = self.last_book.as_ref().map_or_else(String::new, |book| {
            ptt_runtime::report_text::fill(
                text.hud_meta,
                &[
                    &book.elapsed_ms.to_string(),
                    &self.accepted.to_string(),
                    &skip_total.to_string(),
                ],
            )
        });
        let probe = self.hud_probe_line();
        let content = HudContent {
            monitoring: self.watching && self.fault.is_none(),
            mini: self.settings.hud.tier == ptt_settings::HudTier::Mini,
            status_text: status.to_owned(),
            pair_text: pair,
            sequence_text: self
                .last_book
                .as_ref()
                .map_or_else(String::new, |book| format!("#{}", book.sequence)),
            tone,
            verdict_text: verdict,
            verdict_meta,
            dimmed,
            dimmed_note: self.last_book.as_ref().map_or_else(String::new, |book| {
                ptt_runtime::report_text::fill(
                    text.hud_ago,
                    &[&book.received_at.elapsed().as_secs().to_string()],
                )
            }),
            column_titles: (
                text.monitor_col_available.to_owned(),
                text.monitor_col_competing.to_owned(),
            ),
            header_titles: (
                text.monitor_col_rate.to_owned(),
                text.monitor_col_stock.to_owned(),
            ),
            available: side_rows("available"),
            competing: side_rows("competing"),
            probe_text: probe
                .as_ref()
                .map_or_else(String::new, |(line, _)| line.clone()),
            probe_more: probe
                .filter(|(_, more)| *more > 0)
                .map_or_else(String::new, |(_, more)| format!("+{more}")),
            placement: self
                .hud_placement
                .then(|| ptt_platform_win::HudPlacementBar {
                    hint: text.hud_place_hint.to_owned(),
                    opacity_text: format!("{}%", self.settings.hud.clamped_opacity()),
                    // 按钮写要切去的那一档,不是现在这档:按钮是动作,不是状态。
                    tier_label: match self.settings.hud.tier {
                        ptt_settings::HudTier::Mini => text.hud_tier_expanded,
                        ptt_settings::HudTier::Expanded => text.hud_tier_mini,
                    }
                    .to_owned(),
                    done_label: text.hud_place_done.to_owned(),
                }),
        };
        // 位置解析前先吸收未取走的拖动,否则一次事件驱动的刷新会把刚拖好
        // 的卡片弹回旧位置。
        self.absorb_hud_drag();
        // 待抓条随内容出现/消失,窗口高度跟着变(§4:队列空了整条消失,
        // 浮窗自动矮 20px);摆放模式顶上多 22px。
        let size = self.hud_size();
        let bounds = self.hud_bounds(size);
        if let Some(hud) = self.hud.as_mut() {
            if let Some(bounds) = bounds {
                let _ = hud.set_bounds(bounds);
            }
            if let Err(error) = hud.set_content(content) {
                self.push_log(format!("HUD update failed: {error}"));
            }
        }
    }

    #[cfg(not(windows))]
    pub(crate) fn toggle_hud(&mut self) {
        self.hud_visible = !self.hud_visible;
    }

    #[cfg(not(windows))]
    pub(crate) fn refresh_hud(&mut self) {}
}
