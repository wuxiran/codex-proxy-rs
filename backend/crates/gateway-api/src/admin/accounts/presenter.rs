//! Admin 领域结果到安全 HTTP wire 的展示投影

use super::*;
use gateway_admin::model::quota_forecast::{
    AccountQuotaForecast, AccountQuotaForecastReport, QuotaExhaustion,
};

pub(super) fn account_page_data(
    result: AccountDirectoryPage,
    page: u32,
    page_size: u16,
    now: DateTime<Utc>,
    time: crate::time::TimePresenter,
) -> AccountPageData {
    let total_pages = if result.total == 0 {
        0
    } else {
        u32::try_from(result.total.div_ceil(u64::from(page_size))).unwrap_or(u32::MAX)
    };
    AccountPageData {
        items: result
            .items
            .into_iter()
            .map(|item| account_view(item, now, time))
            .collect(),
        page: PageMeta::new(page, u32::from(page_size), result.total, total_pages),
        summary: AccountSummaryView {
            total: result.summary.total,
            normal: result.summary.normal,
            quota_exhausted: result.summary.quota_exhausted,
            rate_limited: result.summary.rate_limited,
            disabled: result.summary.disabled,
            error: result.summary.error,
            expired: result.summary.expired,
        },
    }
}

pub(super) fn account_refresh_data(
    result: AccountRefreshResult,
    now: DateTime<Utc>,
    time: crate::time::TimePresenter,
) -> AccountRefreshData {
    AccountRefreshData {
        account: account_view(result.account, now, time),
    }
}

pub(super) fn account_models_data(result: ProviderModels) -> AccountModelsData {
    AccountModelsData {
        models: result
            .models
            .into_iter()
            .map(|model| {
                let id = model.id.to_string();
                AccountModelView {
                    label: id.clone(),
                    id,
                }
            })
            .collect(),
    }
}

pub(super) fn account_view(
    item: AccountDirectoryItem,
    now: DateTime<Utc>,
    time: crate::time::TimePresenter,
) -> AccountView {
    let AccountDirectoryItem {
        account,
        concurrency,
        recent_errors,
        ticket,
        capacity,
        capabilities,
        plan_type_display,
        projection,
        usage,
        quota,
    } = item;
    // fork：票据到点且已不能调度（不是正常/限流）的账号在目录里显示为「已过期」，
    // 与 store 的筛选口径一致；调度状态不变。
    let ticket_expired = ticket
        .expires_at
        .is_some_and(|expires_at| expires_at <= now)
        && !matches!(
            projection.status,
            DomainAccountStatus::Normal | DomainAccountStatus::RateLimited
        );
    let status = if ticket_expired {
        AccountListStatus::TicketExpired.as_str()
    } else {
        projection.status.as_str()
    }
    .to_owned();
    let cooldown = projection.cooldown;
    let expires_at = account
        .access_token_expires_at
        .as_ref()
        .map(|value| time.rfc3339(value));
    let added_at = time.rfc3339(&account.created_at);
    let updated_at = time.rfc3339(&account.updated_at);
    let usage_period = quota.usage_window().map(|(_, period)| period);
    let mut usage = account_usage_view(usage, usage_period, now, time);
    usage.estimated_quota_usd_display = estimated_quota_usd_display(&quota); // fork: billing
    if account.authentication_kind == "api_key" {
        usage.window_label_display = "本地累计".to_owned();
    }
    let (quota, refresh_token_expires_at) = account_quota_view(quota, cooldown, now, time);
    AccountView {
        capabilities: capabilities.into(),
        id: account.id.clone(),
        name: account.name,
        notes: account.notes,
        provider: account.provider_kind.to_string(),
        groups: account
            .groups
            .into_iter()
            .map(AccountGroupRefView::from)
            .collect(),
        resource_ref: account.id,
        email: account.email,
        account_id: account.upstream_account_id,
        user_id: account.upstream_user_id,
        label: None,
        plan_type: account.plan_type,
        plan_type_display: plan_type_display.unwrap_or_else(|| "未知套餐".to_owned()),
        authentication_kind: account.authentication_kind,
        has_refresh_token: account.has_refresh_token,
        status,
        error_reason: projection
            .error_reason
            .filter(|_| !ticket_expired)
            .map(|reason| reason.as_str().to_owned()),
        error_message: projection.error_message.filter(|_| !ticket_expired),
        enabled: account.enabled,
        concurrency_limit: account.concurrency_limit.map(|limit| limit.get()),
        concurrency: AccountConcurrencyView {
            in_flight: concurrency.in_flight,
            limit: concurrency.limit,
        },
        ticket: account_ticket_view(ticket, time),
        recent_errors: AccountRecentErrorsView {
            request_count: recent_errors.request_count,
            error_count: recent_errors.error_count,
        },
        capacity: AccountCapacityView {
            used_slots: capacity.used_slots,
            total_slots: capacity.total_slots,
        },
        weight: account.weight.get(),
        model_access: account.model_access,
        outbound_proxy_endpoint: account
            .outbound_proxy
            .as_ref()
            .map(|proxy| proxy.endpoint()),
        access_token_expires_at: expires_at,
        access_token_expires_at_display: account
            .access_token_expires_at
            .as_ref()
            .map(|value| time.datetime(value)),
        refresh_token_expires_at,
        next_refresh_at: account.next_refresh_at.map(|value| time.rfc3339(&value)),
        next_refresh_at_display: account.next_refresh_at.map(|value| time.datetime(&value)),
        added_at,
        added_at_display: time.datetime(&account.created_at),
        updated_at,
        updated_at_display: time.datetime(&account.updated_at),
        quota,
        usage,
    }
}

