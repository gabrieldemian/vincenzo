//! Utility functions
/// transform bytes into a human readable format.
pub fn to_human_readable(mut n: f64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB"];
    let delimiter = 1000_f64;

    if n < delimiter {
        // for bytes, format without trailing zeros
        if n.fract() == 0.0 {
            return format!("{:.0} {}", n, units[0]);
        } else {
            let formatted = format!("{:.2}", n);
            let trimmed = formatted.trim_end_matches('0').trim_end_matches('.');
            return format!("{} {}", trimmed, units[0]);
        }
    }

    let mut u: i32 = 0;
    let r = 10_f64;

    while (n * r).round() / r >= delimiter && u < (units.len() as i32) - 1 {
        n /= delimiter;
        u += 1;
    }

    // for larger units, format with 2 decimal places but remove trailing zeros
    let formatted = format!("{:.2}", n);
    let trimmed = formatted.trim_end_matches('0').trim_end_matches('.');
    format!("{} {}", trimmed, units[u as usize])
}

/// Round to significant digits (rather than digits after the decimal).
///
/// Not implemented for `f32`, because such an implementation showed precision
/// glitches (e.g. `precision_f32(12300.0, 2) == 11999.999`), so for `f32`
/// floats, convert to `f64` for this function and back as needed.
///
/// Examples:
/// ```ignore
///   precision_f64(1.2300, 2)                      // 1.2<f64>
///   precision_f64(1.2300_f64, 2)                  // 1.2<f64>
///   precision_f64(1.2300_f32 as f64, 2)           // 1.2<f64>
///   precision_f64(1.2300_f32 as f64, 2) as f32    // 1.2<f32>
/// ```
pub fn precision_f64(x: f64, decimals: u32) -> f64 {
    if x == 0. || decimals == 0 {
        0.
    } else {
        let shift = decimals as i32 - x.abs().log10().ceil() as i32;
        let shift_factor = 10_f64.powi(shift);

        (x * shift_factor).round() / shift_factor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    pub fn readable_size() {
        let n = 1.0;
        assert_eq!(to_human_readable(n), "1 B");

        let n = 1.000;
        assert_eq!(to_human_readable(n), "1 B");

        let n = 740.0;
        assert_eq!(to_human_readable(n), "740 B");

        let n = 7_040.0;
        assert_eq!(to_human_readable(n), "7.04 KB");

        let n = 483_740.0;
        assert_eq!(to_human_readable(n), "483.74 KB");

        let n = 28_780_000.0;
        assert_eq!(to_human_readable(n), "28.78 MB");

        let n = 1_950_000_000.0;
        assert_eq!(to_human_readable(n), "1.95 GB");
    }
}
