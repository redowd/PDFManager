//! 配置面板：持久化到 app_meta，接口脱敏，保存后热更新运行配置。
//!
//! 优先级：app_meta（本面板保存的值） > 进程环境变量 > .env 文件 > 内置默认值。
//! 默认值统一由 `Config::load()` 计算（其中环境变量/.env 会覆盖内置默认）。
//! 保存后仅对之后开始的任务/轮次生效，运行中的任务沿用自身配置快照。

use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::config::Config;
use crate::db::Db;

pub const KEY_MINERU_TOKEN: &str = "settings.mineru_token";
pub const KEY_MINERU_API_URL: &str = "settings.mineru_api_url";
pub const KEY_IMAGE_FOLDER: &str = "settings.image_folder";
pub const KEY_JSON_FOLDER: &str = "settings.json_folder";
pub const KEY_MD_FOLDER: &str = "settings.md_folder";
/// 检索索引目录：变更后旧索引不可直接沿用，需要重新加载或重建
pub const KEY_SEARCH_INDEX_FOLDER: &str = "settings.search_index_folder";
/// EPUB 导出目录：仅影响之后导出的电子书，不影响已有文件与 OCR 产物
pub const KEY_EPUB_FOLDER: &str = "settings.epub_folder";
/// 繁→简自定义映射：每行 `繁体词=>简体词`，空串表示不启用
pub const KEY_ZH_T2S_MAPPING: &str = "settings.zh_t2s_mapping";

pub const KEY_OCR_DAILY_UPLOAD_LIMIT: &str = "settings.ocr_daily_upload_limit";
pub const KEY_OCR_UPLOAD_INTERVAL: &str = "settings.ocr_upload_interval";
pub const KEY_BATCH_DOC_LIMIT: &str = "settings.batch_doc_limit";
pub const KEY_BATCH_CHUNK_LIMIT: &str = "settings.batch_chunk_limit";
pub const KEY_MAX_PAGES: &str = "settings.max_pages";
pub const KEY_MAX_FILE_SIZE_MB: &str = "settings.max_file_size_mb";
pub const KEY_SCHEDULER_SORT_MODE: &str = "settings.scheduler_sort_mode";

pub const KEY_MINERU_POLL_TIMEOUT: &str = "settings.mineru_poll_timeout";
pub const KEY_MINERU_POLL_INTERVAL: &str = "settings.mineru_poll_interval";
pub const KEY_OCR_FETCH_MAX_RETRIES: &str = "settings.ocr_fetch_max_retries";
pub const KEY_OCR_FETCH_TIMEOUT: &str = "settings.ocr_fetch_timeout";
pub const KEY_OCR_DOWNLOAD_MAX_RETRIES: &str = "settings.ocr_download_max_retries";
pub const KEY_OCR_DOWNLOAD_TIMEOUT: &str = "settings.ocr_download_timeout";
pub const KEY_OCR_DOWNLOAD_MAX_CYCLES: &str = "settings.ocr_download_max_cycles";
pub const KEY_OCR_RETRY_BACKOFF_BASE: &str = "settings.ocr_retry_backoff_base";
pub const KEY_OCR_RETRY_BACKOFF_MAX: &str = "settings.ocr_retry_backoff_max";
pub const KEY_OCR_RETRY_JITTER_RATIO: &str = "settings.ocr_retry_jitter_ratio";

pub const KEY_RESUME_UPLOAD_URL_TTL: &str = "settings.resume_upload_url_ttl";
pub const KEY_RESUME_MAX_UPLOAD_ATTEMPTS: &str = "settings.resume_max_upload_attempts";

