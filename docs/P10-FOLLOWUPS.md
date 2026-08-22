# P10 遗留清单（2026-08-22）

云端评审（main → p10-base）之后剩下的东西，给下一个接手的会话。
probe boost 的两条（不重排、Monitor 未接 pulse）已修，见 `0039c20`、`95e5a63`。

## 1. 今日折叠只读单个 context_key（已修 2026-08-22，见 `1749d2a`、`115a7ba`）

`game_ui_build` 是 stable key 的材料（`crates/ptt-trade-domain/src/lib.rs:259`），
客户端版本一变 key 就轮换。`load_pulse` 当时折叠的是页面窗口，而页面窗口只有一个
context，所以升级当天上午的抓取被今日折叠排除在外 —— Analytics 的"今日"列、以及
Convert/Opportunities/Watchlist 上读同一份 pulse 的结构注记都少算，直到下一个 UTC
午夜的 rollup 把两个 context 合起来自愈。

修法：把"跨 context 取今天"抽成 `ptt_runtime::rollup::today_window`，`load_pulse`
改成自己读这个窗口而不是借页面的；`analytics_probe` 那份内联副本也改成调同一个
函数，两条路径不会再漂。回归测试 `today_window_reads_every_context_of_the_game`
（`crates/ptt-runtime/tests/rollup.rs`）。

页面窗口本身没动：一本书必须自洽，所以它继续只读一个 context。

## 2. radar 探针优先级恒为 High（已裁定并实施 2026-08-23，见 `93b018c`）

四个 push 点原本全写 `High`，于是 `raised()` 无处可升、全同级列表也无序可排，
机会页那四条实际是字母序前四条。裁定：**待确认 = Medium，缺价 = Low**，两者都
不占顶格，稀缺/高流转加权才有空间。理由与完整表格在
`CORE-TRADING-MODEL.md`「探针分级」。

（云端评审把这条路径算进了 probe boost 的影响面，那是错的 —— 当时它在这条路上
确实是空操作。现在不是了。）

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

## 5. AppShell 不持有数据库连接（结构性，未做）

第 3 条引用的那个待办，这里给它一条正式条目。

每次页面读都自己 `MarketStore::open(default_database_path())`，全 app 没有一个
长期持有者。7 个开库点：

- `crates/ptt-app/src/shell/mod.rs`：1230 `load_window`、1269 `load_analytics`、
  1300 `load_pulse`
- `crates/ptt-app/src/shell/pages/season.rs`：112 `ensure_season_info`、
  151 `start_new_season`、176 `purge_old_season`、204 `vacuum_store`

收益最大的点是 Monitor（第 3 条）：常驻界面、刷新频率最高，且一次刷新现在开
两个连接。

**不紧急**：这些读都跑在 background executor 上，不阻塞 UI 线程。真正的理由是
结构性的 —— "谁持有连接"目前没有答案，每加一个页面读就多一个 open，散得越久
越难收。

**动它之前要先想清楚的**：连接怎么跨线程共享。页面读跑在后台执行器上且允许
两次刷新赛跑（见 `CORE-TRADING-MODEL.md` 8.4），一个 AppShell 持有的连接会被
多个后台任务同时用 —— 加锁、每线程一个、还是连接池，这是这条待办真正的成本，
不是把 7 个 open 删掉那么简单。`vacuum_store` 尤其要单独想：它现在有自己的
连接，而 VACUUM 在 watching 期间会烧穿 5s busy_timeout 打断捕获写入。
