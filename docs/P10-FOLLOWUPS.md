# P10 遗留清单（2026-08-22）

P10 云端评审之后剩下的东西，加上首次实机测试的反馈（第 6 条起），
给下一个接手的会话。
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

---

# 实机反馈清单（2026-08-23 首次实机测试）

来源：用户 `bug 2026-08-23.md` + 六张截图。下面都是**需要商讨或设计**才能动的，
当场能修的（设置页参数说明）已经落在 `df4aff0`。

## 6. 雷达表格在窄窗口整片空白（已修 2026-08-23，与第 15 条同一次改动）

"宽度不够就不画"的假设被实验**证伪**：ptt-ui-preview 画廊拉到 1100px（表格可用宽
约 1078 < 列宽总和 1250）时所有行照常渲染，只是右侧被裁；`PTT_PREVIEW_PROBE=1`
在 1100/1500 宽下 cells_drawn 完全相同。上游（gpui-component 0.5.1
`virtual_list.rs:627-687`）宽度退化时是"全画"，不存在"返回空"的分支。

剩下两个嫌疑，按序排查：

1. **页面外壳缺防溢出**：`opportunities.rs:454`（body）与 `:458`（页面根）都没有
   `min_w(px(0.))` / `overflow_hidden`，而画廊（`preview.rs:487-495`）有。gpui 文本
   的最小内容宽 = 不换行整行宽（见第 7 条），缺这一层时窄窗口下页面整体向右溢出
   而不是重排。
2. **每次扫描 refresh() 把列宽清零一帧**：`sync_radar_table`（`opportunities.rs:303`）
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
  上更值。
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

## 7. 路线明细内容被右边界裁掉（已修 2026-08-23）

两层根因：**gpui 文本的最小内容宽 = 不换行的整行宽**（gpui-0.2.2
`text.rs:347-376`，MinContent 与 MaxContent 量出同一个值的缓存短路）；`kv_row`
的值容器是 `flex_1` 但没有 `min_w(0)`（`ui.rs:725-731`），于是永远压不进面板、
永远拿不到确定宽度、永远不换行，还把 340px 的 `detail_panel`（`ui.rs:700`）撑宽
——右边被窗口边界裁掉而不是被面板边框裁掉。

修法：`ui.rs:725` 的值 div 加 `.min_w(px(0.))`，`detail_panel` 同加。**选换行**，
不选截断/横滚——`kv_row` 的文档注释（`ui.rs:707-710`）本来就写着"值会长到两三行"，
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

## 9. 兑换页负收益路线（排序键已修 2026-08-23；对照行与基准半仍未做）

从真实库逐位复算，截图上每个数字全部对上。三部分：

- **数据半（诚实的部分）**：直兑 taker 深度只有 1855 神聖石（5000 的 37%）。
  "可见深度 37404"是**对侧**竞争方挂单（你的对手在卖，不是你能吃的买单），
  同页并排不加区分是误读的直接来源。
- **bug 半（主因）**：两条路都部分成交时 `compare_paths`
  （ptt-trade-engine `route.rs:413-421`）按 amount_out **绝对值**排序，
  "单价差 20% 但烧掉更多本金"的绕路胜出；按市价总值它比直兑少 3451 混沌。
  修法：部分成交时按已实现单价（amount_out/consumed_input）或按市价总值排。
  §5 的"排序不重开"裁定管的是雷达路线排序，不覆盖这里。
- **基准半**："比直兑低"的基准是直兑前 1855 个的均价**线性外推**到任意投入
  （`route_accounting.rs:493-508` 的 simulate），是个不可达的数，违反
  CORE-TRADING-MODEL"快速价是保守下界"的约定；且路线自己的外推有旗标、
  基准的外推是静默的。engine 的 compare_best_to_direct 在双部分成交时本来
  会拒绝比较（IncomparableCoverage），convert_model（`reports.rs:503-534`）
  没用它，绕过了这道保护。

改法顺序（待拍板）：先修排序键 → 把直兑作为对照行并排列出（让"直兑只能吃
1855"这个真约束可见）→ 文案说明绕路代价。单独只做"低于直兑就折叠"会掩盖问题。

