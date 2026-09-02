# D · 代码健康度（工具化检测）

- 检测日期：2026-09-02，基线 commit `8103b55`（main）
- 环境：Windows 11，rustc 1.88.0 / cargo 1.88.0（与 `rust-version = "1.88"` 完全一致）
- 方法：只跑 clippy / test / cargo tree / grep 统计；第 6 项对定位到的每一处用 `sed -n` 看了前后几行判断可达性。**没有逐行人工阅读**。
- 严重程度定义：Blocker = 不修不能发；Should-Fix = 发前应修；Nice-to-Have = 记下即可。

---

## 1. clippy

**现象**：`cargo clippy --workspace --all-targets` → 0 条 warning，18 个 crate 全过（含测试与探针 target）。仓库没有 `[lints]` 配置，也没有 `-D warnings`，这是裸 clippy 的结果。
**为什么用户会在意**：这是项目自定的门槛（CLAUDE.md），已达标。
**严重程度**：无问题。

## 2. 测试

**现象**：`cargo test --workspace` → **674 通过 / 0 失败 / 2 忽略**。
被忽略的两个都在 `ptt-ocr-onnx`，理由写在 `#[ignore]` 上：

| 测试 | 忽略原因 |
|---|---|
| `official_model_contract_and_real_inference` | 需要本地打包好的 ONNX Runtime 1.28 DLL |
| `old_traditional_ctc_assisted_crop_is_strong_and_near_neighbor_is_rejected` | 需要私有的 1.0 截图语料 + 本地 ONNX Runtime |

**为什么用户会在意**：这两条是 OCR 主路径的"真推理"回归测试，在干净 checkout 上永远不跑；OCR 模型或 DLL 升级时只能靠手动跑。单人工具可接受，但要知道这层保护是手动的。
**严重程度**：Nice-to-Have。

## 3. 重复依赖

`cargo tree --duplicates` 报出 **40 个名字**有多份。先剔除假重复：`serde`、`serde_json`、`log`、`either`、`mime_guess`、`windows-sys 0.61.2` 各出现两次但**版本相同**，那是 host（proc-macro / build script）和 target 各编一份，不是版本分裂。

真正的版本分裂，按"份数"和"大件"排序：

| crate | 版本 | 谁拉进来的（追到仓内 crate 为止） | 大件？ |
|---|---|---|---|
| windows-sys | 0.52 / 0.59 / 0.61 | 0.52 ← notify 7 ← gpui-component；0.59 ← winreg ← embed-resource（gpui 的 build-dep）；0.61 是主线 | 是 |
| syn | 1 / 2 / 3 | 1 ← hidden-trait ← blade-graphics ← gpui；3 ← bytemuck_derive ← blade-graphics | 编译时间 |
| getrandom | 0.2 / 0.3 / 0.4 | 0.2 ← nanorand ← flume ← gpui | 否 |
| itertools | 0.11 / 0.13 / 0.14 | 0.11 ← rust-i18n ← gpui-component；0.13 ← gpui-component 直接 | 否 |
| png | 0.17 / 0.18 | 0.17 ← tiny-skia ← resvg ← gpui；0.18 ← image 0.25 ← gpui | 中 |
| sha2 + digest/block-buffer/crypto-common/cpufeatures | 0.10 / 0.11 | 0.11 ← rust-embed-utils ← gpui_util ← gpui；0.10 是仓内 pin `=0.10.9` + gpui_http_client | 否 |
| thiserror(+impl) | 1 / 2 | 1 ← async_zip 0.0.17（ptt-app 自己引的，也是 gpui_util 引的）；2 是仓内 workspace 依赖 | 否 |
| toml / toml_datetime / serde_spanned / winnow | 0.8 / 1.1 | 0.8 ← rust-i18n；1.1 ← embed-resource（build-dep） | 否 |
| base64 | 0.22 / 0.23 | 0.22 ← usvg + zed-reqwest ← gpui；**0.23 ← ureq 3.4 ← ptt-app、ptt-exchange-history（仓内自己引的）** | 否 |
| rand / rand_core | 0.8 / 0.9 | 0.8 ← phf_generator ← html5ever（build）← gpui-component | 否 |
| libloading | 0.8 / 0.9 | 0.8 ← ash（Vulkan）← blade-graphics；0.9 ← ort（ptt-ocr-onnx） | 否 |
| strum(+macros) | 0.26 / 0.27 | 0.26 ← naga ← blade-graphics | 否 |
| bitflags | 1 / 2 | 1 ← lsp-types、globwalk ← gpui-component | 否 |
| hashbrown | 0.15 / 0.17 | 两边都在 gpui 树里 | 否 |
| event-listener / futures-lite / async-channel / fastrand | 2 vs 5 / 1 vs 2 / 1 vs 2 / 1 vs 2 | 老版本全部 ← async-std ← zed-async-tar ← gpui_http_client | 否 |
| windows-link / -registry / -strings / -targets / _x86_64_msvc | 两代 | 跟着 windows-sys 三代分裂 | 是 |
| rustc-hash、shlex | 1 / 2 | gpui 树内 | 否 |

