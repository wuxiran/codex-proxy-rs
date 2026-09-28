//! fork：额度预测的消耗速率、耗尽结论与曲线（fork 增补字段）。

use chrono::Duration;
use gateway_admin::model::{
    quota_forecast::{QuotaExhaustion, account_quota_forecasts},
    quota_forecast_sampling::QuotaForecastCurvePoint,
};

use super::quota_forecast::{forecast, now, quota, samples, window};

#[test]
fn exhaustion_extrapolates_the_sampled_burn_rate_within_the_source_window() {
    // 6 天用掉 90%：0.625%/h，剩余 10% 需 16 小时，早于 1 天后的重置。
    let mut fast = window("week", 7);
    fast.used_percent = Some(90.0);
    let [week, month] = forecast(&quota(vec![fast]));
    assert!((week.burn_percent_per_hour.unwrap() - 0.625).abs() < 1e-9);
    assert_eq!(
        week.exhaustion,
        Some(QuotaExhaustion::At(now() + Duration::hours(16)))
    );
    assert_eq!(month.exhaustion, week.exhaustion);
    assert_eq!(week.window_start_at, Some(now() - Duration::days(6)));

    // 6 天只用 20%：按同一速率到重置也用不完。
    let [week, _] = forecast(&quota(vec![window("week", 7)]));
    assert_eq!(week.exhaustion, Some(QuotaExhaustion::AfterReset));
}

#[test]
fn reached_quota_is_a_fact_and_needs_no_rate() {
    let mut full = window("week", 7);
    full.used_percent = Some(100.0);
    let [week, _] = forecast(&quota(vec![full]));
    assert_eq!(week.exhaustion, Some(QuotaExhaustion::Reached));
    let mut limited = window("week", 7);
    limited.limit_reached = true;
    limited.local_usage = None;
    let [week, _] = forecast(&quota(vec![limited]));
    assert_eq!(week.exhaustion, Some(QuotaExhaustion::Reached));
    assert!(week.burn_percent_per_hour.is_none());
}

#[test]
fn curve_is_shown_without_an_estimate_but_never_outside_the_window() {
    let mut tiny = window("week", 7);
    tiny.used_percent = Some(3.0);
    let source = quota(vec![tiny]);
    let mut sample = samples(&source).remove(0);
    let point = |observed_at, used_percent| QuotaForecastCurvePoint {
        observed_at,
        used_percent,
    };
    sample.curve = vec![
        point(now() - Duration::days(7), 0.0),
        point(now() - Duration::days(2), 1.0),
        point(now(), 3.0),
        point(now() + Duration::hours(1), 4.0),
    ];
    let [week, _] = account_quota_forecasts(&source, now() - Duration::days(60), now(), &[sample]);
    assert!(week.unavailable_reason.is_some());
    assert!(week.exhaustion.is_none());
    assert!(week.burn_percent_per_hour.is_none());
    assert_eq!(
        week.curve,
        vec![point(now() - Duration::days(2), 1.0), point(now(), 3.0)]
    );

    let mut expired = window("week", 7);
    expired.reset_at = Some(now());
    let [week, _] = forecast(&quota(vec![expired]));
    assert!(week.curve.is_empty());
    assert!(week.window_start_at.is_none());
}
