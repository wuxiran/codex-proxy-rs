//! 经营日报：北京时间自然日区间。

use chrono::NaiveDate;
use gateway_admin::ops_report::day_range;

#[test]
fn beijing_day_starts_at_16_utc() {
    let (start, end) = day_range(NaiveDate::from_ymd_opt(2026, 9, 24).unwrap()).unwrap();
    assert_eq!(start.to_rfc3339(), "2026-09-23T16:00:00+00:00");
    assert_eq!(end.to_rfc3339(), "2026-09-24T16:00:00+00:00");
}
