use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 运行配置：优先读取进程环境变量，其次读取 .env 文件（当前目录 / 可执行文件目录 / backend/.env）。
/// .env 中的相对路径按该 .env 所在目录解析，保证从项目根目录启动 exe 时仍复用 backend/ 下的数据。
#[derive(Debug, Clone)]
pub struct Config {
    pub mineru_token: String,
    pub mineru_api_url: String,
    pub max_file_size_mb: u64,
    pub max_size_bytes: u64,
    pub max_pages: u32,
    pub database_path: PathBuf,
    pub upload_folder: PathBuf,
    pub output_folder: PathBuf,
    pub output_md_folder: PathBuf,
    pub output_json_folder: PathBuf,
    pub image_folder: PathBuf,
    pub temp_folder: PathBuf,
    /// 全文检索索引目录（Tantivy）。与 OCR 产物分离，便于整体重建与损坏隔离
    pub search_index_folder: PathBuf,
    /// EPUB 导出目录：预览弹窗导出的电子书统一落盘位置
    pub epub_folder: PathBuf,
    /// 繁→简自定义映射原文（每行 `繁体词=>简体词`，空为不启用）
    pub zh_t2s_mapping: String,
    pub ocr_max_workers: usize,
    pub mineru_poll_timeout: u64,
    pub mineru_poll_interval: u64,
    pub ocr_daily_upload_limit: i64,
    pub ocr_upload_interval: u64,
    pub ocr_fetch_max_retries: u32,
    pub ocr_fetch_timeout: u64,
    pub ocr_download_max_retries: u32,
    pub ocr_download_timeout: u64,
    pub ocr_download_max_cycles: u32,
    pub ocr_retry_backoff_base: f64,
    pub ocr_retry_backoff_max: f64,
    pub ocr_retry_jitter_ratio: f64,
    pub resume_upload_url_ttl: u64,
    pub resume_max_upload_attempts: i64,
    pub batch_doc_limit: usize,
    pub batch_chunk_limit: usize,
    pub scheduler_sort_mode: String,
    pub port: u16,
    pub open_browser: bool,
    pub cors_origins: Vec<String>,
}

struct EnvValue {
    value: String,
    /// .env 文件所在目录，用于解析相对路径
    base_dir: PathBuf,
}

fn env_file_values() -> HashMap<String, EnvValue> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join(".env"));
        candidates.push(cwd.join("backend").join(".env"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(".env"));
            candidates.push(dir.join("backend").join(".env"));
        }
    }
    let mut map: HashMap<String, EnvValue> = HashMap::new();
    for path in candidates {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let base_dir = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let key = k.trim().to_string();
            let mut val = v.trim().to_string();
            if val.len() >= 2
                && ((val.starts_with('"') && val.ends_with('"'))
                    || (val.starts_with('\'') && val.ends_with('\'')))
            {
                val = val[1..val.len() - 1].to_string();
            }
            map.entry(key).or_insert(EnvValue { value: val, base_dir: base_dir.clone() });
        }
    }
    map
}

fn get(envs: &HashMap<String, EnvValue>, key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .or_else(|| envs.get(key).map(|v| v.value.clone()))
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn get_parse<T: std::str::FromStr>(
    envs: &HashMap<String, EnvValue>,
    key: &str,
    default: T,
) -> T {
    std::env::var(key)
        .ok()
        .or_else(|| envs.get(key).map(|v| v.value.clone()))
        .and_then(|v| v.trim().parse::<T>().ok())
        .unwrap_or(default)
}

/// 解析路径：进程环境变量按当前工作目录解析；.env 中的值按该 .env 所在目录解析
fn get_path(
    envs: &HashMap<String, EnvValue>,
    key: &str,
    default: PathBuf,
) -> PathBuf {
    if let Ok(v) = std::env::var(key) {
        if !v.trim().is_empty() {
            return PathBuf::from(v.trim());
        }
    }
    if let Some(entry) = envs.get(key) {
        let p = PathBuf::from(&entry.value);
        if p.is_absolute() {
            return p;
        }
        return entry.base_dir.join(p);
    }
    default
}

fn default_if_drive_exists(preferred: &str, fallback: PathBuf) -> PathBuf {
    // 默认把体积巨大的 JSON / 临时分块放到 E 盘；机器上没有 E 盘时退回项目目录，避免启动失败
    if let Some(prefix) = preferred.get(..2) {
        if Path::new(&format!("{}\\", prefix)).exists() {
            return PathBuf::from(preferred);
        }
    }
    fallback
}

/// 若当前目录下存在历史数据目录 `backend/data`，默认沿用 backend/ 下的数据库与产物目录，
/// 这样把 exe 放在项目根目录双击运行时无需任何配置即可看到已有数据。
fn has_legacy_data() -> bool {
    Path::new("backend").join("data").is_dir()
}

fn legacy_backend_dir() -> Option<PathBuf> {
    if has_legacy_data() {
        Some(PathBuf::from("backend"))
    } else {
        None
    }
}

/// 数据库里保存的历史产物路径（如 `./output\md\xxx.md`）是相对旧版后端运行目录的。
/// exe 从项目根目录双击启动时，相对路径必须按 `backend/` 而不是 cwd 解析，否则预览/重命名/删除全部失败。
fn data_base_dir() -> &'static PathBuf {
    static BASE: OnceLock<PathBuf> = OnceLock::new();
    BASE.get_or_init(|| {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        if has_legacy_data() {
            cwd.join("backend")
        } else {
            cwd
        }
    })
}

