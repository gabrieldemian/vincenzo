//! Utility functions
/// transform bytes into a human readable format.
pub fn to_human_readable(n: u64) -> String {
    let mut n = n as f64;

    let units = ["B", "KB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB"];
    let delimiter = 1000_f64;

    if n < delimiter {
        return format!("{} {}", n, "B");
    }

    let mut u: i32 = 0;
    let r = 10_f64;

    while (n * r).round() / r >= delimiter && u < (units.len() as i32) - 1 {
        n /= delimiter;
        u += 1;
    }

    format!("{:.2} {}", n, units[u as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    pub fn readable_size() {
        let n = 483_740;
        assert_eq!(to_human_readable(n), "483.74 KB");

        let n = 28_780_000;
        assert_eq!(to_human_readable(n), "28.78 MB");

        let n = 1_950_000_000;
        assert_eq!(to_human_readable(n), "1.95 GB");
    }
}
