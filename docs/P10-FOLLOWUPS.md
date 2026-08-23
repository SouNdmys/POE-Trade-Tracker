# P10 遗留清单（2026-08-22）

P10 云端评审之后剩下的东西，加上首次实机测试的反馈（第 6 条起），
给下一个接手的会话。
probe boost 的两条（不重排、Monitor 未接 pulse）已修，见 `0039c20`、`95e5a63`。

每条「已修」后面跟的是真正落地的 commit 短 hash——写下来是因为条目正文讲的是
**为什么**，而只有 hash 能带人去看**改了哪几行**，隔一个赛季回来时这是唯一还对得上的
线索。本轮四条裁定的完整理由都写进了 `CORE-TRADING-MODEL.md`（`4bbaa81`），
不在本文件里重复。

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

- `crates/ptt-app/src/shell/mod.rs`：`load_window`、`load_analytics`、`load_pulse`
  （这三个的行号本轮已经漂过一次，只记函数名）
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

---

# 实机反馈清单（2026-08-23 首次实机测试）

来源：用户 `bug 2026-08-23.md` + 六张截图。下面都是**需要商讨或设计**才能动的，
当场能修的（设置页参数说明）已经落在 `df4aff0`。

## 6. 雷达表格在窄窗口整片空白（已修 2026-08-23，见 `062403e`，与第 15 条同一次改动）

"宽度不够就不画"的假设被实验**证伪**：ptt-ui-preview 画廊拉到 1100px（表格可用宽
约 1078 < 列宽总和 1250）时所有行照常渲染，只是右侧被裁；`PTT_PREVIEW_PROBE=1`
在 1100/1500 宽下 cells_drawn 完全相同。上游（gpui-component 0.5.1
`virtual_list.rs:627-687`）宽度退化时是"全画"，不存在"返回空"的分支。

剩下两个嫌疑，按序排查：

1. **页面外壳缺防溢出**：`opportunities.rs` 的 `render_opportunities` 里 body 与
   页面根都没有 `min_w(px(0.))` / `overflow_hidden`，而画廊（`preview.rs` 的
   `Gallery::render`，内容层那一个 `.overflow_hidden()`）有。gpui 文本
   的最小内容宽 = 不换行整行宽（见第 7 条），缺这一层时窄窗口下页面整体向右溢出
   而不是重排。
2. **每次扫描 refresh() 把列宽清零一帧**：`sync_radar_table`（`opportunities.rs`）
   每次都调 refresh → `prepare_col_groups`（gpui-component `state.rs:278-290`）把全部
   列 bounds 清零，那一帧格子按 Definite(0) 布局 → 空白；表头 canvas 回写 bounds 时
   不带 notify（`state.rs:857-859`），要靠别的重绘救回。

修法按序试，一次一步：① body/页面根加 `.min_w(px(0.))`（照画廊的已知好形状）
② 列宽总和 1250 压到 ~1090（risks 240→180、verdict 150→130、out 140→120、
kind 110→90）③ 仍偶发就改成"只在列真变化时 refresh，行变化只 notify"。

免费线索：上游横向滚动条**默认是开的**（`TableOptions::default()` 的
scrollbar_visible），实机窄窗口能不能横滚值得确认——滚不动本身就是证据。

钉法：普通测试钉不住（无 gpui test-support）；给画廊加一个照抄雷达页外壳
（两层包裹 + 底部探针条 + 详情面板）的 "radar frame" 页。

**已修（①②③ 三步都做了）**：三步各自只堵住一条路，所以一起做。

- **① 页面外壳**：`opportunities.rs` 的 body 与页面根都加了 `.min_w(px(0.))`，body
  另加 `.overflow_hidden()`。理由和 shell 那一路 `min_h(0)` 是同一个：flex 子项的
  自动最小尺寸等于它的内容，而这一页的内容是「固定宽表格 + 固定宽详情面板」。缺这
  一层时，比两者之和更窄的窗口不是裁切而是让整页向右长出去，把表头和底部探针条一起
  带走。
- **② 列宽总预算**：新常量 `RADAR_TABLE_WIDTH_BUDGET = 1090`，八列之和不得超过它。
  七根固定列压成 kind 80 / edge 80 / depth 90 / out 120 / verdict 120 / light 90 /
  risks 120 = 700，route 拿剩下的 300–390（见第 15 条）。verdict、light、risks 压得
  比原方案（130/110/180）更狠：它们装的是 `whitespace_nowrap` 的徽章，这张表给得起
  的任何宽度都放不下整句，而全文点一下详情面板就有——像素花在「这行到底是哪条路线」
  上更值。**预算有编译期护栏（2026-08-23 补）**：`COL_ROUTE_MAX_WIDTH` 是
  「预算 − 七根固定列」这个减法，固定列一旦加宽到吃掉 route 列 300px 的地板，天花板
  就会掉到地板下面，而 `route_column_width` 里的 `clamp(地板, 天花板)` 在上下界颠倒
  时会 panic ——一次看起来只是调样式的改数字，会让雷达页画第一行时整个崩掉。现在
  `opportunities.rs` 里有一条 `const _: () = assert!(COL_ROUTE_MIN_WIDTH <=
  COL_ROUTE_MAX_WIDTH, …)`，把这件事从运行时崩溃变成编译失败，报错文案直接说该改
  哪个数。
- **③ 不再每次扫描都 refresh**：`set_rows` 现在返回「列有没有动」，`sync_radar_table`
  只在返回 true 时 `state.refresh(cx)`，否则只 `cx.notify()`。`refresh` 既是列宽变化
  唯一的送达通道，也正是把全部列 bounds 清零一帧的那个动作；只换行不换列的扫描没有
  理由付这一帧。上游 `TableState::new` 自己会调一次 `prepare_col_groups`，所以
  col_groups 不会因为少调 refresh 而空着。

