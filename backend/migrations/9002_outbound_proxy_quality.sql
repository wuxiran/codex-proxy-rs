-- fork 独立号段(9xxx)：出口地区随连通性测试一起观测；地区查询尽力而为，允许只有 IP 而没有地区。
-- 质量检测结论随列表下发，逐项明细保存在 quality_report，仅在查看报告时读取。
-- 用独立子表承载，避免 ALTER 上游表 outbound_proxies；父表 last_test_success 由代码在同事务里保证。
-- 注意：原 geo_valid 里的 `last_test_success is true` 引用父表列，CHECK 跨不了表，此处去掉该子句。
create table outbound_proxy_quality (
  proxy_id text primary key references outbound_proxies(id) on delete cascade,
  last_test_country text,
  last_test_country_code text,
  last_test_region text,
  last_test_city text,
  quality_checked_at timestamptz,
  quality_score smallint,
  quality_grade text,
  quality_status text,
  quality_summary text,
  quality_report jsonb,
  constraint outbound_proxy_quality_geo_valid check (
    num_nonnulls(last_test_country, last_test_country_code) = 0
    or (
      num_nonnulls(last_test_country, last_test_country_code) = 2
      and char_length(last_test_country) between 1 and 128
      and last_test_country_code ~ '^[A-Z]{2}$'
    )
  ),
  constraint outbound_proxy_quality_geo_detail_valid check (
    (last_test_region is null and last_test_city is null)
    or last_test_country is not null
  ),
  constraint outbound_proxy_quality_complete check (
    num_nonnulls(
      quality_checked_at, quality_score, quality_grade,
      quality_status, quality_summary, quality_report
    ) in (0, 6)
  ),
  constraint outbound_proxy_quality_valid check (
    quality_checked_at is null
    or (
      quality_score between 0 and 100
      and quality_grade in ('A', 'B', 'C', 'D', 'F')
      and quality_status in ('healthy', 'warn', 'challenge', 'failed')
      and char_length(quality_summary) between 1 and 256
      and jsonb_typeof(quality_report) = 'object'
    )
  )
);
