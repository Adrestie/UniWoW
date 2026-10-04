//! Values between keys: smooth, without overshooting the keys, as the Clamped Auto keys of Unity.

/// The value at `t` of `points` `(time, value)`, sorted by time: the first value before the first
/// point, the last after the last, and between two points a cubic whose slopes keep it between
/// their values. Each end, and each point that is a peak or a trough, has a flat slope.
pub fn sample(points: &[(f64, f64)], t: f64) -> f64 {
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return 0.0;
    };
    if t <= first.0 {
        return first.1;
    }
    if t >= last.0 {
        return last.1;
    }
    let index = points.partition_point(|point| point.0 <= t) - 1;
    let ((t0, v0), (t1, v1)) = (points[index], points[index + 1]);
    let width = t1 - t0;
    let s = (t - t0) / width;
    let (m0, m1) = (slope(points, index) * width, slope(points, index + 1) * width);
    let (s2, s3) = (s * s, s * s * s);
    (2.0 * s3 - 3.0 * s2 + 1.0) * v0 + (s3 - 2.0 * s2 + s) * m0 + (3.0 * s2 - 2.0 * s3) * v1 + (s3 - s2) * m1
}

/// The slope at point `index`.
fn slope(points: &[(f64, f64)], index: usize) -> f64 {
    if index == 0 || index + 1 == points.len() {
        return 0.0;
    }
    let ((tp, vp), (t, v), (tn, vn)) = (points[index - 1], points[index], points[index + 1]);
    let (h0, h1) = (t - tp, tn - t);
    let (d0, d1) = ((v - vp) / h0, (vn - v) / h1);
    if d0 * d1 <= 0.0 {
        return 0.0;
    }
    // Fritsch and Butland: a weighted harmonic mean of the two slopes, which keeps the curve
    // monotonic between the points.
    3.0 * (h0 + h1) / ((2.0 * h1 + h0) / d0 + (h1 + 2.0 * h0) / d1)
}

#[cfg(test)]
mod tests {
    use super::sample;

    #[test]
    fn the_ends_hold_and_two_keys_ease_in_and_out() {
        let points = [(0.0, 0.0), (60.0, 10.0)];
        assert_eq!(sample(&points, -5.0), 0.0);
        assert_eq!(sample(&points, 80.0), 10.0);
        assert!((sample(&points, 30.0) - 5.0).abs() < 1e-9);
        assert!(sample(&points, 6.0) < 1.0, "it starts slowly");
        assert_eq!(sample(&[(10.0, 3.0)], 0.0), 3.0);
    }

    #[test]
    fn the_curve_never_overshoots_its_keys() {
        let points: [(f64, f64); 6] = [
            (0.0, 0.0),
            (10.0, 9.0),
            (12.0, 10.0),
            (40.0, -4.0),
            (41.0, 8.0),
            (60.0, 8.5),
        ];
        for pair in points.windows(2) {
            let (low, high) = (pair[0].1.min(pair[1].1), pair[0].1.max(pair[1].1));
            for step in 0..=100 {
                let t = pair[0].0 + (pair[1].0 - pair[0].0) * f64::from(step) / 100.0;
                let value = sample(&points, t);
                assert!(
                    value >= low - 1e-9 && value <= high + 1e-9,
                    "{value} outside {low}..{high} at {t}"
                );
            }
        }
    }

    #[test]
    fn the_curve_goes_through_every_key() {
        let points = [(0.0, 1.0), (15.0, 4.0), (30.0, 2.0)];
        for (t, v) in points {
            assert!((sample(&points, t) - v).abs() < 1e-9);
        }
    }
}
