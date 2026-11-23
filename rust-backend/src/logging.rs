use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

static LOG_FILE: OnceLock<Mutex<File>> = OnceLock::new();

/// 初始化文件日志。release 版为 Windows GUI 子系统，没有控制台，
/// 所有运行信息统一写入日志文件，便于排查问题。
pub fn init(log_path: &Path) {
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(log_path) {
        let _ = LOG_FILE.set(Mutex::new(file));
    }
    info("======== 应用启动 ========");
}

pub fn info(message: &str) {
    write("INFO", message);
    #[cfg(debug_assertions)]
    std::println!("{}", message);
}

pub fn error(message: &str) {
    write("ERROR", message);
    #[cfg(debug_assertions)]
    std::eprintln!("{}", message);
}

fn write(level: &str, message: &str) {
    if let Some(file) = LOG_FILE.get() {
        if let Ok(mut f) = file.lock() {
            let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            let _ = writeln!(f, "[{}] {:<5} {}", ts, level, message);
            let _ = f.flush();
        }
    }
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::logging::info(&format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        $crate::logging::error(&format!($($arg)*))
    };
}
