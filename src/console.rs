#[macro_export]
macro_rules! console_info {
    ($($arg:tt)*) => {
        { ::std::println!($($arg)*) }
    };
}

#[macro_export]
macro_rules! console_error {
    ($($arg:tt)*) => {
        { ::std::eprintln!($($arg)*) }
    };
}