impl From<(AccountQuotaForecastReport, crate::time::TimePresenter)> for AccountQuotaForecastData {
    fn from((report, time): (AccountQuotaForecastReport, crate::time::TimePresenter)) -> Self {
        Self {
            account_id: report.account_id,
            generated_at: time.rfc3339(&report.generated_at),
            forecasts: report
                .forecasts
                .into_iter()
                .map(|value| quota_forecast_view(value, time))
                .collect(),
        }
    }
}

fn quota_forecast_view(
    forecast: AccountQuotaForecast,
    time: crate::time::TimePresenter,
) -> AccountQuotaForecastView {
    AccountQuotaForecastView {
        period: match forecast.period {
            AccountUsagePeriod::Weekly => "weekly",
            AccountUsagePeriod::Monthly => "monthly",
        },
        target_days: forecast.target_seconds as f64 / 86_400.0,
        extrapolated: forecast.extrapolated,
        source: forecast.source.map(|source| QuotaForecastSourceView {
            label: source.label,
            used_percent: source.used_percent,
            used_percent_display: source
                .used_percent
                .map_or_else(|| "—".to_owned(), |value| format!("{value:.1}%")),
            observed_at: source.observed_at.map(|value| time.rfc3339(&value)),
            observed_at_display: source
                .observed_at
                .map_or_else(|| "—".to_owned(), |value| time.datetime(&value)),
            reset_at: time.rfc3339(&source.reset_at),
            tokens_display: display_optional_tokens(source.tokens),
            usd_display: forecast_usd_display(source.usd),
        }),
        unavailable_reason: forecast.unavailable_reason,
        low_sample: forecast.low_sample,
        incomplete_cost: forecast.incomplete_cost,
        incomplete_tokens: forecast.incomplete_tokens,
        estimated_tokens: forecast.estimated_tokens,
        estimated_tokens_display: display_optional_tokens(forecast.estimated_tokens),
        estimated_usd: forecast.estimated_usd,
        estimated_usd_display: forecast_usd_display(forecast.estimated_usd),
        window_start_at: forecast.window_start_at.map(|value| time.rfc3339(&value)),
        curve: forecast
            .curve
            .iter()
            .map(|point| QuotaForecastCurvePointView {
                observed_at: time.rfc3339(&point.observed_at),
                used_percent: point.used_percent,
            })
            .collect(),
        burn_percent_per_hour: forecast.burn_percent_per_hour,
        burn_percent_per_hour_display: forecast
            .burn_percent_per_hour
            .map_or_else(|| "—".to_owned(), |value| format!("{value:.2}%/h")),
        exhaustion: forecast.exhaustion.map(|exhaustion| match exhaustion {
            QuotaExhaustion::Reached => QuotaExhaustionView {
                kind: "reached",
                at: None,
                at_display: None,
            },
            QuotaExhaustion::At(at) => QuotaExhaustionView {
                kind: "at",
                at: Some(time.rfc3339(&at)),
                at_display: Some(time.datetime(&at)),
            },
            QuotaExhaustion::AfterReset => QuotaExhaustionView {
                kind: "afterReset",
                at: None,
                at_display: None,
            },
        }),
    }
}

