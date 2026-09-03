//! 自更新：问一次 GitHub 有没有新版本，下载、按包自带的清单核对、就地替换。
//!
//! 这个模块的形状是刻意的：**有风险的判断全是纯函数**（版本比较、清单解析、
//! 条目名归一化、对账、换文件计划），底下的 `#[cfg(test)]` 全部测它们；碰网络
//! 和碰磁盘的部分薄到只剩"取字节、写字节"，没有藏判断。
//!
//! ## 为什么这条路上不许 panic
//!
//! release 档位是 `panic = "abort"`。任何线程上的一次 panic 会当场杀掉进程，
//! 没有栈回滚、没有提示。这个检查在窗口刚开出来不久就跑，输入又全部来自网络和
//! 磁盘——一段畸形 JSON、一次断掉的下载、一个没见过的 zip 条目、一个不存在的
//! 环境变量，都得落进 `Result` 变成一句话，绝不能变成崩溃。所以 CLAUDE.md 里
//! "个人工具，unwrap 可以接受"那条**在这个文件里不适用**。
//!
//! ## 校验证明的是完整性，不是来源
//!
//! MANIFEST.json 装在它自己描述的那个 zip 里。所以核对哈希只能证明"这包在传输
//! 途中没缺没坏"，**不能**证明"这包是我发的"。它挡的是截断和损坏，不是恶意的
//! 发布。真要挡后者需要离线的公钥签名，这里没有，也不假装有。

use std::fmt;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_lite::io::AsyncReadExt as _;
use gpui::SemanticVersion;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// 只问 `latest`：草稿和预发布不会从这个端点出来，省得自己过滤。
const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/SouNdmys/POE-Trade-Tracker/releases/latest";

/// GitHub 的匿名 API 没有 User-Agent 直接回 403，这不是可选的礼貌。
const USER_AGENT: &str = concat!(
    "poe-trade-tracker/",
    env!("CARGO_PKG_VERSION"),
    " (updater)"
);

/// 挑资产时优先认这个词，但不强求——包名里的 "preview" 以后会改，slug 不会。
const PRODUCT_SLUG: &str = "poe-trade-tracker";

const MANIFEST_NAME: &str = "MANIFEST.json";

/// 打包脚本自己卡 55 MiB 的预算。这里给到 96 MiB 是留出余量，同时让一个坏掉的
/// 或者恶意的响应填不满硬盘。
const MAX_ARCHIVE_BYTES: u64 = 96 * 1024 * 1024;

/// release 的 JSON 元数据，几十 KB 顶天了。
const MAX_METADATA_BYTES: u64 = 1024 * 1024;

/// 解压后允许写出的总字节。zip 可以做成一个小文件解出几个 G，这道闸是防那个的。
const MAX_UNPACKED_BYTES: u64 = 192 * 1024 * 1024;

/// 清单本身不该有一兆。
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

/// 九个文件的包，五百个条目已经是"这不是我们的包"了。
const MAX_ENTRIES: usize = 512;

/// 这两个进程自己占着：exe 是正在跑的映像，onnxruntime.dll 被 ort 塞进一个
/// 永不释放的 `static OnceLock` 里。它们删不掉，但**能改名**——Windows 允许给
/// 一个已打开的文件改名，只要不删。其余七个直接盖。
const LOCKED_FILES: [&str; 2] = ["ptt-app.exe", "onnxruntime.dll"];

/// 让位用的后缀。留在原地等下次启动 `clean_leftovers` 扫掉：这一轮里它还被占着。
const OLD_SUFFIX: &str = ".old";

/// 新文件先落到目的地旁边，全都落稳了才开始换。
const NEW_SUFFIX: &str = ".new-update";

/// 下下来的包落在哪。固定的名字：同一时刻只可能有一份待装的更新，而且从网上
/// 下来的名字不参与拼路径，省掉一整类“资产名里带 `..\` ”的问题。
const PENDING_ARCHIVE_NAME: &str = "pending-update.zip";

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// 更新路上所有会出的岔子，一种类型说完。
///
/// `Display` 写的是英文,而且是给人看的大白话——调用方要本地化就照着这些变体
/// 分支去 `i18n`,不要把中文字面量塞回这里。
#[derive(Debug)]
pub enum UpdateError {
    /// 连不上：断网、DNS 挂了、TLS 谈崩了、超时。
    Unreachable(String),
    /// 连上了,但对面不给:403 是限流(每小时 60 次),404 是还没发过 release。
    Rejected(u16),
    /// 回复不是我们认识的形状。
    MalformedRelease(String),
    /// 这个版本没挂可下载的 zip。
    NoPackage { tag: String },
    /// 下载超过了我们愿意收下的大小。
    TooLarge { limit_bytes: u64 },
    /// 读写文件失败。
    Storage { path: PathBuf, reason: String },
    /// zip 本身打不开。
    BadArchive(String),
    /// 内容和它自带的清单对不上,每条一句话。
    Mismatch(Vec<String>),
    /// 安装目录不让写——多半是解压到了 `C:\Program Files`。
    ReadOnlyInstall { directory: PathBuf },
    /// 换到一半停了。说清楚现在目录是什么状态,这比装作没事重要。
    HalfApplied {
        reason: String,
        /// 已经是新版本的文件(相对安装目录)。
        already_new: Vec<String>,
        /// 主程序是否已经放回原样。
        program_restored: bool,
    },
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable(detail) => {
                write!(f, "could not reach github ({detail})")
            }
            Self::Rejected(403) => write!(
                f,
                "github turned the request away (403) - too many checks from this network in the last hour"
            ),
            Self::Rejected(404) => write!(f, "this project has no published release yet (404)"),
            Self::Rejected(status) => write!(f, "github answered with status {status}"),
            Self::MalformedRelease(detail) => {
                write!(
                    f,
                    "github's answer was not in a shape we understand ({detail})"
                )
            }
            Self::NoPackage { tag } => {
                write!(f, "release {tag} has no downloadable .zip package attached")
            }
            Self::TooLarge { limit_bytes } => write!(
                f,
                "the download is larger than the {} MiB we are willing to accept - stopped",
                limit_bytes / (1024 * 1024)
            ),
            Self::Storage { path, reason } => {
                write!(f, "could not use {} ({reason})", path.display())
            }
            Self::BadArchive(detail) => {
                write!(f, "the downloaded package would not open ({detail})")
            }
            Self::Mismatch(complaints) => {
                write!(f, "the download did not match its manifest")?;
                for complaint in complaints {
                    write!(f, "; {complaint}")?;
                }
                Ok(())
            }
            Self::ReadOnlyInstall { directory } => write!(
                f,
                "this folder is not writable - move the app out of Program Files: {}",
                directory.display()
            ),
            Self::HalfApplied {
                reason,
                already_new,
                program_restored,
            } => {
                write!(f, "the update stopped part way ({reason})")?;
                if *program_restored {
                    write!(f, "; the program itself was put back the way it was")?;
                } else {
                    write!(f, "; the program itself could not be put back")?;
                }
                if already_new.is_empty() {
                    write!(f, "; no support files were changed")
                } else {
                    write!(
                        f,
                        "; these support files are already the new version: {}",
                        already_new.join(", ")
                    )
                }
            }
        }
    }
}

impl std::error::Error for UpdateError {}

fn storage_error(path: &Path, error: &std::io::Error) -> UpdateError {
    UpdateError::Storage {
        path: path.to_path_buf(),
        reason: error.to_string(),
    }
}

// ---------------------------------------------------------------------------
// GitHub 的回复
// ---------------------------------------------------------------------------

