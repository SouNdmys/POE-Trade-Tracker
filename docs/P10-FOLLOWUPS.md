# P10 遗留清单（2026-08-22）

云端评审（main → p10-base）之后剩下的东西，给下一个接手的会话。
probe boost 的两条（不重排、Monitor 未接 pulse）已修，见 `0039c20`、`95e5a63`。

## 1. 今日折叠只读单个 context_key（未修，bug）

`load_window`（`crates/ptt-app/src/shell/mod.rs:1224`）只取 `live_context(...).stable_key()`
这一个 context 的观测。而 `game_ui_build` 是 stable key 材料的一部分
（`crates/ptt-trade-domain/src/lib.rs:259`），客户端版本一变 key 就轮换。

后果：升级当天，上午的抓取落在旧 key 下，被 `load_pulse` →
`analytics_model` → `today_stats` 的今日折叠排除在外。Analytics 页面的"今日"
列、以及 Convert/Opportunities/Watchlist 上读同一份 pulse 的结构注记，都会
少算，直到下一个 UTC 午夜的 rollup 把两个 context 合起来自愈
（`ensure_daily_rollups` 按构造排除今天，所以只有今日折叠有这个洞）。

影响有界：不丢数据，旧 key 的行都在库里，第二天照常出现。发布日对正在
交易的人是几小时的降级建议，其余时间不可见。

**正确写法已经存在**：`crates/ptt-runtime/src/bin/analytics_probe.rs:80-94`
遍历 `rollup::game_context_keys(&store, game)`，用 `load_observations_between`
把今天所有 context 的观测并起来，注释写着"a release today must not hide the
morning"。生产路径的 `load_pulse`（`crates/ptt-app/src/shell/mod.rs:1293`，
就在 `load_window` 下面）没有这段迭代，两边漂了。修的时候镜像它即可。

## 2. radar 探针优先级恒为 High（设计问题，待讨论，不是 bug）

radar 的 4 个 push 点全部硬写 `ProbePriority::High`：
`crates/ptt-workflows/src/radar.rs` 的 322、398、406（经
`missing_conversion_probe`/`confirm_conversion_probe`）和 499 的内联构造。

因为 `raised()` 是 `High → High`，`boost_probe_candidates` 在 Opportunities
路径上是可证明的空操作 —— 排序也是（全同级，稳定排序原样保留字母序）。
priority 这个字段在这条路径上不携带任何信息，UI 上四个探针都标 High。

云端评审把这条路径也算进了 probe boost 的影响面，那是错的。

要不要给 radar 探针分级（比如 missing path 比 confirmation 更急）是产品判断，
不是修 bug。在决定之前，reports.rs:1340 那个 boost 调用留着无害。

## 3. Monitor 刷新新增一次开库（本次改动引入的代价）

`load_probe_queue` 现在会调 `load_pulse`，后者
`MarketStore::open(default_database_path())`。Monitor 每次刷新因此开**两个**
连接（`load_window` 一个 + `load_pulse` 一个），之前只有一个。

Monitor 是常驻界面，刷新频率最高，所以它是"AppShell 持有连接"那个待办里
收益最大的一个点。同一模式散布在 `load_analytics`、`load_window`、
`load_pulse` 和 `shell/pages/season.rs` 的四处。改动跑在 background executor
上，不阻塞 UI 线程，所以不紧急。

## 4. ptt-runtime 的 Cargo.toml description 已过时（小事，记一笔）

`crates/ptt-runtime/Cargo.toml:8` 现在写的是：

> "Background actor runtime for POE Trade Tracker (skeleton; full port lands in P2)"

早就不是骨架了。这个 crate 现在装的是页面模型与报表（`reports.rs`）、采集管道、
日 rollup，以及 `src/bin/*_probe.rs` 那批验证探针。

过时的描述比没有描述更坑人 —— 它会让人以为这里没什么东西，从而不去读。
顺手改的时候一起修掉即可。