**哪一步真正消掉了空白，未能实机确证**：布局钉不住（无 gpui test-support），
`PTT_PREVIEW_PROBE=1` 只证明表格仍然虚拟化（cells_drawn=454 / cells_if_eager=3500，
exit 0），不证明窄窗口不空白。按机制推断：① 堵的是「整页溢出窗口、表格被推出可视
区」，③ 堵的是「扫描后那一帧按 Definite(0) 布局」，② 让两者都不容易再触发。实机若
仍有残留，剩下的嫌疑只有上游 canvas 回写 bounds 不带 notify（`state.rs:857-859`）
那一条，那要么等上游，要么自己找一个能触发重绘的时机。

**这次没动的**：`chips(…, 2)` 的两枚上限没改——risks 只剩 120px，两枚徽章必定被裁，
但裁多裁少本来就一直在发生，改上限是显示策略的另一次决定。画廊也没有加那个
"radar frame" 页（原钉法建议），因为宽度这部分已经能用普通 cargo test 钉住了。

## 7. 路线明细内容被右边界裁掉（已修 2026-08-23，见 `affbede`）

两层根因：**gpui 文本的最小内容宽 = 不换行的整行宽**（gpui-0.2.2
`text.rs:347-376`，MinContent 与 MaxContent 量出同一个值的缓存短路）；`kv_row`
的值容器是 `flex_1` 但没有 `min_w(0)`（`ui.rs` 的 `kv_row`），于是永远压不进面板、
永远拿不到确定宽度、永远不换行，还把 340px 的 `detail_panel`（`ui.rs` 同文件）撑宽
——右边被窗口边界裁掉而不是被面板边框裁掉。

修法：`kv_row` 的值 div 加 `.min_w(px(0.))`，`detail_panel` 同加。**选换行**，
不选截断/横滚——`kv_row` 的文档注释本来就写着"值会长到两三行"，
换行是既定意图，只是没生效。风险：面板变高，必要时给 radar_detail 内层套
`scrollable()`。画廊 kit 页的 `kv_row("risks", …)` 是现成回归用例（改前溢出、
改后换行）。

**已修**：`ui.rs` 的 kv_row 值容器与 detail_panel 都加了 `.min_w(px(0.))`，
`kv_row` 的文档注释里记了"最小内容宽 = 整行宽"这条机制。回归用例是画廊 kit 页的
risks 行，值换成一串没有空格可断的长中文（实机那条"结构性流动性"），普通 cargo
test 钉不住（无 gpui test-support），靠肉眼看画廊 + `PTT_PREVIEW_PROBE=1` 的
探针门。

## 8. 最大化时的字号密度（设计问题，需要设计稿）

用户原话：窗口化看着舒服，最大化就"看着很吃力"，但要看全内容又必须最大化。

这不是 bug，是**这套 UI 的尺寸是按非最大化的宽度设计的**。用户自己提了一个方向：
要不要让 claude design 出一版。在决定"是调密度还是重做一版"之前不要零敲碎打改字号
——那只会让两种宽度都不对。

## 9. 兑换页负收益路线（**主因已修** 2026-08-23，见 `70852e1`、`073c82e`、`2f1da76`、`4f162b9`；剩下的见文末）

从真实库逐位复算，截图上每个数字全部对上。四部分：

- **数据半（诚实的部分，显示已做一半）**：直兑 taker 深度只有 1855 神聖石（5000 的
  37%）。"可见深度 37404"是**对侧**竞争方挂单（你的对手在卖，不是你能吃的买单），
  同页并排不加区分是误读的直接来源。
  **2026-08-23 上线了一半**：路线的每一条腿现在都打一行「市面挂着 X，
  这一趟要吃掉 Y（Z%）」，分母只算**该方向上 taker 执行类型的行**，三档上色。
  裁定与完整理由在 `CORE-TRADING-MODEL.md`「按腿的「立刻吃下的覆盖度」」。
  **仍未做的是对侧那一半**：37404 那个数仍然不带区分地并排在旁边。
- **bug 半（排序，已修）**：`compare_paths`（ptt-trade-engine `route.rs`）现在是
  可执行 → **中途搁浅比例** → **已实现单价** → 残余腿数 → 跳数 → 资产名。理由在
  `CORE-TRADING-MODEL.md`「兑换路线排序」。回归测试 `compare_paths_tests`。
- **口径半（本次主因，已修，见 `4f162b9`）**：页面原本把「吃穿多档的混合均价」当利润
  基准，于是利润的正负号取决于**你输入了多少**。实测同一条汇率优于直兑 11.11% 的路线，
  输入 10 读作 −44.44%、输入 5000 读作 −99.88%。现在每条路线按**可达汇率**（每条腿首档
  报价的乘积）定价，利润百分比是两个汇率的比值，与输入量无关；混合均价降级成带标注的
  「清仓价」，不再参与任何比较。同一次改动上了「不要错杀」（排序只决定顺序）和「汇率
  劣于基准就隐藏」两条规则，**顺序不可颠倒**。完整裁定在 `CORE-TRADING-MODEL.md`
  「兑换页围绕汇率算，不围绕数量算」。
- **文案（已修，见 `2f1da76`）**：`-{}（比直兑低 {}）` 配上一个自带负号的百分比，渲染成
  「比直兑低 −13.38%」。方向归模板的字，符号不再重复；配对逻辑收进
  `report_text::versus_direct`，页面和文本报表共用一份。

**本次结论：左旋那条路是「任何档位都不盈利」，不是「有汇率但吃不满」。** 两条腿各六档
穷举 36 个组合，最好的端到端 10.5677，直兑最优 10.8、最差 10.78——**一个组合都没赢**。
它当初排第 1 是旧的「按绝对产出排序」造成的（吃 2526 出 21786 vs 直兑吃 1855 出 20030）。
逐档表在 `CORE-TRADING-MODEL.md` 同一节。

**仍未做**（按建议顺序）：

