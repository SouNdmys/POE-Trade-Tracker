# A. 前 10 分钟体验 —— 1.0 发布前评审

评审视角：一个从未见过本项目、只照 README 走的新用户。两条路径分别推演：
**（甲）下载 release zip 直接用**；**（乙）clone 源码自己编译**。
终点都是"看到第一条有意义的输出"。

核对依据：README.md（最后一次修改 2026-08-28）、CLAUDE.md、docs/、`packaging/*.ps1`、
`ptt-settings` 默认值、`ptt-app/src/i18n.rs` 里的应用内指南文本、GitHub Releases 页
（截至 2026-09-02：v1.0.0–v1.0.5 六个版本，工作区已是 1.0.7）。

---

## 按严重程度排序的发现

### 1. README 与应用内指南描述的是"没有 Exchange 页"的旧产品，而 v1.0.5 已经带着官方 API 出货 —— Blocker

**具体现象。** `ptt-exchange-history` crate（提交 `72f796c`，2026-08-31）和 Exchange 页
（`8ba1100`）都包含在 tag v1.0.5 里。程序启动即向
`https://web.poecdn.com/api/currency-exchange/...` 抓取小时线（联赛名非空时自动开始，
14 天回补约 336 个请求）。但 README 仍然写着：

- 开头："There is no game API and no price site behind it"、"It shows that across eight pages"
  （实际 9 页，`Page::ALL` 长度为 9）；
- "What this does to your game client"："The only network traffic is the updater"；
- "First run" 七步流程与应用内 `guide_first_run` / `guide_pages` 均**不提** Exchange 页、
  不提联赛名设置，`guide_pages` 只列了 8 页。

**为什么用户会在意。** 两层伤害。其一，Exchange 是唯一**不需要校准、不需要 OCR 语言包、
不需要 2560×1440**、几分钟就能出第一份有价值输出的路径（i18n 原话："the sync fills in
on its own within minutes"），却在新用户唯一会读的两份文档里完全隐形——新用户被引导去走
最难的 OCR 路径。其二，"What this does to your game client" 是这份 README 里最像承诺的一节
（逐条列举没有哪些 Win32 调用、只有一条网络请求），现在对一个已发布的版本来说是**假的**。
一个谨慎的陌生人会因此不信整篇文档。

**严重程度：Blocker。** 不是功能缺陷，是发布物与说明书的一致性；发版前必须把 README 的
开头、页面数、网络流量声明、First run、应用内 `guide_first_run`/`guide_pages` 一并改到
1.0.7 的实际形态。

---

### 2. 联赛名是一个没有示例、没有校验、没有选择器的自由文本框 —— Should-Fix（高）

**具体现象。** `ExchangeTuning.league` 默认空字符串（= 关闭同步）。唯一入口是
Settings → Season & storage 里一个标签为 "exchange league (GGG name, empty = off)" 的输入框，
没有 placeholder、没有示例值、没有从 API 拉出的候选列表。填错只在跑完一整轮之后，以一行
日志报出："N hours stored but 0 league rows -- check the league name (leagues in the data: …)"。
这行日志显示在 shell 底部的状态条，而状态条只渲染 `self.log.back()`——**只有最后一行**，
下一条任何日志就会把它盖掉。

**为什么用户会在意。** 用户得自己知道 GGG 的精确字符串（POE2 是 "Runes of Aldur"，
POE1 联赛 id 是短名如 "Allflame"，硬核带 "HC " 前缀，大小写和空格都得对）。作者知道，
新用户不知道；填错之后程序表现得像"数据还在来的路上"（Exchange 页显示 "no exchange data
yet -- the sync fills in on its own within minutes"），要不是恰好瞥到那一行日志，会一直等下去。
这是 Exchange 路径的总开关，也是它唯一的隐性知识点。

**严重程度：Should-Fix。** 最低限度：输入框给一个真实示例作 placeholder，把"联赛名可疑"
从一次性日志升级为 Exchange 页的持续提示，并把 "leagues in the data" 列表直接摆在提示里
让用户点选。

