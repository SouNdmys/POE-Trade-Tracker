# E. 文档与实际实现的偏差（1.0 发布前评审，2026-09-02）

范围：README.md、CLAUDE.md、docs/CORE-TRADING-MODEL.md、docs/P10-FOLLOWUPS.md、docs/UI-DESIGN.md、
docs/RELEASE-NOTES-TEMPLATE.md、packaging/、LICENSE.md、licenses/。方法：只 grep 常量名 / 函数名 /
字符串，不构建不运行。每条格式：**现象 / 为什么用户在意 / 严重程度**，并标注偏差方向
（文档超前 · 代码超前 · 互相矛盾）。

---

## 1. README 的网络与隐私承诺已经失实 — **Blocker**（代码超前）

**现象**：README 在三处做了硬承诺——开头 "There is no game API and no price site behind it ...
nothing is fetched"；「What this does to your game client」一节 "The only network traffic is the
updater: a GET to api.github.com ... once per launch"；「Where your data lives」"No server, no
account ...". 代码实际：`crates/ptt-exchange-history/src/fetch.rs:47-48` 向
`https://web.poecdn.com/api/currency-exchange/...` 拉官方成交历史；
`crates/ptt-app/src/shell/exchange_sync.rs:123-129, 379` 是一个常驻后台定时器（每整点、最短 60 s
一轮）自动同步；`crates/ptt-app/Cargo.toml:43` 与 `crates/ptt-runtime/Cargo.toml:14` 都依赖它。
请求带显式 User-Agent（`fetch.rs:11, 81`），匿名、无 POST/PUT（全仓 grep 无命中）——所以"数据不
离开本机"仍成立，但"唯一的网络流量是更新器"已经不对。

**为什么用户在意**：这一节是 README 里最像"安全声明"的部分，写法是逐条否定（"no X, no Y"）。任何人
用防火墙一看就能发现 poecdn 流量，此时整节的可信度归零，连那些确实成立的承诺（无进程注入、无键盘钩子）
也会被怀疑。

---

## 2. README 完全没有 Exchange 页及其配置 — **Blocker**（代码超前）

**现象**：README "It shows that across eight pages"，「What each page answers」表 8 行；代码
`crates/ptt-app/src/shell/mod.rs:67-77` 的 `Page` 有 9 个变体，含 `Exchange`。1.0.7 的 release
commit 主题就是 "POE1 parity for the Exchange page"。与之配套的设置项 README 一个都没提：
`league`（`crates/ptt-settings/src/lib.rs:551`，默认空串）、`exchange_backfill_days = 14`、
`hour_retention_days = 14`、`trend_days = 7`（`lib.rs:570-580`）。

**为什么用户在意**：联赛名不填、填错，Exchange 页就是空的。代码甚至专门做了"联赛名可疑"诊断并列出
CDN 里实际出现的联赛名（`exchange_sync.rs:146-151`：POE1 3.29 在 CDN 叫 "Allflame" 而不是
"Curse of the Allflame"）——说明作者知道这是坑，但 README 的「First run」七步里没有第八步。
用户记忆里"POE1 联赛名应为 Allflame"与代码别名表一致，与 README 无法比对（README 未提）。

---

## 3. "默认什么都不清理" 只对一半数据成立 — **Should-Fix**（互相矛盾）

**现象**：README "Nothing is pruned by default: raw retention is 0 days"。抓取侧确实
`raw_retention_days: 0`（`lib.rs:537`），但交易所小时表默认 14 天清理
（`default_exchange_hour_retention_days` = 14，`lib.rs:575`），且 `exchange_sync.rs` 每轮返回
`days_pruned`。

**为什么用户在意**：用户按 README 以为小时级成交数据全季保留，两周后发现赛季初的小时数据没了，
且没有任何设置页面之外的提示。

---

## 4. "no clipboard access" 不成立 — **Should-Fix**（代码超前）

