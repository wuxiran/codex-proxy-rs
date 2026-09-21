//! 账号额度的只读容量估算，不参与金额结算或账号调度。

use chrono::{DateTime, Duration, Utc};

use super::provider_credentials::{AccountUsagePeriod, ProviderQuota, ProviderQuotaWindow};
use super::quota_forecast_sampling::{
    QuotaForecastCurvePoint, QuotaForecastMethod, QuotaForecastSample,
};

const DAY_SECONDS: u64 = 86_400;
const MIN_USED_PERCENT: f64 = 5.0;
const LOW_SAMPLE_PERCENT: f64 = 10.0;

#[derive(Debug, Clone, PartialEq)]
pub struct AccountQuotaForecastReport {
    pub account_id: String,
    pub generated_at: DateTime<Utc>,
    pub forecasts: [AccountQuotaForecast; 2],
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountQuotaForecast {
    pub period: AccountUsagePeriod,
    pub target_seconds: u64,
    pub extrapolated: bool,
    pub source: Option<QuotaForecastSource>,
    pub unavailable_reason: Option<&'static str>,
    pub low_sample: bool,
    pub incomplete_cost: bool,
    pub incomplete_tokens: bool,
    pub estimated_tokens: Option<u64>,
    pub estimated_usd: Option<f64>,
    /// 剩余估算始终属于源窗口，不随目标周期折算。
    pub remaining_tokens: Option<u64>,
    pub remaining_usd: Option<f64>,
    /// 源窗口起点，供消耗曲线确定横轴；窗口边界无效时为空。
    pub window_start_at: Option<DateTime<Utc>>,
    /// 源窗口内已观测的已用比例；与能否预测无关，样本不足时仍可展示。
    pub curve: Vec<QuotaForecastCurvePoint>,
    pub burn_percent_per_hour: Option<f64>,
    pub exhaustion: Option<QuotaExhaustion>,
}

/// 按近期消耗速率外推的耗尽结论，始终属于源窗口。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum QuotaExhaustion {
    /// 上游已报告额度用尽，是事实而非估算。
    Reached,
    At(DateTime<Utc>),
    /// 按当前速率，窗口重置前不会用尽。
    AfterReset,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuotaForecastSource {
    pub label: String,
    pub used_percent: Option<f64>,
    pub observed_at: Option<DateTime<Utc>>,
    pub reset_at: DateTime<Utc>,
    pub tokens: Option<u64>,
    pub usd: Option<f64>,
}

/// 优先预测真实的对应窗口；缺少对应周期时只给出明确标识的 7/30 天容量折算。
/// 本地日志不能证明站外消耗或完整留存，因此即使样本充足也不声称官方额度。
#[must_use]
pub fn account_quota_forecasts(
    quota: &ProviderQuota,
    account_added_at: DateTime<Utc>,
    now: DateTime<Utc>,
    samples: &[QuotaForecastSample],
) -> [AccountQuotaForecast; 2] {
    [AccountUsagePeriod::Weekly, AccountUsagePeriod::Monthly].map(|period| {
        let selected = quota
            .usage_windows()
            .find(|(_, source_period)| *source_period == period)
            .or_else(|| quota.usage_windows().min_by_key(|(_, period)| *period));
        let mut forecast = AccountQuotaForecast {
            period,
            target_seconds: match period {
                AccountUsagePeriod::Weekly => 7 * DAY_SECONDS,
                AccountUsagePeriod::Monthly => 30 * DAY_SECONDS,
            },
            extrapolated: false,
            source: None,
            unavailable_reason: Some("没有可统计的周/月额度窗口，请先刷新账号额度。"),
            low_sample: false,
            incomplete_cost: false,
            incomplete_tokens: false,
            estimated_tokens: None,
            estimated_usd: None,
            remaining_tokens: None,
            remaining_usd: None,
            window_start_at: None,
            curve: Vec::new(),
            burn_percent_per_hour: None,
            exhaustion: None,
        };
        if let Some((window, source_period)) = selected {
            forecast.project(
                window,
                source_period,
                quota.observed_at,
                account_added_at,
                now,
                samples.iter().find(|sample| sample.key == window.key),
            );
        }
        forecast
    })
}

impl AccountQuotaForecast {
    fn project(
        &mut self,
        window: &ProviderQuotaWindow,
        source_period: AccountUsagePeriod,
        observed_at: Option<DateTime<Utc>>,
        account_added_at: DateTime<Utc>,
        now: DateTime<Utc>,
        sample: Option<&QuotaForecastSample>,
    ) {
        let (Some(seconds), Some(reset_at)) = (window.window_seconds, window.reset_at) else {
            return;
        };
        self.extrapolated = source_period != self.period;
        if !self.extrapolated {
            self.target_seconds = seconds;
        }
        let percent = window
            .used_percent
            .filter(|p| p.is_finite() && (0.0..=100.0).contains(p));
        let usage = sample.map(|sample| &sample.usage);
        let usd = usage
            .filter(|usage| usage.known_cost_count > 0)
            .map(|usage| usage.usd)
            .filter(|value| value.is_finite() && *value >= 0.0);
        let start = i64::try_from(seconds)
            .ok()
            .and_then(Duration::try_seconds)
            .and_then(|duration| reset_at.checked_sub_signed(duration));
        self.source = Some(QuotaForecastSource {
            label: window.label.clone(),
            used_percent: percent,
            observed_at,
            reset_at,
            tokens: usage.map(|usage| usage.tokens),
            usd,
        });
        self.incomplete_cost = usage.is_none_or(|usage| {
            usage.unavailable_cost_count > 0
                || usage.known_cost_count == 0
                || usage.known_cost_count != usage.request_count
        });
        self.incomplete_tokens = usage.is_some_and(|usage| usage.missing_token_count > 0);
        let method = sample.map_or(QuotaForecastMethod::Cumulative, |sample| sample.method);
        let Some(start) = start.filter(|start| *start <= now && now < reset_at) else {
            self.unavailable_reason = Some("额度窗口已过期或边界无效，请刷新账号额度后重试。");
            return;
        };
        self.window_start_at = Some(start);
        self.curve = sample
            .map(|sample| {
                let mut curve = sample.curve.clone();
                curve.retain(|point| start <= point.observed_at && point.observed_at <= now);
                curve
            })
            .unwrap_or_default();
        if !observed_at.is_some_and(|observed| start <= observed && observed <= now) {
            self.unavailable_reason = Some("缺少本周期的额度快照，请先刷新账号额度。");
            return;
        }
        if sample.is_some_and(|sample| sample.discontinuous) {
            self.unavailable_reason =
                Some("额度观测出现回落或累计记录不连续，正在重新积累配对样本。");
            return;
        }
        if account_added_at > start && method == QuotaForecastMethod::Cumulative {
            self.unavailable_reason = Some(
                "本周期开始时的记录不完整，正在积累至少 5 个百分点的配对观测，无需等待下次重置。",
            );
            return;
        }
        let Some(percent) = percent else {
            self.unavailable_reason = Some("已用比例未知，请刷新额度后查看预测。");
            return;
        };
        if window.limit_reached || percent >= 100.0 {
            self.exhaustion = Some(QuotaExhaustion::Reached);
        }
        let Some(usage) = usage.filter(|usage| usage.request_count > 0) else {
            self.unavailable_reason = Some("本周期没有网关用量记录，暂时无法预测额度。");
            return;
        };
        let Some(sample) = sample.filter(|sample| {
            sample.end_at == observed_at.unwrap_or(now)
                && start <= sample.start_at
                && sample.start_at <= sample.end_at
                && sample.sampled_percent.is_finite()
                && sample.sampled_percent >= MIN_USED_PERCENT
                && sample.sampled_percent <= percent
        }) else {
            self.unavailable_reason =
                Some("有效额度进度不足 5 个百分点或采样边界无效，请继续积累用量。");
            return;
        };
        self.low_sample = sample.sampled_percent < LOW_SAMPLE_PERCENT
            || (method == QuotaForecastMethod::Incremental && sample.block_count < 2);
        self.project_exhaustion(sample, percent, reset_at);
        // 漏记和个别缺失只影响精度，仍按已记录数值估算，不按请求数补齐未知消耗。
        // 预测是近似展示值；不复用为账单金额，也不把月折算当成自然月或额外余额。
        let capacity_factor = 100.0 / sample.sampled_percent;
        let factor = capacity_factor * self.target_seconds as f64 / seconds as f64;
        let tokens = Some(usage.tokens).filter(|tokens| *tokens > 0);
        self.estimated_tokens = tokens.and_then(|value| estimate_tokens(value, factor));
        self.estimated_usd = usd.and_then(|value| estimate(value, factor));
        let remaining_factor = (100.0 - percent) / sample.sampled_percent;
        self.remaining_tokens = tokens.and_then(|value| estimate_tokens(value, remaining_factor));
        self.remaining_usd = usd.and_then(|value| estimate(value, remaining_factor));
        self.unavailable_reason = if self.estimated_tokens.is_none() && self.estimated_usd.is_none()
        {
            Some("本周期暂无可用于估算的 Token 或费用数据，请积累用量后重试。")
        } else {
            None
        };
    }
}

impl AccountQuotaForecast {
    /// 复用已通过 5 个百分点门槛的同一配对样本做线性外推；不另设更宽松的速率口径。
    fn project_exhaustion(
        &mut self,
        sample: &QuotaForecastSample,
        percent: f64,
        reset_at: DateTime<Utc>,
    ) {
        let seconds = (sample.end_at - sample.start_at).num_milliseconds() as f64 / 1000.0;
        let rate = sample.sampled_percent / seconds;
        if seconds <= 0.0 || !rate.is_finite() || rate <= 0.0 {
            return;
        }
        self.burn_percent_per_hour = Some(rate * 3600.0);
        if self.exhaustion.is_some() {
            return;
        }
        let remaining_ms = (100.0 - percent) / rate * 1000.0;
        // 超出可表示范围的外推必然晚于重置，不需要精确时刻。
        let at = Some(remaining_ms)
            .filter(|value| value.is_finite() && *value < i64::MAX as f64)
            .and_then(|value| Duration::try_milliseconds(value.round() as i64))
            .and_then(|duration| sample.end_at.checked_add_signed(duration));
        self.exhaustion = Some(match at {
            Some(at) if at < reset_at => QuotaExhaustion::At(at),
            _ => QuotaExhaustion::AfterReset,
        });
    }
}

fn estimate(value: f64, factor: f64) -> Option<f64> {
    let estimate = value * factor;
    (estimate.is_finite() && estimate >= 0.0).then_some(estimate)
}

fn estimate_tokens(value: u64, factor: f64) -> Option<u64> {
    estimate(value as f64, factor)
        .filter(|value| value.round() < u64::MAX as f64)
        .map(|value| value.round() as u64)
}