---

### 3. 未校准就按"开始监视"不会拒绝，而是静默套用 2560×1440 预设 —— Should-Fix（高）

**具体现象。** `ProfileSettings` 三个区域都是 `Option`，默认 `None`。启动时
`shell/mod.rs` 只对 `Some` 的区域调用 `set_region_override`，`None` 就什么都不做——
识别路由于是回落到 `ptt-recognition/src/profiles/*.rs` 里内建的 2560×1440 工厂坐标。
整个 `ptt-app`/`ptt-vision`/`ptt-monitoring` 里没有 `GetSystemMetrics`/`SM_CXSCREEN`
之类的桌面尺寸检查，程序不知道也不问用户的屏幕是多大。

后果分两种：2560×1440 + 繁中 客户端的用户不校准也能读到东西（README 的 "Nothing is read
until the three regions are framed" 因此**不准确**）；其他分辨率的用户按下开始后，每一帧都被
身份闸门跳过，Monitor 页只显示"跳过"计数，理由是面板不在预期位置——不是"你还没校准"。

**为什么用户会在意。** README 和指南确实用粗体说了必须校准，但程序自己不说。一个 1920×1080
的用户跳过 README（大多数人都会）之后，看到的是一个"活着但什么都不读"的监视器，只能靠
`guide_trouble` 第一条反向推理。作者在 2560×1440 上从未撞到过这堵墙。

**严重程度：Should-Fix。** 最小修法：三个区域任一为 `None` 时，Monitor 页顶部给一行
"未校准，正在使用 2560×1440 预设"的持久提示；顺手把 README 那句改准。

---

### 4. 运行时前置条件没有写全：VC++ 运行库、Windows 最低版本 —— Should-Fix

**具体现象。** 仓库没有 `.cargo/config.toml`，Rust MSVC 目标默认动态链接 CRT，
`ptt-app.exe` 依赖 `vcruntime140.dll`；`fetch-onnxruntime.ps1` 拉的微软官方
`onnxruntime.dll` 同样依赖 `MSVCP140.dll`/`VCRUNTIME140.dll`。README 的 Requirements
只写了 "Windows, x64" 和 OCR 语言包，没提 VC++ 2015–2022 Redistributable，也没有最低
Windows 版本（`Windows.Media.Ocr` 需要 Windows 10+；GPUI 的 Windows 后端走 DirectX +
DirectComposition，同样 Windows 10+）。

**为什么用户会在意。** 干净的 Windows（新装机、虚拟机）上：exe 直接弹"找不到 VCRUNTIME140.dll"
——这个还算能搜；`onnxruntime.dll` 加载失败则只是 stderr 一句警告，而 exe 是 GUI 子系统
没有控制台，README 自己也承认 "You just see fewer currencies recognised"。用户看不到任何
能把"通货名少了"和"少装了一个运行库"连起来的线索。

**严重程度：Should-Fix。** Requirements 加两行（VC++ Redistributable x64、Windows 10 或更新）
就够；更好的是在 Settings → About 里显示 "ONNX fallback: loaded / not loaded (原因)"。

---

### 5. 默认 profile 是"PoE 2 + 繁體中文客户端"，为作者而选 —— Should-Fix

**具体现象。** `default_active_profile()` 返回 `Poe2 + TraditionalChinese`。没有首次启动
向导，Basics 段不会主动问。README 自己写道：错的 profile "reads nothing and never says why"。
繁中 profile 还额外要求 `zh-Hant-*` 的 Windows OCR 识别器；英文客户端用户的机器多半没装。

**为什么用户会在意。** GitHub 上的英文 README 面向的读者大概率玩英文客户端。他们不改
Basics 就开始监视，得到的是每帧 "OCR unavailable"（指南也承认这个错误信息"never names the
language it wanted"）。四个 profile 的语料量差异（51/26/12/6 帧）README 写得很诚实，但
默认值把语料最厚的那个给了作者自己，而不是把"必须先选"这一步摆在用户面前。

