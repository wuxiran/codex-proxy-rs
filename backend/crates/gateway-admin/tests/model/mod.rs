//! 模型模块测试：镜像 src/model/ 中带逻辑的模块

mod account_groups;
mod account_tickets; // fork: account tickets
mod audit;
mod backup;
mod client_keys;
mod observability;
mod ops_report; // fork: ops report
mod plugins;
mod provider_credentials;
mod proxies;
mod quota_forecast;
mod quota_forecast_sampling;

mod fork_quota_forecast; // fork: quota forecast
mod fork_quota_forecast_sampling; // fork: quota forecast
mod retention;
