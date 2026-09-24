//! 请求侧决策：拿桶内的有效模板和请求自带的 state 对照，决定改不改、怎么改。

use serde::{Deserialize, Serialize};

use crate::settings::{InjectMode, Settings};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    /// 请求没带 state（或带的不是模板），把模板加上/换上。
    Inject,
    /// `replace-only`：请求带着受限档 state，换成模板。
    Substitute,
    /// 原样放行。
    Pass,
    /// 桶键不全，无法归属，原样放行。
    Skip,
    /// 响应侧把一张新模板存进了桶。
    Harvest,
}

impl Decision {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inject => "inject",
            Self::Substitute => "substitute",
            Self::Pass => "pass",
            Self::Skip => "skip",
            Self::Harvest => "harvest",
        }
    }

    /// 本决策是否会把模板写进请求（尚未考虑 dry_run）。
    pub const fn rewrites(self) -> bool {
        matches!(self, Self::Inject | Self::Substitute)
    }
}

/// 一次决策的结果；`replacement` 为 `None` 表示不改请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub decision: Decision,
    pub reason: &'static str,
    pub replacement: Option<String>,
}

impl Verdict {
    const fn keep(decision: Decision, reason: &'static str) -> Self {
        Self {
            decision,
            reason,
            replacement: None,
        }
    }
}

pub fn decide(live: Option<&str>, carried: Option<&str>, settings: &Settings) -> Verdict {
    let Some(live) = live else {
        return match carried {
            Some(value) if settings.degraded_lengths.contains(&value.len()) => {
                Verdict::keep(Decision::Pass, "no live template for a degraded state")
            }
            _ => Verdict::keep(Decision::Pass, "no live template"),
        };
    };
    if carried == Some(live) {
        return Verdict::keep(Decision::Pass, "header already current");
    }
    match settings.inject_mode {
        InjectMode::Always => Verdict {
            decision: Decision::Inject,
            reason: match carried {
                None => "added (request carried no state)",
                Some(value) if settings.degraded_lengths.contains(&value.len()) => {
                    "replaced degraded state"
                }
                Some(_) => "replaced carried state",
            },
            replacement: Some(live.to_owned()),
        },
        InjectMode::ReplaceOnly => match carried {
            Some(value) if settings.degraded_lengths.contains(&value.len()) => Verdict {
                decision: Decision::Substitute,
                reason: "replaced degraded state",
                replacement: Some(live.to_owned()),
            },
            Some(_) => Verdict::keep(Decision::Pass, "carried length is not degraded"),
            None => Verdict::keep(Decision::Pass, "request carried no state (replace-only)"),
        },
    }
}

/// 每个请求恰一行；只有长度，永不含票值。
pub fn log(
    settings: &Settings,
    decision: Decision,
    account: &str,
    model: &str,
    len: Option<usize>,
    reason: &str,
) {
    if !settings.log_decisions {
        return;
    }
    tracing::info!(
        target: "turn_state",
        decision = decision.as_str(),
        account,
        model,
        len = len.unwrap_or(0),
        reason,
        dry_run = settings.dry_run,
        "[turn-state] decision"
    );
}