fn forecast_usd_display(value: Option<f64>) -> String {
    value.map_or_else(
        || "—".to_owned(),
        |value| format_decimal_currency(&format!("{value:.10}"), "USD"),
    )
}

pub(super) fn account_quota_view(
    mut quota: ProviderQuota,
    cooldown: Option<gateway_core::account::AccountCooldown>,
    now: DateTime<Utc>,
    time: crate::time::TimePresenter,
) -> (AccountQuotaView, Option<String>) {
    quota.apply_limit_reached_display();
    let refresh_token_expires_at = quota
        .refresh_token_expires_at
        .map(|value| time.rfc3339(&value));
    let refreshed_at_display = quota
        .observed_at
        .map_or_else(|| "—".to_owned(), |value| time.relative(value, now));
    let windows = quota
        .windows
        .into_iter()
        .map(|value| quota_window_view(value, time))
        .collect();
    let rate_limited_until = cooldown.map(|value| time.rfc3339(&value.until.into()));
    let rate_limit_recovery_display = cooldown.map(|value| {
        let until = DateTime::<Utc>::from(value.until);
        if value.kind.requires_probe() && until <= now {
            return "等待探测成功".to_owned();
        }
        let minutes = (until - now).num_seconds().saturating_add(30) / 60;
        match minutes {
            ..1 => time.datetime(&until),
            1..60 => format!("剩余 {minutes} 分钟"),
            _ if minutes % 60 == 0 => format!("剩余 {} 小时", minutes / 60),
            _ => format!("剩余 {} 小时 {} 分", minutes / 60, minutes % 60),
        }
    });
    let rate_limit_reason = cooldown.map(|value| {
        if value.kind.is_capacity_freeze() {
            "capacity_freeze".to_owned()
        } else {
            "upstream_rate_limit".to_owned()
        }
    });
    let recovery_probe_required = cooldown.is_some_and(|value| value.kind.requires_probe());
    (
        AccountQuotaView {
            refreshed_at_display,
            limit_reached: quota.limit_reached,
            rate_limited_until,
            rate_limit_recovery_display,
            rate_limit_reason,
            recovery_probe_required,
            windows,
            credits: quota.credits.map(|credits| AccountQuotaCreditsView {
                has_credits: credits.has_credits,
                unlimited: credits.unlimited,
                balance: credits.balance,
            }),
        },
        refresh_token_expires_at,
    )
}

pub(crate) fn quota_window_view(
    window: ProviderQuotaWindow,
    time: crate::time::TimePresenter,
) -> AccountQuotaWindowView {
    let ProviderQuotaWindow {
        key,
        group,
        label,
        limit_id,
        limit_name,
        role,
        local_usage_attribution: _,
        window_seconds,
        used_percent,
        reset_at,
        limit_reached,
        local_usage,
        provider_data: _,
    } = window;
    let label_display = limit_name
        .as_ref()
        .map_or_else(|| label.clone(), |name| format!("{name} · {label}"));
    AccountQuotaWindowView {
        label_display,
        window_label_display: label,
        key,
        group,
        limit_id,
        limit_name,
        role: role.map(|value| value.as_str().to_owned()),
        window_seconds,
        used_percent,
        used_percent_display: used_percent
            .map_or_else(|| "—".to_owned(), |value| format!("{value:.1}%")),
        limit_reached,
        local_usage: local_usage
            .as_ref()
            .map(|value| quota_local_usage(value, time)),
        reset_at_display: reset_at.map_or_else(|| "—".to_owned(), |value| time.datetime(&value)),
    }
}

