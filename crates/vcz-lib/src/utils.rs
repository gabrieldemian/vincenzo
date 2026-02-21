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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readable_size() {
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

        let n = u64::MAX;
        assert_eq!(to_human_readable(n as f64), "18.45 EB");

        let n = u128::MAX;
        assert_eq!(to_human_readable(n as f64), "340282366920938.38 YB");
    }
}