1. **对侧那一半**：「可见深度 37404」是竞争方挂单，和你能吃的量并排显示、不加区分。
2. **基准的另一种口径**：`compare_best_to_direct` 的 `IncomparableCoverage` 保护还在
   引擎里，而 `convert_model` 仍然不走它（现在也不需要——它比的是混合均价，而页面已经
   不用混合均价比了）。要不要把这个保护改成「按可达汇率比」是独立一次改动。
3. 流动性闸只认**已经发生的**搁浅（吃满的腿看不出后面还剩多少余量）。要让它也能判
   「这条路各腿本来就薄」，得把整侧的 `listed_stock` 顺着 `PairFill` 带出
   `MarketDepthIndex`，那是一次独立改动。**页面那一层现在能看到池子了**（按腿覆盖度 +
   每条路线的「这个汇率上市面能吃下 N 个」），但 `compare_paths` 看不到。
4. **`execution_eligible` 里还藏着一个尺寸键（待裁定）**。它的合取项之一就是
   `is_fully_filled`，所以它会原样重演覆盖键那个"填 5000 就换胜者"的翻转。今天不发作，
   因为它同时要求 `product_execution_allowed`，而这个标志在
   `ptt-market-book/src/lib.rs:504` 恒为 `false`，生产上每条路线的 `execution_eligible`
   都是 `false`、键是常数。这是**偶然安全**，不是结构安全：`tests/engine.rs` 的 fixture
   自己把那个标志打开了，删掉这个键那两条测试立刻变红，等于现场演示了翻转还在。要真修，
   得决定 `execution_eligible` 到底是"能按请求量整口吃下"还是"这条路的性质允许即时吃"，
   而这个字段 reports、radar、triangle、execution_safety 都在读，改语义影响面比这次大。
5. **雷达的分流跟着变了（未处理）**：`radar.rs` 的
   `if !best.is_fully_filled { 推 confirm 探针; continue }`，以前有吃满的路线时
   `best_path` 必是它，现在 `best_path` 可能是一条部分成交但单价更好的路线，于是这个对
   会从机会表挪进探针队列。排序键本身没动。**雷达也还没用上「可达汇率」这套口径**——
   它仍然按引擎的混合均价判盈亏，兑换页与雷达页因此可能对同一对给出不同结论。要不要统一
   是另一次裁定，影响面比兑换页大（雷达要扫上百对）。
6. **诊断没有落地的家**：本次的逐档穷举与「哪些候选被隐藏了」是用一个临时 bin 跑的，
   跑完删了。要复现得再写一次。`coverage_probe` 加一个 `--convert FROM TO SIZE` 分支
   大概十几行，值得，但不在本次范围。
7. **同一张卡上两个口径的数字差一点（本次留下的，2026-08-23）**：汇率行写
   「5000 → 54000」（可达汇率 × 输入量），紧接着的按腿覆盖度行写「这一趟要吃掉 53989」
   （`leg_take_amount`，用的是逐档混合均价外推）。两个数问的是两件事——前者是「我挂
   10.8 能换回多少」，后者是「我现在去扫这条腿要吃掉多少」——但并排差 11 个，读者第一
   反应会是取整 bug。要么把 `leg_take_amount` 也改成按首档算（那它就不再是 taker 口径，
   和这一行的立意冲突），要么在文案上把两者的问题分开说明。**没动是因为这是一次口径
   裁定，不是格式调整。**

## 10. 覆盖与缺口需要「忽略」（新功能，待设计）

用户原话：`混沌石 → 萊基亞塔的流動` 这种一个几百 D 的高价值辅助宝石，根本不会有人
挂 chaos 单去卖，等于永远扫不到，但建议一直挂在那儿。

关注列表侧已经有两套机制：`ignored_suggestions`（忽略，证据翻倍后重新出现）和
`hidden_assets`（隐藏，纯显示层）。覆盖与缺口这一侧一套都没有。

设计要定的是：这里的"忽略"该照哪一套？关注列表的忽略是"不是现在，不是永远"，
覆盖缺口如果照抄，那"永远不会有人挂单"这种情况就会反复回来——但做成永久忽略，
又会在赛季变化后把真的该扫的对永久埋掉。可能需要第三种语义。

**2026-08-23 破案补记**：实机那三条（萊基亞塔的流動 / 完美崇高石 / 完美工匠石）
不是这个功能缺失造成的——它们就在 settings.json 的 focus_assets 里（8-20/21 测试
期抓过并设为目标），开赛季清理删了原始数据但**设置不随赛季清空**，造成"从没关注
过"的错觉。当次解法：关注页把它们点成不关注（或去抓一次）。忽略功能本身仍值得
做，优先级下降。

## 11. 赛季开始时间不可调（设计变更，待讨论）

用户原话：赛季中途才下载这个程序的人，会犹豫到底要不要开启新赛季。提议改成
**按时间段清理数据**，同时保留"开启新赛季"这个选项，默认把之前的数据全部隔离。

现状：`MarketStore::start_season(game, label, started_at)` **后端已经接受任意时间戳**，
是 UI 硬写了 `Utc::now()`（`crates/ptt-app/src/shell/pages/season.rs:152`）。所以
"让用户填一个开始日期"本身是小改动。

但用户提的是更大的东西，动之前要对着 P10 的三条既有裁定看：`started_at` 单调、
换季 = 钳制归档（不删任何数据）、清理是独立的两击动作。"按时间段清理"和"换季"
是两件事，别把它们合成一个按钮——那正是 P10 特意拆开的。

## 12. 界面没说「隐藏」按钮为什么只在部分行出现（小事）

用户问过。规则在 `crates/ptt-app/src/shell/pages/watchlist.rs:351`：
`!settlement && choice == FocusChoice::Unlisted` —— 只有**非结算通货**且选了
**不关注**的行才给隐藏。代码里有注释解释理由（给了角色的行先取消角色，结算通货不能
藏），缺的是界面上没有任何提示。