pub(super) fn quota_local_usage(usage: &AccountUsage, time: crate::time::TimePresenter) -> Value {
    let total_tokens = usage.total_tokens.unwrap_or_default();
    serde_json::json!({
        "billing": AccountBillingView::from((&usage.billing, usage.request_count)),
        "requestCount": usage.request_count,
        "requestCountDisplay": format_number(usage.request_count),
        "inputTokens": usage.input_tokens.unwrap_or_default(),
        "inputTokensDisplay": display_optional_tokens(usage.input_tokens),
        "outputTokens": usage.output_tokens.unwrap_or_default(),
        "outputTokensDisplay": display_optional_tokens(usage.output_tokens),
        "cachedInputTokens": usage.cached_input_tokens.unwrap_or_default(),
        "cachedInputTokensDisplay": display_optional_tokens(usage.cached_input_tokens),
        "imageInputTokens": usage.image_input_tokens.unwrap_or_default(),
        "imageInputTokensDisplay": display_optional_tokens(usage.image_input_tokens),
        "imageOutputTokens": usage.image_output_tokens.unwrap_or_default(),
        "imageOutputTokensDisplay": display_optional_tokens(usage.image_output_tokens),
        "imageRequestCount": usage.image_request_count,
        "imageRequestFailedCount": usage.image_request_failed_count,
        "totalTokens": total_tokens,
        "totalTokensDisplay": format_compact_number(total_tokens),
        "requestBuckets": time.request_buckets(usage.request_buckets.iter().map(|bucket| (bucket.bucket_start, bucket.request_count))),
    })
}

pub(super) fn account_usage_view(
    usage: Option<AccountUsage>,
    period: Option<AccountUsagePeriod>,
    now: DateTime<Utc>,
    time: crate::time::TimePresenter,
) -> AccountUsageView {
    let Some(usage) = usage else {
        return empty_account_usage();
    };
    let known_count = usage
        .cost_coverage
        .provider_reported_count
        .saturating_add(usage.cost_coverage.calculated_count);
    let cost_estimate_status = if known_count == 0 {
        "unknown"
    } else if usage.cost_coverage.unavailable_count > 0 {
        "partial"
    } else {
        "known"
    };
    AccountUsageView {
        billing: AccountBillingView::from((&usage.billing, usage.request_count)),
        estimated_quota_usd_display: None,
        window_label_display: match period {
            Some(AccountUsagePeriod::Weekly) => "周额度窗口",
            Some(AccountUsagePeriod::Monthly) => "月额度窗口",
            None => "周/月额度窗口",
        }
        .to_owned(),
        request_count: Some(usage.request_count),
        request_count_display: format_number(usage.request_count),
        input_tokens: usage.input_tokens,
        input_tokens_display: display_optional_tokens(usage.input_tokens),
        output_tokens: usage.output_tokens,
        output_tokens_display: display_optional_tokens(usage.output_tokens),
        cached_input_tokens: usage.cached_input_tokens,
        cached_input_tokens_display: display_optional_tokens(usage.cached_input_tokens),
        reasoning_output_tokens: usage.reasoning_output_tokens,
        reasoning_output_tokens_display: display_optional_tokens(usage.reasoning_output_tokens),
        image_input_tokens: usage.image_input_tokens,
        image_input_tokens_display: display_optional_tokens(usage.image_input_tokens),
        image_output_tokens: usage.image_output_tokens,
        image_output_tokens_display: display_optional_tokens(usage.image_output_tokens),
        image_request_count: Some(usage.image_request_count),
        image_request_count_display: format_number(usage.image_request_count),
        image_request_failed_count: Some(usage.image_request_failed_count),
        image_request_failed_count_display: format_number(usage.image_request_failed_count),
        total_tokens: usage.total_tokens,
        total_tokens_display: display_optional_tokens(usage.total_tokens),
        cache_write_input_tokens: usage.cache_write_input_tokens,
        cache_write_input_tokens_display: display_optional_tokens(usage.cache_write_input_tokens),
        last_used_at: usage.last_used_at.map(|value| time.rfc3339(&value)),
        last_used_at_full_display: usage.last_used_at.map(|value| time.datetime(&value)),
        last_used_at_display: usage
            .last_used_at
            .map_or_else(|| "—".to_owned(), |value| time.relative(value, now)),
        cost_estimate_status: cost_estimate_status.to_owned(),
        known_cost_count: Some(known_count),
        partial_cost_count: Some(u64::from(cost_estimate_status == "partial")),
        unknown_cost_count: Some(usage.cost_coverage.unavailable_count),
        costs: usage.costs.iter().map(account_currency_cost_view).collect(),
        models: usage
            .models
            .into_iter()
            .map(|model| account_model_usage_view(model, now, time))
            .collect(),
    }
}

