//! 服务态观测：按桶累计上游签发了哪一档 state，并区分「本请求有没有注入模板」。
//!
//! 这一区分不可省略：桶内有模板时每个请求都被注入，上游随即不再签发新 state，该桶在
//! 一个有效期内观测不到任何数据（盲区）；注入后上游仍签发受限档才是「模板失效」信号。
//! 快照写到 `<dir>/observations.json`，最短 60 秒一次，且是把本实例的增量合并进文件，
//! 不整体覆盖——蓝绿两个实例各写各的增量。

use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant, SystemTime},
};

use serde::{Deserialize, Serialize};

use crate::{classify::LengthClass, decision::Decision, record::unix_seconds};

const OBSERVATIONS_FILE: &str = "observations.json";
const SNAPSHOT_VERSION: u32 = 1;
const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(60);
const RING_CAPACITY: usize = 100;
const HOURLY_RETENTION_HOURS: u64 = 48;
const MAX_LENGTH_KEYS: usize = 64;

/// 一次响应侧观测的输入。
#[derive(Debug, Clone)]
pub struct ObservationInput {
    pub account: String,
    pub model: String,
    /// 上游本次签发的 state 长度；`None` = 上游沉默。
    pub issued_len: Option<usize>,
    pub class: Option<LengthClass>,
    pub decision: Decision,
    /// 本请求是否真的把模板发给了上游。
    pub injected: bool,
    pub at: SystemTime,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BucketTally {
    pub normal: u64,
    pub degraded: u64,
    pub unknown: u64,
    /// 上游没签发 state 的次数。
    pub silent: u64,
    pub injected_total: u64,
    /// 注入了模板且上游沉默：模板被接受，同时也是观测盲区。
    pub injected_silent: u64,
    /// 注入了模板上游仍签发受限档：这个桶的模板救不了它。
    pub injected_degraded: u64,
    pub injected_normal: u64,
    pub substituted: u64,
    pub last_seen_at: u64,
    pub last_issued_len: Option<usize>,
    pub last_issued_at: u64,
    pub lengths: BTreeMap<usize, u64>,
}

impl BucketTally {
    fn add(&mut self, other: &Self) {
        self.normal += other.normal;
        self.degraded += other.degraded;
        self.unknown += other.unknown;
        self.silent += other.silent;
        self.injected_total += other.injected_total;
        self.injected_silent += other.injected_silent;
        self.injected_degraded += other.injected_degraded;
        self.injected_normal += other.injected_normal;
        self.substituted += other.substituted;
        if other.last_seen_at >= self.last_seen_at {
            self.last_seen_at = other.last_seen_at;
        }
        if other.last_issued_at >= self.last_issued_at && other.last_issued_len.is_some() {
            self.last_issued_at = other.last_issued_at;
            self.last_issued_len = other.last_issued_len;
        }
        for (len, count) in &other.lengths {
            *self.lengths.entry(*len).or_default() += count;
        }
        while self.lengths.len() > MAX_LENGTH_KEYS {
            let smallest = self
                .lengths
                .iter()
                .min_by_key(|(_, count)| **count)
                .map(|(len, _)| *len);
            match smallest {
                Some(len) => {
                    self.lengths.remove(&len);
                }
                None => break,
            }
        }
    }

