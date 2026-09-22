-- fork 独立号段(9xxx)：计价身份与本地计算成本独立于最终费用来源；历史模型不猜测回填。
-- 用独立子表承载，避免 ALTER 巨表 model_requests 取 ACCESS EXCLUSIVE 锁（发版锁表地雷）。
create table model_request_billing (
  model_request_id text primary key references model_requests(id) on delete cascade,
  response_model text,
  billing_model text,
  calculated_cost_amount numeric(20, 10),
  calculated_cost_currency text,
  constraint model_request_billing_calculated_cost_ck check (
    (calculated_cost_amount is null and calculated_cost_currency is null)
    or (calculated_cost_amount is not null and calculated_cost_amount >= 0
        and calculated_cost_currency is not null and calculated_cost_currency ~ '^[A-Z]{3}$')
  )
);

-- 仅来源已明确为本地计算的历史金额可迁移；真实上游金额仍由 provider_reported 区分。
insert into model_request_billing (model_request_id, calculated_cost_amount, calculated_cost_currency)
select id, cost_amount, cost_currency
from model_requests
where cost_source = 'calculated';