**严重程度：Should-Fix。** 首次启动（`profiles` 为空）时把 Basics 段置顶或弹一次选择；
或至少让 Monitor 在 profile 从未被显式保存过时给一行提示。

---

### 6. "多久能看到第一条有价值的输出"没有任何预期管理 —— Should-Fix

**具体现象。** README 的 First run 结束在 "Come back to the app. The watchlist and the
radar only know the pairs you flipped past."，没说要翻多少对、多久。实际约束（从代码和
docs 推）：Radar/Convert 需要至少三个已抓取的市场围成一个环；报表窗口默认 24 小时；
面板要静止约一秒才读一次；抓够第一条雷达路线通常意味着有目的地翻十几个交易对。
Exchange 路径则是：填联赛名 → 首轮"十几秒才有第一条进度"（代码注释原话）→ 数分钟内
回补完 14 天。这些数字**只存在于 i18n 字符串和源码注释里**。

**为什么用户会在意。** 新用户判断"这东西到底有没有在工作"的唯一依据就是时间预期。没有它，
OCR 路径上"翻了三个对什么都没有"会被理解为坏了；Exchange 路径上等 30 秒没数据也会被
理解为坏了。作者知道节奏，用户不知道。

**严重程度：Should-Fix。** README First run 末尾加一段"预期：Exchange 页几分钟；雷达需要
至少三个互相连通的交易对，翻完约十个对后回来看"。

---

### 7. 源码路径：跳过 ONNX 运行库这一步"不会让构建失败"，而 README 把这叫做陷阱却没在程序里堵上 —— Should-Fix

**具体现象。** `cargo run -p ptt-app` 不需要 `fetch-onnxruntime.ps1`；README 明确写
"Skipping that step does not break the build, which is the trap"，并说唯一信号是 GUI 进程
里看不见的 stderr 警告。`PTT_ONNXRUNTIME_DLL` 只在 `ptt-recognition/src/route.rs:481`
和 probe 里被读取，应用界面上没有任何地方显示 ONNX 后端是否加载成功。

**为什么用户会在意。** 从源码构建的人恰恰是最可能漏掉 PowerShell 那一步的人（README 给的
命令序列是对的，但四条里第三条是"可选看起来不可选"的）。漏掉后症状是"某些通货名识别不出来"，
和 OCR 语言包缺失、和校准偏移的症状**一模一样**，三个原因在界面上不可区分。

**严重程度：Should-Fix。** 与第 4 条同一个修法：About 段一行 ONNX 状态。

---

### 8. README 提到的 `book-probe --manifest` 语料回归，新 clone 跑不了 —— Nice-to-Have

**具体现象。** `tests/manifests/*.json` 在仓库里，但它们指向的截图被 `.gitignore`
（`tests/screenshots/**/*.png`）排除；`#[ignore]` 的那条测试要 `PTT_PRIVATE_SCREENSHOT_ROOT`。
README 说明了这一点（"driven by the book-probe --manifest binary … not by cargo test"、
"needs a private screenshot corpus"）。

**为什么用户会在意。** 想改识别代码的贡献者没有任何方式验证自己没弄坏四个 profile。
这是清楚说明了的限制，不是缺陷；记在这里是因为它意味着"两条基准线都过"不等于"识别没坏"。

**严重程度：Nice-to-Have。** 出于隐私不提交截图是对的；可以考虑放一两张打码的公开样张。

---

### 9. Windows 显示缩放（125% / 150%）对预设和截图校准的影响没有说明 —— Nice-to-Have（需实测）

**具体现象。** 预设坐标是"desktop-pixel"，`region_overlay.rs` 给自己的线程设了
Per-Monitor-V2 DPI 感知，捕获走 GDI `BitBlt`。README 和 P1 标定笔记都只说 2560×1440
"windowed fullscreen"，没提缩放比例。用户拿来校准的截图（Win+Shift+S 或游戏内截图）是物理
像素，而窗口化全屏的游戏渲染同样是物理像素——大概率没问题，但文档没说，用户也无从判断。

