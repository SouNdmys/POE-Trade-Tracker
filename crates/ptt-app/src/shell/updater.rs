//! 自更新在界面这一侧的那一半:什么时候去问、答案存在哪、按钮按下去发生什么。
//!
//! 所有判断都在 [`crate::update`] 里,这里只做三件事:在正确的时刻跑一次、把
//! 结果放进一个用户看得见的状态、把出错的那一句翻译成读得懂的话。
//!
//! ## 为什么状态是一个枚举而不是几个 bool
//!
//! "在查吗""查到了吗""下到一半吗""装完了吗"是同一件事的四个阶段,拆成四个
//! 布尔量就一定会出现"既在下载又已是最新"这种画不出来的组合。一个枚举同一时刻
//! 只可能是一个值,界面照着它分支,画不出自相矛盾的画面。
//!
//! ## 为什么这里不 panic
//!
//! release 档位是 `panic = "abort"`,而这条路上的输入全部来自网络和磁盘。
//! `update` 模块把每一种失败都做成了 [`UpdateError`],这一层的职责就是把它接住
//! 变成一行字,不是让它变成崩溃。

use std::time::Duration;

use gpui::Context;

use crate::i18n::Text;
use crate::shell::AppShell;
use crate::update::{self, Applied, Release, UpdateError};

/// 手动"现在检查"的最短间隔。
///
/// GitHub 的匿名 API 每小时每 IP 只有 60 次。按钮不设间隔的话,一个手快的人
/// 一分钟就能把一小时的额度点光,之后连启动时的那次自动检查都问不出来——
/// 想省事的人反而先失去这个功能。
pub(crate) const MANUAL_CHECK_COOLDOWN: Duration = Duration::from_secs(60);

/// 更新这件事现在走到了哪一步。
#[derive(Debug, Default)]
pub(crate) enum UpdateState {
    /// 还没问过。窗口刚开出来是这个值,`tick` 里踢一次就走。
    #[default]
    Idle,
    /// 正在问 GitHub。
    Checking,
    /// 问过了,当前这份就是最新的。
    UpToDate,
    /// 有一个更新的版本,还没开始下。
    Available(Release),
    /// 正在下载并按包自带的清单核对。
    Downloading(Release),
    /// 核对过了,正在换安装目录里的文件。
    Installing,
    /// 换完了。新版本要等下一次启动才跑得起来。
    Installed(Applied),
    /// 这一轮没走通。原样存着错误本身而不是一句英文,是为了让语言开关切过去
    /// 之后这句话也跟着变。
    Failed(UpdateError),
}

impl UpdateState {
    /// 正在跑一件不能被打断的事。
    fn is_busy(&self) -> bool {
        matches!(
            self,
            Self::Checking | Self::Downloading(_) | Self::Installing
        )
    }

    /// 现在不该再去问 GitHub 了。
    ///
    /// 除了"正在忙",装完也算:此刻跑着的仍然是旧的那个 exe,再问一次得到的
    /// 还是"有新版本",于是按钮邀请用户把刚装好的东西再装一遍。这一步之后
    /// 唯一有意义的动作是重启。
    pub(crate) fn blocks_a_new_check(&self) -> bool {
        self.is_busy() || matches!(self, Self::Installed(_))
    }

    /// 有新版本可装(而且还没开始装)。
    pub(crate) fn offers_an_install(&self) -> bool {
        matches!(self, Self::Available(_))
    }

    /// 装完之后不能再开监视,必须先重启。
    ///
    /// 换文件那一步已经把新的 `onnxruntime.dll` 落到磁盘上了,可此刻跑着的
    /// 还是**旧的** exe。识别库是按需加载的——用户不按「开始监视」它就一直
    /// 没被 `LoadLibrary` 过——所以这一按,等于把新版的原生库塞进旧版的进程
    /// 里。两版之间只要 ort 的 ABI 动过,拿到的就是一次访问违例,而
    /// `panic = "abort"` 下没有第二次机会:整个程序当场消失,用户看到的是
    /// "点了开始监视就闪退",跟更新联系不到一起。
    ///
    /// 只挡"开始",不挡"停止":已经在跑的那次监视是旧库配旧 exe,自洽的。
    pub(crate) fn blocks_a_new_watch(&self) -> bool {
        matches!(self, Self::Installed(_))
    }

