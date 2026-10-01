//! fork：额度预测曲线 / 消耗速率 / 耗尽结论，以及账号费用来源（模型计价与真实上游费用分列）的展示投影。

use gateway_admin::model::{
    provider_credentials::AccountUsagePeriod,
    quota_forecast::{
        AccountQuotaForecast, AccountQuotaForecastReport, QuotaExhaustion, QuotaForecastSource,
    },
    quota_forecast_sampling::QuotaForecastCurvePoint,
};
use gateway_api::admin::accounts::AccountQuotaForecastData;

#[test]
fn fork_quota_forecast_projection_exposes_curve_burn_and_exhaustion() {
    let now = "2026-09-12T00:00:00Z".parse().unwrap();
    let forecast = AccountQuotaForecast {
        period: AccountUsagePeriod::Weekly,
        target_seconds: 7 * 86_400,
        extrapolated: false,
        source: Some(QuotaForecastSource {
            label: "周额度".to_owned(),
            used_percent: Some(100.0),
            observed_at: Some(now),
            reset_at: now,
            tokens: Some(1_000_000),
            usd: Some(0.1234),
        }),
        unavailable_reason: None,
        low_sample: false,
        incomplete_cost: true,
        incomplete_tokens: false,
        estimated_tokens: Some(1_000_000),
        estimated_usd: None,
        window_start_at: Some(now),
        curve: vec![QuotaForecastCurvePoint {
            observed_at: now,
            used_percent: 42.5,
        }],
        burn_percent_per_hour: Some(0.75),
        exhaustion: Some(QuotaExhaustion::At(now)),
    };
    let mut monthly = forecast.clone();
    monthly.exhaustion = Some(QuotaExhaustion::AfterReset);
    monthly.burn_percent_per_hour = None;
    monthly.period = AccountUsagePeriod::Monthly;
    monthly.extrapolated = true;
    monthly.target_seconds = 30 * 86_400;
    let view = AccountQuotaForecastData::from(AccountQuotaForecastReport {
        account_id: "acct_forecast".to_owned(),
        generated_at: now,
        forecasts: [forecast, monthly],
    });
    let value = serde_json::to_value(view).unwrap();
    let week = &value["forecasts"][0];
    assert_eq!(week["windowStartAt"], "2026-09-12T08:00:00+08:00");
    assert_eq!(
        week["curve"],
        serde_json::json!([{ "observedAt": "2026-09-12T08:00:00+08:00", "usedPercent": 42.5 }])
    );
    assert_eq!(week["burnPercentPerHourDisplay"], "0.75%/h");
    assert_eq!(
        week["exhaustion"],
        serde_json::json!({
            "kind": "at",
            "at": "2026-09-12T08:00:00+08:00",
            "atDisplay": "2026-09-12 08:00:00"
        })
    );
    let month = &value["forecasts"][1];
    assert_eq!(month["exhaustion"]["kind"], "afterReset");
    assert!(month["exhaustion"]["at"].is_null());
    assert_eq!(month["burnPercentPerHourDisplay"], "—");
}

#[test]
fn account_billing_keeps_missing_zero_partial_and_signed_difference_distinct() {
    use gateway_admin::model::accounts::AccountBillingAmounts;
    use gateway_api::admin::accounts::AccountBillingView;
    for (price, upstream, pc, uc, total, price_display, upstream_display, difference) in [
        (Some("2"), None, 1, 0, 1, "$2.00", "未提供", None),
        (Some("2"), Some("0"), 1, 1, 1, "$2.00", "$0.00", Some("2")),
        (Some("2"), Some("3"), 1, 1, 1, "$2.00", "$3.00", Some("-1")),
        (
            Some("2"),
            Some("1"),
            2,
            1,
            2,
            "$2.00",
            "$1.00（部分 1/2）",
            None,
        ),
        (None, None, 0, 0, 0, "未提供", "未提供", None),
    ] {
        let amounts = AccountBillingAmounts {
            model_price_usd: price.map(|s| s.parse().unwrap()),
            upstream_cost_usd: upstream.map(|s| s.parse().unwrap()),
            model_price_count: pc,
            upstream_cost_count: uc,
        };
        let value = serde_json::to_value(AccountBillingView::from((&amounts, total))).unwrap();
        assert_eq!(value["modelPriceAmountUsdDisplay"], price_display);
        assert_eq!(value["upstreamCostAmountUsdDisplay"], upstream_display);
        assert_eq!(value["differenceAmountUsd"].as_str(), difference);
        if difference == Some("-1") {
            assert_eq!(value["differenceAmountUsdDisplay"], "-$1.00");
        }
        if difference.is_none() {
            assert_eq!(value["differenceAmountUsdDisplay"], "不可计算");
        }
    }
}

#[test]
fn account_model_view_exposes_full_identity_and_mismatch_without_mixing_cost_sources() {
    use gateway_admin::model::accounts::{
        AccountBillingAmounts, AccountCost, AccountModelIdentity, AccountModelUsage,
    };
    use gateway_api::admin::accounts::ModelUsageView;
    let now = "2026-09-19T00:00:00Z".parse().unwrap();
    for (response, billing, route, mismatch) in [
        (
            Some("gpt-5.6-luna"),
            Some("gpt-5.6-luna"),
            "gpt-6-astra",
            true,
        ),
        (
            Some("gpt-6-astra"),
            Some("gpt-6-astra"),
            "gpt-6-astra",
            false,
        ),
        (None, None, "gpt-6-astra", false),
        (None, Some("gpt-5.6-luna"), "gpt-6-astra", true),
        (None, None, "gpt-5.6-luna", true),
    ] {
        let usage = AccountModelUsage {
            identity: AccountModelIdentity {
                key: "identity-fixture".to_owned(),
                requested_model_id: Some("gpt-6-astra".to_owned()),
                upstream_model_id: Some(route.to_owned()),
                response_model: response.map(str::to_owned),
                billing_model: billing.map(str::to_owned),
            },
            billing: AccountBillingAmounts {
                model_price_usd: Some("0.2".parse().unwrap()),
                upstream_cost_usd: None,
                model_price_count: 1,
                upstream_cost_count: 0,
            },
            model: "gpt-6-astra".to_owned(),
            request_count: 1,
            success_count: 1,
            input_tokens: Some(100),
            output_tokens: Some(10),
            cached_tokens: Some(0),
            cache_write_tokens: None,
            reasoning_tokens: None,
            image_input_tokens: None,
            image_output_tokens: None,
            image_request_count: 0,
            image_request_failed_count: 0,
            total_tokens: Some(110),
            cost_coverage: Default::default(),
            costs: vec![AccountCost {
                currency: "USD".to_owned(),
                amount: "99".parse().unwrap(),
            }],
            last_used_at: now,
        };
        let view = serde_json::to_value(ModelUsageView::from((usage, now))).unwrap();
        assert_eq!(view["mismatch"], mismatch);
        assert_eq!(view["requestedModelId"], "gpt-6-astra");
        assert_eq!(view["upstreamModelId"], route);
        assert_eq!(view["responseModel"].as_str(), response);
        assert_eq!(view["billingModel"].as_str(), billing);
        assert_eq!(view["billingAmountUsd"], "0.2");
        assert_eq!(view["billing"]["modelPriceAmountUsd"], "0.2");
        assert_eq!(view["billing"]["upstreamCostAmountUsdDisplay"], "未提供");
        assert!(view["billing"]["differenceAmountUsd"].is_null());
    }
}
