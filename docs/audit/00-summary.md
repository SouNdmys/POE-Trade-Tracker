# 00 · POE Trade Tracker 1.0 发布前评审总结

评审日期 2026-09-02，基线 commit `8103b55`（main，工作区版本 1.0.7）。
五个领域由五个并行子 Agent 各自评审，完整发现在同目录：

| 领域 | 文件 | 主要方法 |
|---|---|---|
| A 前 10 分钟体验 | `A-first-ten-minutes.md` | 照 README 推演新用户两条路径（release zip / 源码编译） |
| B 盲目自信的错误输出 | `B-silent-wrong-outputs.md` | 五类故障场景逐一推演，grep 错误传播链 |
| C UI 状态与视觉层级 | `C-ui-states.md` | 13 页面 × 空/加载/报错/过时 矩阵 |
| D 代码健康度 | `D-code-health.md` | clippy / test / cargo tree / grep 计数，不读逻辑 |
| E 文档与实现偏差 | `E-docs-drift.md` | README、CLAUDE.md、docs/ 承诺逐条到代码核对 |

**核实边界**：没有任何 Agent 构建或运行过程序，结论来自文档、grep、clippy 与测试输出。
B 领域标"需验证"的 B-7（rollup 失败后 prune 会不会删掉未折叠的原始数据）由主 Agent
核实：`prune_raw_days` 逐天核对真实 rollup 行，缺一对就拒绝删那天，**不会丢数据**，
已从候选 Blocker 降为 Should-Fix。

---

## 1. 产品定位

POE Trade Tracker 是一款 Windows 单机桌面工具，面向《流亡黯道》（POE1/POE2）玩家。
它在玩家翻看游戏内通货交易所面板时，用屏幕 OCR 把每一页订单书读下来存进本地 SQLite，
再从这堆"你亲眼看过的书"里推算：每种通货相对你选定结算货币的估值、哪些通货稀缺哪些过剩、
你走过的交易对之间存在哪些多跳路线、来回一趟能剩多少。1.0.5 之后它还会按小时从官方
`web.poecdn.com` 拉取交易所历史成交，用同一套环路算法跑"大雷达"，并做赛季节奏分析。
它的设计原则写在 CORE-TRADING-MODEL 开头：**显示一个数字让人判断，胜过调一个阈值替人判断**。
技术上是 Rust + GPUI，18 个 crate，674 个测试，PolyForm Noncommercial 许可，作者自用为主。

从第三方角度看，它的工程底子已经是发布级的；差距集中在两个边缘：
**产品对自己的描述**（README 还停在没有网络功能的旧版本），以及
**出错时产品对用户说什么**（好几条故障链路的终点是一行会被下一条日志覆盖的状态文字）。

---

## 2. 已达交付标准的三项亮点（停止微调）

1. **OCR 摄入纪律：宁可跳过，不猜。** 精确匹配禁模糊、双读确认、行序不变量点名剔除单行、
   每侧中位带且落选原因可见；配合监视器健康带和 HUD 三态（过时数字降灰不抹掉、"8s 前"，
   待机与真跳过拆分并有测试守着）。这是全项目"数据过时"处理的范本。（B、C 一致认定）
2. **工程基线。** plain clippy 零 warning、674 个测试全绿且不靠 `[lints]` 拐杖；
   storage/settings 生产路径零 panic；release profile 已调满（lto / strip / opt-level=s，16 MB）；
   打包脚本 `--locked`、ONNX 1.28.0 固定 SHA、README 里的文件数与体积精确到个位。（D、E）
3. **诚实的安装与引导。** 安装/更新链路对"证明了什么、没证明什么"直说；应用内 OCR 识别器
   引导带 PowerShell 命令并点名 WSUS 静默失败；README 逐条 Win32 承诺（无 FindWindow /
   SendInput / PostMessage 等）全仓 grep 逐条成立；空状态都带下一步（"已有 12 根需要 288 根"）。（A、C、E）

---

## 3. Top 10 核心问题（按严重程度降序）