**为什么用户会在意。** 2560×1440 的机器开 125% 缩放很常见。如果预设在缩放下恰好也对，
一句话就能省掉用户的怀疑；如果不对，那第 3 条的静默跳过会更常发生。

**严重程度：Nice-to-Have。** 实测一次，在 Requirements 的 Display 段落写一句结论。

---

### 10. 发布页停在 v1.0.5，工作区已是 1.0.7（25 个 commit），README 描述的是哪一个？ —— Nice-to-Have

**具体现象。** Releases 页最新为 v1.0.5（2026-09-02），`Cargo.toml` 是 1.0.7，
`f61fde0 release: 1.0.7` 已提交未打 tag 未发包。装了 1.0.5 的用户，更新器会说
"this is the newest version"，而 docs/ 描述的 P12 小时账本、POE1 对齐都是他拿不到的。

**为什么用户会在意。** 第 1 条修好 README 之后，README 会描述 1.0.7；如果 tag 和 zip 不同步
跟上，README 又会领先发布物。这是发版流程的一致性问题，本身不难，但要有人记得。

**严重程度：Nice-to-Have**（发版清单项，不是产品缺陷）。

---

### 11. README 没有一张截图 —— Nice-to-Have

**具体现象。** README 里有一段 HTML 注释详细写了"应该放一张 Radar 页截图，放在
docs/screenshots/radar.png"，但 `docs/screenshots/` 不存在。27 KB 纯文字。

**为什么用户会在意。** 这是一个 GUI 工具；一个陌生人决定要不要花 10 分钟装它，第一眼
看的是图。作者显然知道（注释写得比很多项目的 README 还认真），只是没做。

**严重程度：Nice-to-Have。**

---

## 已达交付标准的亮点（不必再微调）

1. **安装 / 更新链路的诚实度。** "不要解压到 Program Files"、更新器先探测可写性再动手、
   `MANIFEST.json` 只证明完整不证明来源、`.old` 换名再清扫、更新后禁止启动新监视直到重启——
   每一条都把"它证明了什么、没证明什么"说清楚了。README 的 Install/Updates/Where your data
   lives 三节可以直接作为同类工具的范本。
2. **OCR 识别器的引导。** 应用内 "How to use" 段把"显示语言 ≠ OCR 功能"、繁中六个 tag 哪些
   算数、简中不算、WSUS/组策略下安装按钮为什么静默失败、三条 PowerShell 命令（查/列/装）
   全写了，中英双语。这是新用户最容易卡住的坑，而它被填得比 Microsoft 自己的文档还细。
3. **构建与打包脚本的可复现性。** `fetch-onnxruntime.ps1` 固定版本 + 在 scratch 目录校验
   SHA-256 后才落盘 + 幂等；`package-preview.ps1` 用显式 allow-list 而不是目录拷贝、
   从 `assets.rs` 读 pin 而不是复制一份、断言运行时布局、55 MiB 预算；两者都处理了
   PS 5.1 的 `$PSScriptRoot` 时序坑。一次写对，不用再动。

## 建议裁减

- **`Alt+F12` "capture one frame" 热键行。** README 原话："Nothing binds it. It does nothing
  today." Overlay 段画着一个不做任何事的快捷键，`Hotkeys.manual_capture` 还持久化到
  settings.json。一个死 UI 项加一段解释它为什么是死的文档，比删掉它成本高。
- **README "What this does to your game client" 里逐个 API 名的否定清单**（`FindWindow`、
  `SendInput`、`WH_MOUSE_LL` 等）。它已经在网络流量那一条上过期了，而且每加一个功能都要
  有人记得回来核对整份清单。要么缩成三句能长期为真的承诺（不碰进程、不注入输入、网络请求
  只有更新器和 poecdn 只读拉取），要么用 grep 在 CI 里自动核对——但仓库没有 CI。