**现象**：README 「What this does to your game client」末尾 "No registry writes ... no clipboard
access". 代码 `crates/ptt-app/src/shell/pages/settings.rs:358` 用
`gpui_component::clipboard::Clipboard` 给「How to use」里的 PowerShell 命令加了复制按钮。

**为什么用户在意**：是用户点击才写剪贴板，行为无害；但这一句出现在逐条否定式的安全声明里，一条不实
就拖累整段。改措辞为"只在你点复制按钮时写剪贴板"即可。

---

## 5. crate 地图漏了 `ptt-exchange-history`；`ptt-runtime` 的 description 仍是骨架时代 — **Should-Fix**（代码超前）

**现象**：`crates/` 下 18 个 crate，CLAUDE.md 与 README 的 crate 表都是 17 个，缺
`ptt-exchange-history`（GGG 官方通货交易所历史 API 的解析/拉取/映射，`src/lib.rs:1-7`）。
CLAUDE.md 首行"POE2 交易追踪工具"也已是 POE1 + POE2。另外 P10-FOLLOWUPS #4 记的
`crates/ptt-runtime/Cargo.toml:8` description "Background actor runtime ... (skeleton; full port
lands in P2)" 至今没改——文档说"顺手改掉"，代码没改，两边一致地过时。地图里其余 17 个 crate 都存在，
职责描述抽查（ptt-runtime、ptt-app、ptt-strategy）仍准确。

**为什么用户在意**：CLAUDE.md 是给协作 agent 看的入口，漏一个 crate 意味着后续会话不知道 API
拉取逻辑住在哪里，会去错地方改；`cargo` 元数据里的 description 会出现在任何依赖图工具里。

---

## 6. UI-DESIGN.md 的窗口尺寸与设置页结构落后于代码 — **Should-Fix**（互相矛盾）

**现象**：UI-DESIGN §1.4 "主窗口按 **1280×800** 设计与验收。所有列宽预算在这个宽度下成立"，§3
列宽预算也按 1280；代码 `crates/ptt-app/src/lib.rs:31 WORKBENCH_SIZE = (1180.0, 640.0)`，README
同样写 1180×640。设置页：UI-DESIGN §10 说"四段：基本 / 浮窗 / 赛季与存储 / 算法参数"，README 列出六项
（Basics、overlay、season & storage、algorithm numbers、usage guide、About）。其余几何常量一致：
表格行高 28（`theme.rs:558 H_TABLE_ROW`）、左导航 108（`theme.rs:568 W_NAV`）、明细栏 300
（`theme.rs:570 W_DETAIL`）、字体 YaHei UI / Cascadia Mono / Consolas（`theme.rs:519-521`）。

**为什么用户在意**：UI-DESIGN 自称"决策记录"，列宽预算是它最具体的内容；预算基准宽度和实际不符，
后续任何按文档做的排版都会在真实窗口下溢出。

---

## 7. 热键默认值在代码内部有两个答案 — **Should-Fix**（互相矛盾）

**现象**：README "Ctrl+Alt+F10 start/stop watching ... Anything unrecognised is normalised to the
default and written back"。`crates/ptt-settings/src/lib.rs:78` 默认 `"Ctrl+Alt+F10"`（与 README
一致），但 `crates/ptt-platform-win/src/hotkeys.rs:132`
`StartMonitoringHotKey::DEFAULT_SETTING_VALUE = "Ctrl+Shift+F10"`，注释写明是 ".NET 1.0" 遗留。

**为什么用户在意**：如果归一化路径走的是 platform-win 这个常量，用户手改 settings.json 写错一次，
写回的"默认"会是 Ctrl+Shift+F10，而 README 和设置页都说 Ctrl+Alt+F10——用户会以为热键坏了。
本评审未追踪归一化调用链，需要作者确认哪个常量真正生效。

---

## 8. 第三方许可通知不齐 — **Should-Fix**（发布合规）