未分裂但值得知道的大件：`tokio 1.53` 只由 `gpui_http_client → zed-reqwest → hyper` 拉进来，应用自己用的是 `ureq`（同步）+ `smol`；`windows 0.61.3` / `windows-core 0.61.2` 各只有一份；`image 0.25`、`ort 2.0.0-rc.13`、`rusqlite 0.37` 各一份。

**现象**：几乎所有分裂都来自 `gpui 0.2` / `gpui-component 0.5` 这两个上游，仓库自己能动的只有一处：`ureq 3.4` 带来的 `base64 0.23`（gpui 走 0.22）。
**为什么用户会在意**：代价是编译时间和体积，不是行为；release 下 `ptt-app.exe` 剥符号后 16.3 MB，已经可接受。想消掉只能等 gpui 升级，不值得为此 fork。
**严重程度**：Nice-to-Have（不建议动）。

## 4. unwrap / expect / panic 计数

`grep` 统计 `crates/*/src` 全部 `.rs`（含测试 mod 与探针）。"探针"列单独统计 `src/bin/`，那是开发工具不是用户路径。

| crate | src 行数 | unwrap | expect | panic! | 探针 unwrap/expect/panic | tests/ 目录文件数 |
|---|---:|---:|---:|---:|---|---:|
| ptt-app | 24 377 | 1 | 72 | 5 | 无 bin 目录（preview.rs 是 dev 画廊） | 0 |
| ptt-runtime | 15 277 | 4 | 165 | 2 | 0 / 1 / 0（7 个探针） | 1 |
| ptt-strategy | 5 801 | 0 | 48 | 0 | – | 6 |
| ptt-platform-win | 5 637 | 9 | 28 | 0 | – | 1 |
| ptt-recognition | 5 169 | 29 | 19 | 3 | 4 / 5 / 0 | 0 |
| ptt-trade-engine | 3 355 | 0 | 20 | 0 | – | 1 |
| ptt-workflows | 2 927 | 0 | 24 | 0 | – | 1 |
| ptt-ocr-onnx | 2 798 | 19 | 2 | 0 | 0 / 1 / 0 | 2 |
| ptt-vision | 2 073 | 0 | 8 | 0 | 0 / 6 / 0 | 2 |
| ptt-storage | 1 771 | **0** | **0** | **0** | – | 3 |
| ptt-ocr-win | 1 767 | 24 | 4 | 0 | 0 / 1 / 0 | 1 |
| ptt-market-book | 1 615 | 0 | 40 | 0 | – | 0 |
| ptt-trade-domain | 1 540 | 0 | 21 | 0 | – | 0 |
| ptt-core | 1 330 | 14 | 2 | 0 | – | 1 |
| ptt-settings | 1 056 | 11 | 1 | 1 | – | 0 |
| ptt-exchange-history | 963 | 5 | 14 | 0 | – | 0 |
| ptt-catalog | 733 | 1 | 9 | 2 | 0 / 0 / 0（catalog_repin） | 0 |
| ptt-monitoring | 474 | 0 | 1 | 0 | 0 / 0 / 0（session_probe） | 0 |