要不要加一句提示是产品判断：加了会让本来就有四行角色说明的顶部更长。

## 13. 选中行被高亮涂掉（已修 2026-08-23，见 `f967ec8`）

选中高亮是行顶层一块带背景的绝对定位元素（gpui-component `state.rs:1082-1099`）。
上游主题装载会把 table_active 的透明度夹到 ≤0.2（`schema.rs:636-638`），但我们直接
写 colors 结构体绕过了夹取：`theme.rs` 的 table_active = SELECTED（0xEFE9DE，
alpha=1.0）→ 不透明实底盖住整行文字。同文件的 list_active 同病（树/列表
控件也会中招）。右键高亮只画边框不画底，所以右键没这现象——反向印证。

修法：两处改成低透明度 wash（如 rgba 0xEFE9DE33），选中感由 table_active_border
承担。测试：断言 apply_ledger_theme 后 `table_active.a <= 0.2`，先红后修——四个
UI 问题里唯一能用普通 cargo test 钉住的。

**已修**：新常量 `SELECTED_WASH_ALPHA = 0.2`，`table_active` / `list_active` 都改成
`hsla_of(SELECTED).alpha(SELECTED_WASH_ALPHA)`。写 colors 结构体的那一段拆成
`apply_ledger_colors(&mut ThemeColor)`（`apply_ledger_theme` 要 `App`，拆开才测得到），
回归测试 `active_row_highlights_stay_translucent`（`theme.rs` 的 `theme_tests`）。

## 14. 扫描后选中索引不重映射（已修 2026-08-23，见 `c72444f`）

`set_rows`（`opportunities.rs`）只换 rows 不动 selected_row；每次扫描后
选中索引指向新行集里的**另一条路线**，详情面板静悄悄换内容。与第 13 条无关
（那次索引恰好没错位，详情是对的），单独修。

**已修**：新增纯函数 `remap_selection(旧选中, 旧行集, 新行集) -> 新选中`
（`opportunities.rs`）。`sync_radar_table` 在把新行集交给 `set_rows` **之前**先算出
答案——旧行集是唯一还记得"那个索引指的是哪条路线"的东西，行一换就没了。换完之后
按结果落地：位置真的变了才 `set_selected_row`（上游那个方法顺手把行滚进视野，路线
没挪的扫描没理由拽一下列表），这次扫描根本没扫到就 `clear_selection`。

**身份键取 `(kind, path_asset_ids)`**：一个是"兑换还是环路"，一个是资产序列，两个
都由市场决定、扫描只是转述，所以两次扫描对同一条路线给出同一个键。**没有用
`RadarItem::item_id`**：它长得最像主键，其实是 `conversion-{n}-…`，`n` 是这条路线在
本次扫描里的入列序号（`radar.rs:723`、`762`），前面任何一条路线这次没扫出来，后面
所有路线的 id 就整体前移——正是本条要修的那种漂移。

**改在 shell 这一层，不在 delegate**：`set_rows` 在 `RadarTable` 上，而 selected_row
住在上游的 `TableState`（gpui-component `state.rs:224-269`），delegate 够不着。上游
库一行没动。

**保守的一处**：同一个环路从不同起点进入时 `path_asset_ids` 是一次旋转，键因此不同，
会被判成"这次没扫到"而清除选中。清除是安全方向（宁可没有详情，不要错的详情），暂不
为旋转做归一化。

回归测试在 `opportunities.rs` 底部新的 `selection_tests`，两条，改前都红：
`a_reordered_scan_keeps_the_selection_on_the_same_route`（三条路线重排，选中的那条
从中间挪到队首，断言 0；改前恒 1）、
`a_scan_without_the_selected_route_clears_the_selection`（新行集里没有那条路线，断言
None；改前恒 Some(1)）。

## 15. 路径列宽方案（已修 2026-08-23，见 `062403e`，与第 6 条同一次改动）

上游 Column 只有固定像素宽，无 flex/auto/按内容测量；拖拽默认开但每次 refresh
被 column.width 重建吃回去（拖了白拖，这本身也是要修的）。方案：set_rows 里按
最长 route 文本算期望宽，clamp(期望, 300, 总预算 − 其余七列)；总预算与第 6 条的
压缩一起定（~1090）。必须加迟滞（只增不减、变化 >24px 才动），否则表格每次扫描
抖动，比截断更烦。宽度计算可用普通测试钉。

**已修**：期望宽在 `RadarTable::new` 与 `RadarTable::set_rows` 里算，走新的
`route_column_width`——取当前行集里最长的一条 route 文本，用 `monospace_width` 估宽
（CJK 一个字宽、其余约 0.6 字宽），加一格 cell padding（XSmall 是左右各 4px），再
`clamp(300, 1090 − 700 = 390)`。为什么是估不是量：gpui 能精确量文本，但只在布局过程
里，而列宽必须在第一次布局之前就存在；字体是等宽的，所以逐字加宽度就够，差几个像素
无所谓——两头都被 clamp 接住。

**迟滞**：`fit_route_column` 只增不减，且只有期望宽超过当前宽 24px
（`COL_ROUTE_GROWTH_STEP`）才真的改。扫描每几秒换一批行，逐帧贴合会让它后面七列跟着
横向挪；读者顺着一行往下看时，「稳定地被截断」好过「精确但是在动」。

**拖了白拖也修了**：`AppShell` 里本来就有的 `cx.subscribe(&radar_table, …)` 多接一个
`TableEvent::ColumnWidthsChanged`，把宽度写回 `RadarTable::columns`，refresh 于是重建
出用户拖的那套而不是出厂那套。同时置 `widths_are_the_readers` 标志让自动贴合退休：
否则用户手动把 route 拉窄，下一次扫描又会按「只增不减」把它撑回去，还是白拖。语言或
目录变化走 `Self::new` 整体重建，标志跟着复位。

