//! Utility functions
/// transform bytes into a human readable format.
pub fn to_human_readable(mut n: f64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB", "ZiB", "YiB"];
    let delimiter = 1024_f64;
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

#[macro_export]
macro_rules! as_expr {
    ($e:expr) => {
        $e
    };
}
#[macro_export]
macro_rules! as_item {
    ($i:item) => {
        $i
    };
}
#[macro_export]
macro_rules! as_pat {
    ($p:pat) => {
        $p
    };
}
#[macro_export]
macro_rules! as_stmt {
    ($s:stmt) => {
        $s
    };
}
#[macro_export]
macro_rules! as_ty {
    ($t:ty) => {
        $t
    };
}
#[macro_export]
macro_rules! as_ident {
    ($t:ident) => {
        $t
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    pub fn readable_size() {
        let n = 495353_f64;
        assert_eq!(to_human_readable(n), "483.74 KiB");

        let n = 30_178_876_f64;
        assert_eq!(to_human_readable(n), "28.78 MiB");

        let n = 2093903856_f64;
        assert_eq!(to_human_readable(n), "1.95 GiB");
    }
}