对第 6 项范围内的三个 crate，按 `#[cfg(test)]` 边界把上面的数字拆开（生产 = 不在测试 mod 里）：

| crate | 生产 unwrap | 生产 expect | 生产 panic!/unreachable! | 其余全在测试 mod |
|---|---:|---:|---:|---|
| ptt-app | 0 | 4（+2 在 dev 画廊 preview.rs） | 0 + 1 个 `unreachable!` | update.rs 一个文件的测试就占了 57 个 expect |
| ptt-settings | 0 | 1 | 0 | 11 unwrap + 1 panic 全是测试 |
| ptt-storage | 0 | 0 | 0 | – |

**现象**：用户路径三 crate 加起来生产代码只有 5 个 `expect`、0 个 `unwrap`、0 个 `panic!`。总表里数字大的 crate（ptt-runtime 165、ptt-market-book 40）没有拆分，从 `tests/` 目录数和 `#[cfg(test)]` 惯例看多数应在测试内，但**未逐一验证**。
**为什么用户会在意**：这决定"程序会不会在我点按钮时消失"。见第 6 项的逐条结论。
**严重程度**：整体健康。ptt-runtime / ptt-recognition 的生产侧拆分若要做，是后续可选项（Nice-to-Have）。

## 5. 孤立 crate

对 18 个 workspace 成员逐一 grep 别的 `Cargo.toml`：

| crate | 被谁依赖 |
|---|---|
| ptt-app | 无（唯一根，bin） |
| ptt-runtime | ptt-app |
| ptt-workflows | ptt-runtime |
| ptt-ocr-win | ptt-recognition |
| ptt-exchange-history | **ptt-app、ptt-runtime** |
| ptt-monitoring / ptt-platform-win / ptt-settings / ptt-storage | ptt-app、ptt-runtime |
| ptt-strategy | ptt-runtime、ptt-workflows |
| ptt-trade-engine | ptt-runtime、ptt-strategy、ptt-workflows |
| ptt-market-book | ptt-runtime、ptt-strategy、ptt-trade-engine、ptt-workflows |
| ptt-catalog | ptt-exchange-history、ptt-recognition、ptt-runtime |
| ptt-ocr-onnx | ptt-recognition、ptt-runtime |
| ptt-recognition | ptt-app、ptt-monitoring、ptt-runtime |
| ptt-vision | ptt-monitoring、ptt-ocr-onnx、ptt-ocr-win、ptt-recognition、ptt-runtime |
| ptt-trade-domain | 7 个 |
| ptt-core | 9 个 |

**现象**：**没有孤立 crate**，每个库 crate 都从 `ptt-app` 可达。`ptt-exchange-history` 被 ptt-app 和 ptt-runtime 依赖，是交易所历史抓取（ureq + gzip）的实现，只是 CLAUDE.md 的 crate 地图漏写了它。
**为什么用户会在意**：地图是新会话读代码的第一入口，漏一个 crate 会让人以为它是死代码。
**严重程度**：Nice-to-Have（补一行文档）。

## 6. 用户可达的 panic 清单

范围：`ptt-app/src`、`ptt-settings`、`ptt-storage`。每处 `sed -n` 看过前后几行。

### 6.0 全局前提：崩了就是"窗口消失"，没有任何痕迹