    fn record(&mut self, input: &ObservationInput) {
        let at = unix_seconds(input.at);
        self.last_seen_at = self.last_seen_at.max(at);
        if input.injected {
            self.injected_total += 1;
        }
        if input.decision == Decision::Substitute {
            self.substituted += 1;
        }
        match (input.issued_len, input.class) {
            (None, _) => {
                self.silent += 1;
                if input.injected {
                    self.injected_silent += 1;
                }
            }
            (Some(len), class) => {
                *self.lengths.entry(len).or_default() += 1;
                self.last_issued_len = Some(len);
                self.last_issued_at = at;
                match class {
                    Some(LengthClass::Normal) => {
                        self.normal += 1;
                        if input.injected {
                            self.injected_normal += 1;
                        }
                    }
                    Some(LengthClass::Degraded) => {
                        self.degraded += 1;
                        if input.injected {
                            self.injected_degraded += 1;
                        }
                    }
                    Some(LengthClass::Unknown) | None => self.unknown += 1,
                }
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HourTally {
    pub hour: u64,
    pub normal: u64,
    pub degraded: u64,
    pub unknown: u64,
    pub silent: u64,
    pub injected: u64,
}

impl HourTally {
    fn add(&mut self, other: &Self) {
        self.normal += other.normal;
        self.degraded += other.degraded;
        self.unknown += other.unknown;
        self.silent += other.silent;
        self.injected += other.injected;
    }

    fn record(&mut self, input: &ObservationInput) {
        if input.injected {
            self.injected += 1;
        }
        match (input.issued_len, input.class) {
            (None, _) => self.silent += 1,
            (Some(_), Some(LengthClass::Normal)) => self.normal += 1,
            (Some(_), Some(LengthClass::Degraded)) => self.degraded += 1,
            (Some(_), _) => self.unknown += 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationEvent {
    pub id: String,
    pub at: u64,
    pub account: String,
    pub model: String,
    pub decision: Decision,
    pub class: Option<LengthClass>,
    pub len: Option<usize>,
    pub injected: bool,
}

/// 整体快照；也是 `observations.json` 的文件格式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ObservationSnapshot {
    pub version: u32,
    pub updated_at: u64,
    /// 键为 `<账号>/<模型>`。
    pub buckets: BTreeMap<String, BucketTally>,
    pub hourly: Vec<HourTally>,
    pub histogram: BTreeMap<usize, u64>,
    pub events: Vec<ObservationEvent>,
}

impl Default for ObservationSnapshot {
    fn default() -> Self {
        Self {
            version: SNAPSHOT_VERSION,
            updated_at: 0,
            buckets: BTreeMap::new(),
            hourly: Vec::new(),
            histogram: BTreeMap::new(),
            events: Vec::new(),
        }
    }
}

impl ObservationSnapshot {
    fn merge(&mut self, delta: &Delta, now: SystemTime) {
        for (key, tally) in &delta.buckets {
            self.buckets.entry(key.clone()).or_default().add(tally);
        }
        let mut hourly: BTreeMap<u64, HourTally> = self
            .hourly
            .drain(..)
            .map(|tally| (tally.hour, tally))
            .collect();
        for (hour, tally) in &delta.hourly {
            hourly
                .entry(*hour)
                .or_insert_with(|| HourTally {
                    hour: *hour,
                    ..HourTally::default()
                })
                .add(tally);
        }
        let cutoff = unix_seconds(now).saturating_sub(HOURLY_RETENTION_HOURS * 3600);
        self.hourly = hourly
            .into_values()
            .filter(|tally| tally.hour >= cutoff)
            .collect();
        for (len, count) in &delta.histogram {
            *self.histogram.entry(*len).or_default() += count;
        }
        self.events.extend(delta.events.iter().cloned());
        // 事件 id 是 UUIDv7，按时间有序；并集去重后只留最新的一环。
        self.events.sort_by(|a, b| b.id.cmp(&a.id));
        self.events.dedup_by(|a, b| a.id == b.id);
        self.events.truncate(RING_CAPACITY);
        self.updated_at = unix_seconds(now);
    }
}

#[derive(Default)]
struct Delta {
    buckets: BTreeMap<String, BucketTally>,
    hourly: BTreeMap<u64, HourTally>,
    histogram: BTreeMap<usize, u64>,
    events: VecDeque<ObservationEvent>,
}

impl Delta {
    fn is_empty(&self) -> bool {
        self.buckets.is_empty() && self.events.is_empty()
    }

    fn record(&mut self, input: &ObservationInput) {
        let key = format!("{}/{}", input.account, input.model);
        self.buckets.entry(key).or_default().record(input);
        let hour = unix_seconds(input.at) / 3600 * 3600;
        self.hourly
            .entry(hour)
            .or_insert_with(|| HourTally {
                hour,
                ..HourTally::default()
            })
            .record(input);
        if let Some(len) = input.issued_len {
            *self.histogram.entry(len).or_default() += 1;
        }
        self.events.push_back(ObservationEvent {
            id: uuid::Uuid::now_v7().to_string(),
            at: unix_seconds(input.at),
            account: input.account.clone(),
            model: input.model.clone(),
            decision: input.decision,
            class: input.class,
            len: input.issued_len,
            injected: input.injected,
        });
        while self.events.len() > RING_CAPACITY {
            self.events.pop_front();
        }
    }
}

struct Inner {
    /// 没有目录时的合并基底；有目录时基底就是文件本身。
    base: ObservationSnapshot,
    delta: Delta,
    last_flush: Instant,
}

pub struct Observations {
    dir: Option<PathBuf>,
    inner: Mutex<Inner>,
}

impl Observations {
    pub fn in_memory() -> Self {
        Self::with_dir(None)
    }

    pub fn open(dir: &Path) -> Self {
        Self::with_dir(Some(dir.to_path_buf()))
    }

    fn with_dir(dir: Option<PathBuf>) -> Self {
        Self {
            dir,
            inner: Mutex::new(Inner {
                base: ObservationSnapshot::default(),
                delta: Delta::default(),
                last_flush: Instant::now(),
            }),
        }
    }

    pub fn record(&self, input: &ObservationInput) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.delta.record(input);
        if inner.last_flush.elapsed() >= SNAPSHOT_INTERVAL {
            self.flush_locked(&mut inner, input.at);
        }
    }

    /// 立即合并增量并返回整体快照（管理端读取用）。
    pub fn snapshot(&self, now: SystemTime) -> ObservationSnapshot {
        let Ok(mut inner) = self.inner.lock() else {
            return ObservationSnapshot::default();
        };
        self.flush_locked(&mut inner, now)
    }

    fn flush_locked(&self, inner: &mut Inner, now: SystemTime) -> ObservationSnapshot {
        inner.last_flush = Instant::now();
        let Some(dir) = &self.dir else {
            let delta = std::mem::take(&mut inner.delta);
            inner.base.merge(&delta, now);
            return inner.base.clone();
        };
        let path = dir.join(OBSERVATIONS_FILE);
        let Ok(_guard) = crate::fs_util::lock(dir) else {
            return inner.base.clone();
        };
        let mut merged = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ObservationSnapshot>(&bytes).ok())
            .filter(|snapshot| snapshot.version == SNAPSHOT_VERSION)
            .unwrap_or_default();
        if inner.delta.is_empty() {
            inner.base = merged.clone();
            return merged;
        }
        let delta = std::mem::take(&mut inner.delta);
        merged.merge(&delta, now);
        match serde_json::to_vec(&merged) {
            Ok(bytes) if crate::fs_util::atomic_write(dir, OBSERVATIONS_FILE, &bytes).is_ok() => {}
            _ => {
                // 写不进去就把增量留着，下次再合；不能让这一段观测凭空消失。
                inner.delta = delta;
                tracing::warn!(
                    target: "turn_state",
                    "[turn-state] observations snapshot could not be written; keeping the delta"
                );
            }
        }
        inner.base = merged.clone();
        merged
    }
}