/// 读取 app_meta 中持久化的配置覆盖项并合并到 Config（非法/缺失的项保持原值）
pub fn load_overrides(db: &Db, cfg: &mut Config) -> anyhow::Result<()> {
    if let Some(token) = db.get_meta(KEY_MINERU_TOKEN, None)? {
        if !token.is_empty() {
            cfg.mineru_token = token;
        }
    }
    if let Some(v) = db.get_meta(KEY_MINERU_API_URL, None)? {
        if !v.trim().is_empty() {
            cfg.mineru_api_url = v;
        }
    }
    if let Some(v) = db.get_meta(KEY_IMAGE_FOLDER, None)? {
        if !v.trim().is_empty() {
            cfg.image_folder = PathBuf::from(v);
        }
    }
    if let Some(v) = db.get_meta(KEY_JSON_FOLDER, None)? {
        if !v.trim().is_empty() {
            cfg.output_json_folder = PathBuf::from(v);
        }
    }
    if let Some(v) = db.get_meta(KEY_MD_FOLDER, None)? {
        if !v.trim().is_empty() {
            cfg.output_md_folder = PathBuf::from(v);
        }
    }
    if let Some(v) = db.get_meta(KEY_SEARCH_INDEX_FOLDER, None)? {
        if !v.trim().is_empty() {
            cfg.search_index_folder = PathBuf::from(v);
        }
    }
    if let Some(v) = db.get_meta(KEY_EPUB_FOLDER, None)? {
        if !v.trim().is_empty() {
            cfg.epub_folder = PathBuf::from(v);
        }
    }
    if let Some(v) = db.get_meta(KEY_ZH_T2S_MAPPING, None)? {
        // 与目录不同：映射允许为空串（清除映射），存在键即生效
        cfg.zh_t2s_mapping = v;
    }
    if let Some(v) = db.get_meta(KEY_SCHEDULER_SORT_MODE, None)? {
        if v == "shortest_first" || v == "id_asc" {
            cfg.scheduler_sort_mode = v;
        }
    }

    load_num(db, KEY_OCR_DAILY_UPLOAD_LIMIT, &mut cfg.ocr_daily_upload_limit)?;
    load_num(db, KEY_OCR_UPLOAD_INTERVAL, &mut cfg.ocr_upload_interval)?;
    load_num(db, KEY_BATCH_DOC_LIMIT, &mut cfg.batch_doc_limit)?;
    load_num(db, KEY_BATCH_CHUNK_LIMIT, &mut cfg.batch_chunk_limit)?;
    load_num(db, KEY_MAX_PAGES, &mut cfg.max_pages)?;
    load_num(db, KEY_MAX_FILE_SIZE_MB, &mut cfg.max_file_size_mb)?;
    load_num(db, KEY_MINERU_POLL_TIMEOUT, &mut cfg.mineru_poll_timeout)?;
    load_num(db, KEY_MINERU_POLL_INTERVAL, &mut cfg.mineru_poll_interval)?;
    load_num(db, KEY_OCR_FETCH_MAX_RETRIES, &mut cfg.ocr_fetch_max_retries)?;
    load_num(db, KEY_OCR_FETCH_TIMEOUT, &mut cfg.ocr_fetch_timeout)?;
    load_num(
        db,
        KEY_OCR_DOWNLOAD_MAX_RETRIES,
        &mut cfg.ocr_download_max_retries,
    )?;
    load_num(db, KEY_OCR_DOWNLOAD_TIMEOUT, &mut cfg.ocr_download_timeout)?;
    load_num(
        db,
        KEY_OCR_DOWNLOAD_MAX_CYCLES,
        &mut cfg.ocr_download_max_cycles,
    )?;
    load_num(
        db,
        KEY_OCR_RETRY_BACKOFF_BASE,
        &mut cfg.ocr_retry_backoff_base,
    )?;
    load_num(
        db,
        KEY_OCR_RETRY_BACKOFF_MAX,
        &mut cfg.ocr_retry_backoff_max,
    )?;
    load_num(
        db,
        KEY_OCR_RETRY_JITTER_RATIO,
        &mut cfg.ocr_retry_jitter_ratio,
    )?;
    load_num(
        db,
        KEY_RESUME_UPLOAD_URL_TTL,
        &mut cfg.resume_upload_url_ttl,
    )?;
    load_num(
        db,
        KEY_RESUME_MAX_UPLOAD_ATTEMPTS,
        &mut cfg.resume_max_upload_attempts,
    )?;

    cfg.max_size_bytes = cfg.max_file_size_mb * 1024 * 1024;
    Ok(())
}

fn load_num<T: FromStr>(db: &Db, key: &str, target: &mut T) -> anyhow::Result<()> {
    if let Some(raw) = db.get_meta(key, None)? {
        if let Ok(v) = raw.trim().parse::<T>() {
            *target = v;
        }
    }
    Ok(())
}

/// 目录占用统计（设置页展示磁盘压力）
#[derive(Debug, Clone, Serialize, Default)]
pub struct DirStat {
    pub file_count: u64,
    pub total_bytes: u64,
}

pub fn dir_stat(dir: &Path) -> DirStat {
    fn walk(dir: &Path, stat: &mut DirStat) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                walk(&entry.path(), stat);
            } else {
                stat.file_count += 1;
                stat.total_bytes += meta.len();
            }
        }
    }
    let mut stat = DirStat::default();
    walk(dir, &mut stat);
    stat
}