回归测试在 `opportunities.rs` 底部新的 `column_width_tests`，四条，改前三条红：
`every_column_together_fits_the_table_width_budget`（八列之和 ≤ 预算；改前 1250）、
`a_long_route_widens_its_column_up_to_the_ceiling`（超长 route 顶到 390；改前恒 300）、
`the_route_column_only_widens_in_steps_worth_seeing`（+14px 不动、+41px 才动；改前连
起点都贴在地板上）、`a_dragged_route_width_survives_the_next_scan`（拖到 200 之后一次
超长扫描不许把它撑回 390；加标志前正是 390）。测试的资产 id 用目录里没有的字母串，
`asset_name` 会原样回退成 id，所以 route 文本要多长就有多长。

## 16. 确认探针是恒定噪音，挤掉了有用建议（已修 2026-08-23，见 `72d8815`）

实机那四条"确认一个机会"全部来自三角腿分支（`radar.rs`），不是部分成交分支（本次
扫描 29+6+1=36 全对上，confirm_conversion_probe 一条没推）。根因：
`product_execution_allowed` 硬编码 false（ptt-market-book `lib.rs:504`，任何设置
改不了）→ triangle.execution_eligible 恒 false → **每个盈利闭环无条件推腿探针**，
100% 恒定噪音，把 6 条 MissingForwardQuote 永久挤出机会页那四个槽位。

**已修**：腿探针条件由 `!execution_eligible` 收紧成
`!execution_eligible && !legs_all_fresh(...)`（新私有函数 `legs_all_fresh`，
`crates/ptt-workflows/src/radar.rs`）——三条腿都在 fresh 窗口内就不推。裁定理由，
以及"提醒不提醒"与"能不能执行"为什么是两件事，在 `CORE-TRADING-MODEL.md`
「闭环全新鲜就不再提醒确认」。硬编码的 `product_execution_allowed` 没动，仍恒为
false。回归测试在 `crates/ptt-workflows/tests/radar.rs`：
`a_fully_fresh_profitable_loop_is_not_filed_for_confirmation`（改前红），对照
`a_stale_legged_profitable_loop_is_still_filed_for_confirmation`（腿龄 30 分钟，
探针照推，证明不是把功能整个关掉）。

## 17. 估值恒定取两侧最薄档（已修 2026-08-23，见 `d87c97b`、`769fefa`）

三段叠加：`reports.rs:377` 把全部候选边都装进 selected；Instant 候选按 stock 降序
（market-book `lib.rs:901-914`，本批置信度全同所以排序实际就是库存序）；
`anchor_value.rs` 的 best_rate 用 max_by，captured_at 全平局时返回**最后一个**
= 库存最小档。实测四个方向全部选中最薄档（鎖骨买价来自 stock=2 的挂单，stock=99
的被忽略）。

**已修（第二、三段）**：`d87c97b` 给 `best_rate` 的比较键加了第三键 stock
（「taker 标志 → captured_at → stock」），平局取最深档。裁定理由——为什么最薄的
一行是最不可信的一行、为什么没写成「取队首」——在 `CORE-TRADING-MODEL.md`
「估值平局取最深档」。回归测试
`a_valuation_reads_the_deepest_level_when_the_whole_book_shares_one_capture`
（`anchor_value.rs` 的 `mod tests`，数据是 2026-08-23 那本 ancient-clavicle 的
taker 阶梯原样搬过来）。改前红：卖价取 41:1（stock=41）、买价取 47:1（stock=2）——
和实机诊断逐位对上；改后取 44:1（stock=3740）与 49:1（stock=99）。

`769fefa` 补了它留下的两个洞：那条回归测试的样本恰好就是 stock 降序，所以「显式比
深度」和「取第一个」在它上面答案相同，把整个比较器删掉换成 `.next()` 测试照绿；而
深度本身也会平局（同一次抓取的三档同为 stock=500），`max_by` 又返回并列里的最后
一个，同样三行能把估值差出 7.3%。现在第四键是 rate（同样能吃、同样新、同样深的两
档，取更好的那个价），测试改成对候选列表的每一种旋转与反向旋转都跑一遍。

影响面只有关注页的估值列（`value_against_anchor` 全仓只被 `watchlist_model`
调用一处）。市场分析页的价值列走的是 rollup 价格序列（`market_analytics.rs:392-410`），
不经过这条路径，数字不变。

**第一段没修**：「全部候选边都装进 selected」这一段仍然原样，独立记为第 21 条。

## 18. 赛季边界的三个潜在隐患（今天不触发，记账）

- `today_window` 从当天 00:00 读起，不过 clamp_to_season——当天中午开新赛季，
  上午开赛前的数据会混进今日折叠。
- rollup 侧赛季地板只精确到"天"（from_day 取 started_at 的日期），开赛当天若存在
  日汇总会整天计入（含开赛前半天）。
- purge_before_active_season 清 raw 后 earliest_capture_day 前移，开赛日永远不会
  被 rollup、也不拿 mark（本次无损，耦合不显眼）。

## 19. bp 统一成百分比（已修 2026-08-23，见 `2b4d7f5`）

雷达页已经显示百分比（`opportunities.rs`），兑换页显示 bp——同一个量两种写法。

**已修**：新增共用格式化函数 `percent_from_basis_points`（`report_text.rs`），
所有显示点改走它（`report_text.rs` 的 8 对模板、`reports.rs`、`opportunities.rs`、
`convert.rs`、`analytics.rs` 的 `signed_bps` → `signed_percent`、`history.rs`）。
裁定理由——为什么两位小数是无损的、为什么符号必须单独写而不能交给除法、为什么设置页
输入单位故意不一起改——在 `CORE-TRADING-MODEL.md`「界面统一用百分比，不用 bp」。

