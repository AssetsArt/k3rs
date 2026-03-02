/// Write an info-level log to stdout.
macro_rules! log_info {
    ($($arg:tt)*) => {
        let _ = writeln!(std::io::stdout(), "[k3rs-init] {}", format!($($arg)*));
    };
}

/// Write an error-level log to stderr.
macro_rules! log_error {
    ($($arg:tt)*) => {
        let _ = writeln!(std::io::stderr(), "[k3rs-init] ERROR: {}", format!($($arg)*));
    };
}