    /// 这一轮是坏消息——界面用它决定这行字画成红的。
    pub(crate) fn is_failure(&self) -> bool {
        matches!(self, Self::Failed(_))
    }

    /// 值得强调一下的好消息:有新版本、或者装完了等重启。
    pub(crate) fn is_good_news(&self) -> bool {
        matches!(self, Self::Available(_) | Self::Installed(_))
    }

    /// 手上这个新版本怎么称呼,没有就是 `None`。
    ///
    /// 装完之后仍然给得出一个值,而且换成了 `Applied` 自己记下的版本号:那一行
    /// 不能在换完文件的瞬间消失——"已装好,重启后生效"如果不说装的是哪一版,
    /// 用户下次开起来无从对照。
    pub(crate) fn new_version_label(&self) -> Option<String> {
        match self {
            Self::Available(release) | Self::Downloading(release) => Some(release.tag.clone()),
            Self::Installed(applied) => Some(applied.version.to_string()),
            _ => None,
        }
    }

    /// 这一步在界面上的那句话。
    ///
    /// 每个分支都有话说,包括"还没问过"——一个还没答话的更新检查不该让关于页
    /// 看起来像坏了。
    pub(crate) fn line(&self, text: &Text) -> String {
        match self {
            Self::Idle => text.update_state_idle.to_owned(),
            Self::Checking => text.update_state_checking.to_owned(),
            Self::UpToDate => text.update_state_current.to_owned(),
            Self::Available(_) => text.update_state_available.to_owned(),
            Self::Downloading(_) => text.update_state_downloading.to_owned(),
            Self::Installing => text.update_state_installing.to_owned(),
            Self::Installed(_) => text.update_state_installed.to_owned(),
            Self::Failed(error) => error_message(error, text),
        }
    }
}

/// 一个 [`UpdateError`] 说给人听的样子。
///
/// 按变体分支而不是直接印 `Display`:那边写的是英文,而这个界面是双语的。
/// 技术细节只在它能改变用户下一步动作的时候才带上——"连不上"带上原因(断网、
/// DNS、超时是三件不同的事),"回复看不懂"不带(那串 serde 的报错对读的人
/// 没有任何用)。
pub(crate) fn error_message(error: &UpdateError, text: &Text) -> String {
    match error {
        UpdateError::Unreachable(detail) => {
            format!("{} ({detail})", text.update_error_unreachable)
        }
        UpdateError::Rejected(403) => text.update_error_rate_limited.to_owned(),
        UpdateError::Rejected(404) => text.update_error_no_release.to_owned(),
        UpdateError::Rejected(status) => format!("{} ({status})", text.update_error_rejected),
        UpdateError::MalformedRelease(_) => text.update_error_malformed.to_owned(),
        UpdateError::NoPackage { tag } => format!("{} ({tag})", text.update_error_no_package),
        UpdateError::TooLarge { .. } => text.update_error_too_large.to_owned(),
        UpdateError::Storage { path, .. } => {
            format!("{}: {}", text.update_error_storage, path.display())
        }
        UpdateError::BadArchive(_) => text.update_error_bad_archive.to_owned(),
        UpdateError::Mismatch(_) => text.update_error_mismatch.to_owned(),
        UpdateError::ReadOnlyInstall { directory } => {
            format!("{}: {}", text.update_error_read_only, directory.display())
        }
        // 这一个变体的细节就是它存在的理由:目录现在是半新半旧,不说清哪几个
        // 文件已经是新的,用户没法判断该不该直接重启。
        UpdateError::HalfApplied {
            already_new,
            program_restored,
            ..
        } => {
            let program = if *program_restored {
                text.update_half_program_restored
            } else {
                text.update_half_program_lost
            };
            let mut line = format!("{} — {program}", text.update_error_half_applied);
            if !already_new.is_empty() {
                line.push_str(&format!(
                    " — {}: {}",
                    text.update_half_already_new,
                    already_new.join(", ")
                ));
            }
            line
        }
    }
}