回归测试在 `report_text.rs` 底部新的 `percentage_tests`：
`no_report_template_prints_basis_points`（遍历 `report_pairs()`，两种语言都不许
再出现 "bp"）、`the_worse_than_direct_line_reads_as_a_percentage`（实机那条
-1338bp → `-3451 (-13.38% vs direct)`）、
`basis_points_convert_to_two_decimals_without_loss`（0、±1、±99、100、-1338、
10000 的逐字符断言，钉住负号那一格）。三条改前全红。

**正负号补记（2026-08-23，本轮收尾）**：改成百分比之后还剩一处不一致——分析页给正数
加 "+"（`analytics.rs` 的 `signed_percent`），而同两个数字在 `analytics_report_lines`
（`reports.rs`）里是光秃秃的。这两处不是巧合并列：文字报表的文档注释写着它是**页面的
对照基准**，一个正的漂移在一处读作 `+2.57%`、另一处读作 `2.57%`，正好制造第 19 条要
消灭的那种「这是不是两个不同的数字」的怀疑。裁定按页面走：这两个数是**移动**不是
**水平**，没有符号就读成了后者。做法是把符号规则搬进 `report_text` 的新函数
`signed_percent_from_basis_points`，两边都调它——规则只要还归某一页所有，另一页迟早
再漂开。零算「不是下跌」，跟着带 "+"：一列都带符号的数字里，独独一个不带的会被读成
另一类数，而不是更小的数。回归测试两处：`reports.rs` 底部新的 `analytics_sign_tests`
（正数带 "+" 改前红，负数不许出现 `+-`／`--` 的双符号）、`report_text.rs`
`percentage_tests` 里的 `a_drift_is_written_with_the_sign_it_moved_in`。

**仍未做**：`i18n.rs` 那 7 处设置页标签仍写 bp（输入单位，独立一次改动；过渡措施是
六条 `tuning_*_note` 都写上"100bp = 1%"，`tuning_min_bps_note` 另点明"这里填 bp，
页面上显示成百分比"）。兑换页 tier 行的实物说明（"每 100 神聖石少换约 13 混沌"）
没做。`analysis.rs:102` 与 `bin/engine_replay_probe.rs:378` 的 `profit={}bp` 也没动：
那两行是硬写英文的诊断转储，根本没进双语目录，属于另一类问题。

## 20. 杂项记账（2026-08-23）

- **radar.stake=10 太小**：18 个目标里 14 个 10 混沌买不起一个整东西（NO-PATH），
  靠 minimum_input_for_one_unit 自动加注救回，但各目标注额不同、横向不可比。
  建议用户在设置页改成 100–1000。
- **stable_key 前缀硬写 "poe1-context-v2:"**（trade-domain `lib.rs:248`）：POE2 的
  context key 也叫这个名，功能无错（game 在被哈希材料里）、读库误导；改动会轮换
  全部 key，单独评估。
- analytics_probe 的文档注释写成 `analytics-probe`（连字符），照敲报
  `no bin target named`（`bin/analytics_probe.rs:8`）。
- **`coverage_probe` 的资产 id 必须用连字符**：`coverage-probe FROM TO` 里的两个 id
  要写 `ancient-clavicle`，照 `crates/ptt-catalog/data/*/currency_master*.json` 里的
  下划线形式敲成 `ancient_clavicle`，会得到 `no selection entry for ... -> ...`
  ——看起来像「这对没抓到数据」，实际只是字符串没对上。两种写法都存在是有原因的：
  目录文件用下划线，`MarketAssetId::try_new` 干脆不接受下划线（只收小写字母、数字和
  连字符），`live.rs` 装载目录时逐个 `replace('_', "-")`。probe 拿命令行参数直接和
  `MarketAssetId` 比字符串，中间没有这一步转换。与上一条 `analytics_probe` 的连字符
  坑同类：都是**照文档／目录原样敲，得到一个看起来像数据问题的假象**。

## 21. 估值仍会读被选拔拒绝、低置信度的边（未修，2026-08-23 记账）

第 17 条「三段叠加」里**没修的第一段**，单独立条。不是回归：`d87c97b` 与 `769fefa`
修的是第二、三段（同一本书里几档全平局时该选谁），这一段问的是更前面的问题——
**谁有资格进这场比较**——从头到尾没动过。

现状两句话：

- `crates/ptt-runtime/src/reports.rs:377` 把每个方向的**全部候选边**原样装进
  `market.selected`（`selected.extend(entry.candidate_edges.iter().cloned())`），
  其中包含 `accepted_for_selection == false` 的边。这一行本身是对的，它当初是为了修
  「选中边被重复计入」的 bug，注释也写明候选里已经含选中边。
- `crates/ptt-strategy/src/anchor_value.rs` 的 `best_rate` 只过滤方向和新鲜度，四个
  比较键（taker 标志 → `captured_at` → stock → rate）里**没有一个看
  `accepted_for_selection`，也没有一个看 `effective_confidence_ppm`**。

后果实测：一条同时带 `PriceOutlier` + `LowConfidence`（置信度 100 ppm，正常一批是
990000 ppm）、但库存 9999 的边，会在第三键上赢过所有正常档，**由它决定关注页的估值**。
这正是第 17 条裁定要挡的那类数——只不过那次挡的是「最薄的一行」，而这一条是「最假的
一行」，并且它恰好还很深。

**修法方向，三条，取舍不一样：**

1. **在装进 `selected` 时过滤掉 `accepted_for_selection == false`**——看着最省事，
   实际上是错的，而且错得不显眼。`market.selected` 有四个读者：挂单策略的竞争队列
   （`MakerRequest.competing`）、关注页估值（`value_against_anchor`）、流动性锚推荐
   （`recommend_liquidity_anchors`）、History 页的价格序列（`price_points`）。在源头
   砍一刀会静默改掉后面三页的数字。更要命的是 `accepted_for_selection` **是相对
   Instant 这个选拔策略说的**：Instant 要求 taker，于是**每一条 maker 参考边都带
   `WrongExecutionType`、一律 `accepted=false`**——按这个标志过滤等于把整个竞争侧删光。
   `maker_strategy.rs` 里已经有一段注释把这件事讲透了（大意：判据要用证据、不要用选拔
   结果，因为 Instant 选拔下每条 maker 行都"被拒绝"，而那跟它诚不诚实无关）。