#[derive(Debug, Serialize)]
pub struct SettingsResponse {
    pub mineru_token_masked: Option<String>,
    pub has_mineru_token: bool,
    pub image_folder: String,
    pub json_folder: String,
    pub md_folder: String,
    pub search_index_folder: String,
    pub epub_folder: String,
    pub zh_t2s_mapping: String,
    /// 当前生效值（含目录与 API 基址）
    pub values: Map<String, Value>,
    /// 默认值（来自 .env / 内置默认，不含 app_meta 覆盖）
    pub defaults: Map<String, Value>,
    pub image_dir: DirStat,
    pub json_dir: DirStat,
    pub md_dir: DirStat,
    pub search_index_dir: DirStat,
    pub epub_dir: DirStat,
    pub restart_required: bool,
}

/// token 掩码：仅保留首尾各 4 位
pub fn mask_token(token: &str) -> String {
    let chars: Vec<char> = token.chars().collect();
    if chars.is_empty() {
        return String::new();
    }
    if chars.len() <= 8 {
        return "********".to_string();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{}****{}", head, tail)
}

#[derive(Debug, Deserialize)]
pub struct SettingsUpdate {
    /// 缺省或空串 = 不修改，避免误清空
    #[serde(default)]
    pub mineru_token: Option<String>,
    #[serde(default)]
    pub clear_mineru_token: bool,
    #[serde(default)]
    pub mineru_api_url: Option<String>,
    #[serde(default)]
    pub image_folder: Option<String>,
    #[serde(default)]
    pub json_folder: Option<String>,
    #[serde(default)]
    pub md_folder: Option<String>,
    #[serde(default)]
    pub search_index_folder: Option<String>,
    #[serde(default)]
    pub epub_folder: Option<String>,
    /// 繁→简自定义映射：传 Some 即整体覆盖（含空串 = 清除）
    #[serde(default)]
    pub zh_t2s_mapping: Option<String>,
    #[serde(default)]
    pub ocr_daily_upload_limit: Option<i64>,
    #[serde(default)]
    pub ocr_upload_interval: Option<u64>,
    #[serde(default)]
    pub batch_doc_limit: Option<usize>,
    #[serde(default)]
    pub batch_chunk_limit: Option<usize>,
    #[serde(default)]
    pub max_pages: Option<u32>,
    #[serde(default)]
    pub max_file_size_mb: Option<u64>,
    #[serde(default)]
    pub scheduler_sort_mode: Option<String>,
    #[serde(default)]
    pub mineru_poll_timeout: Option<u64>,
    #[serde(default)]
    pub mineru_poll_interval: Option<u64>,
    #[serde(default)]
    pub ocr_fetch_max_retries: Option<u32>,
    #[serde(default)]
    pub ocr_fetch_timeout: Option<u64>,
    #[serde(default)]
    pub ocr_download_max_retries: Option<u32>,
    #[serde(default)]
    pub ocr_download_timeout: Option<u64>,
    #[serde(default)]
    pub ocr_download_max_cycles: Option<u32>,
    #[serde(default)]
    pub ocr_retry_backoff_base: Option<f64>,
    #[serde(default)]
    pub ocr_retry_backoff_max: Option<f64>,
    #[serde(default)]
    pub ocr_retry_jitter_ratio: Option<f64>,
    #[serde(default)]
    pub resume_upload_url_ttl: Option<u64>,
    #[serde(default)]
    pub resume_max_upload_attempts: Option<i64>,
}

/// 目录路径合法性：绝对路径 + 不含 Windows 非法字符
pub fn validate_folder(raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("目录不能为空".into());
    }
    if let Some(ch) = trimmed.chars().find(|c| matches!(c, '*' | '?' | '"' | '<' | '>' | '|')) {
        return Err(format!("目录包含非法字符: {}", ch));
    }
    let path = PathBuf::from(trimmed);
    if !path.is_absolute() {
        return Err("请填写绝对路径".into());
    }
    Ok(path)
}

/// 目录可写校验：创建目录并写删探针文件
pub fn ensure_writable(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("无法创建目录: {}", e))?;
    let probe = dir.join(".write_test.tmp");
    std::fs::write(&probe, b"ok").map_err(|e| format!("目录不可写: {}", e))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