pub(super) fn account_model_usage_view(
    usage: AccountModelUsage,
    now: DateTime<Utc>,
    time: crate::time::TimePresenter,
) -> ModelUsageView {
    let known_count = usage
        .cost_coverage
        .provider_reported_count
        .saturating_add(usage.cost_coverage.calculated_count);
    let cost_estimate_status = if known_count == 0 {
        "unknown"
    } else if usage.cost_coverage.unavailable_count > 0 {
        "partial"
    } else {
        "known"
    };
    // fork: billing —— 账号金额按模型计价口径展示，不再从 costs 里找 USD
    let usd = usage.billing.model_price_usd.as_ref();
    // fork: served-mismatch —— 请求模型与上游 / 响应 / 计费模型任一不一致即标记
    let identity = usage.identity;
    let mismatch = identity
        .requested_model_id
        .as_ref()
        .is_some_and(|requested| {
            [
                &identity.upstream_model_id,
                &identity.response_model,
                &identity.billing_model,
            ]
            .into_iter()
            .flatten()
            .any(|model| model != requested)
        });
    ModelUsageView {
        key: identity.key,
        requested_model_id: identity.requested_model_id,
        upstream_model_id: identity.upstream_model_id,
        response_model: identity.response_model,
        billing_model: identity.billing_model,
        mismatch,
        billing: AccountBillingView::from((&usage.billing, usage.request_count)),
        model: usage.model,
        request_count: usage.request_count,
        request_count_display: format_number(usage.request_count),
        success_rate: (usage.request_count > 0)
            .then(|| usage.success_count as f64 * 100.0 / usage.request_count as f64),
        success_rate_display: if usage.request_count == 0 {
            "—".to_owned()
        } else {
            format!(
                "{:.1}%",
                usage.success_count as f64 * 100.0 / usage.request_count as f64
            )
        },
        input_tokens: usage.input_tokens,
        input_tokens_display: display_optional_tokens(usage.input_tokens),
        output_tokens: usage.output_tokens,
        output_tokens_display: display_optional_tokens(usage.output_tokens),
        cached_input_tokens: usage.cached_input_tokens,
        cached_input_tokens_display: display_optional_tokens(usage.cached_input_tokens),
        image_input_tokens: usage.image_input_tokens,
        image_input_tokens_display: display_optional_tokens(usage.image_input_tokens),
        image_output_tokens: usage.image_output_tokens,
        image_output_tokens_display: display_optional_tokens(usage.image_output_tokens),
        image_request_count: usage.image_request_count,
        image_request_count_display: format_number(usage.image_request_count),
        image_request_failed_count: usage.image_request_failed_count,
        image_request_failed_count_display: format_number(usage.image_request_failed_count),
        total_tokens: usage.total_tokens,
        total_tokens_display: display_optional_tokens(usage.total_tokens),
        billing_amount_usd: usd.map(|amount| amount.as_str().to_owned()),
        billing_amount_usd_display: usd.map_or_else(
            || "—".to_owned(),
            |amount| format_decimal_currency(amount.as_str(), "USD"),
        ),
        cost_estimate_status: cost_estimate_status.to_owned(),
        known_cost_count: known_count,
        partial_cost_count: u64::from(cost_estimate_status == "partial"),
        unknown_cost_count: usage.cost_coverage.unavailable_count,
        costs: usage.costs.iter().map(account_currency_cost_view).collect(),
        last_used_at: time.rfc3339(&usage.last_used_at),
        last_used_at_full_display: Some(time.datetime(&usage.last_used_at)),
        last_used_at_display: time.relative(usage.last_used_at, now),
    }
}