2. **在 `best_rate` 里按 `accepted_for_selection` 过滤**——影响面精确锁死在估值这一条路
   （`value_against_anchor` 全仓只被 `watchlist_model` 调用一处），不碰另外三个读者。
   但判据仍然是错的那一个，理由同上：估值本来就要读 maker 边（taker 只是第一个比较键，
   不是准入条件），按这个标志一滤，买卖两侧会瘸掉一侧。
3. **在 `best_rate` 里按证据过滤**——照 `maker_strategy` 已经在用的那套：看
   `risk_flags` 里有没有 `PriceOutlier` / `OutsideTopBookBand`，再加一道置信度地板。
   判据对、影响面也窄，代价是两个：一是仓里从此有第三处在回答「这一行诚不诚实」，
   三处漂开就是下一个这样的 bug；二是**置信度地板现在住在 `QuoteSelectionPolicy` 里，
   而 `best_rate` 手上没有 policy**——`ValuationRequest` 只有 asset/anchor/mode/edges/
   include_historical 五个字段，要么给它加一个字段，要么让阈值有第二个家。

倾向是 3，但那道地板要从哪儿来得先定，所以今天只记账。**改之前先写红测试**：
一条 stock 极大、带 `PriceOutlier` + `LowConfidence` 的边，和一条正常边，断言估值取
正常那条。

## 22. 挂单会不会卡住：用供需倍率而不是挂单量（未做，2026-08-23 新开）

第 9 条那半「按腿的立刻吃下覆盖度」上线后必须紧跟着说清楚的一件事：**它回答的是
taker 那一半**（我现在立刻去吃这条腿的存量，够不够、划不划算），**不回答 maker 那一半**
（我挂出去的单会不会卡住）。

为什么挂单量比值在挂单方向上没有意义：POE 的成交是你挂出自己的汇率，别人只会按**你的
汇率或对你更好的价**来吃你，否则你的单就一直挂着。所以**挂单方向上不存在"逐档吃到更差
的价"**，"市面上挂了多少"也就不构成对你挂单的约束。逐档变差只在你主动吃存量时才成立。

**用户真正怕的那个灾难（原话）**：换进某个桥通货之后，挂两小时卖不出去，回头一看市场
汇率已经比自己挂出去的便宜很多——供过于求，只能割。**0.5 赛季为此亏过几十 D。**

**正确的信号是供需分类，不是挂单量**：P10 已经有这套东西了——供不应求 / 供需均衡 /
供过于求（`CORE-TRADING-MODEL.md`「供需归属规则」，`ptt-strategy/src/day_rollup.rs` 是
唯一可审计点；页面上是 `StructuralNote`）。要做的是把它接到**兑换页每条腿的中间通货**
上：这一趟会让你在 X 上停留，而 X 现在是供过于求——挂出去大概率要等，而且等的过程中价
还在往下走。

**动它之前要定的**：
- 判据是**倍率**（demand/supply 比）还是倍率 + 绝对量？镜子那种"全市面 41 个"的通货
  倍率会很飘，`quiet_floor_anchor_units` 已经在管这件事，要复用而不是另起一套。
- 中间通货停留时间没法预测，所以这条只能是**提示**，不能变成排序键——和第 9 条那条
  「数量不参与排序」的裁定同源。
- 兑换页现在已经有一个 `need_structural`（只给最终想要的那个通货）。中间腿要用的是
  同一份 pulse，问题只是"给哪几个资产取注记、放在哪一行"。

**不要把它和第 9 条那个覆盖度合成一个信号**——一个是"现在能不能吃下"，一个是"挂出去
能不能出手"，合成一格就等于把两种完全不同的风险涂成同一种颜色。

## 23. r4 截图复查：三处显示层残留（已修，2026-08-23）

用户拿 r4 包实测兑换页，五张截图逐数核对。汇率不随数量变、亏汇率隐藏、左旋反向
正确出现——核心裁定全部成立。但揪出三处显示层残留，均已修：

1. **清仓价三行还在打「比直兑低 X%」的负数**（截图里 `-39063（比直兑低 0.08%）`）。
   文本报告的 `tier_line` 在汇率改造时删掉了这个对比，但 GPUI 页面 `tier_row` 有
   自己的一份句子，漏改了——**两套渲染各持一份措辞就是这么漂移的**。修法：抽成可测
   的纯函数，三行只留「进多少 → 出多少」。`2998367`
2. **超过挂单总量之后份额百分比变成噪音**（`要吃掉 83333333（1796364%）`）。旁边的
   判语「比现有挂单还多——一次吃不完」已经把话说完了，百分比是重复且在大额输入下
   膨胀成没人能掂量的数。修法：吃得下才打百分比。`d9694c8`
3. **路线列表借用引擎排名，序随数量变、直兑被夹在中间、最好汇率的路沉底**。修法：
   可见行按首档汇率降序，直兑垫底当地板；`compare_paths` 未动。设计理由追加在
   `CORE-TRADING-MODEL.md`「兑换页围绕汇率算」的附记里。`99230a5`

另：中文界面所有「腿」的说法换成「步」（用户：「这条腿」听着不像人话）。`980ead0`

**这轮的教训（记给以后的验收）**：第 1 条是探员报告说了「已删」而实际只删了一半——
同一句话在两处渲染各有一份副本时，验收必须两处都查，跑文本报告不能代表界面。

## 24. r5 深查：五处已修，四处待你拍板（2026-08-23）

r5 三处修复**实机验证通过**：拿用户真实库（1682 条观测）跑 5 个通货对 × 11 个持仓量，
共 55 组，路线集合、汇率、百分比**全部逐字相同，零违规**。截图上看到的都对。

