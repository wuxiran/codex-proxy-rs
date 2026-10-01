//! 上游声明的服务模型与实际发送模型的对照。
//!
//! 上游有两处声明：响应头（`openai-model`，WebSocket 上是 metadata 帧里的同名头）和正文
//! `response.created` / 终态事件的 `response.model`。换模型只体现在其中一处时也算换了，
//! 所以两处分别比较，任一处不一致即 [`ServedMatch::Mismatch`]。
//!
//! 比较只忽略 ASCII 大小写，与官方客户端一致：带日期的快照名和裸名不算同一个模型，
//! 价格别名也不算。两处都没有声明时是 [`ServedMatch::Unknown`]，不当作一致。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServedMatch {
    Match,
    Mismatch,
    /// 上游没有给出任何模型声明。
    Unknown,
}

impl ServedMatch {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::Mismatch => "mismatch",
            Self::Unknown => "unknown",
        }
    }
}

/// `sent` 是最终发给上游的模型名（已经过管理端映射）。
pub fn compare(sent: &str, header: Option<&str>, body: Option<&str>) -> ServedMatch {
    let mut declared = [header, body].into_iter().flatten().peekable();
    if declared.peek().is_none() {
        return ServedMatch::Unknown;
    }
    if declared.all(|model| model.eq_ignore_ascii_case(sent)) {
        ServedMatch::Match
    } else {
        ServedMatch::Mismatch
    }
}

/// 业务请求发现换模型后的处置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServedMismatchAction {
    /// 只记录。非模拟运行时，这一轮签发的票仍然不入库，正在复用的旧票仍然失效。
    #[default]
    Observe,
    /// 中止换模型的响应，不重放业务请求。旧版 drop-pair 配置沿用阻断模式。
    #[serde(alias = "drop-pair")]
    Block,
}

impl ServedMismatchAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::Block => "block",
        }
    }
}
