//! 薄网络层：从 CDN 取一个小时的原始字节，判断全部留给纯函数。
//!
//! update.rs 的纪律照搬：全 Result（release 是 `panic = "abort"`）、限时、
//! 限长、显式 User-Agent。这里刻意不解析、不重试、不睡眠——重试节奏和
//! 缓存属于调用方，网络层做得越少，测不到的代码就越少。

use std::time::Duration;

use thiserror::Error;

/// GGG 要求调用 API 的程序带可识别的 User-Agent。
/// 公开端点用不上 OAuth 那套格式，但带上名字和联系方式是正确做法。
const USER_AGENT: &str = concat!(
    "POE-Trade-Tracker/",
    env!("CARGO_PKG_VERSION"),
    " (contact: soundmys1994@gmail.com)"
);

/// 实测一小时全联赛约 1.3MB；赛季开服的小时会更大，留一个数量级的余量。
/// 超过 16MB 说明端点的行为变了，该报错让人来看，而不是默默吃内存。
const MAX_HOUR_BYTES: u64 = 16 * 1024 * 1024;

/// 单请求限时。后台补拉挂在一个假死的网络上没有任何意义。
const TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Debug, Error)]
pub enum FetchError {
    /// 对面拒绝（HTTP 状态码）。CDN 上没见过限速头，但 404/5xx 都可能。
    #[error("server rejected the request with status {0}")]
    Rejected(u16),
    #[error("response exceeded the {limit_bytes} byte limit")]
    TooLarge { limit_bytes: u64 },
    /// 根本没连上。断网、DNS、TLS 全归这类——对调用方来说都是"稍后再试"。
    #[error("could not reach the endpoint: {0}")]
    Unreachable(String),
}

/// URL 拼装是纯函数，单独拎出来是为了测试能锁住路径形状——
/// 这个字符串写错的话，其余一切都对也全白搭。
///
/// 参数是仓库里的游戏键（"poe1" / "poe2"），不是 CDN 的 realm：POE1 是原版，
/// 路径里根本没有 realm 段，只有 POE2 才多一截 "/poe2"。两者的差别只让这一个
/// 函数知道，存储、缓存文件名照旧用 "poe1" 当键。
#[must_use]
pub fn hour_url(game: &str, hour_ts: u64) -> String {
    match game {
        "poe1" => format!("https://web.poecdn.com/api/currency-exchange/{hour_ts}"),
        realm => format!("https://web.poecdn.com/api/currency-exchange/{realm}/{hour_ts}"),
    }
}

/// 持有连接池的抓取器。回补一次要跑几百个请求，
/// 每次现建 agent 会把 TLS 握手也做几百遍。
pub struct ExchangeFetcher {
    agent: ureq::Agent,
}

impl Default for ExchangeFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl ExchangeFetcher {
    #[must_use]
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .build();
        Self {
            agent: config.into(),
        }
    }

    /// 取一个整点小时的原始字节。不解析：原始字节要先落盘缓存
    /// （CDN 数据不可变，缓存永不过期），解析失败时人还能看到原文。
    pub fn fetch_hour(&self, game: &str, hour_ts: u64) -> Result<Vec<u8>, FetchError> {
        let mut response = self
            .agent
            .get(hour_url(game, hour_ts))
            .header("User-Agent", USER_AGENT)
            .call()
            .map_err(classify_transport)?;
        response
            .body_mut()
            .with_config()
            .limit(MAX_HOUR_BYTES)
            .read_to_vec()
            .map_err(classify_transport)
    }
}

/// 把 ureq 的错分成"对面拒绝"和"根本没连上"两类，别的都当连不上。
fn classify_transport(error: ureq::Error) -> FetchError {
    match error {
        ureq::Error::StatusCode(status) => FetchError::Rejected(status),
        ureq::Error::BodyExceedsLimit(_) => FetchError::TooLarge {
            limit_bytes: MAX_HOUR_BYTES,
        },
        other => FetchError::Unreachable(other.to_string()),
    }
}

#[cfg(test)]
mod fetch_tests {
    use super::*;

    #[test]
    fn url_shape_is_locked() {
        assert_eq!(
            hour_url("poe2", 1_788_159_600),
            "https://web.poecdn.com/api/currency-exchange/poe2/1788159600"
        );
    }

    /// POE1 是原版，CDN 路径里没有 realm 段——拼成 "/poe1/" 会得到 404，
    /// 用户拉 3.29 赛季时就是这么撞上的。存储键照旧叫 "poe1"，只有 URL 不带它。
    #[test]
    fn poe1_url_has_no_realm_segment() {
        assert_eq!(
            hour_url("poe1", 1_787_054_400),
            "https://web.poecdn.com/api/currency-exchange/1787054400"
        );
    }
}