**现象**：README「Acknowledgements」承认使用 Lucide（ISC，两枚 SVG 图标）与 gpui / gpui-component
（Apache-2.0），但 `licenses/` 只有 ONNX Runtime 两个文件与 PaddlePaddle 一个文件；
`packaging/package-preview.ps1:139` 只打包该目录。ISC 与 Apache-2.0 都要求随分发附带版权/许可声明。
README 对此是诚实的（"ships LICENSE.md plus notices for ONNX Runtime and PaddlePaddle"），所以不算
"许可承诺不实"，但缺口本身存在；README 还声称 499 个依赖全是宽松许可，却没有任何 THIRD-PARTY 汇总文件
能让人核对这句话。PolyForm 本体 `LICENSE.md` 齐全，SPDX 标识与 README 一致。

**为什么用户在意**：这是 1.0 对外发布最容易被外人挑出来的形式问题，而修复只是把两段许可文本放进
`licenses/`。

---

## 9. README 的环境变量清单与测试描述不全 — **Nice-to-Have**（代码超前）

**现象**：README 列了 5 个环境变量；代码另有 `PTT_LIGHT`、`PTT_POE`、`PTT_PREVIEW_PROBE`、
`PTT_PRIVATE_SCREENSHOT_ROOT`（全仓 grep）。README "The single test that needs a private screenshot
corpus is `#[ignore]`d"——实际 `#[ignore` 2 处。README 表格里 "PoE 1 · English 12 frames" 与同行
"ten real screenshots" 数字互相打架。

**为什么用户在意**：从源码构建的人会用 `PTT_POE` / `PTT_LIGHT` 切游戏和主题做验证，文档不写就得
自己 grep。

---

## 10. 界面文本约定有零星违例，且约定措辞与实际分层不符 — **Nice-to-Have**

**现象**：CLAUDE.md "界面文本全部走 `report_text::pick`"。实际：ptt-runtime 走 `report_text`，
ptt-app 走 `src/i18n.rs` 的双语表（546 处 CJK 命中都在表内，合规）。真正内联的中文只有三处：
`crates/ptt-app/src/shell/mod.rs:2367 "面板没开着"`、`shell/pages/settings.rs:714 "繁中"`、
`shell/pages/analytics.rs:51 format!("{}万", ...)`。`reports.rs` 的 43 处命中全是测试断言。

**为什么用户在意**：英文界面下这三处会冒出中文；"万"的量级单位在英文模式下尤其突兀。

---

## 11. P10-FOLLOWUPS 的状态标记：一处代码已超前 — **Nice-to-Have**（代码超前）

**现象**：#11 "赛季开始时间不可调（设计变更，待讨论）"，文中说 UI 硬写 `Utc::now()`。现在
`crates/ptt-app/src/shell/pages/season.rs:245, 341` 已有 `season_boundary(cx)`，只在没有输入时才
回退到 `Utc::now()`——看起来已部分落地，清单没更新。#10（覆盖缺口"忽略"）`ptt-workflows/src` 无
coverage 模块，按未做处理，与清单一致。#4 未修，与清单一致。抽查的"已修"项——#13
`SELECTED_WASH_ALPHA = 0.2`（`theme.rs:110`）、#14 `remap_selection`（`opportunities.rs:499`）、
#19 `percent_from_basis_points`（`report_text.rs:418`）——全部能在代码里找到，"已修"标记可信。

**为什么用户在意**：清单是"下一步做什么"的依据，标"待讨论"的项其实做了一半，会重复排期。

---

## 12. 发版元数据小漂移 — **Nice-to-Have**

**现象**：workspace `version = "1.0.7"`（`Cargo.toml:25`）且各 crate `version.workspace = true`，
唯 `crates/ptt-platform-win/Cargo.toml:3 version = "0.1.0"` 没跟。README 不写版本号（好）。
`docs/RELEASE-NOTES-TEMPLATE.md:67-90` 示例仍是"假想的 0.2.0"、文件名 `-preview.zip`，
而打包脚本产物也叫 `-preview.zip`——1.0 正式版还带 preview 后缀，要么改脚本要么改说法。
README "Alt+F12 capture one frame ... It does nothing today" 与代码一致（`lib.rs:84` 默认存在该键）：
文档诚实，但一个明确不工作的按键仍出现在设置界面。