**现象**：`[profile.release] panic = "abort"` + `main.rs` 的 `windows_subsystem = "windows"`（无控制台）+ 全仓没有 `panic::set_hook` + ptt-app 没有任何日志库（只有 13 处 `eprintln!`，在无控制台的 GUI 下是黑洞）。
**为什么用户会在意**：下面每一条 panic 无论多冷门，触发时的表现都一样——程序原地消失，没有对话框、没有日志文件、没法复现给自己看。它把所有 Nice-to-Have 的 panic 都放大成"不知道为什么闪退"。修法是启动时装一个 panic hook 把信息写进 `%LOCALAPPDATA%` 下的 crash.log 再弹一个 MessageBox，超过 3 行故不贴代码。
**严重程度**：**Should-Fix**（发版前唯一建议动代码的一条）。

### 6.1 生产 expect 逐条

| 位置 | 触发条件 | 用户可达？ | 严重程度 |
|---|---|---|---|
| `crates/ptt-app/src/lib.rs:81` `.expect("failed to open window")` | 启动时 GPUI 建不出主窗口：显卡驱动 / DirectX 初始化失败、无 GPU 的远程桌面会话 | 可达（启动） | Nice-to-Have —— 建不出窗口本来也没法继续，但叠加 6.0 就是"双击没反应" |
| `crates/ptt-app/src/shell/hud.rs:69` `.expect("calibrated regions have positive dimensions")` | 宽高已被 `.max(1)` 钳住，`RectI::new` 只剩 `x + width` 溢出 i32 一种失败；需要手改 settings.json 把标定区域的 x 或 width 写成 20 亿量级 | 理论可达（手改设置 + 开 HUD） | Nice-to-Have —— UI 拖出来的区域到不了这个值 |
| `crates/ptt-app/src/backend.rs:126` / `:173` `.expect("spawn … thread")` | 操作系统拒绝创建线程（句柄耗尽） | 启动时 | Nice-to-Have —— 实际不会发生，不建议动 |
| `crates/ptt-settings/src/lib.rs:825` `.expect("settings model always serializes")` | serde_json 序列化一个纯数据 struct；NaN 会被写成 null 而不是报错，实际不可能失败 | 每次保存设置 | 无需处理 |
| `crates/ptt-app/src/preview.rs:49` / `:553` | 仅在 `ptt-ui-preview` 开发画廊里 | 不是用户路径 | 不计 |

### 6.2 查过并确认安全的位置（不用动）

| 位置 | 为什么安全 |
|---|---|
| `shell/pages/opportunities.rs:605` `unreachable!` | 前一句 `if let Unavailable … return` 已经把另一种情况返回了 |
| `shell/pages/opportunities.rs:1309` `pair[0]` / `pair[1]` | 来自 `.windows(2)`，每个 pair 保证两个元素 |
| `ui.rs:910` `points[0]` | 函数开头 `if points.len() < 2 { return; }` |
| `ui.rs:1059` `segment[0]` | 只在 `segment.len() == 1` 分支里取 |
| `ui.rs:1007` `x / width … as usize` | `slot_at` 先判 `slots == 0 || width <= 0.0 || x >= width` 再算 |
| `shell/updater.rs:219` `done * 100 / total` 整数除法 | 前面 `if total == 0 { return None; }`，且先 `min(total)` 再 `saturating_mul` |
| `calibrate.rs:421` JPEG 头 `bytes[index + 2..8]` | 在 `while index + 9 < bytes.len()` 里，越界不可能；PNG 分支用 `.get(16..24)?` |
| `theme.rs:858` `LOCK.lock()` | 用 `unwrap_or_else(PoisonError::into_inner)`，锁中毒不 panic |
| `shell/pages/exchange.rs:670` `(CHART_WIDTH / 2.0) as usize` | 常量 |

### 6.3 存储与设置的失败路径（不是 panic，但和"闪退"同一类担心）