**已修（只做了第一步：排序键）**：`compare_paths` 的第三个键从 amount_out 绝对值
换成**已实现单价** = `amount_out / 首腿 consumed_input`。比大小走交叉相乘
（`left.out * right.spent` 对 `right.out * left.spent`），全程整数：两个 u64 拓宽成
u128 相乘，u64 的平方必定装得下 u128，所以这里根本没有溢出分支、更没有饱和；也不做
除法，免得整数除把两个不同的比率抹成平局（8.3 的"f64 只在绘图边界"照旧）。分母取
**首腿**的 consumed_input——后面每一腿花的都是首腿买回来的东西，所以"请求量 − 首腿
剩余"就是用户手里那种资产的全部账单。

**为什么选已实现单价而不是按市价总值**：市价总值要给残余（没花掉的本金、卡在中间
资产里的存货）定价，而 `compare_paths` 是个只看两条 `ConversionPath` 的纯比较函数；
往排序里塞价格预言机就得把 mark rate 灌进 engine 层，而 mark rate 现在住在
ptt-strategy 的 `route_accounting`，engine 有意不持有它。何况"没花掉的本金按自己市价
折算"正好又要用上本条第三段点名不可信的那个外推基准。已实现单价尺度无关，问的正是
实机答错的那个问题："每花掉一枚本金换回多少"。代价是卡在中间资产的残余被记作零价值，
方向保守，可接受。

**吃满的路线之间的序没变**：两条都吃满就都花了请求量，分母相同，比值退化成原来的
amount_out 比较。回归测试在 `route.rs` 底部新的 `compare_paths_tests`：
`a_partial_route_that_burns_more_capital_for_a_worse_price_does_not_win`（改前红，
数字照搬实机——直兑吃 1855 出 20030、绕路吃 2526 出 21786，改前绕路排第一），
外加 `two_fully_filled_routes_still_rank_by_absolute_output` 守住吃满那一半。

**影响面**：雷达页不受影响。`radar.rs:406` 对 `!best.is_fully_filled` 的结果直接
`continue` 并改推探针，部分成交的 best 进不了列表；吃满的 best 选谁没变，所以 §5
「流动性 > 利润 > 跳数」的 `compare_items` 输入一个都没动。雷达侧唯一可能变的是
全员部分成交时被点名去确认的那条路线（`confirm_conversion_probe` 的 notes 里那个
残余条数），推不推、推给谁那一对资产都不变。兑换页会变：某些尺寸的最佳路线换成
单价更好的那条，`derive_route_accounting` 的数字随之改变。

**这次没动的**：`compare_best_to_direct` 的 IncomparableCoverage 保护、
`convert_model`（`reports.rs:503-534`）绕过它这件事、`route_accounting.rs:493-508`
的直兑线性外推，以及展示层的对照行与文案，全部留给后续。

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

## 13. 选中行被高亮涂掉（已修 2026-08-23）

选中高亮是行顶层一块带背景的绝对定位元素（gpui-component `state.rs:1082-1099`）。
上游主题装载会把 table_active 的透明度夹到 ≤0.2（`schema.rs:636-638`），但我们直接
写 colors 结构体绕过了夹取：`theme.rs:257` 的 table_active = SELECTED（0xEFE9DE，
alpha=1.0）→ 不透明实底盖住整行文字。`theme.rs:249` 的 list_active 同病（树/列表
控件也会中招）。右键高亮只画边框不画底，所以右键没这现象——反向印证。

修法：两处改成低透明度 wash（如 rgba 0xEFE9DE33），选中感由 table_active_border
承担。测试：断言 apply_ledger_theme 后 `table_active.a <= 0.2`，先红后修——四个
UI 问题里唯一能用普通 cargo test 钉住的。

**已修**：新常量 `SELECTED_WASH_ALPHA = 0.2`，`table_active` / `list_active` 都改成
`hsla_of(SELECTED).alpha(SELECTED_WASH_ALPHA)`。写 colors 结构体的那一段拆成
`apply_ledger_colors(&mut ThemeColor)`（`apply_ledger_theme` 要 `App`，拆开才测得到），
回归测试 `active_row_highlights_stay_translucent`（`theme.rs` 的 `theme_tests`）。

## 14. 扫描后选中索引不重映射（已修 2026-08-23）

`set_rows`（`opportunities.rs:127-140`）只换 rows 不动 selected_row；每次扫描后
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