fn apply_num<T>(
    db: &Db,
    key: &str,
    raw: Option<T>,
    min: T,
    max: T,
    label: &str,
    target: &mut T,
) -> Result<(), String>
where
    T: Copy + PartialOrd + Display,
{
    let Some(value) = raw else { return Ok(()) };
    if value < min || value > max {
        return Err(format!("{} 需在 {} ~ {} 之间", label, min, max));
    }
    db.set_meta(key, &value.to_string())
        .map_err(|e| format!("保存{}失败: {}", label, e))?;
    *target = value;
    Ok(())
}

/// 保存设置：校验 → 落 app_meta → 返回合并后的新配置（由调用方热更新）
pub fn save(db: &Db, current: &Config, req: &SettingsUpdate) -> Result<Config, String> {
    let mut next = current.clone();

    if req.clear_mineru_token {
        db.set_meta(KEY_MINERU_TOKEN, "")
            .map_err(|e| format!("保存 token 失败: {}", e))?;
        next.mineru_token.clear();
    } else if let Some(token) = req.mineru_token.as_deref() {
        let token = token.trim();
        if !token.is_empty() {
            db.set_meta(KEY_MINERU_TOKEN, token)
                .map_err(|e| format!("保存 token 失败: {}", e))?;
            next.mineru_token = token.to_string();
        }
    }
    if next.mineru_token.trim().is_empty() {
        return Err("MinerU Token 不能为空".into());
    }

    if let Some(raw) = req.mineru_api_url.as_deref() {
        let url = raw.trim();
        if !url.is_empty() {
            if !(url.starts_with("http://") || url.starts_with("https://"))
                || url.chars().any(|c| c.is_whitespace())
            {
                return Err("MinerU API 基址需为 http:// 或 https:// 开头的完整地址".into());
            }
            db.set_meta(KEY_MINERU_API_URL, url)
                .map_err(|e| format!("保存 API 基址失败: {}", e))?;
            next.mineru_api_url = url.to_string();
        }
    }

    let apply_folder = |raw: Option<&str>, key: &str, target: &mut PathBuf| -> Result<(), String> {
        let Some(raw) = raw else { return Ok(()) };
        if raw.trim().is_empty() {
            return Ok(());
        }
        let path = validate_folder(raw)?;
        ensure_writable(&path)?;
        db.set_meta(key, &path.to_string_lossy())
            .map_err(|e| format!("保存目录失败: {}", e))?;
        *target = path;
        Ok(())
    };
    apply_folder(req.image_folder.as_deref(), KEY_IMAGE_FOLDER, &mut next.image_folder)?;
    apply_folder(
        req.json_folder.as_deref(),
        KEY_JSON_FOLDER,
        &mut next.output_json_folder,
    )?;
    apply_folder(
        req.md_folder.as_deref(),
        KEY_MD_FOLDER,
        &mut next.output_md_folder,
    )?;
    apply_folder(
        req.search_index_folder.as_deref(),
        KEY_SEARCH_INDEX_FOLDER,
        &mut next.search_index_folder,
    )?;
    apply_folder(
        req.epub_folder.as_deref(),
        KEY_EPUB_FOLDER,
        &mut next.epub_folder,
    )?;

    if let Some(raw) = req.zh_t2s_mapping.as_deref() {
        // 映射允许为空（清除）；非空时先校验格式再落库，避免存入坏配置
        if let Err(message) = crate::zh_convert::parse_mapping(raw) {
            return Err(format!("繁→简映射格式有误：{}", message));
        }
        db.set_meta(KEY_ZH_T2S_MAPPING, raw)
            .map_err(|e| format!("保存繁→简映射失败: {}", e))?;
        next.zh_t2s_mapping = raw.to_string();
    }

    // 跨字段校验放在写库之前，避免留下半套配置
    let backoff_base = req
        .ocr_retry_backoff_base
        .unwrap_or(current.ocr_retry_backoff_base);
    let backoff_max = req
        .ocr_retry_backoff_max
        .unwrap_or(current.ocr_retry_backoff_max);
    if backoff_max < backoff_base {
        return Err("退避封顶不能小于首次退避".into());
    }

    if let Some(mode) = req.scheduler_sort_mode.as_deref() {
        let mode = mode.trim();
        if !mode.is_empty() {
            if mode != "shortest_first" && mode != "id_asc" {
                return Err("调度排序只支持 shortest_first 或 id_asc".into());
            }
            db.set_meta(KEY_SCHEDULER_SORT_MODE, mode)
                .map_err(|e| format!("保存调度排序失败: {}", e))?;
            next.scheduler_sort_mode = mode.to_string();
        }
    }

    apply_num(
        db,
        KEY_OCR_DAILY_UPLOAD_LIMIT,
        req.ocr_daily_upload_limit,
        1i64,
        20000,
        "每日上传上限",
        &mut next.ocr_daily_upload_limit,
    )?;
    apply_num(
        db,
        KEY_OCR_UPLOAD_INTERVAL,
        req.ocr_upload_interval,
        30u64,
        600,
        "批次上传间隔",
        &mut next.ocr_upload_interval,
    )?;
    apply_num(
        db,
        KEY_BATCH_DOC_LIMIT,
        req.batch_doc_limit,
        1usize,
        200,
        "单批聚合文档数",
        &mut next.batch_doc_limit,
    )?;
    apply_num(
        db,
        KEY_BATCH_CHUNK_LIMIT,
        req.batch_chunk_limit,
        1usize,
        50,
        "单批分块上限",
        &mut next.batch_chunk_limit,
    )?;
    apply_num(
        db,
        KEY_MAX_PAGES,
        req.max_pages,
        1u32,
        5000,
        "单文件页数上限",
        &mut next.max_pages,
    )?;
    apply_num(
        db,
        KEY_MAX_FILE_SIZE_MB,
        req.max_file_size_mb,
        1u64,
        2048,
        "单文件大小上限",
        &mut next.max_file_size_mb,
    )?;
    apply_num(
        db,
        KEY_MINERU_POLL_TIMEOUT,
        req.mineru_poll_timeout,
        60u64,
        86400,
        "批次轮询超时",
        &mut next.mineru_poll_timeout,
    )?;
    apply_num(
        db,
        KEY_MINERU_POLL_INTERVAL,
        req.mineru_poll_interval,
        2u64,
        120,
        "批次轮询间隔",
        &mut next.mineru_poll_interval,
    )?;
    apply_num(
        db,
        KEY_OCR_FETCH_MAX_RETRIES,
        req.ocr_fetch_max_retries,
        0u32,
        10,
        "状态查询重试次数",
        &mut next.ocr_fetch_max_retries,
    )?;
    apply_num(
        db,
        KEY_OCR_FETCH_TIMEOUT,
        req.ocr_fetch_timeout,
        5u64,
        300,
        "状态查询超时",
        &mut next.ocr_fetch_timeout,
    )?;
    apply_num(
        db,
        KEY_OCR_DOWNLOAD_MAX_RETRIES,
        req.ocr_download_max_retries,
        0u32,
        10,
        "ZIP 下载重试次数",
        &mut next.ocr_download_max_retries,
    )?;
    apply_num(
        db,
        KEY_OCR_DOWNLOAD_TIMEOUT,
        req.ocr_download_timeout,
        30u64,
        3600,
        "ZIP 下载超时",
        &mut next.ocr_download_timeout,
    )?;
    apply_num(
        db,
        KEY_OCR_DOWNLOAD_MAX_CYCLES,
        req.ocr_download_max_cycles,
        1u32,
        20,
        "取结果最大轮次",
        &mut next.ocr_download_max_cycles,
    )?;
    apply_num(
        db,
        KEY_OCR_RETRY_BACKOFF_BASE,
        req.ocr_retry_backoff_base,
        0.5f64,
        60.0,
        "首次退避",
        &mut next.ocr_retry_backoff_base,
    )?;
    apply_num(
        db,
        KEY_OCR_RETRY_BACKOFF_MAX,
        req.ocr_retry_backoff_max,
        1.0f64,
        600.0,
        "退避封顶",
        &mut next.ocr_retry_backoff_max,
    )?;
    apply_num(
        db,
        KEY_OCR_RETRY_JITTER_RATIO,
        req.ocr_retry_jitter_ratio,
        0.0f64,
        1.0,
        "退避抖动比例",
        &mut next.ocr_retry_jitter_ratio,
    )?;
    apply_num(
        db,
        KEY_RESUME_UPLOAD_URL_TTL,
        req.resume_upload_url_ttl,
        60u64,
        86400,
        "预签名 URL 有效期",
        &mut next.resume_upload_url_ttl,
    )?;
    apply_num(
        db,
        KEY_RESUME_MAX_UPLOAD_ATTEMPTS,
        req.resume_max_upload_attempts,
        1i64,
        20,
        "同批次最大续传次数",
        &mut next.resume_max_upload_attempts,
    )?;

    next.max_size_bytes = next.max_file_size_mb * 1024 * 1024;
    next.init_directories()
        .map_err(|e| format!("初始化目录失败: {}", e))?;
    Ok(next)
}

