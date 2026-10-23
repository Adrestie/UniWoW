//! Python's own arithmetic where the tools lean on it, so that the numbers come out the same: its
//! rounding (half to even), its `sum` of floats (compensated), its `math.dist` (a correctly
//! rounded norm), its `int(text, 0)`.

/// `round(x)`: halves to the even neighbour.
pub fn round(x: f64) -> f64 {
    x.round_ties_even()
}

/// `int(round(x))`.
pub fn round_int(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// `sum(values)` of floats: Neumaier's compensated sum, as CPython 3.12 and later.
pub fn sum(values: impl IntoIterator<Item = f64>) -> f64 {
    let mut total = 0.0f64;
    let mut c = 0.0f64;
    for x in values {
        let t = total + x;
        if total.abs() >= x.abs() {
            c += (total - t) + x;
        } else {
            c += (x - t) + total;
        }
        total = t;
    }
    if c != 0.0 && c.is_finite() { total + c } else { total }
}

/// A product as two doubles, lossless.
fn dl_mul(x: f64, y: f64) -> (f64, f64) {
    let hi = x * y;
    (hi, x.mul_add(y, -hi))
}

/// A sum as two doubles, lossless when |a| >= |b|.
fn dl_fast_sum(a: f64, b: f64) -> (f64, f64) {
    let x = a + b;
    let z = x - a;
    (x, b - z)
}

/// CPython's `vector_norm` (behind `math.dist` and `math.hypot`).
fn vector_norm(vec: &mut [f64], max: f64, found_nan: bool) -> f64 {
    if max.is_infinite() {
        return max;
    }
    if found_nan {
        return f64::NAN;
    }
    if max == 0.0 || vec.len() <= 1 {
        return max;
    }
    let max_e = frexp_exponent(max);
    if max_e < -1023 {
        for x in vec.iter_mut() {
            *x /= f64::MIN_POSITIVE;
        }
        return f64::MIN_POSITIVE * vector_norm(vec, max / f64::MIN_POSITIVE, found_nan);
    }
    let scale = ldexp(1.0, -max_e);
    let (mut csum, mut frac1, mut frac2) = (1.0f64, 0.0f64, 0.0f64);
    for x in vec.iter() {
        let x = x * scale;
        let (hi, lo) = dl_mul(x, x);
        let (s, slo) = dl_fast_sum(csum, hi);
        csum = s;
        frac1 += lo;
        frac2 += slo;
    }
    let mut h = (csum - 1.0 + (frac1 + frac2)).sqrt();
    let (hi, lo) = dl_mul(-h, h);
    let (s, slo) = dl_fast_sum(csum, hi);
    csum = s;
    frac1 += lo;
    frac2 += slo;
    let x = csum - 1.0 + (frac1 + frac2);
    h += x / (2.0 * h);
    h / scale
}

/// The exponent `frexp` gives: x = m * 2^e with 0.5 <= |m| < 1.
fn frexp_exponent(x: f64) -> i32 {
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    if exp == 0 {
        // Subnormal: normalise first.
        return frexp_exponent(x * 2f64.powi(64)) - 64;
    }
    exp - 1022
}

fn ldexp(x: f64, e: i32) -> f64 {
    // Two steps keep 2^e representable for the exponents met here.
    let half = e / 2;
    x * 2f64.powi(half) * 2f64.powi(e - half)
}

/// `math.dist(p, q)`.
pub fn dist(p: &[f64], q: &[f64]) -> f64 {
    let mut diffs: Vec<f64> = p.iter().zip(q).map(|(a, b)| (a - b).abs()).collect();
    let found_nan = diffs.iter().any(|d| d.is_nan());
    let max = diffs.iter().copied().filter(|d| !d.is_nan()).fold(0.0, f64::max);
    vector_norm(&mut diffs, max, found_nan)
}

/// `int(text, 0)`: decimal, or 0x, 0o, 0b; underscores between digits.
pub fn int_auto(text: &str) -> Option<i64> {
    let t = text.trim();
    let (negative, t) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let lower = t.to_ascii_lowercase();
    let (radix, digits) = if let Some(d) = lower.strip_prefix("0x") {
        (16, d)
    } else if let Some(d) = lower.strip_prefix("0o") {
        (8, d)
    } else if let Some(d) = lower.strip_prefix("0b") {
        (2, d)
    } else {
        // A decimal with a leading zero is refused by Python, but "0" and "00" are taken.
        if lower.len() > 1 && lower.starts_with('0') && !lower.trim_start_matches(['0', '_']).is_empty() {
            return None;
        }
        (10, lower.as_str())
    };
    let digits = digits.strip_prefix('_').unwrap_or(digits);
    if digits.is_empty() || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let value = i64::from_str_radix(&digits.replace('_', ""), radix).ok()?;
    Some(if negative { -value } else { value })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_python() {
        assert_eq!(round(0.5), 0.0);
        assert_eq!(round(1.5), 2.0);
        assert_eq!(round(-2.5), -2.0);
        // sum([0.1] * 10) is 1.0 in Python 3.12 and later.
        assert_eq!(sum(std::iter::repeat_n(0.1, 10)), 1.0);
        assert_eq!(dist(&[0.0, 0.0, 0.0], &[3.0, 4.0, 0.0]), 5.0);
        assert_eq!(int_auto("0x20029"), Some(0x20029));
        assert_eq!(int_auto("131113"), Some(131113));
        assert_eq!(int_auto("0b101"), Some(5));
        assert_eq!(int_auto("012"), None);
        assert_eq!(int_auto("0"), Some(0));
    }
}
