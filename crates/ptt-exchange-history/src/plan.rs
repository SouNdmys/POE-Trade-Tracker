//! 抓取计划：纯的小时网格算术，一行网络代码都没有。
//!
//! 断点续传不追 `next_change_id` 链——水位 +3600 一路推进到最新完整小时，
//! 天然可重启、可跳洞、可测试。`next_change_id` 只在解析后做校验断言。

/// 空响应的两种含义。踩错这条会留下永久空洞，趋势悄悄偏。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmptyHourVerdict {
    /// 距 now 太近：可能只是还没发布（实测延迟 1–2 小时），不写 mark，
    /// 下一轮同步自然重试。
    RetryLater,
    /// 足够老还空着 = 真的没数据（赛季开始前）。写 market_count=0 的 mark，
    /// 永不重访。
    ConfirmedEmpty,
}

/// 三小时护栏：发布延迟实测 1–2 小时，留一小时余量。
const EMPTY_HOUR_GRACE_SECONDS: i64 = 3 * 3600;

#[must_use]
pub fn classify_empty(hour_ts: i64, now_ts: i64) -> EmptyHourVerdict {
    if now_ts - hour_ts < EMPTY_HOUR_GRACE_SECONDS {
        EmptyHourVerdict::RetryLater
    } else {
        EmptyHourVerdict::ConfirmedEmpty
    }
}

/// 这一轮该抓哪些整点，升序（旧 → 新）。
///
/// 升序是刻意的：水位连续前进，中断后重启不留洞。上限 `max_hours_per_run`
/// 是界不是目标——超出的部分下一轮接着跑，和日 rollup 的
/// `MAX_ROLLUP_DAYS_PER_RUN` 同一个精神。
///
/// - `watermark`：已抓的最大整点（来自 `exchange_watermark`），`None` = 冷启动
/// - `floor_ts`：回补下限（赛季起点之类），`None` 时用 `now - backfill_days`
/// - 目标上限：最新**完整**小时（当前小时结构性为空，不抓）
#[must_use]
pub fn plan_fetch(
    watermark: Option<i64>,
    now_ts: i64,
    floor_ts: Option<i64>,
    backfill_days: u64,
    max_hours_per_run: usize,
) -> Vec<i64> {
    let newest_complete = now_ts.div_euclid(3600) * 3600 - 3600;
    if newest_complete <= 0 {
        return Vec::new();
    }
    let default_floor = newest_complete - (backfill_days as i64).saturating_mul(24 * 3600) + 3600;
    let floor = floor_ts.map_or(default_floor, |value| {
        // 下限对齐到整点只能往上取：往下取会抓到下限之前的小时。
        value.div_euclid(3600) * 3600 + i64::from(value.rem_euclid(3600) != 0) * 3600
    });
    let start = watermark.map_or(floor.max(3600), |mark| mark + 3600);
    let mut hours = Vec::new();
    let mut hour_ts = start;
    while hour_ts <= newest_complete && hours.len() < max_hours_per_run {
        hours.push(hour_ts);
        hour_ts += 3600;
    }
    hours
}

/// 往回补历史：从最早的 mark 往更早处走，降序，直到 floor。
///
/// 首测教训：用户把回补天数从 14 改成 30，正向计划只会从水位往前走，
/// 旧历史永远补不进来——"设置没生效"。降序是刻意的：mark 集合始终保持
/// "连续后缀"的形状，中断在哪都能靠"当前最早 mark"重新算出下一步，
/// 不会在中间留洞（日折叠的连续性检查依赖这一点）。
#[must_use]
pub fn plan_backward(
    earliest_mark: Option<i64>,
    now_ts: i64,
    floor_ts: Option<i64>,
    backfill_days: u64,
    max_hours_per_run: usize,
) -> Vec<i64> {
    let Some(earliest) = earliest_mark else {
        // 一小时都没有：冷启动归正向计划管，这里没活。
        return Vec::new();
    };
    let newest_complete = now_ts.div_euclid(3600) * 3600 - 3600;
    let default_floor = newest_complete - (backfill_days as i64).saturating_mul(24 * 3600) + 3600;
    let floor = floor_ts
        .map_or(default_floor, |value| {
            value.div_euclid(3600) * 3600 + i64::from(value.rem_euclid(3600) != 0) * 3600
        })
        .max(3600);
    let mut hours = Vec::new();
    let mut hour_ts = earliest - 3600;
    while hour_ts >= floor && hours.len() < max_hours_per_run {
        hours.push(hour_ts);
        hour_ts -= 3600;
    }
    hours
}