/// 面板展示用的可编辑值（目录与 API 基址一并返回）
fn values_of(cfg: &Config) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("mineru_api_url".into(), json!(cfg.mineru_api_url));
    m.insert(
        "image_folder".into(),
        json!(cfg.image_folder.to_string_lossy()),
    );
    m.insert(
        "json_folder".into(),
        json!(cfg.output_json_folder.to_string_lossy()),
    );
    m.insert(
        "md_folder".into(),
        json!(cfg.output_md_folder.to_string_lossy()),
    );
    m.insert(
        "search_index_folder".into(),
        json!(cfg.search_index_folder.to_string_lossy()),
    );
    m.insert(
        "epub_folder".into(),
        json!(cfg.epub_folder.to_string_lossy()),
    );
    m.insert(
        "ocr_daily_upload_limit".into(),
        json!(cfg.ocr_daily_upload_limit),
    );
    m.insert(
        "ocr_upload_interval".into(),
        json!(cfg.ocr_upload_interval),
    );
    m.insert("batch_doc_limit".into(), json!(cfg.batch_doc_limit));
    m.insert("batch_chunk_limit".into(), json!(cfg.batch_chunk_limit));
    m.insert("max_pages".into(), json!(cfg.max_pages));
    m.insert("max_file_size_mb".into(), json!(cfg.max_file_size_mb));
    m.insert(
        "scheduler_sort_mode".into(),
        json!(cfg.scheduler_sort_mode),
    );
    m.insert(
        "mineru_poll_timeout".into(),
        json!(cfg.mineru_poll_timeout),
    );
    m.insert(
        "mineru_poll_interval".into(),
        json!(cfg.mineru_poll_interval),
    );
    m.insert(
        "ocr_fetch_max_retries".into(),
        json!(cfg.ocr_fetch_max_retries),
    );
    m.insert("ocr_fetch_timeout".into(), json!(cfg.ocr_fetch_timeout));
    m.insert(
        "ocr_download_max_retries".into(),
        json!(cfg.ocr_download_max_retries),
    );
    m.insert(
        "ocr_download_timeout".into(),
        json!(cfg.ocr_download_timeout),
    );
    m.insert(
        "ocr_download_max_cycles".into(),
        json!(cfg.ocr_download_max_cycles),
    );
    m.insert(
        "ocr_retry_backoff_base".into(),
        json!(cfg.ocr_retry_backoff_base),
    );
    m.insert(
        "ocr_retry_backoff_max".into(),
        json!(cfg.ocr_retry_backoff_max),
    );
    m.insert(
        "ocr_retry_jitter_ratio".into(),
        json!(cfg.ocr_retry_jitter_ratio),
    );
    m.insert(
        "resume_upload_url_ttl".into(),
        json!(cfg.resume_upload_url_ttl),
    );
    m.insert(
        "resume_max_upload_attempts".into(),
        json!(cfg.resume_max_upload_attempts),
    );
    m
}

