//! Root view: status strip + monitor content (last book, opportunities,
//! skip histogram). P3 skeleton — layout only, visuals iterate later.

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::time::Duration;

use gpui::{Context, FocusHandle, IntoElement, ParentElement, Render, Styled, Window, div, px};

use crate::theme::*;
use crate::ui::{
    LedgerButton, StatusKind, button, hairline_soft, mono, panel, panel_header, spaced, status_dot,
};

#[cfg(windows)]
use crate::backend::{Backend, UiEvent};

const LOG_CAPACITY: usize = 120;

pub struct AppShell {
    pub focus_handle: FocusHandle,
    #[cfg(windows)]
    backend: Option<Backend>,
    watching: bool,
    accepted: u64,
    skips: BTreeMap<String, u64>,
    last_header: Option<String>,
    last_rows: Vec<String>,
    last_analysis: Vec<String>,
    log: VecDeque<String>,
    fault: Option<String>,
}

impl AppShell {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(120))
                    .await;
                if this
                    .update(cx, |this: &mut AppShell, cx| this.tick(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        Self {
            focus_handle: cx.focus_handle(),
            #[cfg(windows)]
            backend: None,
            watching: false,
            accepted: 0,
            skips: BTreeMap::new(),
            last_header: None,
            last_rows: Vec::new(),
            last_analysis: Vec::new(),
            log: VecDeque::new(),
            fault: None,
        }
    }

    fn push_log(&mut self, line: String) {
        if self.log.len() >= LOG_CAPACITY {
            self.log.pop_front();
        }
        self.log.push_back(line);
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        #[cfg(windows)]
        {
            let events: Vec<UiEvent> = self
                .backend
                .as_ref()
                .map(|backend| backend.drain_events())
                .unwrap_or_default();
            if events.is_empty() {
                return;
            }
            for event in events {
                match event {
                    UiEvent::Accepted {
                        header,
                        rows,
                        analysis,
                    } => {
                        self.accepted += 1;
                        self.push_log(header.clone());
                        self.last_header = Some(header);
                        self.last_rows = rows;
                        self.last_analysis = analysis;
                    }
                    UiEvent::Skipped(reason) => {
                        *self.skips.entry(reason).or_default() += 1;
                    }
                    UiEvent::Fault(message) => {
                        self.fault = Some(message);
                        self.watching = false;
                    }
                    UiEvent::Stopped => {
                        self.watching = false;
                    }
                }
            }
            cx.notify();
        }
        #[cfg(not(windows))]
        {
            let _ = cx;
        }
    }

    fn toggle_watch(&mut self, cx: &mut Context<Self>) {
        #[cfg(windows)]
        {
            if self.watching {
                if let Some(mut backend) = self.backend.take() {
                    backend.stop();
                }
                self.watching = false;
            } else {
                self.fault = None;
                self.backend = Some(Backend::start());
                self.watching = true;
            }
            cx.notify();
        }
        #[cfg(not(windows))]
        {
            let _ = cx;
        }
    }
}

impl Render for AppShell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (dot_kind, state_label) = if self.fault.is_some() {
            (StatusKind::Error, "FAULT")
        } else if self.watching {
            (StatusKind::Monitoring, "WATCHING")
        } else {
            (StatusKind::Idle, "IDLE")
        };
        let skip_total: u64 = self.skips.values().sum();
        let button_label = if self.watching { "Stop" } else { "Start watch" };
        let button_kind = if self.watching {
            LedgerButton::Secondary
        } else {
            LedgerButton::Primary
        };

        let mut skip_lines: Vec<String> = self
            .skips
            .iter()
            .map(|(reason, count)| format!("{count:>5}  {reason}"))
            .collect();
        skip_lines.truncate(10);

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(c(CANVAS))
            .text_color(c(TEXT_PRIMARY))
            .font_family(FONT_UI)
            .child(
                // Status strip.
                div()
                    .h(px(40.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .bg(c(RAIL))
                    .border_b_1()
                    .border_color(c(HAIRLINE_STRONG))
                    .child(status_dot(dot_kind))
                    .child(
                        div()
                            .text_size(fs(FS_12_5))
                            .child(spaced("POE TRADE TRACKER")),
                    )
                    .child(
                        mono(format!(
                            "{state_label}   accepted {}   skips {}",
                            self.accepted, skip_total
                        ))
                        .text_color(c(TEXT_META)),
                    )
                    .child(div().flex_grow())
                    .child(
                        button("watch-toggle", button_kind, button_label, cx)
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_watch(cx))),
                    ),
            )
            .child(
                // Body: three panels.
                div()
                    .flex_grow()
                    .flex()
                    .gap_3()
                    .p_3()
                    .child(
                        panel().flex_grow().child(panel_header("LAST BOOK")).child(
                            div().p_3().flex().flex_col().gap_1().children(
                                std::iter::once(
                                    self.last_header
                                        .clone()
                                        .unwrap_or_else(|| "waiting for a book…".to_owned()),
                                )
                                .chain(self.last_rows.iter().cloned())
                                .map(|line| mono(line).text_size(fs(FS_12))),
                            ),
                        ),
                    )
                    .child(
                        div()
                            .flex_grow()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(panel().child(panel_header("OPPORTUNITIES")).child(
                                div().p_3().flex().flex_col().gap_1().children(
                                    if self.last_analysis.is_empty() {
                                        vec![mono("—").text_size(fs(FS_12))]
                                    } else {
                                        self.last_analysis
                                            .iter()
                                            .map(|line| mono(line.clone()).text_size(fs(FS_12)))
                                            .collect()
                                    },
                                ),
                            ))
                            .child(panel().child(panel_header("SKIPS")).child(
                                div().p_3().flex().flex_col().gap_1().children(
                                    if skip_lines.is_empty() {
                                        vec![mono("—").text_size(fs(FS_12))]
                                    } else {
                                        skip_lines
                                            .into_iter()
                                            .map(|line| {
                                                mono(line)
                                                    .text_size(fs(FS_12))
                                                    .text_color(c(TEXT_META))
                                            })
                                            .collect()
                                    },
                                ),
                            )),
                    ),
            )
            .child(
                // Footer: fault or recent log line.
                div()
                    .h(px(24.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .px_4()
                    .bg(c(RAIL))
                    .border_t_1()
                    .border_color(c(HAIRLINE))
                    .child(match &self.fault {
                        Some(fault) => mono(format!("fault: {fault}"))
                            .text_size(fs(FS_11_5))
                            .text_color(c(DANGER)),
                        None => mono(self.log.back().cloned().unwrap_or_default())
                            .text_size(fs(FS_11_5))
                            .text_color(c(TEXT_META)),
                    })
                    .child(hairline_soft()),
            )
    }
}
