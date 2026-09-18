-- 响应身份与模型计价独立于最终费用来源；历史模型不猜测回填。
alter table model_requests
  add column response_model text,
  add column billing_model text,
  add column calculated_cost_amount numeric(20, 10),
  add column calculated_cost_currency text,
  add constraint model_requests_calculated_cost_ck check (
    (calculated_cost_amount is null and calculated_cost_currency is null)
    or (calculated_cost_amount is not null and calculated_cost_amount >= 0
        and calculated_cost_currency is not null and calculated_cost_currency ~ '^[A-Z]{3}$')
  );

-- 仅来源已明确为本地计算的历史金额可迁移；真实上游金额仍由 provider_reported 区分。
update model_requests
set calculated_cost_amount = cost_amount, calculated_cost_currency = cost_currency
where cost_source = 'calculated';
