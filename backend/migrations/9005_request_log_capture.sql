-- fork 独立号段(9xxx)：请求日志采集开关与测试来源。
-- request_log_enabled：总开关；关闭则任何来源都不新增诊断日志。
-- request_log_test_key_id：指定「测试 client key」的 id；开启时只采集测试来源流量
--   （该测试 key + 后续内部测智入口），客户流量一律不采。
-- 单行 runtime_settings，additive，只增列带默认，旧版本读写不受影响。
alter table runtime_settings
    add column request_log_enabled boolean not null default true,
    add column request_log_test_key_id text
        check (
          request_log_test_key_id is null
          or (
            octet_length(request_log_test_key_id) between 1 and 128
            and request_log_test_key_id = btrim(request_log_test_key_id)
            and request_log_test_key_id !~ '[[:cntrl:]]'
          )
        );