/// 组装读取接口响应（token 脱敏）
pub fn describe(cfg: &Config) -> SettingsResponse {
    let env_defaults = Config::load();
    let has_token = !cfg.mineru_token.trim().is_empty();
    SettingsResponse {
        mineru_token_masked: if has_token {
            Some(mask_token(&cfg.mineru_token))
        } else {
            None
        },
        has_mineru_token: has_token,
        image_folder: cfg.image_folder.to_string_lossy().to_string(),
        json_folder: cfg.output_json_folder.to_string_lossy().to_string(),
        md_folder: cfg.output_md_folder.to_string_lossy().to_string(),
        search_index_folder: cfg.search_index_folder.to_string_lossy().to_string(),
        epub_folder: cfg.epub_folder.to_string_lossy().to_string(),
        zh_t2s_mapping: cfg.zh_t2s_mapping.clone(),
        values: values_of(cfg),
        defaults: values_of(&env_defaults),
        image_dir: dir_stat(&cfg.image_folder),
        json_dir: dir_stat(&cfg.output_json_folder),
        md_dir: dir_stat(&cfg.output_md_folder),
        search_index_dir: dir_stat(&cfg.search_index_folder),
        epub_dir: dir_stat(&cfg.epub_folder),
        restart_required: false,
    }
}