/// 只取用得上的字段,而且每个都 `#[serde(default)]`。
///
/// 故意不用 gpui 自己的 `GithubRelease`:它一个字段都没有 default,少一个
/// `tarball_url` 整份就反序列化失败——而那个字段我们根本不看。顺带也不把自己
/// 钉在 gpui 的内部类型上。
#[derive(Debug, Default, Deserialize)]
struct GithubRelease {
    #[serde(default)]
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

/// release 上挂的一个可下载文件。
#[derive(Debug, Default, Clone, Deserialize)]
pub struct ReleaseAsset {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub browser_download_url: String,
    /// GitHub 报的字节数。只当提前拦一手的依据,不当真——真正的闸在读的时候。
    #[serde(default)]
    pub size: u64,
}

/// 一个比当前版本新、而且挂着能下的包的发布。
#[derive(Debug, Clone)]
pub struct Release {
    pub tag: String,
    pub version: SemanticVersion,
    pub html_url: String,
    pub asset_name: String,
    pub asset_url: String,
    pub asset_size: u64,
}

// ---------------------------------------------------------------------------
// 纯函数:版本
// ---------------------------------------------------------------------------

/// 把一个 release 标签解析成版本号。
///
/// `SemanticVersion` 不吃开头的 `v`,而标签惯例是带的,所以先剥掉。解析不出来
/// 的标签**不是**用户需要看见的错误——那是发布者的笔误,对用户来说就等于"没有
/// 更新",所以这里返回 `None` 而不是 `Err`。
pub fn parse_tag(tag: &str) -> Option<SemanticVersion> {
    let trimmed = tag.trim();
    let body = match trimmed.strip_prefix(['v', 'V']) {
        Some(rest) => rest,
        None => trimmed,
    };
    body.parse::<SemanticVersion>().ok()
}

/// 标签比当前版本新就给出它的版本号,否则 `None`。
///
/// 两边有任何一边解析不了都算"没有更新":宁可不提示,也不要拿一个读不懂的字符串
/// 去骗用户下载。
pub fn newer_version(current: &str, tag: &str) -> Option<SemanticVersion> {
    let candidate = parse_tag(tag)?;
    let installed = parse_tag(current)?;
    if candidate > installed {
        Some(candidate)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// 纯函数:挑资产
// ---------------------------------------------------------------------------

/// 从 release 挂的文件里挑出我们的包。
///
/// 按**形状**挑,不按完整文件名:打包脚本现在叫
/// `poe-trade-tracker-<version>-preview.zip`,哪天不叫 preview 了,写死的字符串
/// 会让更新静默失效。所以规则是"是个 .zip",名字里带产品 slug 的优先。
pub fn pick_asset(assets: &[ReleaseAsset]) -> Option<&ReleaseAsset> {
    let zips: Vec<&ReleaseAsset> = assets
        .iter()
        .filter(|asset| !asset.browser_download_url.is_empty())
        .filter(|asset| asset.name.to_ascii_lowercase().ends_with(".zip"))
        .collect();

    let preferred = zips
        .iter()
        .copied()
        .find(|asset| asset.name.to_ascii_lowercase().contains(PRODUCT_SLUG));
    match preferred {
        Some(asset) => Some(asset),
        None => zips.first().copied(),
    }
}

// ---------------------------------------------------------------------------
// 纯函数:清单
// ---------------------------------------------------------------------------

/// 包自带的清单。顶层是 camelCase,`files` 里的两个键是 PascalCase——这不是
/// 手滑,是 PowerShell 的 `ConvertTo-Json` 按 `[pscustomobject]` 的属性名原样
/// 输出造成的,照着接就行。
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    #[serde(default)]
    pub product: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub configuration: String,
    #[serde(default)]
    pub built_at: String,
    #[serde(default)]
    pub files: Vec<ManifestFile>,
}

/// 清单里的一行:相对安装目录的路径 + 小写十六进制的 SHA-256。
#[derive(Debug, Default, Clone, Deserialize)]
pub struct ManifestFile {
    #[serde(rename = "Path", default)]
    pub path: String,
    #[serde(rename = "Sha256", default)]
    pub sha256: String,
}

/// 剥掉 UTF-8 BOM。
///
/// 打包用的 shell 没有钉死版本。Windows PowerShell 5.1 的
/// `Set-Content -Encoding UTF8` 会写出 `EF BB BF` 开头,pwsh 7 不会——同一个脚本
/// 出来两种字节。`serde_json` 见到 BOM 直接报 "expected value",于是一个能跑的
/// 包会被判成损坏。剥一下,两种都能读。
pub fn strip_bom(bytes: &[u8]) -> &[u8] {
    match bytes.strip_prefix(&[0xef_u8, 0xbb, 0xbf]) {
        Some(rest) => rest,
        None => bytes,
    }
}

/// 解析清单字节。
pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest, UpdateError> {
    serde_json::from_slice(strip_bom(bytes))
        .map_err(|error| UpdateError::BadArchive(format!("MANIFEST.json: {error}")))
}

// ---------------------------------------------------------------------------
// 纯函数:条目名与哈希
// ---------------------------------------------------------------------------

/// 把 zip 条目名归一化成清单里那种写法。
///
/// 同样是打包 shell 不定造成的:Windows PowerShell 5.1 的 `Compress-Archive`
/// 把嵌套条目写成 `assets\ocr\x`,pwsh 7 写成 `assets/ocr/x`。清单里永远是斜杠,
/// 不换的话整个 `assets/ocr/` 都会被判成"清单里有、包里没有"。
pub fn normalize_entry_name(raw: &str) -> String {
    let swapped = raw.replace('\\', "/");
    match swapped.strip_prefix("./") {
        Some(rest) => rest.to_string(),
        None => swapped,
    }
}

/// 哈希比对不区分大小写。
///
/// 打包脚本写的是小写,但 `Get-FileHash` 原生给大写,中间任何一环改了主意都不该
/// 让一个好包被判成坏包。
pub fn hashes_match(left: &str, right: &str) -> bool {
    left.trim().eq_ignore_ascii_case(right.trim())
}

/// 一条相对路径能不能安全地拼到安装目录下面。
///
/// 清单是从网上下来的。一行 `..\..\Windows\System32\x` 就能让"更新"写到安装目录
/// 外面去。绝对路径、盘符、`..`、空段一律不收。
pub fn is_safe_relative(path: &str) -> bool {
    let normal = normalize_entry_name(path);
    if normal.is_empty() || normal.starts_with('/') || normal.contains(':') {
        return false;
    }
    normal
        .split('/')
        .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// 把一条清单路径拼到安装目录下,拼不安全就 `None`。
pub fn safe_join(root: &Path, relative: &str) -> Option<PathBuf> {
    if !is_safe_relative(relative) {
        return None;
    }
    let mut out = root.to_path_buf();
    for segment in normalize_entry_name(relative).split('/') {
        out.push(segment);
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// 纯函数:对账
// ---------------------------------------------------------------------------

/// 从 zip 里实际看到的一个条目。
#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    /// 已经归一化过的名字。
    pub name: String,
    pub sha256: String,
}

/// 把包的内容和它自带的清单、以及 release 标签三方对账。
///
/// 双向都查:清单点名的每个文件都得在包里且哈希对得上,**并且**包里除了
/// MANIFEST.json 之外的每个文件都得在清单上。只查一个方向的话,一个没被点名的
/// 多余文件会跟着被写进安装目录——一份马上要盖到安装目录上的东西里出现来路不明
/// 的文件,正是校验存在的理由。
///
/// 再说一遍:这证明的是完整性,不是来源。清单和包同源。
pub fn reconcile(
    manifest: &Manifest,
    tag: &str,
    entries: &[ArchiveEntry],
) -> Result<(), UpdateError> {
    let mut complaints: Vec<String> = Vec::new();

    // 版本必须和标签一致。对不上说明下到的是另一个版本的包,后面所有哈希即使
    // 全对也没有意义。
    match (parse_tag(&manifest.version), parse_tag(tag)) {
        (Some(packaged), Some(announced)) if packaged != announced => {
            complaints.push(format!(
                "the package says it is version {packaged} but the release is tagged {announced}"
            ));
        }
        (None, _) => {
            complaints.push(format!(
                "the package does not say which version it is ({:?})",
                manifest.version
            ));
        }
        _ => {}
    }

    if manifest.files.is_empty() {
        complaints.push("the manifest lists no files at all".to_string());
    }

    // 包里的条目,按归一化后的名字索引;目录条目(以 / 结尾)没有内容,不参与对账。
    let mut present: std::collections::BTreeMap<&str, &ArchiveEntry> =
        std::collections::BTreeMap::new();
    for entry in entries {
        if entry.name.ends_with('/') || entry.name.is_empty() {
            continue;
        }
        if entry.name == MANIFEST_NAME {
            continue;
        }
        if present.insert(entry.name.as_str(), entry).is_some() {
            complaints.push(format!("the package contains {} twice", entry.name));
        }
    }

    let mut claimed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for file in &manifest.files {
        let wanted = normalize_entry_name(&file.path);
        if !is_safe_relative(&wanted) {
            complaints.push(format!("the manifest lists an unsafe path: {}", file.path));
            continue;
        }
        // 清单自己写重了也不行。`swap_plan` 是照清单一行一行排的:写两遍就换
        // 两遍,而第二遍走到 `rename_aside` 时原地放着的已经是刚换上去的新文件,
        // 于是新的被改名成 `.old` 盖掉了真正的旧版本——那个 `.old` 正是出事时
        // 用来复原的东西。
        if !claimed.insert(wanted.clone()) {
            complaints.push(format!("the manifest lists {wanted} twice"));
            continue;
        }
        match present.get(wanted.as_str()) {
            None => complaints.push(format!(
                "{wanted} is listed in the manifest but missing from the package"
            )),
            Some(entry) => {
                if !hashes_match(&entry.sha256, &file.sha256) {
                    complaints.push(format!(
                        "{wanted} does not match the hash the manifest gives for it"
                    ));
                }
            }
        }
    }

    for name in present.keys() {
        if !claimed.contains(*name) {
            complaints.push(format!(
                "{name} is in the package but not listed in the manifest"
            ));
        }
    }

    if complaints.is_empty() {
        Ok(())
    } else {
        Err(UpdateError::Mismatch(complaints))
    }
}

// ---------------------------------------------------------------------------
// 纯函数:换文件计划
// ---------------------------------------------------------------------------

/// 一个文件该怎么换。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// 进程占着,删不掉但能改名让位。
    RenameAside,
    /// 没人占,直接盖。
    Overwrite,
}

/// 计划里的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    pub path: String,
    /// 清单给这个文件的 SHA-256。
    ///
    /// 带在计划里而不是回头去翻清单,是因为 `apply` 落每一个文件的时候都要拿它
    /// 再对一次:`stage` 核对的是内存里那份字节,`apply` 是从磁盘上重新读的,
    /// 中间隔着一次落盘。
    pub sha256: String,
    pub placement: Placement,
}

/// 按清单排出换文件的顺序和方式。
///
/// 让位的排在前面是有意的:那两个才是真会失败的一步(文件正被占用、权限不够),
/// 而它们**可逆**——改回来就当没发生。等到开始盖其余文件时,剩下的都是同盘内的
/// 改名,几乎不会失败;万一失败,前面那步还能撤回。反过来排的话,主程序换到一半
/// 出事,已经被盖掉的文件是拿不回来的。
pub fn swap_plan(manifest: &Manifest) -> Vec<PlannedFile> {
    let mut plan: Vec<PlannedFile> = manifest
        .files
        .iter()
        .map(|file| {
            let normal = normalize_entry_name(&file.path);
            let leaf = match normal.rsplit('/').next() {
                Some(name) => name.to_ascii_lowercase(),
                None => String::new(),
            };
            let placement = if LOCKED_FILES.contains(&leaf.as_str()) {
                Placement::RenameAside
            } else {
                Placement::Overwrite
            };
            PlannedFile {
                path: normal,
                sha256: file.sha256.clone(),
                placement,
            }
        })
        .collect();
    plan.sort_by_key(|file| match file.placement {
        Placement::RenameAside => 0,
        Placement::Overwrite => 1,
    });
    plan
}

/// 在一个路径尾巴上接后缀,不经过 `String`。
///
/// 走 `to_string_lossy` 的话,一个非 UTF-8 的安装路径会被悄悄改写,然后改名改到
/// 别的地方去。
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

// ---------------------------------------------------------------------------
// 一:问一次
// ---------------------------------------------------------------------------

/// 这一次检查是谁发起的。
///
/// 存在的理由只有一个:两种检查该等多久不一样。开机那次是背着人跑的,它必须
/// 短;手点的那次人正盯着按钮等答案,可以久一点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckKind {
    /// 开窗之后自己跑的那一次。
    Startup,
    /// 关于页上"现在检查"按下去的那一次。
    Manual,
}

/// 这一次检查最多等多久。
///
/// 开机那次给 12 秒:没人在等它,而一个在假死网络上挂着的后台请求毫无价值。
/// 手点那次给 30 秒:人已经决定要等了,这时候提前掐掉换来的不是"快",是一句
/// 假的"连不上"——而慢线上多等 18 秒往往就问出来了。
pub(crate) const fn check_timeout(kind: CheckKind) -> Duration {
    match kind {
        CheckKind::Startup => Duration::from_secs(12),
        CheckKind::Manual => Duration::from_secs(30),
    }
}

/// 问 GitHub 有没有比 `current_version` 新的版本。
///
/// 一次启动只问一次,失败也不重试:匿名 API 每小时每 IP 只有 60 次,循环重试会
/// 把额度烧光,然后连"有没有新版本"都问不出来了。
pub fn latest_release(
    current_version: &str,
    kind: CheckKind,
) -> Result<Option<Release>, UpdateError> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(check_timeout(kind)))
        .build();
    let agent: ureq::Agent = config.into();

    let mut response = agent
        .get(LATEST_RELEASE_URL)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(classify_transport)?;

    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_METADATA_BYTES)
        .read_to_vec()
        .map_err(classify_transport)?;

    let release: GithubRelease = serde_json::from_slice(strip_bom(&body))
        .map_err(|error| UpdateError::MalformedRelease(error.to_string()))?;

    // 标签读不懂就是"没有更新"。发布者写错标签不该变成用户面前的一句报错。
    let Some(version) = newer_version(current_version, &release.tag_name) else {
        return Ok(None);
    };

    let Some(asset) = pick_asset(&release.assets) else {
        return Err(UpdateError::NoPackage {
            tag: release.tag_name,
        });
    };

    if asset.size > MAX_ARCHIVE_BYTES {
        return Err(UpdateError::TooLarge {
            limit_bytes: MAX_ARCHIVE_BYTES,
        });
    }

    Ok(Some(Release {
        tag: release.tag_name,
        version,
        html_url: release.html_url,
        asset_name: asset.name.clone(),
        asset_url: asset.browser_download_url.clone(),
        asset_size: asset.size,
    }))
}

/// 把 ureq 的错分成"对面拒绝"和"根本没连上"两类,别的都当连不上。
fn classify_transport(error: ureq::Error) -> UpdateError {
    match error {
        ureq::Error::StatusCode(status) => UpdateError::Rejected(status),
        ureq::Error::BodyExceedsLimit(_) => UpdateError::TooLarge {
            limit_bytes: MAX_METADATA_BYTES,
        },
        other => UpdateError::Unreachable(other.to_string()),
    }
}

// ---------------------------------------------------------------------------
// 二:下载与核对
// ---------------------------------------------------------------------------

/// 装一次更新要走的三段路,按先后顺序。
///
/// 分成三段而不是"下载中/安装中"两段,是因为这三段各自的进度是**不同的单位**:
/// 下载数的是字节,核对数的是包里的条目,而写盘那一下没有中间量可数。混成一个
/// 百分比就等于自己编一个分母。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Stage {
    /// 从 GitHub 收字节。
    #[default]
    Downloading,
    /// 把收到的字节落到磁盘上,包含一次 `sync_all`。
    Saving,
    /// 逐条解压、逐条哈希,和包自带的清单对账。
    Checking,
}

impl Stage {
    /// 存进 `AtomicU8` 的那个数。
    ///
    /// 手写而不是 `as u8`:枚举以后重排顺序时,`as` 会悄悄换掉编码,而这里
    /// 编码错了只会画错一句话——正是那种没人会发现的错。
    const fn code(self) -> u8 {
        match self {
            Self::Downloading => 0,
            Self::Saving => 1,
            Self::Checking => 2,
        }
    }

    /// 读回来。认不出的数当"在下载"——这条路上不许 panic,而三段里只有第一段
    /// 是"还早得很",猜错的代价最小。
    const fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Saving,
            2 => Self::Checking,
            _ => Self::Downloading,
        }
    }

    /// 这一段有没有可以数的中间量。写盘没有:一次 `write_all` 加一次
    /// `sync_all`,中间没有回音。
    pub const fn is_countable(self) -> bool {
        !matches!(self, Self::Saving)
    }
}

/// 界面画一帧进度要的全部东西。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProgressSnapshot {
    pub stage: Stage,
    /// 已经走完的量。下载段是字节,核对段是条目。
    pub done: u64,
    /// 这一段总共有多少。数不出来就是 0——界面据此不画进度条,而不是画一根
    /// 假装知道总数的。
    pub total: u64,
}

/// 一次安装走到哪了。后台线程只写,界面线程只读。
///
/// 用原子量而不是通道:`backend.rs` 里和监视线程之间就是这个形状,而且这条路
/// 是 `panic = "abort"`——一次 `store` 不会失败,一次 `send` 会遇到对面已经没了。
/// 丢掉中间值在这里恰恰是对的:界面每 120ms 只画一次,要的就是"最新那个数",
/// 不是一条积压的历史。
///
/// 三个原子量各自更新,所以读的人可能正好撞在换段的中间,拿到新段配旧数——
/// 后果是一帧里那根条画得不准,120ms 之后自己就对了。为这个上一把锁,就是在
/// 下载的热路径上加一次可能阻塞,不划算。
#[derive(Debug, Default)]
pub struct Progress {
    stage: std::sync::atomic::AtomicU8,
    done: std::sync::atomic::AtomicU64,
    total: std::sync::atomic::AtomicU64,
}

impl Progress {
    /// 进入新的一段。`total` 数不出来就传 0。
    pub fn begin(&self, stage: Stage, total: u64) {
        use std::sync::atomic::Ordering::Relaxed;
        // 先把数清零再换段:反过来的话,界面有机会看见新段配着上一段的进度,
        // 也就是一根已经满了的条。
        self.done.store(0, Relaxed);
        self.total.store(total, Relaxed);
        self.stage.store(stage.code(), Relaxed);
    }

    /// 又走完了 `amount`(下载是字节,核对是条目)。
    pub fn advance(&self, amount: u64) {
        use std::sync::atomic::Ordering::Relaxed;
        self.done.fetch_add(amount, Relaxed);
    }

    /// 现在这一帧。
    pub fn snapshot(&self) -> ProgressSnapshot {
        use std::sync::atomic::Ordering::Relaxed;
        ProgressSnapshot {
            stage: Stage::from_code(self.stage.load(Relaxed)),
            done: self.done.load(Relaxed),
            total: self.total.load(Relaxed),
        }
    }
}

/// 下好、核对过、可以动手换的一份更新。
#[derive(Debug, Clone)]
pub struct StagedUpdate {
    pub tag: String,
    pub version: SemanticVersion,
    /// 落在 `%LOCALAPPDATA%\PoeTradeTracker\updates\` 里的那个 zip。
    pub archive: PathBuf,
    pub manifest: Manifest,
    pub plan: Vec<PlannedFile>,
}