| # | 等级 | 领域 | 问题 |
|---|---|---|---|
| 1 | **Blocker** | A/E | **README 与应用内引导描述的是没有 Exchange 页的旧产品，且网络/隐私承诺已失实。** "no game API"、"nothing is fetched"、"the only network traffic is the updater"、"eight pages"、"Nothing is pruned by default" 对 1.0.5+ 全部为假：程序启动即向 `web.poecdn.com` 每小时同步，交易所小时表默认 14 天清理；`league`、backfill/retention/trend 设置项一个没提。作者已知（昨天刚做），但版本已发出去，仍算阻断。 |
| 2 | **Blocker** | B | **settings.json 损坏 → 静默回出厂默认，下次保存直接覆盖原文件，无备份。** `SettingsStore::load` 把"文件不存在"和"JSON 坏了"都归成 `Defaults`，shell 丢弃 `loaded.status`，启动即 save。标定区域、联赛名、关注名单一夜消失，界面和新装一模一样。`LoadStatus` 枚举已存在但没人读。 |
| 3 | **Blocker** | C/B/A | **联赛名到同步失败这整条链路都是哑的。** 联赛名是无示例、无校验、无选择器的自由文本，默认空 = 关闭；同步出错只 `push_log` 进底部状态行，下一条日志即覆盖；Exchange 页空状态文案在"联赛名拼错 / 429 限速 / 断网"三种情况下一模一样，只是"落后 N 小时"越来越大；永久性错误每 30s 无退避重试。你正要把 POE1 联赛改成 Allflame，这是唯一会核对它的地方。 |
| 4 | **Blocker** | B | **pulse 读失败 → 结构性警示静默消失。** `load_pulse` 全 `.ok()?`，页面照常渲染只是少了"结构性薄流动性"之类注记，方向是让路线看起来更安全。修法在 3 行内：把错误串进 notes。 |
| 5 | Should-Fix | A | **未校准就按"开始监视"不拒绝，静默套用 2560×1440 预设。** 三区域为 `None` 时不调 `set_region_override`，代码里没有任何桌面尺寸检查；其他分辨率用户只看到"跳过"计数。README "Nothing is read until framed" 因此不准确。 |
| 6 | Should-Fix | D | **任何 panic 都是"窗口无声消失"。** `panic = "abort"` + `windows_subsystem = "windows"` + 无 `panic::set_hook` + 无日志文件。这是 D 唯一建议发版前改的代码；不改，其余冷路径 panic（启动开窗失败、手改坏 HUD 坐标）全部表现为"双击没反应"。 |
| 7 | Should-Fix | B | **OCR 读到的库存数字没有任何合理性约束，前缀差一字的通货会被认成另一个。** 汇率有单调序 + 中位带，库存只有 `NoStock`；一位误读直接进覆盖度、流动性档位与雷达排序（流动性是第一排序键）。崇高石/完美崇高石这类"合法命中"落错对并污染历史，精确匹配挡不住。 |
| 8 | Should-Fix | C | **全局 chrome 不显示当前是 POE1 还是 POE2、哪个联赛、哪个赛季。** 顶部带只有呼吸点/监视中/已接受/已跳过。两游戏都在用时，切错档案的每个数字都"看起来对"。 |
| 9 | Should-Fix | C | **新鲜度与报错落点不齐。** 关注列表页没有任何"多久前"（其余五个数据页都有）；兑换页结论带与表格行没有"抓取于何时"；六个数据页加载失败以"故障: <Rust 英文原文>"当空状态，样式与"—"同灰同位置，中文界面也是英文。 |
| 10 | Should-Fix | B | **">3h 空小时永久标记"会把 API 发布延迟固化成永久空洞。** 实测官方通货历史 API 本来就延迟 1–2 小时；CDN 空回包/晚发布的小时被永久标空，趋势悄悄偏，文档自己也承认。 |

**11–16 位（未进前十但发版前值得看一眼）**：百分比三套格式、±999 截断只在交易所页、
"1214万"硬编码中文（C）；前置条件缺 VC++ 运行库与 Windows 最低版本、默认 profile 是
PoE2 繁中为作者而选、首次有价值输出的时间预期为零（A）；`licenses/` 缺 Lucide 与 gpui
通知（E）；热键默认值代码里有两个答案（E）；赛季读失败被当"无赛季"导致窗口不再钳制（B）；
日 rollup 失败被 `let _ =` 吞掉、历史悄悄变少（B，已核实不丢数据）。

