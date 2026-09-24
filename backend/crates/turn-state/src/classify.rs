//! 票据长度判别。长度只说明上游签发了哪一档 state，不代表模型质量。
//!
//! 上游多次调整过票据长度（观测过 292/312、332/356，后来统一到 ~780），所以正常/受限
//! 两档长度由运行设置提供；设置为空时退回「≥200 字节可见 ASCII 即可入库」的下限规则。

use serde::{Deserialize, Serialize};

use crate::settings::Settings;

/// 票据的最小可信长度：低于它的值不入库、不注入。
pub const MIN_TURN_STATE_LEN: usize = 200;

/// 一次上游签发的 state 属于哪一档。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LengthClass {
    /// 可入库复用的模板长度。
    Normal,
    /// 受限/降级态，永不入库；`replace-only` 模式下是唯一会被替换的形状。
    Degraded,
    /// 既不在模板表也不在受限表；上游若改了格式，所有响应都会落到这里。
    Unknown,
}

impl LengthClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Degraded => "degraded",
            Self::Unknown => "unknown",
        }
    }
}

pub fn classify(len: usize, settings: &Settings) -> LengthClass {
    if settings.degraded_lengths.contains(&len) {
        return LengthClass::Degraded;
    }
    let normal = if settings.template_lengths.is_empty() {
        len >= MIN_TURN_STATE_LEN
    } else {
        settings.template_lengths.contains(&len)
    };
    if normal {
        LengthClass::Normal
    } else {
        LengthClass::Unknown
    }
}

pub fn printable_ascii(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_graphic())
}

/// 一个值是否有资格成为模板：可见 ASCII 且长度属于正常档。
pub fn storable(value: &str, settings: &Settings) -> bool {
    printable_ascii(value) && classify(value.len(), settings) == LengthClass::Normal
}