/// 更新的落脚点。数据库在 `%LOCALAPPDATA%\PoeTradeTracker\` 下面,所以这里另开
/// 一层 `updates\`,永远不去碰同级的 `market.sqlite`。
pub fn updates_dir() -> Result<PathBuf, UpdateError> {
    let local = std::env::var("LOCALAPPDATA").map_err(|_| UpdateError::Storage {
        path: PathBuf::from("%LOCALAPPDATA%"),
        reason: "this account has no LOCALAPPDATA folder".to_string(),
    })?;
    if local.trim().is_empty() {
        return Err(UpdateError::Storage {
            path: PathBuf::from("%LOCALAPPDATA%"),
            reason: "this account has no LOCALAPPDATA folder".to_string(),
        });
    }
    Ok(Path::new(&local).join("PoeTradeTracker").join("updates"))
}

/// 下载这个 release 的包,把它和自带的清单从头到尾对一遍。
///
/// 对不上就在这里停:一份没核对过的 zip 绝不允许走到 `apply`。
///
/// `progress` 是给界面看的,不参与任何判断:这条路上多写一个原子量,少写也
/// 只是那根条不动,不会改变这一次安装的结果。
pub fn stage(release: &Release, progress: &Progress) -> Result<StagedUpdate, UpdateError> {
    let directory = updates_dir()?;
    fs::create_dir_all(&directory).map_err(|error| storage_error(&directory, &error))?;

    let archive_path = directory.join(PENDING_ARCHIVE_NAME);

    // 分母用 GitHub 报的那个数。它只是"显示用的总数",真正的闸在读的时候,
    // 所以就算它报错了也只是条画得不准,下不满或者提前满,包照样卡在上限上。
    progress.begin(Stage::Downloading, release.asset_size);
    let bytes = download(&release.asset_url, progress)?;

    // 写盘没有中间量可数,传 0 让界面别画条,只换那一句话。
    progress.begin(Stage::Saving, 0);
    let mut file =
        fs::File::create(&archive_path).map_err(|error| storage_error(&archive_path, &error))?;
    file.write_all(&bytes)
        .map_err(|error| storage_error(&archive_path, &error))?;
    file.sync_all()
        .map_err(|error| storage_error(&archive_path, &error))?;
    drop(file);

    // 核对不过就把这几十兆当场删掉,不留在用户的磁盘上等下一轮覆盖。
    let checked = inspect_archive(bytes, progress).and_then(|(manifest, entries)| {
        reconcile(&manifest, &release.tag, &entries)?;
        Ok(manifest)
    });
    let manifest = match checked {
        Ok(manifest) => manifest,
        Err(error) => {
            let _ = fs::remove_file(&archive_path);
            return Err(error);
        }
    };

    Ok(StagedUpdate {
        tag: release.tag.clone(),
        version: release.version,
        archive: archive_path,
        plan: swap_plan(&manifest),
        manifest,
    })
}

/// 把包整个读进内存。上限在 `MAX_ARCHIVE_BYTES`,ureq 读到超限自己会停。
///
/// 自己分块读而不是 `read_to_vec()`:那一句在几十兆读完之前不回话,外面看到的
/// 就是一个静止几十秒的面板。64 KiB 一块,和 `drain_entry` 同一个块大小。
///
/// 上限没有放松。`.limit()` 还在,它超限时从读里吐出来的是一个包着 ureq 错的
/// `io::Error`,`ureq::Error::from` 会把它原样拆回来,所以下面那个 `TooLarge`
/// 的分支和以前接住的是同一件事。
fn download(url: &str, progress: &Progress) -> Result<Vec<u8>, UpdateError> {
    // 分段超时,不用全局的那一个。全局超时把"连不上"和"下得慢"算成同一件
    // 事:26 MiB 配 600 秒,意味着线路低于 44 KB/s 就会被当成故障掐掉,而它
    // 明明一直在推进。有了进度条之后这更难看——用户眼睁睁看着它死在 70%。
    // 所以握手给短的(服务器死了要快点知道),正文给足(慢线也让它下完),
    // 按"最慢也该完成"取值:26 MiB / 1800s ≈ 15 KB/s 的地板。
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(Duration::from_secs(1800)))
        .build();
    let agent: ureq::Agent = config.into();

    let mut response = agent
        .get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(classify_transport)?;

    let mut reader = response
        .body_mut()
        .with_config()
        .limit(MAX_ARCHIVE_BYTES)
        .reader();

    // 不按对面报的大小预分配:那个数来自网络,而这条路上不多设一个信任点。
    let mut bytes: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read =
            std::io::Read::read(&mut reader, &mut buffer).map_err(
                |error| match ureq::Error::from(error) {
                    ureq::Error::BodyExceedsLimit(_) => UpdateError::TooLarge {
                        limit_bytes: MAX_ARCHIVE_BYTES,
                    },
                    other => classify_transport(other),
                },
            )?;
        if read == 0 {
            break;
        }
        match buffer.get(..read) {
            Some(chunk) => bytes.extend_from_slice(chunk),
            None => return Err(UpdateError::BadArchive("short read".to_string())),
        }
        progress.advance(read as u64);
    }
    Ok(bytes)
}

/// zip64 的包我们不收,而且要在把字节交出去**之前**回绝。
///
/// async_zip 读中央目录之前,会照尾记录里写的条目数先 `Vec::with_capacity` 一把
/// (`async_zip::base::read::file`)。普通尾记录里那个数是 u16,顶天 65535,要不了
/// 多少内存;可一旦包声明自己是 zip64,条目数换成 u64——一个 98 字节的畸形包写上
/// 2^60,那一句当场 "capacity overflow"。release 是 `panic = "abort"`:那不是一条
/// 错误消息,是进程原地消失。下面 `MAX_ENTRIES` 那道闸排在这一步后面,拦不住。
///
/// 而我们的包是 `Compress-Archive` 出的九个文件,永远不会是 zip64。所以先在尾巴上
/// 找一眼 zip64 定位器的签名:有就当这不是我们的包。窗口取 async_zip 自己搜尾记录
/// 的那段范围(尾部 66 KiB),再往前的同样四个字节它根本看不见,不必管。
///
/// 代价是压缩数据的最后 66 KiB 里恰好蹦出这四个字节时会误伤一个好包,概率大约
/// 六万分之一,而结果是一句"这个包打不开"、可以重下——比进程无声无息地死掉划算
/// 得多。
fn declares_zip64(bytes: &[u8]) -> bool {
    /// zip64 end of central directory locator 的签名,小端。
    const ZIP64_EOCDL_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x06, 0x07];
    /// 窗口要盖住 async_zip 那个搜索**真正够得着**的范围,宁可宽不可窄。
    ///
    /// 它从尾巴往回搜签名,一次读 2048 字节的缓冲,读完才判断"是不是已经越过
    /// 18 + 4 + 65535 的下界了"——所以最后那一次搜索的起点已经在下界之外,
    /// 实际能命中的位置还要再往前一整个缓冲。定位器又在尾记录前面 24 字节。
    /// 少算这两截,一个把假尾记录藏在 66 KiB 处的包就能绕过这道闸。
    const TAIL_BYTES: usize = 18 + 4 + u16::MAX as usize + 2048 + 64;

    let start = bytes.len().saturating_sub(TAIL_BYTES);
    let window = match bytes.get(start..) {
        Some(window) => window,
        None => bytes,
    };
    window.windows(4).any(|four| four == ZIP64_EOCDL_SIGNATURE)
}

/// 把字节交给解析器,先过一道 zip64 的闸。两个入口共用,免得漏掉一个。
async fn open_archive(
    bytes: Vec<u8>,
) -> Result<async_zip::base::read::mem::ZipFileReader, UpdateError> {
    if declares_zip64(&bytes) {
        return Err(UpdateError::BadArchive(
            "the package says it is zip64, which our packages never are".to_string(),
        ));
    }
    async_zip::base::read::mem::ZipFileReader::new(bytes)
        .await
        .map_err(|error| UpdateError::BadArchive(error.to_string()))
}

/// 打开 zip,取出清单,把每个条目的 SHA-256 算出来。
///
/// 只算不写:这一步之后才知道这包能不能信,写进安装目录是下一步的事。
///
/// 这一段的进度按**条目**数,不按字节:分母得是打开包之前就说得出的数,而
/// 解压后的总字节要等全部解完才知道。九个条目里有两个十几兆的,所以条走得不
/// 匀——但它在动,而且每一格都是真的走完了一条。
fn inspect_archive(
    bytes: Vec<u8>,
    progress: &Progress,
) -> Result<(Manifest, Vec<ArchiveEntry>), UpdateError> {
    futures_lite::future::block_on(async move {
        let zip = open_archive(bytes).await?;

        let names = entry_names(&zip)?;
        if names.len() > MAX_ENTRIES {
            return Err(UpdateError::BadArchive(format!(
                "{} entries is not our package",
                names.len()
            )));
        }

        let manifest_index = names
            .iter()
            .position(|name| name == MANIFEST_NAME)
            .ok_or_else(|| {
                UpdateError::BadArchive("the package has no MANIFEST.json".to_string())
            })?;

        progress.begin(Stage::Checking, names.len() as u64);

        let mut budget = MAX_UNPACKED_BYTES;
        let mut manifest_bytes: Vec<u8> = Vec::new();
        let manifest_limit = MAX_MANIFEST_BYTES.min(budget);
        drain_entry(
            &zip,
            manifest_index,
            manifest_limit,
            &mut budget,
            &mut |chunk| {
                manifest_bytes.extend_from_slice(chunk);
                Ok(())
            },
        )
        .await?;
        let manifest = parse_manifest(&manifest_bytes)?;

        let mut entries = Vec::with_capacity(names.len());
        for (index, name) in names.iter().enumerate() {
            if name.ends_with('/') {
                // 目录条目没有内容。
                entries.push(ArchiveEntry {
                    name: name.clone(),
                    sha256: String::new(),
                });
                progress.advance(1);
                continue;
            }
            let mut hasher = Sha256::new();
            let remaining = budget;
            drain_entry(&zip, index, remaining, &mut budget, &mut |chunk| {
                hasher.update(chunk);
                Ok(())
            })
            .await?;
            entries.push(ArchiveEntry {
                name: name.clone(),
                sha256: hex(&hasher.finalize()),
            });
            progress.advance(1);
        }

        Ok((manifest, entries))
    })
}

/// 条目名,归一化过,顺序和 zip 里的索引一一对应。
fn entry_names(
    zip: &async_zip::base::read::mem::ZipFileReader,
) -> Result<Vec<String>, UpdateError> {
    zip.file()
        .entries()
        .iter()
        .map(|entry| {
            entry
                .filename()
                .as_str()
                .map(normalize_entry_name)
                .map_err(|error| UpdateError::BadArchive(format!("entry name: {error}")))
        })
        .collect()
}

/// 流式读一个条目,每块交给 `sink`,同时从总预算里扣。
///
/// 不用 `read_to_end`:一个几百 KB 的 zip 可以解出几个 G。分块读才能在越线的
/// 那一刻停下,而不是等内存先没。
async fn drain_entry(
    zip: &async_zip::base::read::mem::ZipFileReader,
    index: usize,
    limit: u64,
    budget: &mut u64,
    sink: &mut dyn FnMut(&[u8]) -> Result<(), UpdateError>,
) -> Result<(), UpdateError> {
    let mut reader = zip
        .reader_without_entry(index)
        .await
        .map_err(|error| UpdateError::BadArchive(error.to_string()))?;

    let mut buffer = [0u8; 64 * 1024];
    let mut written: u64 = 0;
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|error| UpdateError::BadArchive(error.to_string()))?;
        if read == 0 {
            break;
        }
        written = written.saturating_add(read as u64);
        if written > limit {
            return Err(UpdateError::TooLarge { limit_bytes: limit });
        }
        *budget = budget.saturating_sub(read as u64);
        if *budget == 0 {
            return Err(UpdateError::TooLarge {
                limit_bytes: MAX_UNPACKED_BYTES,
            });
        }
        match buffer.get(..read) {
            Some(chunk) => sink(chunk)?,
            None => return Err(UpdateError::BadArchive("short read".to_string())),
        }
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // 两位十六进制,`write!` 到 String 不会失败,但也不必用 unwrap 去赌。
        out.push(nibble(byte >> 4));
        out.push(nibble(byte & 0x0f));
    }
    out
}

fn nibble(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        10..=15 => (b'a' + value - 10) as char,
        // 入参已经被掩成 4 位,这条分支到不了;给个不会被误认成十六进制的字符,
        // 让哈希对不上而不是让进程死掉。
        _ => '?',
    }
}

// ---------------------------------------------------------------------------
// 三:换文件
// ---------------------------------------------------------------------------

/// 换完之后的样子。
#[derive(Debug, Clone)]
pub struct Applied {
    pub install_dir: PathBuf,
    pub version: SemanticVersion,
    /// 换掉了哪些(相对安装目录)。
    pub replaced: Vec<String>,
    /// 改名放一边、这一轮删不掉的旧文件。下次启动 `clean_leftovers` 收。
    pub left_behind: Vec<String>,
}

/// 安装目录:exe 旁边。
pub fn install_dir() -> Result<PathBuf, UpdateError> {
    let exe = std::env::current_exe().map_err(|error| UpdateError::Storage {
        path: PathBuf::from("<current exe>"),
        reason: error.to_string(),
    })?;
    match exe.parent() {
        Some(parent) => Ok(parent.to_path_buf()),
        None => Err(UpdateError::Storage {
            path: exe,
            reason: "the program does not seem to live in a folder".to_string(),
        }),
    }
}