---

## 4. 完成标准（Definition of Done）

宣称 1.0 正式完成前，以下全部满足：

1. README 删掉"无 API / 不联网 / 只有更新器联网"的表述，新增"数据来源"一节，写明每小时向
   `web.poecdn.com` 同步、同步什么、存在哪、默认保留多久；页面数改为 9 并补 Exchange 页说明。
2. README 与应用内首启引导（`guide_first_run` / `guide_pages`）都提到联赛名怎么填、填错会怎样，
   并给出 POE1 短名（如 Allflame）与 POE2 的示例各一个。
3. 设置文件加载区分"不存在"与"损坏"：损坏时保留 `.bak`、界面明示"设置已重置，备份在 X"，
   不静默覆盖。`LoadStatus` 要么接上要么删掉。
4. Exchange 页有"上次同步结果"落点：成功时间或失败原因 + 时间，且"联赛不存在 / 限速 /
   断网"三种原因可区分；永久性错误停止重试或退避。
5. 联赛名在设置页可校验（API 报错时已能列出数据里的真名，把它显示出来即可）。
6. pulse 读失败在页面上留一条注记，不再静默丢掉结构性警示。
7. 三区域未校准或桌面分辨率 ≠ 预设时，"开始监视"拒绝或明显警告，而不是套用 2560×1440。
8. 装 panic hook，把 panic 信息写到数据库旁的日志文件（至少弹一个消息框），让崩溃可报告。
9. 全局 chrome 常驻显示 游戏（POE1/POE2）+ 联赛名。
10. 每个数据页的核心数字旁有数据时间；加载失败有独立样式与双语文案，与空状态可区分。
11. 库存数字有合理性带或至少"库存未确认"标记；近名通货有消歧守卫。
12. 交易所账本：距今不足 API 延迟（≥ 3h 可调）的空小时不永久标空。
13. README 前置条件补 VC++ 2015–2022 运行库、Windows 最低版本；首启选 profile 而非默认繁中。
14. `licenses/` 补 Lucide（ISC）、gpui（Apache-2.0）与字体通知；`ptt-platform-win` 版本对齐
   workspace；RELEASE-NOTES 模板去掉 0.2.0 假想示例。
15. 发版 commit 上 `cargo fmt --all` 无 diff、plain clippy 零 warning、`cargo test --workspace`
   全绿；GitHub Releases 与工作区版本号一致。

---

## 5. 建议裁减的内容

维护成本高、对用户无实际价值，或"看似有防护实则无人读"的东西：

- **`Alt+F12` "capture one frame" 热键**：README 自认 "It does nothing today"，删掉比解释便宜。（A）
- **README 逐个 Win32 API 名的否定清单**：已在网络一条上过期，没有 CI 核对；缩成三句能长期
  为真的承诺。（A、E）
- **雷达页"可执行性"+"风险"两列（约 170px）**：稀薄期全表同值、充足期大多为空；折进事实带，
  把宽度还给路径列。（C）
- **`tuning.convert_sizes`**：已被 CORE-TRADING-MODEL §7 降级为默认候选，若兑换页总要求填
  持仓，它就是 24 个参数里的僵尸项。（C）
- **状态栏里的"ledger N hours in K ms"计时行**：会顶掉同步错误，它是开发指标不是用户信息。（B）
- **`docs/AUDIT.md`**（本次评审提示词）移入 `docs/audit/` 或删除；**UI-DESIGN.md 已否决的
  方案 B 段落**、**RELEASE-NOTES-TEMPLATE 的 0.2.0 假想示例** 删除。（E）
- **`ptt-ui-preview` 开发画廊（9.8 MB exe）**：确认打包清单已排除。（D）
- **不建议裁减**：D 提到 `ptt-ocr-win` 只被 ptt-recognition 依赖，但 README 明确
  Windows.Media.Ocr 是主引擎、PP-OCRv5 只是回退，它是生产路径，保留。

---

## 附：各领域自报的亮点与裁减，已合并进上文；重复项已去重。