/// 现在允不允许再问一次 GitHub。
///
/// 纯函数拿出来单独放,是因为这两道闸都是"点不出问题"的关键:正在忙的时候
/// 再点一下会开出第二条并行的下载,刚问过就再问会烧掉每小时 60 次的额度。
/// 这两件事都不容易在界面上试出来,但很容易在测试里钉住。
pub(crate) fn may_check_again(state: &UpdateState, since_last: Option<Duration>) -> bool {
    if state.blocks_a_new_check() {
        return false;
    }
    match since_last {
        Some(elapsed) => elapsed >= MANUAL_CHECK_COOLDOWN,
        None => true,
    }
}

impl AppShell {
    /// 一次启动只问一次。`tick` 每 120ms 叫一遍,插销负责让它只响第一次。
    ///
    /// 放在 `tick` 而不是 `new` 里:开窗那一帧不该等一个网络请求,哪怕它是
    /// 扔到后台线程去的——`new` 里多做一件事就多一件可能挡住第一帧的事。
    pub(crate) fn kick_update_check(&mut self, cx: &mut Context<Self>) {
        if self.update_checked {
            return;
        }
        self.update_checked = true;
        self.begin_update_check(cx);
    }

    /// 关于页那个"现在检查"按钮。
    pub(crate) fn check_for_update_now(&mut self, cx: &mut Context<Self>) {
        if !self.can_check_update() {
            return;
        }
        self.begin_update_check(cx);
    }

    /// 按钮现在该不该画出来。
    pub(crate) fn can_check_update(&self) -> bool {
        may_check_again(
            &self.update_state,
            self.last_update_check.map(|at| at.elapsed()),
        )
    }