## 15. 路径列宽方案（已修 2026-08-23，与第 6 条同一次改动）

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

## 16. 确认探针是恒定噪音，挤掉了有用建议（已修 2026-08-23）

实机那四条"确认一个机会"全部来自三角腿分支（`radar.rs:491-512`），不是部分成交
分支（本次扫描 29+6+1=36 全对上，confirm_conversion_probe 一条没推）。根因：
`product_execution_allowed` 硬编码 false（ptt-market-book `lib.rs:504`，任何设置
改不了）→ triangle.execution_eligible 恒 false → **每个盈利闭环无条件推腿探针**。
重抓不改变结论，100% 恒定噪音。真伤害：噪音沾结算通货的高流转 boost 升到 High，
加 take(4)，把 6 条"这对你从没抓过"（MissingForwardQuote）永久挤出屏幕。

方案 A（倾向）：三角所有腿都在 fresh 窗口内就不推（capture_time_evidence 已存在，
不用改签名），先写红测试（全 Fresh 盈利闭环 → probe_candidates 为空）。备选 A'：
take(4) 前按 reason 保槽。只改文案（B）不解决挤占，不单独用。

**已修（方案 A）**：腿探针的条件由 `!execution_eligible` 收紧成
`!execution_eligible && !legs_all_fresh(...)`。新私有函数 `legs_all_fresh`
（`crates/ptt-workflows/src/radar.rs`）读 `triangle.capture_time_evidence` 里
**最早**那一腿的抓取时间，按 `selection.policy.freshness` 分档，只有落在 Fresh 才
算全新鲜——保守方向是"最旧的腿都新鲜才算数"，没有抓取时间戳的一律不算新鲜（无戳
的走线正是一次抓取能了结的情形）。`run_opportunity_radar` 的签名没动；`now` 在三角
循环前取一次 `Utc::now()`，同一批里共享一条腿的两个闭环不会对"这腿还新不新鲜"给出
两个答案。硬编码的 `product_execution_allowed` 没动，它仍恒为 false——这条修的是
"推不推探针"，不是"能不能执行"。§5 的雷达排序与 2026-08-23 的探针分级都没碰。

回归测试在 `crates/ptt-workflows/tests/radar.rs`：
`a_fully_fresh_profitable_loop_is_not_filed_for_confirmation`（改前红，三条腿全被
推），对照 `a_stale_legged_profitable_loop_is_still_filed_for_confirmation`（腿龄
30 分钟，探针照推，证明不是把功能整个关掉）。两条共用新 fixture `loop_selection`，
它把 `product_execution_allowed` 留在出厂的 false，复现的就是实机那个恒 false 的
条件。

## 17. 估值恒定取两侧最薄档（已修 2026-08-23）

三段叠加：`reports.rs:377` 把全部候选边都装进 selected；Instant 候选按 stock 降序
（market-book `lib.rs:901-914`，本批置信度全同所以排序实际就是库存序）；
`anchor_value.rs:100-112` best_rate 用 max_by，captured_at 全平局时返回**最后一个**
= 库存最小档。实测四个方向全部选中最薄档（鎖骨买价来自 stock=2 的挂单，stock=99
的被忽略）。估值被最容易挂错价、最容易消失的行系统性决定。修向：平局取队首
（最深档）。先红测试。

**已修**：`best_rate` 的比较键从「taker 标志 → captured_at」加成
「taker 标志 → captured_at → stock」，`max_by` 于是在前两键平局时挑库存最深的那档。
没有改成「取队首」的写法：依赖迭代器顺序等于把结论押在上游 Instant 排序不变上，
显式比 stock 才让「平局取最深」这件事在代码里读得出来，也顺带不受候选顺序变化影响。
第一键与第二键的语义没动（能吃的优先、新的优先），只是给平局补了个决胜键——实机
一本书 12 条边共用一个 captured_at，所以决胜键才是真正拍板的那个。

回归测试 `a_valuation_reads_the_deepest_level_when_the_whole_book_shares_one_capture`
（`anchor_value.rs` 的 `mod tests`），数据就是 2026-08-23 那本 ancient-clavicle 的
taker 阶梯原样搬过来。改前红：卖价取 41:1（stock=41）、买价取 47:1（stock=2）——
和实机诊断逐位对上；改后取 44:1（stock=3740）与 49:1（stock=99）。