**SQLite**：`MarketStore::open` 在 ptt-app 里有 7 处调用（`shell/mod.rs` ×5、`shell/exchange_sync.rs` ×1、`shell/pages/season.rs` ×1），全部 `.map_err(|e| format!("storage: {e}"))?` 或 `.ok()?` 或 `match`，没有一处 unwrap。打开时设 WAL + `busy_timeout` 5 秒。数据库被锁、被删、被占用 → 页面显示一行错误，不崩。
**设置文件**：手改坏 JSON → `LoadStatus::Defaults` 静默回默认；未来 schema → `FutureSchemaReadOnly` 拒绝写回。**现象**：坏文件不会被备份，下一次保存直接覆盖（`lib.rs:915-919` 的测试就是这么写的），用户手改的内容会丢；UI 是否提示"已回到默认"未在本次范围内验证。
**为什么用户会在意**：单人工具手改设置是常事，改坏一个逗号就丢整份配置。
**严重程度**：Nice-to-Have。

## 7. 构建与工程配置观察

| 项 | 现象 | 严重程度 |
|---|---|---|
| README 构建说明 | 有：`cargo run -p ptt-app`（第 315 行）、`package-preview.ps1` 跑 `cargo build --release --locked -p ptt-app`（第 355 行）；且明确警告"跳过 fetch-onnxruntime 不会让 build 失败，这是坑"（第 327 行） | 无问题 |
| `[profile.release]` | 齐全：`codegen-units = 1`、`lto = "thin"`、`panic = "abort"`、`strip = "symbols"`、`opt-level = "s"`；产出 16.3 MB | 无问题 |
| MSRV / toolchain | 根 `rust-version = "1.88"`，本机恰好 1.88.0，**没有 `rust-toolchain.toml`**，没有 CI（无 `.github/workflows`）。MSRV 等于"我现在装的版本"，从未在别的版本上验证过。另外 4 个 crate 没继承 `rust-version.workspace`（ptt-core、ptt-ocr-win、ptt-platform-win、ptt-trade-engine），2 个 crate 手写 `edition = "2024"` 而非 `.workspace = true`（ptt-ocr-onnx、ptt-platform-win） | Nice-to-Have：加一个 `rust-toolchain.toml` 把 1.88 钉住，3 行以内 |
| 探针二进制 | 13 个 `*_probe` bin 分布在 6 个 crate，`cargo test --workspace` / `--all-targets` 每次都会编它们；`target/release/` 里还留着一个已不存在的 `gap_probe.exe`（8 月 22 日） | Nice-to-Have，无害 |
| 2 个 `#[ignore]` 测试 | 见第 2 项 | Nice-to-Have |

---

## 已达交付标准的亮点

1. **裸 clippy 零 warning + 674 测试全绿**，没有靠 `[lints]` 或 `allow` 全局压制；这在 24k 行的 GPUI 应用上不常见。
2. **存储与设置两层在生产路径上一个 unwrap 都没有**：7 处开库全部降级成错误文本，设置有"未来 schema 只读"保护，锁中毒也处理了。用户路径三 crate 生产代码只剩 5 个 `expect`，其中 4 个是"发生了程序也没法继续"的场景。
3. **发布配置是完整的**：release profile 五项全开，打包脚本用 `--locked`，README 把 ONNX DLL 这个真正的坑写在了显眼处。

## 建议裁减

- **crate 层面没有可删的**：18 个成员全部从 `ptt-app` 可达，没有只为探针存在的 crate。
- **`ptt-ui-preview`（`preview.rs`，556 行，编出 9.8 MB 的 exe）是开发画廊**，会跟着 `cargo build --release` 一起产出；确认 `package-preview.ps1` 的显式清单没把它装进发布包即可，不必删代码。
- **`ptt-ocr-win` 只被 `ptt-recognition` 一家依赖**：若 Windows.Media.Ocr 这条路线在 1.0 里已不是生产路由，它和它的 24 个 unwrap 是最容易整块拿掉的；这一点本次工具化检测判断不了，需要人拍板。
- 依赖层面不建议裁：分裂几乎全在 gpui 上游，唯一自家的 `ureq → base64 0.23` 换掉的收益不值一次网络层改动。