但这轮深查（6 组调查 agent + 实机探针）又揪出更严重的一批。**已修五处**：

1. **小额持仓会让路线凭空消失（最严重）**。搜索拿用户持仓当输入量，而
   `route.rs` 的 `if propagated.quanta == 0 { continue }` 会丢掉任何中途归零的路径。
   于是**比你持仓还贵的桥通货，会把经过它的所有路线全删掉**。实机数据：
   `chaos → divine` 持仓 20 时只有 1 条路、50 时 3 条、100 时 7 条、200 时才 10 条全出；
   `chaos → 削切之兆` **持仓 100 以下一条都没有**，页面显示「还没有路线」并请你重新抓取
   —— 而库里躺着 8 条盈利汇率。这是裁定三要防的**错杀走后门进来了**：不是因为汇率亏被
   隐藏，是压根没被枚举。改法：候选枚举固定跑在一个没人输入过的规模上，与持仓求并集；
   页面上每个数字仍按用户输入的量、按首档汇率算。`45421e6`
2. **只有第一步是按用户输入的量衡量的**。引擎把上一步的**实际产出**交给下一步，所以
   一步吃不满，后面每一步的"需求量"都跟着缩水。实机表现：三跳路线的最后一步在 500 和
   50000 两个持仓下**都是 27**——不会随输入变的警告不叫警告；同一条腿挂在不同路线下还
   会一个判黄一个判红。也正是"表头说 500→88、腿说 27"那个对不上的根源。改成按首档汇率
   的前缀积把输入量一路带下去。`dd1b955`
3. **挂单策略的百分比随持仓变**（同一个病，右边那个面板）。基准是 `instant_rate_of`
   返回的**混合均价**，吃穿几档取决于你填多少。实机：同一行同一个价，持仓 500 时
   6.24%、169 时 6.44%、50000 时 5.82%；另一对 11.03% 对 4.76%，**差一倍多**。改成拿
   首档汇率当基准——挂单本来就是按你的价或更好的价成交，竞争对手是"现在能拿到的最好价"，
   不是"清掉某一包货的平均价"。`23731c1`
4. **并列汇率时直兑被顶回列表中间**。恰好等于直兑的绕路能过隐藏闸，然后按步数比赢了
   一步的直兑。直兑现在无条件垫底。`af862ef`
5. **不足 1% 显示成「0%」**（`要吃掉 5275（0%）`，实际 0.93%）。同上。`af862ef`

**还有四处需要你拍板，都围绕「已结算/理论/按市价」那三行**：

- **`已结算` 同一个输入会给两个答案**。卡片说「减到 169」，你真填 169，同一行从
  `169 → 26` 变成 `169 → 27`。因为它把推荐量放进**按你请求的量走出来的**那条路重算，
  而请求越大走得越深、混合比例越差。三个 agent 独立报了同一条。
- **`按市价` 把没换出去的本金也算成产出**。它是 `amount_out + 残余按市价折算`，而残余
  包含**第一腿没花掉的本金**。所以「按市价 50000 → 8614」其实大部分是"你那 5 万神聖石
  现在还值 8614"，不是"清仓能换到 8614"——却挂在"清仓价"标题下面。
- **`理论` 一直比 `按市价` 低**（79 对 83、7951 对 8614、508 对 530）。大概率就是上一条
  的镜像（按市价多算了残余），但「理论」低于「按市价」反直觉，标签没说清。
- **`建议减量 169` 和汇率行的 `市面能吃下 22` 差 7.7 倍**，两套容量模型，页面不说哪个
  是哪个；而且卡片底部那一整块到底在讲**哪一条路线**，现在也没写出来（排序改了以后，
  它讲的是汇率最好那条，不再是最不容易卡的那条）。

我的建议：这四条是同一个根——**清仓块整个建立在引擎那套随输入量变的逐档行走上**。
要么给它一个和上面一致的口径，要么把它明确降级成"参考，不是结论"。等你定。

另：`route_leg_coverage` 的"继承下一步判语"经查是**你自己定的规矩**
（`CORE-TRADING-MODEL.md`「第一条腿因此会继承下一条腿更差的判语」），不是 bug，
不动。但实机上它长这样：`市面挂着 273207，这一趟要吃掉 1122` 配红色
「比现有挂单还多——一次吃不完」，读起来是自相矛盾的。要不要改措辞你定。

## 25. 第 24 条那四个待拍板项：已由裁定一次性解决（2026-08-23）

用户裁定「清仓价这个逻辑就是大错特错了」，**整块撤销**——三档、建议减量、零头保本价、
底部风险 chip 全部摘除。第 24 条列的四个问题（已结算不可复现、按市价把本金算成产出、
理论低于按市价、两套容量数字打架）**随之全部消失，不再需要单独解决**。设计理由见
`CORE-TRADING-MODEL.md`「清仓价整块撤销」。`6ce7a42`

同时裁定「过度提醒」，分三刀砍完：①删掉报平安的「现有挂单够吃」chip `55b8b98`；
②删掉「要吃掉大半个盘口」整档——它警告的是吃单滑档，而本页按挂单汇率算，滑档不会发生
（`convert.leg_sweep_percent` 设置随之删除）`e697c9f`；③一条路线最多一行分步提醒，只说
卡在哪一步 `e72b093`。实测截图那张卡：提醒行 14 → 2。

**还没做的**（下一步优先级，我的建议顺序）：
1. 雷达页仍在用混合均价判盈亏，同一对可能和兑换页给出相反结论——现在兑换页三次改造都
   落定了，这是最刺眼的剩余不一致
2. 雷达「深度」列混单位（11 个兆 + 21809 混沌 = 21820），修了第一名会换人
3. 供需线校准（18 个通货里 16 个判「均衡」，等于关着）
4. 时段折叠（低峰期套利）、雷达详情接兑换评估、UI 密度