/// 先探一下这个目录能不能写。
///
/// 这是**动手之前**做的。没有安装程序,用户很可能把 zip 解到了
/// `C:\Program Files`,那里每一次写都是 ERROR_ACCESS_DENIED,而这个程序没有提权
/// 的路子。换到一半才发现,留下的是一个半新半旧的目录——那是所有结局里最坏的一个。
fn ensure_writable(directory: &Path) -> Result<(), UpdateError> {
    let probe = directory.join(".ptt-update-write-probe");
    match fs::File::create(&probe) {
        Ok(file) => {
            drop(file);
            let _ = fs::remove_file(&probe);
            Ok(())
        }
        Err(_) => Err(UpdateError::ReadOnlyInstall {
            directory: directory.to_path_buf(),
        }),
    }
}

/// 把核对过的包换到安装目录里。
///
/// 两段:先把所有新文件落到各自目的地旁边(只新增,失败了删掉就等于没发生),
/// 全部落稳之后才开始换。换的时候先动那两个被占用的(可逆),再盖其余的。
///
/// 第一段里每个文件都会**再算一次**哈希跟清单对(见 `unpack_all`)。`stage` 核
/// 对的是它下到内存里的那份,这里读的是落过盘、隔了几分钟的那份,不是同一件事。
pub fn apply(staged: &StagedUpdate) -> Result<Applied, UpdateError> {
    apply_into(staged, &install_dir()?)
}

/// 真正干活的那份,安装目录从参数进来。
///
/// 分出这个参数只为一件事:让测试能拿一个临时目录把整条换文件的路真的跑一遍。
/// `install_dir()` 定死在 exe 旁边,而这条路恰恰是最不该只靠读代码来相信的
/// ——顺序错了、撤回漏了,要等到有人的安装目录被换坏那天才知道。
fn apply_into(staged: &StagedUpdate, root: &Path) -> Result<Applied, UpdateError> {
    let root = root.to_path_buf();
    ensure_writable(&root)?;

    let bytes =
        fs::read(&staged.archive).map_err(|error| storage_error(&staged.archive, &error))?;

    // ---- 第一段:落新文件 -------------------------------------------------
    let mut dropped: Vec<DroppedFile> = Vec::new();
    let unpack = unpack_all(bytes, &root, &staged.plan, &mut dropped);
    if let Err(error) = unpack {
        for file in &dropped {
            let _ = fs::remove_file(&file.temporary);
        }
        return Err(error);
    }

    // ---- 第二段:换 -------------------------------------------------------
    let mut aside: Vec<(PathBuf, PathBuf)> = Vec::new(); // (目的地, 让位后的名字)
    let mut replaced: Vec<String> = Vec::new();
    let mut left_behind: Vec<String> = Vec::new();
    let mut overwritten: Vec<String> = Vec::new();

    for file in &dropped {
        let result = match file.planned.placement {
            Placement::RenameAside => rename_aside(&file.destination, &file.temporary, &mut aside),
            Placement::Overwrite => fs::rename(&file.temporary, &file.destination)
                .map_err(|error| storage_error(&file.destination, &error)),
        };

        if let Err(error) = result {
            // 撤回:让位过的原样搬回来。已经盖掉的没法还原,如实说。
            let restored = undo_aside(&aside);
            for leftover in &dropped {
                let _ = fs::remove_file(&leftover.temporary);
            }
            return Err(UpdateError::HalfApplied {
                reason: error.to_string(),
                already_new: overwritten,
                program_restored: restored,
            });
        }

        replaced.push(file.planned.path.clone());
        if file.planned.placement == Placement::Overwrite {
            overwritten.push(file.planned.path.clone());
        }
    }

    for (_, old) in &aside {
        if let Some(name) = old.file_name() {
            left_behind.push(name.to_string_lossy().into_owned());
        }
    }

    Ok(Applied {
        install_dir: root,
        version: staged.version,
        replaced,
        left_behind,
    })
}

/// 一个已经落到目的地旁边、等着被换上去的文件。
struct DroppedFile {
    planned: PlannedFile,
    destination: PathBuf,
    temporary: PathBuf,
}

/// 把计划里的每个文件从 zip 里解到目的地旁边的 `*.new-update`,边写边核对。
///
/// 为什么这里要再算一遍哈希:`stage` 核对的是它自己下到内存里的那份字节,而
/// `apply` 是从 `%LOCALAPPDATA%` 里把 zip **重新读回来**的。中间隔着一次落盘,
/// 隔着几十秒到几分钟——磁盘坏道、杀软"修复"、另一个程序改写了那个文件,都会让
/// 真正写进安装目录的和核对过的不是同一份东西。而这一步之后就是换文件了,那时
/// 再发现已经晚了。核对放在第一段里还有一个好处:落 `*.new-update` 是纯新增,
/// 这一段里发现不对,删掉临时文件就等于什么都没发生过。
fn unpack_all(
    bytes: Vec<u8>,
    root: &Path,
    plan: &[PlannedFile],
    dropped: &mut Vec<DroppedFile>,
) -> Result<(), UpdateError> {
    futures_lite::future::block_on(async move {
        // 这里也过一遍闸,不是多余的:`apply` 是从磁盘上重新读的那份字节,
        // 中间隔着一次落盘,不能假定它还是 `stage` 检过的那些。
        let zip = open_archive(bytes).await?;
        let names = entry_names(&zip)?;
        let mut budget = MAX_UNPACKED_BYTES;

        for planned in plan {
            let Some(destination) = safe_join(root, &planned.path) else {
                return Err(UpdateError::Mismatch(vec![format!(
                    "the manifest lists an unsafe path: {}",
                    planned.path
                )]));
            };
            let index = names
                .iter()
                .position(|name| name == &planned.path)
                .ok_or_else(|| {
                    UpdateError::BadArchive(format!("{} vanished from the package", planned.path))
                })?;

            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|error| storage_error(parent, &error))?;
            }
            let temporary = with_suffix(&destination, NEW_SUFFIX);
            let mut file =
                fs::File::create(&temporary).map_err(|error| storage_error(&temporary, &error))?;
            let remaining = budget;
            let target = temporary.clone();
            let mut hasher = Sha256::new();
            drain_entry(&zip, index, remaining, &mut budget, &mut |chunk| {
                hasher.update(chunk);
                file.write_all(chunk)
                    .map_err(|error| storage_error(&target, &error))
            })
            .await?;
            file.sync_all()
                .map_err(|error| storage_error(&temporary, &error))?;
            drop(file);
            let actual = hex(&hasher.finalize());
            // 先记进 `dropped` 再判:出错时 `apply` 是照着这张表把临时文件删干净的,
            // 漏记一条就在安装目录里留下一个几十兆的孤儿。
            dropped.push(DroppedFile {
                planned: planned.clone(),
                destination,
                temporary,
            });
            if !hashes_match(&actual, &planned.sha256) {
                return Err(UpdateError::Mismatch(vec![format!(
                    "{} does not match the hash the manifest gives for it",
                    planned.path
                )]));
            }
        }
        Ok(())
    })
}

/// 让位再放新的。旧文件留在 `*.old`,这一轮不删——它还被进程占着。
fn rename_aside(
    destination: &Path,
    temporary: &Path,
    aside: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<(), UpdateError> {
    if destination.exists() {
        let old = with_suffix(destination, OLD_SUFFIX);
        // 上一轮留下的同名残骸先让开;删不掉也不致命,改名会覆盖。
        let _ = fs::remove_file(&old);
        fs::rename(destination, &old).map_err(|error| storage_error(destination, &error))?;
        aside.push((destination.to_path_buf(), old));
    }
    fs::rename(temporary, destination).map_err(|error| storage_error(destination, &error))
}

/// 把让过位的搬回来。全部搬回成功才算真的复原。
fn undo_aside(aside: &[(PathBuf, PathBuf)]) -> bool {
    let mut all_back = true;
    for (destination, old) in aside.iter().rev() {
        // 刚放上去的新文件没人占,删得掉。
        let _ = fs::remove_file(destination);
        if fs::rename(old, destination).is_err() {
            all_back = false;
        }
    }
    all_back
}

/// 扫掉上一轮更新留下的 `*.old` 和 `*.new-update`。启动时调用。
///
/// 换文件的那一轮里删不掉这些——exe 和 dll 还被自己占着。等下一次启动,占用它们
/// 的进程已经不在了,才轮得到删。删不掉就跳过:清垃圾失败不该让程序起不来。
/// 返回真正删掉的个数。
///
/// 顺手把 `%LOCALAPPDATA%\PoeTradeTracker\updates\pending-update.zip` 也收了。
/// 那是上一轮下载留下的几十兆,`stage` 只在核对不过时删它,核对过了就一直躺着
/// ——装成功要重启,装失败也没有续传,所以走到"下一次启动"这一刻它一定是废的。
pub fn clean_leftovers() -> usize {
    let mut removed = 0;
    if let Ok(directory) = updates_dir() {
        if fs::remove_file(directory.join(PENDING_ARCHIVE_NAME)).is_ok() {
            removed += 1;
        }
    }
    let Ok(root) = install_dir() else {
        return removed;
    };
    sweep(&root, 0, &mut removed);
    removed
}

/// 递归扫,深度封顶——包里最深的也只是 `assets/ocr/`,不需要往下走很远,
/// 也不该因为安装目录里被人放了一棵大树就在启动时卡住。
fn sweep(directory: &Path, depth: usize, removed: &mut usize) {
    if depth > 3 {
        return;
    }
    let Ok(listing) = fs::read_dir(directory) else {
        return;
    };
    for entry in listing.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            sweep(&path, depth + 1, removed);
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(live) = live_counterpart(&path, name) else {
            continue;
        };
        if live.exists() {
            // 正主还在,这份是真的废料。
            if fs::remove_file(&path).is_ok() {
                *removed += 1;
            }
            continue;
        }
        // 正主不在——上一轮的两次改名之间断了电。这里是唯一还剩下这个文件的
        // 地方,删掉就等于把安装目录弄残,而且再也长不回来。能搬回去的搬回去,
        // 搬不动的就原样留着,总好过什么都不剩。
        if name.ends_with(OLD_SUFFIX) {
            let _ = fs::rename(&path, &live);
        }
    }
}

