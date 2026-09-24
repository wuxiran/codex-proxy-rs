//! Codex `X-Codex-Turn-State` 的采集、落盘、复用与观测，按「账号 × 模型」桶隔离。
//!
//! 本 crate 只含纯逻辑与文件存储，不依赖任何 gateway/provider crate；宿主只在
//! 请求发出前调用 [`TurnStateService::begin_request`]、在拿到上游响应后调用
//! [`Attempt::observe`] / [`Attempt::completed`] 两处钩子。票据值永不进日志、事件或 `Debug`。

pub mod binding;
pub mod classify;
pub mod decision;
pub mod fernet;
pub mod fs_util;
pub mod observe;
pub mod record;
pub mod service;
pub mod settings;
pub mod store;

pub use binding::{credential_binding, egress_fingerprint};
pub use classify::{LengthClass, MIN_TURN_STATE_LEN};
pub use decision::Decision;
pub use fernet::IssuedAtSource;
pub use observe::{BucketTally, ObservationEvent, ObservationSnapshot};
pub use record::{BucketRecord, Source};
pub use service::{
    AccountWidePin, Attempt, BucketSummary, PinRejected, PinStatus, RequestFacts, TurnStateError,
    TurnStateService,
};
pub use settings::{CloudMintSettings, DEFAULT_TTL, InjectMode, MintMode, Settings, SettingsError};