---

## 13. CORE-TRADING-MODEL 数值抽查：全部一致（记录，无偏差）

抽查 14 条，均与代码一致：新鲜度 绿 7200 / 黄 21600 / 跨腿 3600（`ptt-settings lib.rs:943-945`）；
中位基线 ≥ 3 行（`ptt-market-book lib.rs:864`）；冷清下限 10、thin_norm 25%、供需倍率 300%
（`lib.rs:517-525`）；max_hops 3、小雷达环长默认 6 clamp(3,12)（`reports.rs:2353`）；大雷达 120 资产
（`reports.rs:4373`）、环长 clamp(3,4)（`reports.rs:4407`）、放量需 ≥ 8 小时自史
（`exchange_pulse.rs:259`）、policy id `exchange-vwap-hourly`（`exchange_radar.rs:40`）；文档说
"app 按水位缓存"与 `shell/mod.rs:410 exchange_radar_cache` 一致；P12 列出的 12 个函数名全部存在；
`exchange_probe.rs` 调用的 14 个生产函数全部存在（`pub fn` 命中）。唯一没定位到的是大雷达三阈值
300% / 1000 bps / 800 bps 的常量名——settings 里没有同名项，可能硬编码在 `exchange_pulse.rs`，
未继续追。文档自己也标注"首过值，等新赛季校准"。

---

## 已达交付标准的亮点

1. **README「What this does to your game client」的 Win32 层承诺全部经得起 grep**：`FindWindow` /
   `GetForegroundWindow` / `EnumWindows` / `OpenProcess` / `ReadProcessMemory` / `SendInput` /
   `keybd_event` / `PostMessage` / `SendMessage` 全仓零命中；`WH_MOUSE_LL` 只在 `ptt-platform-win`
   的 `mouse_hook.rs` / `self_test.rs`，ptt-app、ptt-runtime、ptt-monitoring 无任何调用；
   `RegisterHotKey` 只在 hotkeys.rs 与 backend.rs。这一节除了 #1、#4 两句以外可以原样保留。
2. **打包与构建描述与脚本严丝合缝**：9 文件 = exe + dll + LICENSE.md + MANIFEST.json + `assets\`(2)
   + `licenses\`(3)；55 MiB 上限（`package-preview.ps1:172`）；ONNX Runtime 1.28.0 + 提取后 DLL 的
   SHA-256（`fetch-onnxruntime.ps1:47-52`）；edition 2024 / rust-version 1.88；OCR 资产 16.5 MB +
   74 KB ≈ 16.6 MB；`book-probe` 二进制确实在 `ptt-recognition`；目录条目数 660 / 1047 精确到个位。
3. **CORE-TRADING-MODEL 的数值与代码同步得很好**（见 #13），且 P12 一节里的函数名可以直接当索引用。
   这份文档不需要再"润色"，只需要继续追加。

## 建议裁减

- `docs/AUDIT.md`（未跟踪，在 docs 根）：是本次评审的提示词，评审结束后应移入 `docs/audit/` 或删除，
  否则会和 CORE-TRADING-MODEL 并列成为"看起来像设计文档"的东西。
- `docs/RELEASE-NOTES-TEMPLATE.md:67-107` 的 0.2.0 假想示例：模板本体 40 行已足够，示例占了一半篇幅
  且版本号/文件名都过时；删掉示例或替换成 1.0.7 真实发版说明。
- `docs/UI-DESIGN.md:99-133` 方案 B "已否决，存档" 与两方案风险对比：已否决的配色方案不会再被引用，
  留着只会让 1280×800 这类过时数字更难被注意到。
- `docs/IDEA-UNIQUE-ITEM-SNIPER.md`：点子稿，不属于"当前实现的文档"，建议移出 docs/ 或在文件头标明
  "未排期"，避免与 P10-FOLLOWUPS 的待办混淆。