/// 这个文件名如果是我们的残骸,它对应的正主叫什么。不是残骸就 `None`。
fn live_counterpart(path: &Path, name: &str) -> Option<PathBuf> {
    let stem = if let Some(stem) = name.strip_suffix(NEW_SUFFIX) {
        stem
    } else if let Some(stem) = name.strip_suffix(OLD_SUFFIX) {
        // `.old` 只认那两个被占用的文件,理由见 `is_our_leftover`。
        if !LOCKED_FILES
            .iter()
            .any(|locked| stem.eq_ignore_ascii_case(locked))
        {
            return None;
        }
        stem
    } else {
        return None;
    };
    if stem.is_empty() {
        return None;
    }
    Some(path.with_file_name(stem))
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod update_tests {
    use super::*;

    /// 手点的那次检查要等得比开机那次久。
    ///
    /// 两次检查走的是同一条 `latest_release`,所以很容易被写成同一个超时。但人
    /// 按下按钮之后是在等答案的,12 秒对一条慢线来说太短——而开机那次没人在等,
    /// 它必须短。同一个数字满足不了两边,这条测的就是"它们确实是两个数"。
    #[test]
    fn a_manual_check_waits_longer_than_the_one_at_launch() {
        assert_eq!(check_timeout(CheckKind::Startup), Duration::from_secs(12));
        assert_eq!(check_timeout(CheckKind::Manual), Duration::from_secs(30));
    }

    fn manifest_file(path: &str, sha256: &str) -> ManifestFile {
        ManifestFile {
            path: path.to_string(),
            sha256: sha256.to_string(),
        }
    }

    fn archive_entry(name: &str, sha256: &str) -> ArchiveEntry {
        ArchiveEntry {
            name: normalize_entry_name(name),
            sha256: sha256.to_string(),
        }
    }

    /// 真实包的九个文件,路径写法照 MANIFEST.json 的样子。
    fn real_manifest() -> Manifest {
        Manifest {
            product: "POE Trade Tracker".to_string(),
            version: "0.2.0".to_string(),
            configuration: "release".to_string(),
            built_at: "2026-08-27T00:00:00.0000000Z".to_string(),
            files: vec![
                manifest_file("LICENSE.md", &"a".repeat(64)),
                manifest_file("assets/ocr/PP-OCRv5_mobile_rec.onnx", &"b".repeat(64)),
                manifest_file("assets/ocr/ppocrv5_dict.txt", &"c".repeat(64)),
                manifest_file("licenses/ort.txt", &"d".repeat(64)),
                manifest_file("onnxruntime.dll", &"e".repeat(64)),
                manifest_file("ptt-app.exe", &"f".repeat(64)),
            ],
        }
    }

    fn entries_for(manifest: &Manifest) -> Vec<ArchiveEntry> {
        let mut entries: Vec<ArchiveEntry> = manifest
            .files
            .iter()
            .map(|file| archive_entry(&file.path, &file.sha256))
            .collect();
        entries.push(archive_entry(MANIFEST_NAME, &"0".repeat(64)));
        entries
    }

    // ---- 版本 ----------------------------------------------------------

    #[test]
    fn a_leading_v_is_stripped() {
        assert_eq!(parse_tag("v0.2.0"), Some(SemanticVersion::new(0, 2, 0)));
        assert_eq!(parse_tag("0.2.0"), Some(SemanticVersion::new(0, 2, 0)));
        assert_eq!(
            parse_tag("  v1.10.3  "),
            Some(SemanticVersion::new(1, 10, 3))
        );
    }

    #[test]
    fn a_tag_that_does_not_parse_is_not_an_update() {
        assert_eq!(parse_tag("nightly"), None);
        assert_eq!(parse_tag("v0.2"), None);
        assert_eq!(parse_tag("0.2.0-preview"), None);
        // 关键性质:读不懂的标签走的是"没有更新",不是报错。
        assert_eq!(newer_version("0.1.0", "nightly"), None);
    }

    #[test]
    fn only_a_strictly_higher_version_counts() {
        assert_eq!(
            newer_version("0.1.0", "v0.2.0"),
            Some(SemanticVersion::new(0, 2, 0))
        );
        assert_eq!(newer_version("0.1.0", "v0.1.0"), None);
        assert_eq!(newer_version("0.2.0", "v0.1.9"), None);
        assert_eq!(
            newer_version("0.9.0", "v0.10.0"),
            Some(SemanticVersion::new(0, 10, 0))
        );
    }

    // ---- 挑资产 --------------------------------------------------------

    #[test]
    fn the_zip_with_the_product_slug_wins() {
        let assets = vec![
            ReleaseAsset {
                name: "notes.txt".to_string(),
                browser_download_url: "https://example/notes".to_string(),
                size: 10,
            },
            ReleaseAsset {
                name: "sources.zip".to_string(),
                browser_download_url: "https://example/src".to_string(),
                size: 20,
            },
            ReleaseAsset {
                name: "poe-trade-tracker-0.2.0-nightly.zip".to_string(),
                browser_download_url: "https://example/pkg".to_string(),
                size: 30,
            },
        ];
        let picked = pick_asset(&assets).map(|asset| asset.name.as_str());
        // 名字里的 "preview" 换成了 "nightly",照样认得出来。
        assert_eq!(picked, Some("poe-trade-tracker-0.2.0-nightly.zip"));
    }

    /// 一份**真实形状**的 GitHub 回复,连字段名和多余的字段都照抄。
    ///
    /// 上面两条测的是 `pick_asset` 这个函数,这条测的是另一件事:GitHub 实际发回
    /// 来的那坨 JSON 能不能落进我们这三个字段。它比我们建模的多几十个键(author、
    /// uploader、reactions、mentions_count……),而且会随时再多几个;`serde` 默认
    /// 忽略不认识的键,但这件事只有真拿一份完整的 body 跑一遍才算数——形状对不上
    /// 的话,后果不是挑错资产,是每一次检查都变成"回复看不懂"。
    ///
    /// 顺带把"发布上挂了别的东西"也一起摆进去:校验和文本、给别人用的构建、
    /// 一个 `.txt` 的更新日志。
    const REAL_RELEASE_BODY: &str = r#"{
      "url": "https://api.github.com/repos/SouNdmys/POE-Trade-Tracker/releases/191919191",
      "assets_url": "https://api.github.com/repos/SouNdmys/POE-Trade-Tracker/releases/191919191/assets",
      "upload_url": "https://uploads.github.com/repos/SouNdmys/POE-Trade-Tracker/releases/191919191/assets{?name,label}",
      "html_url": "https://github.com/SouNdmys/POE-Trade-Tracker/releases/tag/v0.2.0",
      "id": 191919191,
      "author": {
        "login": "SouNdmys",
        "id": 12345678,
        "node_id": "MDQ6VXNlcjEyMzQ1Njc4",
        "avatar_url": "https://avatars.githubusercontent.com/u/12345678?v=4",
        "type": "User",
        "site_admin": false
      },
      "node_id": "RE_kwDOAbCdEf4LZ1Zn",
      "tag_name": "v0.2.0",
      "target_commitish": "main",
      "name": "v0.2.0",
      "draft": false,
      "prerelease": false,
      "created_at": "2026-09-01T10:11:12Z",
      "published_at": "2026-09-01T10:20:00Z",
      "assets": [
        {
          "url": "https://api.github.com/repos/SouNdmys/POE-Trade-Tracker/releases/assets/1",
          "id": 1,
          "node_id": "RA_kwDOAbCdEf4AAAAB",
          "name": "SHA256SUMS.txt",
          "label": null,
          "uploader": { "login": "SouNdmys", "id": 12345678, "type": "User" },
          "content_type": "text/plain",
          "state": "uploaded",
          "size": 512,
          "download_count": 3,
          "created_at": "2026-09-01T10:15:00Z",
          "updated_at": "2026-09-01T10:15:01Z",
          "browser_download_url": "https://github.com/SouNdmys/POE-Trade-Tracker/releases/download/v0.2.0/SHA256SUMS.txt"
        },
        {
          "url": "https://api.github.com/repos/SouNdmys/POE-Trade-Tracker/releases/assets/2",
          "id": 2,
          "node_id": "RA_kwDOAbCdEf4AAAAC",
          "name": "poe-trade-tracker-0.2.0-preview.zip",
          "label": null,
          "uploader": { "login": "SouNdmys", "id": 12345678, "type": "User" },
          "content_type": "application/zip",
          "state": "uploaded",
          "size": 26447479,
          "download_count": 41,
          "created_at": "2026-09-01T10:16:00Z",
          "updated_at": "2026-09-01T10:17:30Z",
          "browser_download_url": "https://github.com/SouNdmys/POE-Trade-Tracker/releases/download/v0.2.0/poe-trade-tracker-0.2.0-preview.zip"
        },
        {
          "url": "https://api.github.com/repos/SouNdmys/POE-Trade-Tracker/releases/assets/3",
          "id": 3,
          "node_id": "RA_kwDOAbCdEf4AAAAD",
          "name": "CHANGELOG.txt",
          "label": null,
          "uploader": { "login": "SouNdmys", "id": 12345678, "type": "User" },
          "content_type": "text/plain",
          "state": "uploaded",
          "size": 8804,
          "download_count": 0,
          "created_at": "2026-09-01T10:18:00Z",
          "updated_at": "2026-09-01T10:18:02Z",
          "browser_download_url": "https://github.com/SouNdmys/POE-Trade-Tracker/releases/download/v0.2.0/CHANGELOG.txt"
        }
      ],
      "tarball_url": "https://api.github.com/repos/SouNdmys/POE-Trade-Tracker/tarball/v0.2.0",
      "zipball_url": "https://api.github.com/repos/SouNdmys/POE-Trade-Tracker/zipball/v0.2.0",
      "body": "packaging: the native runtime has provenance now\r\n",
      "mentions_count": 1,
      "reactions": {
        "url": "https://api.github.com/repos/SouNdmys/POE-Trade-Tracker/releases/191919191/reactions",
        "total_count": 2,
        "+1": 2,
        "hooray": 0,
        "rocket": 0
      }
    }"#;

    #[test]
    fn a_real_github_answer_yields_the_package_and_ignores_the_extras() {
        let release: GithubRelease =
            serde_json::from_slice(strip_bom(REAL_RELEASE_BODY.as_bytes()))
                .expect("a real github body deserializes");
        assert_eq!(release.tag_name, "v0.2.0");
        assert_eq!(
            release.html_url,
            "https://github.com/SouNdmys/POE-Trade-Tracker/releases/tag/v0.2.0"
        );
        assert_eq!(release.assets.len(), 3);

        assert_eq!(
            newer_version("0.1.0", &release.tag_name),
            Some(SemanticVersion::new(0, 2, 0))
        );

        let picked = pick_asset(&release.assets).expect("the package is on there");
        assert_eq!(picked.name, "poe-trade-tracker-0.2.0-preview.zip");
        assert_eq!(
            picked.browser_download_url,
            "https://github.com/SouNdmys/POE-Trade-Tracker/releases/download/v0.2.0/poe-trade-tracker-0.2.0-preview.zip"
        );
        assert_eq!(picked.size, 26_447_479);
        // 真实包的大小要落在我们愿意收的范围里,不然一个正常发布会被当成过大。
        assert!(picked.size < MAX_ARCHIVE_BYTES);
    }

    /// 挂了第二个 zip 的时候,挑中的是先上传的那个 slug 匹配项。
    ///
    /// 记下来是因为这条规则**没有更强的依据**:两个名字里都有 slug,谁在前谁赢。
    /// 挑错了不会装坏东西——包自带的清单对不上,`stage` 当场停下——但用户会看见
    /// "下载和它的清单对不上"而不是"更新好了"。所以一次发布只挂一个 zip。
    #[test]
    fn a_second_zip_on_the_same_release_is_ambiguous() {
        let assets = vec![
            ReleaseAsset {
                name: "POE-Trade-Tracker-0.2.0-source.zip".to_string(),
                browser_download_url: "https://example/source".to_string(),
                size: 900_000,
            },
            ReleaseAsset {
                name: "poe-trade-tracker-0.2.0-preview.zip".to_string(),
                browser_download_url: "https://example/pkg".to_string(),
                size: 26_447_479,
            },
        ];
        let picked = pick_asset(&assets).map(|asset| asset.name.as_str());
        assert_eq!(picked, Some("POE-Trade-Tracker-0.2.0-source.zip"));
    }

    #[test]
    fn a_release_without_a_zip_has_no_asset() {
        let assets = vec![ReleaseAsset {
            name: "poe-trade-tracker.exe".to_string(),
            browser_download_url: "https://example/exe".to_string(),
            size: 1,
        }];
        assert!(pick_asset(&assets).is_none());
        assert!(pick_asset(&[]).is_none());
    }

    // ---- 清单解析 ------------------------------------------------------

    const MANIFEST_JSON: &str = r#"{
        "product": "POE Trade Tracker",
        "version": "0.2.0",
        "configuration": "release",
        "builtAt": "2026-08-27T00:00:00.0000000Z",
        "files": [
            { "Path": "ptt-app.exe", "Sha256": "AABB" },
            { "Path": "assets/ocr/ppocrv5_dict.txt", "Sha256": "ccdd" }
        ]
    }"#;

    #[test]
    fn a_manifest_parses_without_a_bom() {
        let manifest = parse_manifest(MANIFEST_JSON.as_bytes()).expect("plain utf-8 manifest");
        assert_eq!(manifest.version, "0.2.0");
        assert_eq!(manifest.product, "POE Trade Tracker");
        assert_eq!(manifest.configuration, "release");
        assert!(manifest.built_at.starts_with("2026-"));
        assert_eq!(manifest.files.len(), 2);
        assert_eq!(manifest.files[0].path, "ptt-app.exe");
        assert_eq!(manifest.files[1].sha256, "ccdd");
    }

    #[test]
    fn a_manifest_parses_with_a_utf8_bom() {
        // Windows PowerShell 5.1 的 Set-Content -Encoding UTF8 写的就是这三个字节。
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend_from_slice(MANIFEST_JSON.as_bytes());
        let manifest = parse_manifest(&bytes).expect("bom-prefixed manifest");
        assert_eq!(manifest.version, "0.2.0");
        assert_eq!(manifest.files.len(), 2);
    }

    #[test]
    fn a_manifest_missing_fields_still_parses() {
        // 每个字段都有 default,少一个不该让整份读不出来。
        let manifest = parse_manifest(br#"{"version":"0.2.0"}"#).expect("sparse manifest");
        assert_eq!(manifest.version, "0.2.0");
        assert!(manifest.files.is_empty());
        assert!(manifest.product.is_empty());
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(parse_manifest(b"not json at all").is_err());
        assert!(parse_manifest(b"").is_err());
    }

    // ---- 条目名与哈希 --------------------------------------------------

    #[test]
    fn both_slash_spellings_normalise_to_the_same_name() {
        assert_eq!(
            normalize_entry_name("assets\\ocr\\ppocrv5_dict.txt"),
            "assets/ocr/ppocrv5_dict.txt"
        );
        assert_eq!(
            normalize_entry_name("assets/ocr/ppocrv5_dict.txt"),
            "assets/ocr/ppocrv5_dict.txt"
        );
        assert_eq!(normalize_entry_name("./ptt-app.exe"), "ptt-app.exe");
        assert_eq!(
            normalize_entry_name("assets\\ocr\\x"),
            normalize_entry_name("assets/ocr/x")
        );
    }

    #[test]
    fn hash_comparison_ignores_case() {
        assert!(hashes_match("ABCDEF", "abcdef"));
        assert!(hashes_match(" abcdef ", "ABCDEF"));
        assert!(!hashes_match("abcdef", "abcde0"));
        assert!(!hashes_match("abcdef", "abcdef0"));
    }

    #[test]
    fn a_backslash_package_reconciles_against_a_forward_slash_manifest() {
        let manifest = real_manifest();
        // Compress-Archive under Windows PowerShell 5.1 的写法。
        let entries: Vec<ArchiveEntry> = manifest
            .files
            .iter()
            .map(|file| archive_entry(&file.path.replace('/', "\\"), &file.sha256))
            .collect();
        assert!(reconcile(&manifest, "v0.2.0", &entries).is_ok());
    }

    #[test]
    fn an_uppercase_hash_in_the_manifest_still_matches() {
        let mut manifest = real_manifest();
        let entries = entries_for(&manifest);
        for file in &mut manifest.files {
            file.sha256 = file.sha256.to_ascii_uppercase();
        }
        assert!(reconcile(&manifest, "v0.2.0", &entries).is_ok());
    }

    // ---- 路径安全 ------------------------------------------------------

    #[test]
    fn a_path_that_climbs_out_is_refused() {
        assert!(is_safe_relative("assets/ocr/x"));
        assert!(!is_safe_relative("../evil"));
        assert!(!is_safe_relative("assets\\..\\..\\evil"));
        assert!(!is_safe_relative("/absolute"));
        assert!(!is_safe_relative("C:/windows/system32/x"));
        assert!(!is_safe_relative(""));
        assert!(safe_join(Path::new("C:/app"), "..\\evil").is_none());
        assert_eq!(
            safe_join(Path::new("C:/app"), "assets\\ocr\\x"),
            Some(Path::new("C:/app").join("assets").join("ocr").join("x"))
        );
    }

    #[test]
    fn a_manifest_that_climbs_out_is_a_mismatch() {
        let mut manifest = real_manifest();
        manifest.files.push(manifest_file(
            "../../Windows/System32/evil.dll",
            &"9".repeat(64),
        ));
        let entries = entries_for(&manifest);
        let error =
            reconcile(&manifest, "v0.2.0", &entries).expect_err("unsafe path must be refused");
        assert!(format!("{error}").contains("unsafe path"), "{error}");
    }

    // ---- 对账 ----------------------------------------------------------

    #[test]
    fn a_clean_package_reconciles() {
        let manifest = real_manifest();
        let entries = entries_for(&manifest);
        assert!(reconcile(&manifest, "v0.2.0", &entries).is_ok());
    }

    #[test]
    fn a_missing_file_is_caught() {
        let manifest = real_manifest();
        let mut entries = entries_for(&manifest);
        entries.retain(|entry| entry.name != "onnxruntime.dll");
        let error =
            reconcile(&manifest, "v0.2.0", &entries).expect_err("missing file must be caught");
        assert!(
            format!("{error}").contains("onnxruntime.dll is listed in the manifest but missing"),
            "{error}"
        );
    }

    #[test]
    fn a_wrong_hash_is_caught() {
        let manifest = real_manifest();
        let mut entries = entries_for(&manifest);
        for entry in &mut entries {
            if entry.name == "ptt-app.exe" {
                entry.sha256 = "1".repeat(64);
            }
        }
        let error =
            reconcile(&manifest, "v0.2.0", &entries).expect_err("wrong hash must be caught");
        assert!(
            format!("{error}").contains("ptt-app.exe does not match the hash"),
            "{error}"
        );
    }

    #[test]
    fn an_extra_unlisted_entry_is_caught() {
        let manifest = real_manifest();
        let mut entries = entries_for(&manifest);
        entries.push(archive_entry("payload.dll", &"7".repeat(64)));
        let error =
            reconcile(&manifest, "v0.2.0", &entries).expect_err("extra entry must be caught");
        assert!(
            format!("{error}").contains("payload.dll is in the package but not listed"),
            "{error}"
        );
    }

    #[test]
    fn the_manifest_itself_is_not_expected_to_be_listed() {
        // MANIFEST.json 不在自己的 files 里,对账必须专门放它过去,
        // 否则每一个包都会被判成"多了一个没登记的文件"。
        let manifest = real_manifest();
        let entries = entries_for(&manifest);
        assert!(entries.iter().any(|entry| entry.name == MANIFEST_NAME));
        assert!(reconcile(&manifest, "v0.2.0", &entries).is_ok());
    }

    #[test]
    fn directory_entries_do_not_count_as_extras() {
        let manifest = real_manifest();
        let mut entries = entries_for(&manifest);
        entries.push(archive_entry("assets/", ""));
        entries.push(archive_entry("assets\\ocr\\", ""));
        assert!(reconcile(&manifest, "v0.2.0", &entries).is_ok());
    }

    #[test]
    fn a_version_that_disagrees_with_the_tag_is_refused() {
        let manifest = real_manifest();
        let entries = entries_for(&manifest);
        let error =
            reconcile(&manifest, "v0.3.0", &entries).expect_err("version mismatch must be caught");
        let message = format!("{error}");
        assert!(message.contains("says it is version 0.2.0"), "{message}");
        assert!(message.contains("tagged 0.3.0"), "{message}");
    }

    #[test]
    fn a_manifest_with_no_version_is_refused() {
        let mut manifest = real_manifest();
        manifest.version = String::new();
        let entries = entries_for(&manifest);
        let error = reconcile(&manifest, "v0.2.0", &entries)
            .expect_err("a version-less package is not usable");
        assert!(
            format!("{error}").contains("does not say which version"),
            "{error}"
        );
    }

    #[test]
    fn a_duplicate_entry_is_caught() {
        let manifest = real_manifest();
        let mut entries = entries_for(&manifest);
        entries.push(archive_entry("ptt-app.exe", &"f".repeat(64)));
        let error = reconcile(&manifest, "v0.2.0", &entries).expect_err("duplicate must be caught");
        assert!(format!("{error}").contains("twice"), "{error}");
    }

    /// 清单自己把同一个文件写两遍也得拦下。
    ///
    /// 这不是洁癖。`swap_plan` 照清单一行一行排,写两遍就换两遍;第二遍走到
    /// `rename_aside` 时,原地放着的已经是刚换上去的新文件,于是**新的**被改名
    /// 成 `.old` 盖掉了真正的旧版本,而那个 `.old` 正是出事时用来复原的东西。
    /// 撤回这时候撤回的是一份假的。
    #[test]
    fn a_manifest_that_lists_the_same_file_twice_is_caught() {
        let mut manifest = real_manifest();
        // 包里那一份是干净的:每个文件只有一个条目。重复只在清单这一侧。
        let entries = entries_for(&manifest);
        let repeat = manifest.files[0].clone();
        manifest.files.push(repeat);
        let error =
            reconcile(&manifest, "v0.2.0", &entries).expect_err("a repeated row must be caught");
        assert!(format!("{error}").contains("twice"), "{error}");
    }

    // ---- 换文件计划 ----------------------------------------------------

    #[test]
    fn only_the_two_locked_files_are_renamed_aside() {
        let plan = swap_plan(&real_manifest());
        let aside: Vec<&str> = plan
            .iter()
            .filter(|file| file.placement == Placement::RenameAside)
            .map(|file| file.path.as_str())
            .collect();
        // 顺序跟着清单里的顺序走(稳定排序),内容才是重点:只有这两个。
        assert_eq!(aside, vec!["onnxruntime.dll", "ptt-app.exe"]);

        let overwritten: Vec<&str> = plan
            .iter()
            .filter(|file| file.placement == Placement::Overwrite)
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(
            overwritten,
            vec![
                "LICENSE.md",
                "assets/ocr/PP-OCRv5_mobile_rec.onnx",
                "assets/ocr/ppocrv5_dict.txt",
                "licenses/ort.txt",
            ]
        );
    }

    #[test]
    fn the_locked_files_are_swapped_first() {
        // 顺序不是装饰:能失败的那一步排在前面,因为只有它撤得回来。
        let plan = swap_plan(&real_manifest());
        let first_overwrite = plan
            .iter()
            .position(|file| file.placement == Placement::Overwrite);
        let last_aside = plan
            .iter()
            .rposition(|file| file.placement == Placement::RenameAside);
        assert_eq!(last_aside, Some(1));
        assert_eq!(first_overwrite, Some(2));
    }

    #[test]
    fn a_backslash_manifest_still_classifies() {
        let manifest = Manifest {
            version: "0.2.0".to_string(),
            files: vec![
                manifest_file("ptt-app.exe", &"a".repeat(64)),
                manifest_file("assets\\ocr\\ppocrv5_dict.txt", &"b".repeat(64)),
            ],
            ..Manifest::default()
        };
        let plan = swap_plan(&manifest);
        assert_eq!(
            plan[0],
            PlannedFile {
                path: "ptt-app.exe".to_string(),
                sha256: "a".repeat(64),
                placement: Placement::RenameAside
            }
        );
        assert_eq!(
            plan[1],
            PlannedFile {
                path: "assets/ocr/ppocrv5_dict.txt".to_string(),
                sha256: "b".repeat(64),
                placement: Placement::Overwrite
            }
        );
    }

    // ---- 杂项 ----------------------------------------------------------

    #[test]
    fn a_suffix_lands_on_the_file_not_the_folder() {
        let path = Path::new("C:/app/ptt-app.exe");
        assert_eq!(
            with_suffix(path, OLD_SUFFIX),
            PathBuf::from("C:/app/ptt-app.exe.old")
        );
        assert_eq!(
            with_suffix(path, NEW_SUFFIX),
            PathBuf::from("C:/app/ptt-app.exe.new-update")
        );
    }

    #[test]
    fn hex_is_lowercase_and_padded() {
        assert_eq!(hex(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
        assert_eq!(hex(&[]), "");
    }

    #[test]
    fn every_error_says_something_a_person_can_act_on() {
        let cases: Vec<UpdateError> = vec![
            UpdateError::Unreachable("dns failure".to_string()),
            UpdateError::Rejected(403),
            UpdateError::Rejected(404),
            UpdateError::Rejected(500),
            UpdateError::MalformedRelease("expected value".to_string()),
            UpdateError::NoPackage {
                tag: "v0.2.0".to_string(),
            },
            UpdateError::TooLarge {
                limit_bytes: MAX_ARCHIVE_BYTES,
            },
            UpdateError::Storage {
                path: PathBuf::from("C:/app/x"),
                reason: "denied".to_string(),
            },
            UpdateError::BadArchive("truncated".to_string()),
            UpdateError::Mismatch(vec!["ptt-app.exe is missing".to_string()]),
            UpdateError::ReadOnlyInstall {
                directory: PathBuf::from("C:/Program Files/x"),
            },
            UpdateError::HalfApplied {
                reason: "denied".to_string(),
                already_new: vec!["LICENSE.md".to_string()],
                program_restored: true,
            },
        ];
        for case in cases {
            let message = format!("{case}");
            assert!(!message.is_empty());
            assert!(
                message
                    .chars()
                    .next()
                    .is_some_and(|first| !first.is_uppercase()),
                "{message}"
            );
        }
        assert!(
            format!("{}", UpdateError::Unreachable("x".to_string()))
                .contains("could not reach github")
        );
        assert!(
            format!(
                "{}",
                UpdateError::ReadOnlyInstall {
                    directory: PathBuf::from("C:/Program Files/x")
                }
            )
            .contains("not writable")
        );
        assert!(
            format!("{}", UpdateError::Mismatch(vec![])).contains("did not match its manifest")
        );
    }

    // ---- 解析器闸门 ----------------------------------------------------

    /// 一份只有尾巴、没有任何内容的 zip64 包,中央目录里写着 `entry_count` 个条目。
    ///
    /// 98 个字节:zip64 的中央目录尾记录 + 定位器 + 普通的尾记录。普通尾记录里
    /// 那两个条目数写成 0xFFFF,按规矩就是"真数在 zip64 那份里"。
    fn zip64_tail_claiming(entry_count: u64) -> Vec<u8> {
        let mut bytes: Vec<u8> = Vec::new();
        // zip64 end of central directory record
        bytes.extend_from_slice(&0x0606_4b50_u32.to_le_bytes());
        bytes.extend_from_slice(&44_u64.to_le_bytes());
        bytes.extend_from_slice(&45_u16.to_le_bytes());
        bytes.extend_from_slice(&45_u16.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&entry_count.to_le_bytes());
        bytes.extend_from_slice(&entry_count.to_le_bytes());
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        // zip64 end of central directory locator
        bytes.extend_from_slice(&0x0706_4b50_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        // end of central directory record
        bytes.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes.extend_from_slice(&0xFFFF_u16.to_le_bytes());
        bytes.extend_from_slice(&0xFFFF_u16.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes
    }

    /// 这条是整份文件里最要命的一条。
    ///
    /// 不拦住的话,这 98 个字节会让 async_zip 去要 2^60 个条目的内存,当场
    /// "capacity overflow"。release 是 `panic = "abort"`,那不是一条错误消息,
    /// 是进程原地消失。所以这里断言的不是"报了哪个错",是"还活着"。
    #[test]
    fn a_zip64_package_is_refused_instead_of_killing_the_process() {
        let out = inspect_archive(zip64_tail_claiming(1 << 60), &Progress::default());
        assert!(matches!(out, Err(UpdateError::BadArchive(_))));
    }

    /// 条目数小到能装得下也一样回绝:我们的包不是 zip64,判据是"声明了 zip64",
    /// 不是"这个数字看着大不大"。数字多大才算大,是没法在这里判断的事。
    #[test]
    fn a_modest_zip64_package_is_refused_too() {
        let out = inspect_archive(zip64_tail_claiming(3), &Progress::default());
        assert!(matches!(out, Err(UpdateError::BadArchive(_))));
    }

    /// 尾巴上垫 66 KiB 也躲不掉。
    ///
    /// async_zip 是按签名往回搜尾记录的,不是按注释长度算的,所以一份把 zip64
    /// 那几个记录藏在离末尾 66 KiB 处的包,它照样找得到、照样会去要那 2^60 个
    /// 条目的内存。我们的窗口必须盖住它够得着的整段范围,窄一个缓冲就漏一个。
    #[test]
    fn a_zip64_tail_padded_far_from_the_end_is_still_refused() {
        let mut bytes = zip64_tail_claiming(1 << 60);
        bytes.resize(bytes.len() + 66_000, 0);
        let out = inspect_archive(bytes, &Progress::default());
        assert!(matches!(out, Err(UpdateError::BadArchive(_))));
    }

    /// 一坨不是 zip 的字节要落进 `Err`,不是落进 panic。
    #[test]
    fn bytes_that_are_not_a_zip_are_an_error() {
        let out = inspect_archive(b"this is not a zip file".to_vec(), &Progress::default());
        assert!(matches!(out, Err(UpdateError::BadArchive(_))));
    }

    /// 一个真的包必须还能开。
    ///
    /// 上面那道 zip64 闸是拿签名在尾巴上找的,找错了就会把好包也拦下来——而那种
    /// 坏法不会有人发现,更新只是从此"永远打不开"。所以这里拿 `Compress-Archive`
    /// 的写法(嵌套目录、反斜杠条目名)现造一个走完整条路:开包、算哈希、对账。
    #[test]
    fn a_real_package_opens_and_reconciles() {
        use async_zip::base::write::ZipFileWriter;
        use async_zip::{Compression, ZipEntryBuilder};

        let bytes = futures_lite::future::block_on(async {
            let mut out: Vec<u8> = Vec::new();
            let mut writer = ZipFileWriter::new(futures_lite::io::Cursor::new(&mut out));
            let payload = vec![7_u8; 300_000];
            let mut hasher = Sha256::new();
            hasher.update(&payload);
            let digest = hex(&hasher.finalize());
            let manifest = format!(
                concat!(
                    r#"{{"product":"POE Trade Tracker","version":"0.9.0","#,
                    r#""configuration":"release","builtAt":"x","#,
                    r#""files":[{{"Path":"assets/ocr/model.onnx","Sha256":"{}"}}]}}"#
                ),
                digest
            );
            for (name, data) in [
                (MANIFEST_NAME, manifest.into_bytes()),
                // 反斜杠:Windows PowerShell 5.1 的 `Compress-Archive` 就是这么写的。
                (r"assets\ocr\model.onnx", payload),
            ] {
                let entry = ZipEntryBuilder::new(name.into(), Compression::Deflate);
                writer
                    .write_entry_whole(entry, &data)
                    .await
                    .expect("writing a fixture entry");
            }
            writer.close().await.expect("closing the fixture");
            out
        });

        assert!(!declares_zip64(&bytes));
        let (manifest, entries) =
            inspect_archive(bytes, &Progress::default()).expect("a real package opens");
        assert_eq!(manifest.version, "0.9.0");
        // 条目名进来就已经是斜杠写法了,清单才对得上。
        assert!(entries.iter().any(|e| e.name == "assets/ocr/model.onnx"));
        reconcile(&manifest, "v0.9.0", &entries).expect("a real package reconciles");
    }

    // ---- 扫残骸 --------------------------------------------------------

    /// `.new-update` 是我们造的后缀,`.old` 不是。
    ///
    /// 这个清理在**每次启动**时跑,而且是直接删。没有安装程序,包可以被解到任何
    /// 一个本来就有东西的文件夹里——见一个 `.old` 就删,删的是别人的备份。
    #[test]
    fn only_our_own_leftovers_are_swept() {
        let ours = |name: &str| live_counterpart(&PathBuf::from("C:/app").join(name), name);
        assert_eq!(
            ours("ptt-app.exe.old"),
            Some(PathBuf::from("C:/app/ptt-app.exe"))
        );
        assert_eq!(
            ours("onnxruntime.dll.old"),
            Some(PathBuf::from("C:/app/onnxruntime.dll"))
        );
        assert_eq!(
            ours("ptt-app.exe.new-update"),
            Some(PathBuf::from("C:/app/ptt-app.exe"))
        );
        assert_eq!(
            ours("assets-anything.new-update"),
            Some(PathBuf::from("C:/app/assets-anything"))
        );
        // 用户自己的备份,一个都不许碰。
        assert_eq!(ours("报表.xlsx.old"), None);
        assert_eq!(ours("save.old"), None);
        assert_eq!(ours("ptt-app.exe.old.old"), None);
        assert_eq!(ours(".old"), None);
        assert_eq!(ours(".new-update"), None);
        assert_eq!(ours("ptt-app.exe"), None);
    }

    // ---- 真的动磁盘:换文件被打断之后 ------------------------------------

    /// 一个只属于这一条测试的空目录。用完自己收。
    ///
    /// 不引 `tempfile`:这里要的只是"一个没人用的文件夹",而多一个依赖就多一份
    /// 要跟着升的东西。名字带上进程号和一个自增数,并行跑的测试之间不会撞。
    fn scratch(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "ptt-update-test-{tag}-{}-{serial}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a scratch folder");
        dir
    }

    /// 这条钉的是整条更新路上最坏的一个结局。
    ///
    /// 换 dll 那一步是两次改名:先 `onnxruntime.dll` -> `onnxruntime.dll.old`,
    /// 再 `onnxruntime.dll.new-update` -> `onnxruntime.dll`。两次之间进程如果没
    /// 了(崩溃、任务管理器、关机),目录里就**没有** `onnxruntime.dll` 这个名字了。
    ///
    /// 而 dll 排在主程序前面(清单按路径排序,`onnxruntime` < `ptt-app`),所以
    /// 此刻主程序还是旧的那个——**程序照样启动得起来**,启动就会跑
    /// `clean_leftovers`。它要是把 `.old` 和 `.new-update` 一起删了,这台机器上
    /// 就再也没有 onnxruntime.dll 了:程序还开得起来,只是从此永远认不出字,
    /// 而且没有任何提示说是为什么。清垃圾必须先看一眼正主还在不在。
    #[test]
    fn a_swap_cut_in_half_is_put_back_instead_of_swept_away() {
        let dir = scratch("cut-in-half");
        fs::write(dir.join("onnxruntime.dll.old"), b"the dll that works").expect("fixture");
        fs::write(dir.join("onnxruntime.dll.new-update"), b"the new dll").expect("fixture");
        fs::write(dir.join("ptt-app.exe"), b"the old program").expect("fixture");

        let mut removed = 0;
        sweep(&dir, 0, &mut removed);

        assert!(
            dir.join("onnxruntime.dll").exists(),
            "the dll must be put back, not deleted"
        );
        assert_eq!(
            fs::read(dir.join("onnxruntime.dll")).expect("the restored dll"),
            b"the dll that works",
            "the copy that is known to run is the one to restore"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// 主程序那一步被打断也一样:`ptt-app.exe` 这个名字暂时不存在。
    ///
    /// 这一种用户自己是启动不起来的(exe 没了),得靠手动改名回来。但只要他哪天
    /// 从别处启动了这个程序,清理也不许把最后一份删掉。
    #[test]
    fn a_missing_program_is_restored_from_its_aside_copy() {
        let dir = scratch("no-exe");
        fs::write(dir.join("ptt-app.exe.old"), b"the program that runs").expect("fixture");
        fs::write(dir.join("onnxruntime.dll"), b"dll").expect("fixture");

        let mut removed = 0;
        sweep(&dir, 0, &mut removed);

        assert_eq!(
            fs::read(dir.join("ptt-app.exe")).expect("the restored program"),
            b"the program that runs"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// 正主还在的时候,残骸照旧扫掉——这才是每次启动的常态。
    #[test]
    fn leftovers_beside_a_live_file_are_still_swept() {
        let dir = scratch("normal");
        fs::create_dir_all(dir.join("assets/ocr")).expect("fixture");
        fs::write(dir.join("ptt-app.exe"), b"new program").expect("fixture");
        fs::write(dir.join("ptt-app.exe.old"), b"old program").expect("fixture");
        fs::write(dir.join("onnxruntime.dll"), b"new dll").expect("fixture");
        fs::write(dir.join("onnxruntime.dll.old"), b"old dll").expect("fixture");
        fs::write(dir.join("assets/ocr/dict.txt"), b"dict").expect("fixture");
        fs::write(dir.join("assets/ocr/dict.txt.new-update"), b"half").expect("fixture");
        // 用户自己的东西,一根手指都不许碰。
        fs::write(dir.join("my-notes.txt.old"), b"mine").expect("fixture");

        let mut removed = 0;
        sweep(&dir, 0, &mut removed);

        assert_eq!(removed, 3);
        // 见下:`clean_leftovers` 还会额外收掉那个待装的 zip,这里只数目录里的。
        assert!(!dir.join("ptt-app.exe.old").exists());
        assert!(!dir.join("onnxruntime.dll.old").exists());
        assert!(!dir.join("assets/ocr/dict.txt.new-update").exists());
        assert!(dir.join("my-notes.txt.old").exists());
        assert_eq!(
            fs::read(dir.join("ptt-app.exe")).expect("the live program"),
            b"new program",
            "sweeping must never write over a file that is already there"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- 真的动磁盘:落新文件 --------------------------------------------

    /// 造一个 `Compress-Archive` 那种写法的包:嵌套目录用反斜杠。
    fn zip_of(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        use async_zip::base::write::ZipFileWriter;
        use async_zip::{Compression, ZipEntryBuilder};

        futures_lite::future::block_on(async {
            let mut out: Vec<u8> = Vec::new();
            let mut writer = ZipFileWriter::new(futures_lite::io::Cursor::new(&mut out));
            for (name, data) in files {
                let entry = ZipEntryBuilder::new((*name).into(), Compression::Deflate);
                writer
                    .write_entry_whole(entry, data)
                    .await
                    .expect("writing a fixture entry");
            }
            writer.close().await.expect("closing the fixture");
            out
        })
    }

    fn digest_of(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hex(&hasher.finalize())
    }

    /// 落文件那一段必须自己再核对一次,不能靠 `stage` 那一次。
    ///
    /// `stage` 核对的是它下到内存里的那份字节;`apply` 是从
    /// `%LOCALAPPDATA%\PoeTradeTracker\updates\` 把 zip 重新读回来的。中间隔着
    /// 一次落盘和几分钟——磁盘坏道、杀软"修复"、别的程序改写了那个文件,写进安装
    /// 目录的就不再是核对过的那份。而这一段的下一步就是换文件,那时候才发现已经
    /// 换不回来了。
    #[test]
    fn bytes_that_stopped_matching_the_manifest_never_get_swapped_in() {
        let dir = scratch("tampered");
        let real = b"the payload the manifest describes".to_vec();
        let tampered = b"something else entirely".to_vec();
        let manifest = Manifest {
            version: "0.9.0".to_string(),
            files: vec![manifest_file("LICENSE.md", &digest_of(&real))],
            ..Manifest::default()
        };
        // 包里躺的是被换掉的那份,清单说的还是原来那份的哈希。
        let bytes = zip_of(&[(MANIFEST_NAME, b"{}".to_vec()), ("LICENSE.md", tampered)]);

        let mut dropped: Vec<DroppedFile> = Vec::new();
        let out = unpack_all(bytes, &dir, &swap_plan(&manifest), &mut dropped);

        assert!(
            matches!(out, Err(UpdateError::Mismatch(_))),
            "unexpected: {out:?}"
        );
        // 而且是在动真格之前停的:安装目录里没有 LICENSE.md,只有一个待清理的
        // 临时文件,`apply` 照着 `dropped` 就能把它删干净。
        assert!(!dir.join("LICENSE.md").exists());
        assert_eq!(dropped.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    /// 对得上的时候照常落到 `*.new-update`,一个字节都不差。
    #[test]
    fn a_matching_payload_lands_beside_its_destination() {
        let dir = scratch("unpack-ok");
        let payload = vec![3_u8; 40_000];
        let manifest = Manifest {
            version: "0.9.0".to_string(),
            files: vec![manifest_file("assets/ocr/dict.txt", &digest_of(&payload))],
            ..Manifest::default()
        };
        let bytes = zip_of(&[
            (MANIFEST_NAME, b"{}".to_vec()),
            (r"assets\ocr\dict.txt", payload.clone()),
        ]);

        let mut dropped: Vec<DroppedFile> = Vec::new();
        unpack_all(bytes, &dir, &swap_plan(&manifest), &mut dropped)
            .expect("a matching payload unpacks");

        let landed = dir.join("assets/ocr/dict.txt.new-update");
        assert!(landed.exists(), "the new file lands beside its destination");
        assert_eq!(fs::read(&landed).expect("the new file"), payload);
        // 第一段只新增,不碰正主。
        assert!(!dir.join("assets/ocr/dict.txt").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- 真的动磁盘:整条换文件的路 --------------------------------------

    /// 一个假的安装目录 + 一份装着新内容的包,内容按文件名区分新旧。
    fn install_fixture(dir: &Path) -> (Vec<u8>, Manifest) {
        let payloads: Vec<(&str, Vec<u8>)> = vec![
            ("onnxruntime.dll", b"NEW dll".to_vec()),
            ("ptt-app.exe", b"NEW program".to_vec()),
            ("LICENSE.md", b"NEW licence".to_vec()),
            ("assets/ocr/dict.txt", b"NEW dictionary".to_vec()),
        ];
        for (name, _) in &payloads {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("fixture folder");
            }
            fs::write(&path, format!("OLD {name}")).expect("fixture file");
        }
        let manifest = Manifest {
            version: "0.9.0".to_string(),
            // 清单按路径排序,和打包脚本一样。
            files: {
                let mut files: Vec<ManifestFile> = payloads
                    .iter()
                    .map(|(name, data)| manifest_file(name, &digest_of(data)))
                    .collect();
                files.sort_by(|a, b| a.path.cmp(&b.path));
                files
            },
            ..Manifest::default()
        };
        let mut in_zip: Vec<(&str, Vec<u8>)> = vec![(MANIFEST_NAME, b"{}".to_vec())];
        in_zip.extend(payloads);
        (zip_of(&in_zip), manifest)
    }

    fn staged_from(dir: &Path, archive: &Path, bytes: &[u8], manifest: Manifest) -> StagedUpdate {
        let _ = dir;
        fs::write(archive, bytes).expect("the staged archive");
        StagedUpdate {
            tag: "v0.9.0".to_string(),
            version: SemanticVersion::new(0, 9, 0),
            archive: archive.to_path_buf(),
            plan: swap_plan(&manifest),
            manifest,
        }
    }

    /// 一整轮换文件走完之后,目录里每个文件都得是新的,而且那两个被占用的
    /// 留下 `.old` 等下次启动收——那正是 `clean_leftovers` 存在的理由。
    #[test]
    fn a_whole_swap_leaves_every_file_new_and_the_two_asides_behind() {
        let dir = scratch("swap-all");
        let (bytes, manifest) = install_fixture(&dir);
        let archive = dir.join("pending.zip");
        let staged = staged_from(&dir, &archive, &bytes, manifest);

        let applied = apply_into(&staged, &dir).expect("a clean swap");

        for name in ["onnxruntime.dll", "ptt-app.exe", "LICENSE.md"] {
            let got = fs::read_to_string(dir.join(name)).expect(name);
            assert!(got.starts_with("NEW"), "{name} is still {got}");
        }
        assert_eq!(
            fs::read_to_string(dir.join("assets/ocr/dict.txt")).expect("dict"),
            "NEW dictionary"
        );
        // 被占用的那两个删不掉,只能改名放一边。
        assert!(dir.join("onnxruntime.dll.old").exists());
        assert!(dir.join("ptt-app.exe.old").exists());
        assert_eq!(applied.left_behind.len(), 2);
        assert_eq!(applied.replaced.len(), 4);
        // 一个 `.new-update` 都不许剩:剩下的会被下次启动当垃圾删,而它可能是
        // 某个文件唯一的新版本。
        let mut leftovers: Vec<String> = Vec::new();
        for entry in fs::read_dir(&dir).expect("listing").flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(NEW_SUFFIX) {
                leftovers.push(name);
            }
        }
        assert!(leftovers.is_empty(), "{leftovers:?}");

        // 换完之后再开一次程序:残骸收干净,正主一个不动。
        let mut removed = 0;
        sweep(&dir, 0, &mut removed);
        assert_eq!(removed, 2);
        assert_eq!(
            fs::read_to_string(dir.join("ptt-app.exe")).expect("the program"),
            "NEW program"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// 换到一半断电:dll 已经让位,新的还没放上去。
    ///
    /// 这一步在计划里排第一(清单按路径排序,`onnxruntime` 排在 `ptt-app` 前面),
    /// 所以此刻主程序还是旧的,**程序下次照样启动得起来**——也就是说
    /// `clean_leftovers` 一定会跑到这个目录上。它必须把 dll 放回去,而不是把
    /// 最后两份都扫掉。
    #[test]
    fn a_power_cut_between_the_two_renames_is_recovered_at_the_next_launch() {
        let dir = scratch("power-cut");
        let (bytes, manifest) = install_fixture(&dir);
        let plan = swap_plan(&manifest);
        assert_eq!(plan[0].path, "onnxruntime.dll", "the dll is swapped first");

        // 第一段照跑:新文件落到目的地旁边。
        let mut dropped: Vec<DroppedFile> = Vec::new();
        unpack_all(bytes, &dir, &plan, &mut dropped).expect("unpacking");

        // 第二段只走到第一次改名就"断电"。
        let dll = dir.join("onnxruntime.dll");
        fs::rename(&dll, with_suffix(&dll, OLD_SUFFIX)).expect("the aside rename");
        assert!(!dll.exists(), "this is the window we are worried about");

        // 下一次启动。
        let mut removed = 0;
        sweep(&dir, 0, &mut removed);

        assert_eq!(
            fs::read_to_string(&dll).expect("the dll must be back"),
            "OLD onnxruntime.dll",
            "the copy that is known to work is the one to put back"
        );
        // 主程序没被碰过,还是旧的——半新半旧里最不坏的那种:整份都是旧的。
        assert_eq!(
            fs::read_to_string(dir.join("ptt-app.exe")).expect("the program"),
            "OLD ptt-app.exe"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// 换文件那一步失败时,让过位的必须原样搬回来。
    ///
    /// 拿一个"目的地是文件夹"的坏路径去逼真实的失败:`LICENSE.md` 在假安装目录
    /// 里做成一个目录,`fs::rename` 盖不上去。此刻计划里排在它前面的 dll 和 exe
    /// 都已经换成新的了,撤回必须把这两个都搬回旧的。
    #[test]
    fn a_failure_in_the_overwrite_phase_puts_the_program_back() {
        let dir = scratch("undo");
        let (bytes, manifest) = install_fixture(&dir);
        let archive = dir.join("pending.zip");
        // 把 LICENSE.md 换成一个非空目录:改名盖不过去,这一步一定失败。
        fs::remove_file(dir.join("LICENSE.md")).expect("clearing the file");
        fs::create_dir_all(dir.join("LICENSE.md/inner")).expect("the blocking folder");
        let staged = staged_from(&dir, &archive, &bytes, manifest);

        let out = apply_into(&staged, &dir);
        let Err(UpdateError::HalfApplied {
            program_restored, ..
        }) = out
        else {
            panic!("expected a half-applied report, got {out:?}");
        };
        assert!(program_restored, "the two locked files must be rolled back");
        assert_eq!(
            fs::read_to_string(dir.join("ptt-app.exe")).expect("the program"),
            "OLD ptt-app.exe"
        );
        assert_eq!(
            fs::read_to_string(dir.join("onnxruntime.dll")).expect("the dll"),
            "OLD onnxruntime.dll"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// 写不进去的安装目录要在**动手之前**就被回绝。
    ///
    /// 没有安装程序,用户完全可能把 zip 解到 `C:\Program Files`。换到一半才发现
    /// 写不进去,留下的是一个半新半旧的目录——所有结局里最坏的那个。
    #[test]
    fn a_folder_that_does_not_exist_is_refused_before_anything_is_touched() {
        let dir = scratch("readonly");
        let missing = dir.join("nowhere");
        let (bytes, manifest) = install_fixture(&dir);
        let archive = dir.join("pending.zip");
        let staged = staged_from(&dir, &archive, &bytes, manifest);

        let out = apply_into(&staged, &missing);
        assert!(
            matches!(out, Err(UpdateError::ReadOnlyInstall { .. })),
            "unexpected: {out:?}"
        );
        // 装在别处的那份一个字节都没动。
        assert_eq!(
            fs::read_to_string(dir.join("ptt-app.exe")).expect("the program"),
            "OLD ptt-app.exe"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- 真的拿打包脚本出来的那个 zip 跑一遍 ------------------------------

    /// `target/package/` 里最新的那个 zip,没有就 `None`。
    fn packaged_zip() -> Option<PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/package");
        let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
        for entry in fs::read_dir(root).ok()?.flatten() {
            let path = entry.path();
            let is_zip = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"));
            if !is_zip {
                continue;
            }
            let Ok(when) = entry.metadata().and_then(|meta| meta.modified()) else {
                continue;
            };
            if newest.as_ref().is_none_or(|(seen, _)| when > *seen) {
                newest = Some((when, path));
            }
        }
        newest.map(|(_, path)| path)
    }

    /// 拿 `packaging/package-preview.ps1` **真正产出**的那个 zip 走完整条路。
    ///
    /// 上面所有对账测试用的都是手写的 fixture,而 fixture 是照"我们以为脚本会
    /// 输出什么"造的。这条测的是另一件事:两边是不是真的对得上。Windows
    /// PowerShell 5.1 会给 MANIFEST.json 加 BOM、把嵌套条目名写成反斜杠,pwsh 7
    /// 两样都不会——同一个脚本出来两种字节,而只有真跑一次才知道手上这台是哪种。
    ///
    /// `target/package/` 平时是空的,所以没有包时这条**跳过**而不是失败:
    /// 让 `cargo test --workspace` 依赖一次打包不现实。要看它跑,先跑一次
    /// `packaging/package-preview.ps1 -SkipBuild`,再
    /// `cargo test -p ptt-app --lib the_real_package -- --nocapture`。
    #[test]
    fn the_real_package_from_the_packaging_script_installs_end_to_end() {
        let Some(zip) = packaged_zip() else {
            eprintln!("skipped: no zip in target/package - run packaging/package-preview.ps1");
            return;
        };
        eprintln!("checking {}", zip.display());

        let bytes = fs::read(&zip).expect("reading the packaged zip");
        eprintln!("  {} bytes on disk", bytes.len());

        let (manifest, entries) =
            inspect_archive(bytes.clone(), &Progress::default()).expect("the real package opens");
        eprintln!(
            "  manifest: product={:?} version={:?} configuration={:?} files={}",
            manifest.product,
            manifest.version,
            manifest.configuration,
            manifest.files.len()
        );
        for entry in &entries {
            eprintln!("  entry {} sha={}", entry.name, entry.sha256);
        }

        // MANIFEST.json 不在它自己的 files 里,这是脚本的写法。对账必须放过它,
        // 否则每一个好包都会被判成"包里有清单没列的文件"。
        assert!(
            !manifest.files.iter().any(|file| file.path == MANIFEST_NAME),
            "the script does not list MANIFEST.json inside itself"
        );
        assert!(
            entries.iter().any(|entry| entry.name == MANIFEST_NAME),
            "but it is in the zip"
        );

        // 打包脚本是拿正则从根 `Cargo.toml` 的 `^version = "..."` 抠版本号的,
        // 而更新检查比的是 ptt-app 自己的 `CARGO_PKG_VERSION`。两条路取的是同一个
        // 数,这个包才是这棵树的包。对不上说明手上这份是上一个版本剩下的——它证明
        // 不了现在的代码,所以跳过而不是判红:`cargo test --workspace` 不该因为
        // `target/` 里躺着一个旧产物就失败。
        if manifest.version != env!("CARGO_PKG_VERSION") || manifest.configuration != "release" {
            eprintln!(
                "skipped: this package is {} {}, but the tree is {} release - repackage",
                manifest.version,
                manifest.configuration,
                env!("CARGO_PKG_VERSION")
            );
            return;
        }

        // 真实清单里的路径写法必须是 `safe_join` 收得下的那种。
        for file in &manifest.files {
            assert!(is_safe_relative(&file.path), "{}", file.path);
            assert!(!file.path.contains('\\'), "{}", file.path);
            assert_eq!(file.sha256.len(), 64, "{}", file.path);
        }

        // 排序那一步是拿真实文件名跑的:清单按路径排序,两个被占用的文件天然
        // 排在最后(`onnxruntime.dll`、`ptt-app.exe` 字母序在 `licenses/` 后面),
        // 所以"让位的排前面"必须真的把它们提上来,否则可逆的那一步排在了不可逆
        // 的后面。
        let plan = swap_plan(&manifest);
        let aside: Vec<&str> = plan
            .iter()
            .filter(|file| file.placement == Placement::RenameAside)
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(aside, vec!["onnxruntime.dll", "ptt-app.exe"]);
        assert_eq!(
            plan.first().map(|f| f.placement),
            Some(Placement::RenameAside)
        );
        assert_eq!(
            plan.get(1).map(|f| f.placement),
            Some(Placement::RenameAside)
        );

        // 发布时标签就是清单里那个版本号加个 v。
        let tag = format!("v{}", manifest.version);
        reconcile(&manifest, &tag, &entries).expect("the real package reconciles");
        eprintln!("  reconcile ok against tag {tag}");

        // 真的装一遍到一个空目录里,九个条目一个不落地落地。
        let dir = scratch("real-package");
        fs::create_dir_all(&dir).expect("the fake install folder");
        let version = parse_tag(&tag).expect("the packaged version parses");
        let staged = StagedUpdate {
            tag,
            version,
            archive: zip,
            manifest: manifest.clone(),
            plan: swap_plan(&manifest),
        };
        let applied = apply_into(&staged, &dir).expect("the real package installs");
        eprintln!("  installed {} files", applied.replaced.len());

        assert_eq!(applied.replaced.len(), manifest.files.len());
        for file in &manifest.files {
            let landed = dir.join(normalize_entry_name(&file.path));
            let got = fs::read(&landed).unwrap_or_else(|_| panic!("{} did not land", file.path));
            let mut hasher = Sha256::new();
            hasher.update(&got);
            assert!(
                hashes_match(&hex(&hasher.finalize()), &file.sha256),
                "{} landed with the wrong bytes",
                file.path
            );
        }
        // 空目录里没有正主可以让位,所以这一轮一个 `.old` 都不该有。
        assert!(applied.left_behind.is_empty(), "{:?}", applied.left_behind);

        // 真正的升级是装在**已经有一份**的目录上。再装一遍同一个包,这次两个被
        // 占用的文件有正主要让位,`.old` 才该出现——上面那一轮是空目录,走的是
        // `rename_aside` 里"目的地不存在"的那条分支,盖不到这一条。
        let again = apply_into(&staged, &dir).expect("installing over an existing copy");
        eprintln!("  reinstalled, left behind {:?}", again.left_behind);
        let mut asides = again.left_behind.clone();
        asides.sort();
        assert_eq!(asides, vec!["onnxruntime.dll.old", "ptt-app.exe.old"]);
        assert!(dir.join("ptt-app.exe.old").exists());
        // 换完之后目录里每个正主还是对的字节。
        for file in &manifest.files {
            let landed = dir.join(normalize_entry_name(&file.path));
            let got = fs::read(&landed).unwrap_or_else(|_| panic!("{} vanished", file.path));
            let mut hasher = Sha256::new();
            hasher.update(&got);
            assert!(hashes_match(&hex(&hasher.finalize()), &file.sha256));
        }
        // 下一次启动把残骸收干净:两个 `.old`,一个 `.new-update` 都不剩。
        let mut removed = 0;
        sweep(&dir, 0, &mut removed);
        eprintln!("  swept {removed} leftovers");
        assert_eq!(removed, 2);
        assert!(dir.join("ptt-app.exe").exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