#[cfg(test)]
mod plan_tests {
    use super::*;

    /// 2026-08-31 09:30 UTC 附近的一个"现在"。
    const NOW: i64 = 1_788_168_600;
    const NEWEST_COMPLETE: i64 = 1_788_163_200; // 08:00（09:00 是当前小时，不抓）

    #[test]
    fn cold_start_backfills_the_window_oldest_first() {
        let hours = plan_fetch(None, NOW, None, 14, 10_000);
        assert_eq!(hours.len(), 14 * 24);
        assert_eq!(*hours.last().unwrap(), NEWEST_COMPLETE);
        assert!(hours.windows(2).all(|pair| pair[1] == pair[0] + 3600));
    }

    #[test]
    fn resumes_from_the_watermark() {
        let hours = plan_fetch(Some(NEWEST_COMPLETE - 2 * 3600), NOW, None, 14, 10_000);
        assert_eq!(hours, vec![NEWEST_COMPLETE - 3600, NEWEST_COMPLETE]);
    }

    #[test]
    fn nothing_to_do_when_caught_up() {
        assert!(plan_fetch(Some(NEWEST_COMPLETE), NOW, None, 14, 10_000).is_empty());
    }

    #[test]
    fn season_floor_beats_the_default_window() {
        // 赛季 3 天前开的,配置了 floor 就不用回补满 14 天。
        let floor = NEWEST_COMPLETE - 3 * 24 * 3600;
        let hours = plan_fetch(None, NOW, Some(floor), 14, 10_000);
        assert_eq!(hours.len(), 3 * 24 + 1);
        assert_eq!(hours[0], floor);
    }

    #[test]
    fn misaligned_floor_rounds_up_never_down() {
        let floor = NEWEST_COMPLETE - 2 * 3600 + 1;
        let hours = plan_fetch(None, NOW, Some(floor), 14, 10_000);
        assert_eq!(hours, vec![NEWEST_COMPLETE - 3600, NEWEST_COMPLETE]);
    }

    #[test]
    fn run_cap_is_a_bound_not_a_target() {
        let hours = plan_fetch(None, NOW, None, 14, 48);
        assert_eq!(hours.len(), 48);
        // 下一轮从这轮的"水位"接着走，最终能追平。
        let next = plan_fetch(Some(*hours.last().unwrap()), NOW, None, 14, 48);
        assert_eq!(next[0], hours.last().unwrap() + 3600);
    }

    #[test]
    fn backward_extends_history_when_the_window_grows() {
        // 已有 14 天，回补改成 30：往回走 16 天，降序、紧贴最早 mark。
        let earliest = NEWEST_COMPLETE - 14 * 24 * 3600 + 3600;
        let hours = plan_backward(Some(earliest), NOW, None, 30, 10_000);
        assert_eq!(hours.len(), 16 * 24);
        assert_eq!(hours[0], earliest - 3600);
        assert!(hours.windows(2).all(|pair| pair[1] == pair[0] - 3600));
    }

    #[test]
    fn backward_is_idle_when_history_already_reaches_the_floor() {
        let earliest = NEWEST_COMPLETE - 14 * 24 * 3600 + 3600;
        assert!(plan_backward(Some(earliest), NOW, None, 14, 10_000).is_empty());
        assert!(plan_backward(None, NOW, None, 30, 10_000).is_empty());
    }

    #[test]
    fn backward_respects_the_run_cap() {
        let earliest = NEWEST_COMPLETE - 14 * 24 * 3600 + 3600;
        let hours = plan_backward(Some(earliest), NOW, None, 30, 48);
        assert_eq!(hours.len(), 48);
        // 下一轮从新的"最早 mark"接着走。
        let next = plan_backward(Some(*hours.last().unwrap()), NOW, None, 30, 48);
        assert_eq!(next[0], hours.last().unwrap() - 3600);
    }

    #[test]
    fn empty_hour_grace_window() {
        assert_eq!(
            classify_empty(NOW - 3600, NOW),
            EmptyHourVerdict::RetryLater
        );
        assert_eq!(
            classify_empty(NOW - 4 * 3600, NOW),
            EmptyHourVerdict::ConfirmedEmpty
        );
    }
}
