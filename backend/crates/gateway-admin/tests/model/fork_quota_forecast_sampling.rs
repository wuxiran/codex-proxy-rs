//! fork：额度预测曲线只保留当前连续段。

use super::quota_forecast_sampling::{point, select, time};

#[test]
fn curve_keeps_the_current_continuous_segment_in_time_order() {
    // 乱序输入、1 个百分点内的抖动与越界点都不进入曲线；当前点总是最后一个。
    let sample = select(
        vec![
            point(3, 12.0, 300),
            point(1, 5.0, 100),
            point(4, 11.5, 310),
            point(-1, 2.0, 0),
        ],
        point(5, 20.0, 500),
    );
    let curve: Vec<_> = sample
        .curve
        .iter()
        .map(|point| (point.observed_at, point.used_percent))
        .collect();
    assert_eq!(
        curve,
        vec![(time(1), 5.0), (time(3), 12.0), (time(5), 20.0)]
    );

    // 大幅回落代表上游已重置，旧段不与新段连线。
    let sample = select(
        vec![point(1, 40.0, 100), point(2, 50.0, 200), point(3, 1.0, 210)],
        point(4, 3.0, 300),
    );
    let curve: Vec<_> = sample
        .curve
        .iter()
        .map(|point| point.used_percent)
        .collect();
    assert_eq!(curve, vec![1.0, 3.0]);
}
