# POE Trade Tracker

单人自用的 POE2 交易追踪工具（Rust + GPUI 桌面应用，Windows 专用）。
不是给别人用的库：**公共 API 稳定性不需要考虑**，改 `pub fn` 签名不用顾虑下游，
仓内改干净即可。

## crate 地图

由底向上：

- `ptt-trade-domain` — 市场身份、比率、报价与报价边。最底层类型，别的都建在它上面
- `ptt-trade-engine` — 精确多档成交、有界转换、环路分析
- `ptt-market-book` — 从观测里选出一份自洽的"当前书"
- `ptt-strategy` — 执行安全、路线记账、挂单策略、市场政策、估值与价格历史、市场脉搏
- `ptt-workflows` — 关注组、覆盖缺口、探针队列、机会雷达
- `ptt-storage` — SQLite 持久化
- `ptt-settings` — 版本化、抗崩溃的 JSON 设置
- `ptt-catalog` — 每游戏的封闭资产目录与 OCR 词表
- `ptt-core` — 词缀匹配与复合规则引擎
- `ptt-vision` — 桌面抓取与蓝字视觉热路径
- `ptt-ocr-onnx` — 离线 PP-OCRv5 识别后端
- `ptt-ocr-win` — Windows.Media.Ocr 适配层
- `ptt-recognition` — 按 profile 的订单书字段识别路由
- `ptt-platform-win` — 隔离的 Win32 平台服务
- `ptt-monitoring` — 自动监视循环：指纹门控、稳定性、双读确认、去重
- `ptt-runtime` — 页面模型与报表（`reports.rs`）、采集管道、日 rollup，以及 `src/bin/*_probe.rs` 验证探针
- `ptt-app` — GPUI 桌面应用；`shell/mod.rs` 是页面数据的装配层

## 常用命令

```
cargo test --workspace
cargo clippy --workspace --all-targets
cargo check --workspace --all-targets
cargo test -p ptt-runtime --lib <测试名>
```

两条基准线都必须 exit 0。**仓库没有 `[lints]` 配置**，所以 plain clippy 就是标准，
零 warning 是要求，不需要加 `-D warnings`。

环境是 Windows + PowerShell：过滤输出用 `Select-String`、`Select-Object -Last N`，
不是 `grep`/`tail`。

## 不成文约定

- **`*_model` 与 `*_report` 成对**：`watchlist_model`/`opportunities_model`/
  `probe_queue_model` 把 `pulse: Option<&ptt_strategy::MarketPulse>` 放在**最后一个**
  参数；对应的文本包装函数 `watchlist_report`/`opportunities_report`/`probe_queue`
  **不带这个参数**，内部传 `None`。加新页面时照这个形状来
- **`ProbePriority` 的序是反直觉的**：声明顺序 `High, Medium, Low` 且 derive `Ord`，
  所以 **High 是最小值**。升序 = 最紧急在前。写排序前先想清楚方向
- 测试放在同文件底部的 `#[cfg(test)] mod xxx_tests`，每个 mod 自带局部 helper
  （`asset()`、`pulse_asset()`、`book_edge()`），不跨 mod 复用
- Windows 专属代码一律 `#[cfg(windows)]` 门控
- 界面文本全部走 `report_text::` 的 `pick(language, 英文, 中文)`，不要在业务代码里
  内联中文字符串
- `src/bin/*_probe.rs` 是可运行的验证探针，镜像生产路径。**它们会和生产代码漂移**
  （P10 的今日折叠 bug 就是这么来的），改生产路径时顺手对照一下
- 文档注释写"为什么"，不写"这行做了什么"，通常带一句能记住的理由

## 和我协作的方式

- **我在学 Rust，读不太懂代码。** 改动要用人话解释，把我当成不懂编程术语的普通人：
  说清这段在干什么、为什么这么改、会有什么后果。不要只丢一段代码或一个 diff 让我自己看
- **先给最小可用的写法**，防御层后加。个人单用户工具，`panic` 和 `.unwrap()` 可以接受，
  不要一上来就铺错误处理
- **修 bug 必须先写出会 FAIL 的测试**，让我看到它红，再动代码。
  如果顺序反了（先改后写测试），就故意把功能弄坏一次、确认测试会红、再恢复 ——
  没见过红的测试不算数
- 一次只做被要求的事，不要顺手重构
- **一次只做一件事，做完提交再做下一件。** 不要把多个改动混进一个 commit：
  混在一起出了问题，就分不清是哪个改动弄坏的
- **每次提交前先跑 `cargo fmt --all`。** 格式漂移不该占掉 review 的名额：
  上一轮 ultrareview 的两条 finding 全是缩进，真正的 bug 一条都没有

## docs/

内容不要抄进本文件，需要时去读：

- `CORE-TRADING-MODEL.md` — 面板语义、三个档位、主工作流、流动性模型，以及各阶段
  （P7–P10）的算法附记。**所有设计理由都在这里，P10 的也在这份里**。要写新的设计
  理由就追加到这份文档，不要另起一份平行的
- `P1-CALIBRATION-NOTES.md` — POE2 zh-TW @2560×1440 的 OCR 标定：文本极性、几何、
  LayoutProfile 常量、弹窗位置形态
- `P10-FOLLOWUPS.md` — 当前未修项与待讨论项清单