影响面只有关注页的估值列（`value_against_anchor` 全仓只被 `watchlist_model`
调用一处）。市场分析页的价值列走的是 rollup 价格序列（`market_analytics.rs:392-410`），
不经过这条路径，数字不变。

## 18. 赛季边界的三个潜在隐患（今天不触发，记账）

- `today_window` 从当天 00:00 读起，不过 clamp_to_season——当天中午开新赛季，
  上午开赛前的数据会混进今日折叠。
- rollup 侧赛季地板只精确到"天"（from_day 取 started_at 的日期），开赛当天若存在
  日汇总会整天计入（含开赛前半天）。
- purge_before_active_season 清 raw 后 earliest_capture_day 前移，开赛日永远不会
  被 rollup、也不拿 mark（本次无损，耦合不显眼）。

## 19. bp 统一成百分比（已修 2026-08-23）

雷达页已经显示百分比（`opportunities.rs:60-68`），兑换页显示 bp——同一个量两种
写法。方案：全局两位小数百分比（整数运算无损，-1338bp → -13.38%），兑换页 tier
行叠一句实物说明。全部出现位置：report_text.rs 8 处成对字段、i18n.rs 7 处设置页
标签、`analytics.rs:34-40`、`history.rs:99/102/157`（后两处不在 i18n，改时容易漏）。
设置页输入单位（填 bp）单独一次改动，不与显示层混。

**已修**：新增共用格式化函数 `percent_from_basis_points`（`report_text.rs`），
所有显示点改走它。整数运算：100bp 正好等于 1%，所以拆成整数百分位加两位余数，
不引入浮点，也没有可四舍五入的东西。**符号单独写，不交给除法** —— 比 1% 小的数
整除出来的商是 0，而 0 不带符号，照抄雷达页原来那句 `(points % 100).abs()` 会把
-1bp 印成 `0.01%`（大小对、方向错）。这正是精度测试里最先红的那个断言。

改到的显示点：`report_text.rs` 8 对模板（英中各一份）去掉 "bp" 字样，改由传进来
的值自带 "%"；`reports.rs` 的 tier_line、maker_improvement、maker_spread、
maker_over_taker、雷达行、历史异常行、广度中位、锚交叉漂移；`opportunities.rs`
的 `edge_text` 与 scan_accounting 的收益门槛；`convert.rs` 的 tier 判词、
maker_improvement、maker_spread；`analytics.rs` 的 `signed_bps` 改名
`signed_percent`（保留正号：那几列是涨跌幅，不是水位）；`history.rs` 的波动幅度、
挂单高于吃单、异常幅度。原本重复了两遍的那句内联写法（`opportunities.rs` 与
`reports.rs:1471` 的雷达行）一并收敛进共用函数。

`reports.rs:1754`（历史异常行的文本版）不在原清单里，一起改了：它和
`history.rs:157` 是同一个数字的页面／文本双胞胎，只改一边就正好是 CLAUDE.md
点名的那种页面与报表漂移。

回归测试在 `report_text.rs` 底部新的 `percentage_tests`：
`no_report_template_prints_basis_points`（遍历 `report_pairs()`，两种语言都不许
再出现 "bp"）、`the_worse_than_direct_line_reads_as_a_percentage`（实机那条
-1338bp → `-3451 (-13.38% vs direct)`）、
`basis_points_convert_to_two_decimals_without_loss`（0、±1、±99、100、-1338、
10000 的逐字符断言，钉住负号那一格）。三条改前全红。

**这次没动的**：`i18n.rs` 那 7 处设置页标签仍写 bp —— 那是**输入单位**，用户填
进去的就是 bp；显示层改百分比而输入跟着改会错位，输入单位的迁移牵扯 ptt-settings
的字段语义，是独立一次改动。作为补偿，六条 `tuning_*_note` 的说明里都写上了
"100bp = 1%"，`tuning_min_bps_note` 另外点明"这里填 bp，页面上显示成百分比"。
兑换页 tier 行的实物说明（"每 100 神聖石少换约 13 混沌"）没做。
`analysis.rs:102` 与 `bin/engine_replay_probe.rs:378` 的 `profit={}bp` 也没动：
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

