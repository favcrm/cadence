//! Finite CLI output keeps running after a downstream consumer closes.
//!
//! These macros retain the standard formatting and streams without allocating.
//! Long-lived stdout owners must check `output::stdout_closed()` at their own
//! work boundary; a diagnostic on stderr never stops an operation.

#![allow(unused_macros)]

macro_rules! print {
    ($($arg:tt)*) => {
        $crate::console_output::print_format(format_args!($($arg)*))
    };
}

macro_rules! println {
    () => {
        $crate::console_output::print_format_line(format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::console_output::print_format_line(format_args!($($arg)*))
    };
}

macro_rules! eprint {
    ($($arg:tt)*) => {
        $crate::console_output::eprint_format(format_args!($($arg)*))
    };
}

macro_rules! eprintln {
    () => {
        $crate::console_output::eprint_format_line(format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::console_output::eprint_format_line(format_args!($($arg)*))
    };
}