/// 解析数据库中的路径：绝对路径原样返回，相对路径按数据目录（通常为 backend/）解析
pub fn resolve_path(p: &str) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        data_base_dir().join(path)
    }
}

impl Config {
    pub fn load() -> Self {
        let envs = env_file_values();
        let legacy = legacy_backend_dir();
        let default_under = |sub: &str, fallback: &str| -> PathBuf {
            match &legacy {
                Some(base) => base.join(sub),
                None => PathBuf::from(fallback),
            }
        };

        let max_file_size_mb = get_parse(&envs, "MAX_FILE_SIZE_MB", 200u64);
        let output_folder = get_path(&envs, "OUTPUT_FOLDER", default_under("output", "./output"));
        let output_md_folder = {
            if envs.contains_key("OUTPUT_MD_FOLDER") || std::env::var("OUTPUT_MD_FOLDER").is_ok() {
                get_path(&envs, "OUTPUT_MD_FOLDER", output_folder.join("md"))
            } else {
                // Markdown 单个体积大，默认放到 E 盘集中管理；没有 E 盘时退回产物目录
                default_if_drive_exists(
                    "E:\\pdf_manager_output\\md",
                    output_folder.join("md"),
                )
            }
        };
        let output_json_folder = {
            if envs.contains_key("OUTPUT_JSON_FOLDER") || std::env::var("OUTPUT_JSON_FOLDER").is_ok() {
                get_path(&envs, "OUTPUT_JSON_FOLDER", output_folder.join("json"))
            } else {
                default_if_drive_exists(
                    "E:\\pdf_manager_output\\json",
                    output_folder.join("json"),
                )
            }
        };
        let image_folder = {
            if envs.contains_key("IMAGE_FOLDER") || std::env::var("IMAGE_FOLDER").is_ok() {
                get_path(&envs, "IMAGE_FOLDER", output_folder.join("images"))
            } else {
                default_if_drive_exists(
                    "E:\\pdf_manager_output\\images",
                    output_folder.join("images"),
                )
            }
        };
        let temp_folder = {
            if envs.contains_key("TEMP_FOLDER") || std::env::var("TEMP_FOLDER").is_ok() {
                get_path(&envs, "TEMP_FOLDER", PathBuf::from("./temp"))
            } else {
                default_if_drive_exists("E:\\temp", default_under("temp", "./temp"))
            }
        };
        let cors_origins = get(
            &envs,
            "CORS_ORIGINS",
            "http://localhost:3000,http://127.0.0.1:3000",
        )
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

        // 索引默认与 Markdown 一起放到 E 盘；没有 E 盘时退回产物目录。
        // 需要放到别处时用 SEARCH_INDEX_FOLDER 指定，或在设置面板修改后写入 app_meta。
        let search_index_folder = {
            if envs.contains_key("SEARCH_INDEX_FOLDER")
                || std::env::var("SEARCH_INDEX_FOLDER").is_ok()
            {
                get_path(
                    &envs,
                    "SEARCH_INDEX_FOLDER",
                    output_folder.join("search_index"),
                )
            } else {
                default_if_drive_exists(
                    "E:\\pdf_manager_output\\search_index",
                    output_folder.join("search_index"),
                )
            }
        };

        // EPUB 与 Markdown 一起放 E 盘；没有 E 盘时退回产物目录
        let epub_folder = {
            if envs.contains_key("EPUB_FOLDER") || std::env::var("EPUB_FOLDER").is_ok() {
                get_path(&envs, "EPUB_FOLDER", output_folder.join("epub"))
            } else {
                default_if_drive_exists("E:\\pdf_manager_output\\epub", output_folder.join("epub"))
            }
        };

        Self {
            mineru_token: get(&envs, "MINERU_TOKEN", ""),
            mineru_api_url: get(&envs, "MINERU_API_URL", "https://mineru.net/api/v4"),
            max_file_size_mb,
            max_size_bytes: max_file_size_mb * 1024 * 1024,
            max_pages: get_parse(&envs, "MAX_PAGES", 200u32),
            database_path: get_path(
                &envs,
                "DATABASE_PATH",
                default_under("data/pdf_manager.db", "./data/pdf_manager.db"),
            ),
            upload_folder: get_path(
                &envs,
                "UPLOAD_FOLDER",
                default_under("uploads", "./uploads"),
            ),
            output_folder,
            output_md_folder,
            output_json_folder,
            image_folder,
            temp_folder,
            search_index_folder,
            epub_folder,
            zh_t2s_mapping: String::new(),
            ocr_max_workers: get_parse(&envs, "OCR_MAX_WORKERS", 2usize),
            mineru_poll_timeout: get_parse(&envs, "MINERU_POLL_TIMEOUT", 21600u64),
            mineru_poll_interval: get_parse(&envs, "MINERU_POLL_INTERVAL", 8u64),
            ocr_daily_upload_limit: get_parse(&envs, "OCR_DAILY_UPLOAD_LIMIT", 5000i64),
            ocr_upload_interval: get_parse(&envs, "OCR_UPLOAD_INTERVAL", 65u64),
            ocr_fetch_max_retries: get_parse(&envs, "OCR_FETCH_MAX_RETRIES", 3u32),
            ocr_fetch_timeout: get_parse(&envs, "OCR_FETCH_TIMEOUT", 30u64),
            ocr_download_max_retries: get_parse(&envs, "OCR_DOWNLOAD_MAX_RETRIES", 3u32),
            ocr_download_timeout: get_parse(&envs, "OCR_DOWNLOAD_TIMEOUT", 300u64),
            ocr_download_max_cycles: get_parse(&envs, "OCR_DOWNLOAD_MAX_CYCLES", 3u32),
            ocr_retry_backoff_base: get_parse(&envs, "OCR_RETRY_BACKOFF_BASE", 2.0f64),
            ocr_retry_backoff_max: get_parse(&envs, "OCR_RETRY_BACKOFF_MAX", 30.0f64),
            ocr_retry_jitter_ratio: get_parse(&envs, "OCR_RETRY_JITTER_RATIO", 0.3f64),
            resume_upload_url_ttl: get_parse(&envs, "RESUME_UPLOAD_URL_TTL", 1800u64),
            resume_max_upload_attempts: get_parse(&envs, "RESUME_MAX_UPLOAD_ATTEMPTS", 3i64),
            batch_doc_limit: get_parse(&envs, "BATCH_DOC_LIMIT", 30usize),
            batch_chunk_limit: get_parse(&envs, "BATCH_CHUNK_LIMIT", 48usize),
            scheduler_sort_mode: get(&envs, "SCHEDULER_SORT_MODE", "shortest_first"),
            port: get_parse(&envs, "FLASK_RUN_PORT", get_parse(&envs, "PORT", 5000u16)),
            // 桌面版默认只显示应用窗口；需要额外打开系统浏览器时设置 OPEN_BROWSER=1
            open_browser: get(&envs, "OPEN_BROWSER", "0") == "1",
            cors_origins,
        }
    }

    pub fn init_directories(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.upload_folder)?;
        std::fs::create_dir_all(&self.output_folder)?;
        std::fs::create_dir_all(&self.output_md_folder)?;
        std::fs::create_dir_all(&self.output_json_folder)?;
        std::fs::create_dir_all(&self.image_folder)?;
        std::fs::create_dir_all(&self.temp_folder)?;
        std::fs::create_dir_all(&self.search_index_folder)?;
        std::fs::create_dir_all(&self.epub_folder)?;
        if let Some(parent) = self.database_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(())
    }
}