    /// 去问一次,答案回来时写进 `update_state`。
    ///
    /// 形状照抄 `refresh_report`:输入先取成自己的一份,纯函数扔到后台执行器上
    /// 跑,回来在 `this.update` 里写。代次那一道是必须的——一次慢的检查回来时
    /// 用户可能已经点了第二次,让旧答案盖掉新状态就成了"点了没反应"。
    fn begin_update_check(&mut self, cx: &mut Context<Self>) {
        self.last_update_check = Some(std::time::Instant::now());
        self.update_generation = self.update_generation.wrapping_add(1);
        let generation = self.update_generation;
        self.update_state = UpdateState::Checking;
        cx.notify();

        // 冷却结束时叫一声。
        //
        // 冷却是纯粹随时间走的量,而关于段平时没有任何重画的理由——没有新盘口
        // 就没有脏帧。不在到点时标一次脏,那个"过一分钟再问"会一直挂着,按钮
        // 要等用户去点别的地方才回来,看起来就像它坏了。一次检查一个定时器,
        // 一次重画,比每 120ms 标一次脏便宜得多。
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(MANUAL_CHECK_COOLDOWN).await;
            this.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();

        cx.spawn(async move |this, cx| {
            let answer = cx
                .background_executor()
                .spawn(async move { update::latest_release(env!("CARGO_PKG_VERSION")) })
                .await;
            this.update(cx, |this: &mut AppShell, cx| {
                if this.update_generation != generation {
                    return;
                }
                let text = this.text();
                let line = match &answer {
                    Ok(Some(release)) => format!("{} {}", text.update_badge, release.tag),
                    Ok(None) => text.update_state_current.to_owned(),
                    Err(error) => error_message(error, text),
                };
                this.update_state = match answer {
                    Ok(Some(release)) => UpdateState::Available(release),
                    Ok(None) => UpdateState::UpToDate,
                    Err(error) => UpdateState::Failed(error),
                };
                // 底部那一行是流水灯,只留得住一句话;真正要能回头看的那份在
                // 关于段里。这里发一句是为了让人在别的页面上也瞥得见。
                this.push_log(line);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 下载 + 核对 + 换文件,一个按钮走完。
    ///
    /// 三步之间各回一次界面线程改状态,所以进度是真的在动,而不是按下去之后
    /// 静默几十秒。每一步回来都要再验一次代次:下载可能跑几分钟,期间用户
    /// 完全可能又点了"现在检查"。
    pub(crate) fn install_update_now(&mut self, cx: &mut Context<Self>) {
        let UpdateState::Available(release) = &self.update_state else {
            return;
        };
        let release = release.clone();
        self.update_generation = self.update_generation.wrapping_add(1);
        let generation = self.update_generation;
        self.update_state = UpdateState::Downloading(release.clone());
        cx.notify();

        cx.spawn(async move |this, cx| {
            let staged = cx
                .background_executor()
                .spawn(async move { update::stage(&release) })
                .await;
            let staged = match staged {
                Ok(staged) => staged,
                Err(error) => {
                    Self::report_update_failure(&this, cx, generation, error);
                    return;
                }
            };

            let still_ours = this
                .update(cx, |this: &mut AppShell, cx| {
                    if this.update_generation != generation {
                        return false;
                    }
                    this.update_state = UpdateState::Installing;
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !still_ours {
                return;
            }

            let applied = cx
                .background_executor()
                .spawn(async move { update::apply(&staged) })
                .await;
            match applied {
                Ok(applied) => {
                    this.update(cx, |this: &mut AppShell, cx| {
                        if this.update_generation != generation {
                            return;
                        }
                        let line = this.text().update_state_installed.to_owned();
                        this.update_state = UpdateState::Installed(applied);
                        this.push_log(line);
                        cx.notify();
                    })
                    .ok();
                }
                Err(error) => Self::report_update_failure(&this, cx, generation, error),
            }
        })
        .detach();
    }

    /// 把一次失败写回界面。三处失败出口共用,免得代次那一道在某一处被漏掉。
    fn report_update_failure(
        this: &gpui::WeakEntity<Self>,
        cx: &mut gpui::AsyncApp,
        generation: u64,
        error: UpdateError,
    ) {
        this.update(cx, |this: &mut AppShell, cx| {
            if this.update_generation != generation {
                return;
            }
            let line = error_message(&error, this.text());
            this.update_state = UpdateState::Failed(error);
            this.push_log(line);
            cx.notify();
        })
        .ok();
    }
}

#[cfg(test)]
mod updater_tests {
    use super::{MANUAL_CHECK_COOLDOWN, UpdateState, error_message, may_check_again};
    use crate::i18n::{LANGUAGES, text};
    use crate::update::{Applied, Release, UpdateError};
    use std::path::PathBuf;
    use std::time::Duration;

    fn release() -> Release {
        Release {
            tag: "v0.4.0".to_owned(),
            version: "0.4.0".parse().expect("a literal version parses"),
            html_url: "https://example.invalid/releases/v0.4.0".to_owned(),
            asset_name: "poe-trade-tracker-0.4.0-preview.zip".to_owned(),
            asset_url: "https://example.invalid/download.zip".to_owned(),
            asset_size: 40 * 1024 * 1024,
        }
    }

    /// 每一种错误在两种语言下都得有一句非空的话。
    ///
    /// 漏掉一个分支的后果不是编译失败而是一片空白:面板上的"状态"一行什么都
    /// 没有,读的人只会以为程序卡住了,而不是知道更新失败了。
    #[test]
    fn every_failure_says_something_in_both_languages() {
        let errors = [
            UpdateError::Unreachable("dns".to_owned()),
            UpdateError::Rejected(403),
            UpdateError::Rejected(404),
            UpdateError::Rejected(500),
            UpdateError::MalformedRelease("expected value".to_owned()),
            UpdateError::NoPackage {
                tag: "v0.4.0".to_owned(),
            },
            UpdateError::TooLarge {
                limit_bytes: 96 * 1024 * 1024,
            },
            UpdateError::Storage {
                path: PathBuf::from("C:/x"),
                reason: "denied".to_owned(),
            },
            UpdateError::BadArchive("truncated".to_owned()),
            UpdateError::Mismatch(vec!["ptt-app.exe".to_owned()]),
            UpdateError::ReadOnlyInstall {
                directory: PathBuf::from("C:/Program Files/x"),
            },
            UpdateError::HalfApplied {
                reason: "access denied".to_owned(),
                already_new: vec!["LICENSE.md".to_owned()],
                program_restored: true,
            },
        ];
        for language in LANGUAGES {
            for error in &errors {
                let message = error_message(error, text(language));
                assert!(
                    !message.trim().is_empty(),
                    "{language:?} has nothing to say about {error:?}"
                );
            }
        }
    }

    /// 换到一半这个变体,细节就是它存在的理由。
    ///
    /// 目录此刻半新半旧,用户要决定的是"能不能直接重启"。不点名哪几个文件
    /// 已经换过,这句话就等于什么都没说。
    #[test]
    fn a_half_applied_update_names_the_files_that_are_already_new() {
        let error = UpdateError::HalfApplied {
            reason: "access denied".to_owned(),
            already_new: vec!["onnxruntime.dll".to_owned(), "LICENSE.md".to_owned()],
            program_restored: false,
        };
        for language in LANGUAGES {
            let message = error_message(&error, text(language));
            assert!(
                message.contains("onnxruntime.dll") && message.contains("LICENSE.md"),
                "{language:?} hides which files are already new: {message}"
            );
        }
    }

    /// 每一步都要有话说,尤其是"还没问过"。
    ///
    /// 关于页在检查回来之前就画出来了。这一步没有句子的话,那一行是空的,
    /// 看起来和"读取失败"一模一样。
    #[test]
    fn every_step_has_a_sentence_including_the_one_before_the_first_answer() {
        let states = [
            UpdateState::Idle,
            UpdateState::Checking,
            UpdateState::UpToDate,
            UpdateState::Available(release()),
            UpdateState::Downloading(release()),
            UpdateState::Installing,
            UpdateState::Installed(Applied {
                install_dir: PathBuf::from("C:/x"),
                version: "0.4.0".parse().expect("a literal version parses"),
                replaced: vec!["ptt-app.exe".to_owned()],
                left_behind: vec!["ptt-app.exe.old".to_owned()],
            }),
            UpdateState::Failed(UpdateError::Rejected(403)),
        ];
        for language in LANGUAGES {
            for state in &states {
                assert!(
                    !state.line(text(language)).trim().is_empty(),
                    "{language:?} draws a blank line for {state:?}"
                );
            }
        }
    }

    /// "新版本是哪一个"这一行不能在换完文件的瞬间消失。
    ///
    /// 装完之后手上已经没有 `Release` 了(那份在下载阶段就被消耗掉),版本号
    /// 只剩 `Applied` 记着的那个。不接这一路,面板会在最需要说清楚的那一刻
    /// 装完之后「开始监视」必须被拦住,而且只拦这一个方向。
    ///
    /// 拦的是一次闪退:换文件那步已经把新的 `onnxruntime.dll` 放好了,而此刻
    /// 跑着的是旧 exe;识别库按需加载,所以只有按下「开始监视」才会把新库
    /// 塞进旧进程。ort 的 ABI 一变就是访问违例,`panic = "abort"` 下当场
    /// 消失——用户只会觉得"一点开始就闪退",想不到是更新。
    #[test]
    fn a_finished_install_stops_a_new_watch_and_nothing_else() {
        let installed = UpdateState::Installed(Applied {
            install_dir: PathBuf::from("C:/x"),
            version: "0.4.0".parse().expect("a literal version parses"),
            replaced: vec!["ptt-app.exe".to_owned(), "onnxruntime.dll".to_owned()],
            left_behind: Vec::new(),
        });
        assert!(installed.blocks_a_new_watch());

        // 其余每一种状态都不该挡:检查中、有新版没装、失败了,跑着的都还是
        // 完整的一套旧文件。
        for state in [
            UpdateState::Idle,
            UpdateState::Checking,
            UpdateState::UpToDate,
            UpdateState::Available(release()),
            UpdateState::Downloading(release()),
            UpdateState::Installing,
            UpdateState::Failed(UpdateError::Unreachable("dns".to_owned())),
        ] {
            assert!(
                !state.blocks_a_new_watch(),
                "{state:?} still runs a matched set of files, so watching is fine"
            );
        }
    }

    /// 只剩一句"已装好",装的是哪一版没人知道。
    #[test]
    fn the_new_version_is_still_named_after_it_is_installed() {
        let installed = UpdateState::Installed(Applied {
            install_dir: PathBuf::from("C:/x"),
            version: "0.4.0".parse().expect("a literal version parses"),
            replaced: vec!["ptt-app.exe".to_owned()],
            left_behind: Vec::new(),
        });
        assert_eq!(installed.new_version_label().as_deref(), Some("0.4.0"));
        assert_eq!(
            UpdateState::Available(release())
                .new_version_label()
                .as_deref(),
            Some("v0.4.0"),
            "before the install the release's own tag is the honest label"
        );
        assert_eq!(UpdateState::UpToDate.new_version_label(), None);
    }

    /// 正在忙的时候不许再问。
    ///
    /// 少了这道闸,点两下"现在检查"就会有两条请求在跑,两个答案抢着写同一个
    /// 状态;点两下"下载并安装"更糟——两条下载写同一个 pending-update.zip。
    #[test]
    fn a_check_in_flight_blocks_another_one() {
        for state in [
            UpdateState::Checking,
            UpdateState::Downloading(release()),
            UpdateState::Installing,
        ] {
            assert!(
                !may_check_again(&state, None),
                "{state:?} still lets another check start"
            );
        }
    }

    /// 装完之后也不许再问。
    ///
    /// 此刻跑着的还是旧 exe,再问一次得到的还是"有新版本",按钮就在邀请用户
    /// 把刚装好的再装一遍。这一步之后唯一有意义的动作是重启。
    #[test]
    fn an_installed_update_stops_offering_to_check_again() {
        let state = UpdateState::Installed(Applied {
            install_dir: PathBuf::from("C:/x"),
            version: "0.4.0".parse().expect("a literal version parses"),
            replaced: Vec::new(),
            left_behind: Vec::new(),
        });
        assert!(!may_check_again(&state, Some(Duration::from_secs(3600))));
    }

    /// 冷却没到就不许再问。GitHub 的匿名额度是每小时 60 次,按钮不设间隔的话
    /// 一分钟就能点光。
    #[test]
    fn the_button_waits_out_its_cooldown() {
        let idle = UpdateState::UpToDate;
        assert!(
            may_check_again(&idle, None),
            "the very first check must not be held back"
        );
        assert!(!may_check_again(
            &idle,
            Some(MANUAL_CHECK_COOLDOWN - Duration::from_secs(1))
        ));
        assert!(may_check_again(&idle, Some(MANUAL_CHECK_COOLDOWN)));
    }

    /// 失败之后必须还能再试一次——冷却过了就行。断网时启动的那一次注定失败,
    /// 如果失败是终局,接上网线也没有别的办法让它再问一次。
    #[test]
    fn a_failure_can_be_retried_once_the_cooldown_passes() {
        let failed = UpdateState::Failed(UpdateError::Unreachable("offline".to_owned()));
        assert!(!may_check_again(&failed, Some(Duration::from_secs(0))));
        assert!(may_check_again(&failed, Some(MANUAL_CHECK_COOLDOWN)));
    }
}