// fork: billing —— 集成测试从 crate 外构造模型用量视图，复用上面的投影
impl From<(AccountModelUsage, DateTime<Utc>, crate::time::TimePresenter)> for ModelUsageView {
    fn from(
        (usage, now, time): (AccountModelUsage, DateTime<Utc>, crate::time::TimePresenter),
    ) -> Self {
        account_model_usage_view(usage, now, time)
    }
}

pub(super) fn account_currency_cost_view(cost: &AccountCost) -> CurrencyCostView {
    CurrencyCostView {
        currency: cost.currency.clone(),
        estimated_amount: cost.amount.as_str().to_owned(),
        estimated_amount_display: format_decimal_currency(cost.amount.as_str(), &cost.currency),
    }
}

pub(super) fn display_optional_tokens(value: Option<u64>) -> String {
    value.map_or_else(|| "—".to_owned(), format_compact_number)
}

/// 已用比例低于此值时外推误差过大，与额度预测的最小样本门槛一致。
const MIN_ESTIMATE_USED_PERCENT: f64 = 5.0;

/// 整窗口额度 ≈ 窗口内按模型价格计费金额 ÷ 已用比例；金额与比例取自同一个用量窗口。
fn estimated_quota_usd_display(quota: &ProviderQuota) -> Option<String> {
    let (window, _) = quota.usage_window()?;
    let used = window.used_percent.filter(|used| used.is_finite())?;
    if used < MIN_ESTIMATE_USED_PERCENT {
        return Some("已用不足 5%，暂不估算".to_owned());
    }
    let usage = window.local_usage.as_ref()?;
    let spent = usage
        .billing
        .model_price_usd
        .as_ref()?
        .as_str()
        .parse::<f64>()
        .ok()
        .filter(|spent| spent.is_finite() && *spent > 0.0)?;
    let estimate = spent / (used / 100.0);
    let display = format_decimal_currency(&format!("{estimate:.2}"), "USD");
    // 有请求缺少模型价格时金额偏低，外推结果同样偏低。
    Some(if usage.billing.model_price_count < usage.request_count {
        format!("{display}（部分计价，偏低）")
    } else {
        display
    })
}

pub(super) fn empty_account_usage() -> AccountUsageView {
    AccountUsageView {
        estimated_quota_usd_display: None,
        billing: AccountBillingView::from((&Default::default(), 0)),
        window_label_display: "周/月额度窗口".to_owned(),
        request_count: None,
        request_count_display: "—".to_owned(),
        input_tokens: None,
        input_tokens_display: "—".to_owned(),
        output_tokens: None,
        output_tokens_display: "—".to_owned(),
        cached_input_tokens: None,
        cached_input_tokens_display: "—".to_owned(),
        reasoning_output_tokens: None,
        reasoning_output_tokens_display: "—".to_owned(),
        image_input_tokens: None,
        image_input_tokens_display: "—".to_owned(),
        image_output_tokens: None,
        image_output_tokens_display: "—".to_owned(),
        image_request_count: None,
        image_request_count_display: "—".to_owned(),
        image_request_failed_count: None,
        image_request_failed_count_display: "—".to_owned(),
        total_tokens: None,
        total_tokens_display: "—".to_owned(),
        cache_write_input_tokens: None,
        cache_write_input_tokens_display: "—".to_owned(),
        last_used_at: None,
        last_used_at_full_display: None,
        last_used_at_display: "—".to_owned(),
        cost_estimate_status: "unavailable".to_owned(),
        known_cost_count: None,
        partial_cost_count: None,
        unknown_cost_count: None,
        costs: Vec::new(),
        models: Vec::new(),
    }
}

