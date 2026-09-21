-- 出口地区随连通性测试一起观测；地区查询是尽力而为，允许只有 IP 而没有地区。
-- 质量检测结论随列表下发，逐项明细保存在 quality_report，仅在查看报告时读取。
alter table outbound_proxies
    add column last_test_country text,
    add column last_test_country_code text,
    add column last_test_region text,
    add column last_test_city text,
    add column quality_checked_at timestamptz,
    add column quality_score smallint,
    add column quality_grade text,
    add column quality_status text,
    add column quality_summary text,
    add column quality_report jsonb,
    add constraint outbound_proxies_last_test_geo_valid check (
        num_nonnulls(last_test_country, last_test_country_code) = 0
        or (
            num_nonnulls(last_test_country, last_test_country_code) = 2
            and last_test_success is true
            and char_length(last_test_country) between 1 and 128
            and last_test_country_code ~ '^[A-Z]{2}$'
        )
    ),
    add constraint outbound_proxies_last_test_geo_detail_valid check (
        (last_test_region is null and last_test_city is null)
        or last_test_country is not null
    ),
    add constraint outbound_proxies_quality_complete check (
        num_nonnulls(
            quality_checked_at, quality_score, quality_grade,
            quality_status, quality_summary, quality_report
        ) in (0, 6)
    ),
    add constraint outbound_proxies_quality_valid check (
        quality_checked_at is null
        or (
            quality_score between 0 and 100
            and quality_grade in ('A', 'B', 'C', 'D', 'F')
            and quality_status in ('healthy', 'warn', 'challenge', 'failed')
            and char_length(quality_summary) between 1 and 256
            and jsonb_typeof(quality_report) = 'object'
        )
    );