// fork: ticket —— 票据 / 成本 / 到期的安全展示；时间字段跟随部署时区
pub(super) fn account_ticket_view(
    ticket: gateway_admin::model::account_tickets::AccountTicketFacts,
    time: crate::time::TimePresenter,
) -> AccountTicketView {
    let currency = ticket
        .purchase_currency
        .map(|currency| currency.as_str().to_owned());
    let purchase_display = ticket
        .purchase_amount
        .as_deref()
        .zip(currency.as_deref())
        .map(|(amount, currency)| match currency {
            "CNY" => format!("¥{amount}"),
            _ => format_decimal_currency(amount, currency),
        });
    let spent_usd_display = ticket.spent_usd.as_deref().map(|amount| {
        // numeric 聚合带 10 位小数；展示保留两位
        let rounded = amount
            .parse::<f64>()
            .map_or_else(|_| amount.to_owned(), |value| format!("{value:.2}"));
        format_decimal_currency(&rounded, "USD")
    });
    AccountTicketView {
        purchase_amount: ticket.purchase_amount,
        purchase_currency: currency,
        purchase_display,
        purchased_at: ticket
            .purchased_at
            .as_ref()
            .map(|value| time.rfc3339(value)),
        expires_at: ticket.expires_at.as_ref().map(|value| time.rfc3339(value)),
        has_ticket: ticket.ticket_hint.is_some(),
        ticket_hint: ticket.ticket_hint,
        ticket_updated_at: ticket
            .ticket_updated_at
            .as_ref()
            .map(|value| time.rfc3339(value)),
        spent_usd: ticket.spent_usd,
        spent_usd_display,
        auto_revive_attempts: ticket.auto_revive_attempts,
        auto_revive_last_at: ticket
            .auto_revive_last_at
            .as_ref()
            .map(|value| time.rfc3339(value)),
        auto_revive_last_error: ticket.auto_revive_last_error,
    }
}

pub(super) fn map_wire_error(error: WireValidationError) -> AdminError {
    AdminError::bad_request(format!("{} 字段不合法", error.field()))
}

pub(super) fn map_service_error(error: AdminServiceError) -> AdminError {
    super::super::wire::map_admin_service_error(error)
}

impl From<(&gateway_admin::model::accounts::AccountBillingAmounts, u64)> for AccountBillingView {
    fn from(
        (billing, request_count): (&gateway_admin::model::accounts::AccountBillingAmounts, u64),
    ) -> Self {
        let model_price = billing
            .model_price_usd
            .as_ref()
            .map(|amount| amount.as_str().to_owned());
        let upstream_cost = billing
            .upstream_cost_usd
            .as_ref()
            .map(|amount| amount.as_str().to_owned());
        let difference = if request_count > 0
            && billing.model_price_count == request_count
            && billing.upstream_cost_count == request_count
        {
            model_price
                .as_deref()
                .zip(upstream_cost.as_deref())
                .and_then(|(price, upstream)| {
                    let price = price
                        .parse::<gateway_core::metering::Decimal>()
                        .ok()?
                        .scaled();
                    let upstream = upstream
                        .parse::<gateway_core::metering::Decimal>()
                        .ok()?
                        .scaled();
                    let magnitude =
                        gateway_core::metering::Decimal::from_scaled(price.abs_diff(upstream))
                            .ok()?
                            .canonical();
                    Some(if price < upstream {
                        format!("-{magnitude}")
                    } else {
                        magnitude
                    })
                })
        } else {
            None
        };
        let display = |amount: Option<&str>, count: u64, missing: &str| {
            amount.map_or_else(
                || missing.to_owned(),
                |amount| {
                    let display = format_decimal_currency(amount, "USD");
                    if count < request_count {
                        format!("{display}（部分 {count}/{request_count}）")
                    } else {
                        display
                    }
                },
            )
        };
        let difference_display = difference.as_deref().map_or_else(
            || "不可计算".to_owned(),
            |amount| {
                if let Some(magnitude) = amount.strip_prefix('-') {
                    format!("-{}", format_decimal_currency(magnitude, "USD"))
                } else {
                    format_decimal_currency(amount, "USD")
                }
            },
        );
        AccountBillingView {
            model_price_amount_usd_display: display(
                model_price.as_deref(),
                billing.model_price_count,
                "未提供",
            ),
            upstream_cost_amount_usd_display: display(
                upstream_cost.as_deref(),
                billing.upstream_cost_count,
                "未提供",
            ),
            model_price_amount_usd: model_price,
            upstream_cost_amount_usd: upstream_cost,
            difference_amount_usd: difference,
            difference_amount_usd_display: difference_display,
            model_price_count: billing.model_price_count,
            upstream_cost_count: billing.upstream_cost_count,
        }
    }
}
