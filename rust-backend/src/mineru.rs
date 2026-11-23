use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use md5::{Digest, Md5};
use serde_json::{json, Value};

use crate::config::Config;
use crate::pdf_utils::is_temp_path;

/// 图片来源后缀白名单
const IMAGE_EXTS: [&str; 7] = ["jpg", "jpeg", "png", "gif", "webp", "bmp", "svg"];

/// 图片文件名安全校验：内容哈希名（十六进制/字母数字），杜绝路径穿越
pub fn is_safe_image_name(name: &str) -> bool {
    if name.len() < 8 || name.len() > 120 {
        return false;
    }
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    if !IMAGE_EXTS.contains(&ext.to_ascii_lowercase().as_str()) {
        return false;
    }
    stem.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn image_ext_of(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    if IMAGE_EXTS.contains(&ext.as_str()) {
        Some(ext)
    } else {
        None
    }
}

/// 把解压目录中的所有图片保存到统一图片目录。
/// 命名优先沿用 MinerU 的内容哈希文件名，非法名回退为内容 md5；已存在则跳过（不覆盖、自动去重）。
pub fn save_images(extract_dir: &Path, cfg: &Config) -> Result<usize> {
    let mut files = Vec::new();
    collect_files(extract_dir, &mut files)?;
    std::fs::create_dir_all(&cfg.image_folder)?;

    let mut saved = 0usize;
    for src in files {
        let Some(ext) = image_ext_of(&src) else { continue };
        let basename = src
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let target_name = if is_safe_image_name(&basename) {
            basename
        } else {
            let mut buf = Vec::new();
            File::open(&src)?.read_to_end(&mut buf)?;
            let digest = Md5::digest(&buf);
            format!("{:x}.{}", digest, ext)
        };
        let dest = cfg.image_folder.join(&target_name);
        if dest.exists() {
            continue;
        }
        std::fs::copy(&src, &dest)?;
        saved += 1;
    }
    Ok(saved)
}

#[derive(Debug, Clone)]
pub struct MineruResult {
    pub data_id: String,
    pub filename: String,
    pub status: String,
    pub json_path: Option<String>,
    pub error_message: Option<String>,
    pub retryable: bool,
    pub attempts: u32,
    pub exhausted: bool,
}

/// 取结果失败（批次状态查询 / ZIP 下载 / 解压）
#[derive(Debug, Clone)]
pub struct FetchError {
    pub message: String,
    pub retryable: bool,
    pub attempts: u32,
    pub exhausted: bool,
}

impl FetchError {
    fn new(message: String, retryable: bool) -> Self {
        Self { message, retryable, attempts: 1, exhausted: false }
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for FetchError {}

#[derive(Debug, Clone)]
pub struct PollInfo {
    pub finished: bool,
    pub items: Vec<Value>,
    pub message: Option<String>,
    pub error: Option<String>,
    pub retryable: bool,
    pub attempts: u32,
    pub exhausted: bool,
    pub in_progress: bool,
}

/// 408/429 与 5xx 视为临时故障；其余 4xx 是确定性错误
fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error()
}

/// 连接重置 / 超时 / DNS / 代理 / SSL / 分块传输中断等网络异常均可重试
fn is_retryable_req_err(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect() || e.is_request() || e.is_body() || e.is_decode() || e.is_redirect()
}

/// 第 attempt 次重试前的退避：指数递增 + ±抖动，封顶
fn retry_delay(cfg: &Config, attempt: u32) -> Duration {
    let base = cfg.ocr_retry_backoff_base.max(0.0);
    let step = attempt.saturating_sub(1).min(16);
    let mut delay = (base * 2f64.powi(step as i32)).min(cfg.ocr_retry_backoff_max.max(0.0));
    let ratio = cfg.ocr_retry_jitter_ratio.max(0.0);
    if ratio > 0.0 && delay > 0.0 {
        // 用纳秒时间做轻量抖动，避免引入额外随机数依赖
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let unit = (nanos as f64 / u32::MAX as f64) * 2.0 - 1.0;
        delay *= 1.0 + unit * ratio;
    }
    Duration::from_secs_f64(delay.max(0.0))
}


#[derive(Debug, Clone)]
pub struct PlannedUpload {
    pub chunk_id: i64,
    pub doc_id: i64,
    pub filename: String,
    pub path: String,
    pub data_id: String,
    pub upload_url: String,
}

pub struct MineruClient {
    token: String,
    base_url: String,
    http: reqwest::Client,
}

impl MineruClient {
    pub fn new(cfg: &Config) -> Self {
        Self {
            token: cfg.mineru_token.clone(),
            base_url: cfg.mineru_api_url.clone(),
            http: reqwest::Client::new(),
        }
    }

    fn headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", self.token)) {
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
        headers
    }

    pub async fn request_upload_urls(
        &self,
        files: &[(String, String)],
    ) -> Result<(String, Vec<String>)> {
        let url = format!("{}/file-urls/batch", self.base_url);
        let body = json!({
            "files": files.iter().map(|(name, data_id)| json!({"name": name, "data_id": data_id})).collect::<Vec<_>>(),
            "model_version": "vlm",
            "enable_formula": true,
            "enable_table": true
        });
        let resp = self
            .http
            .post(&url)
            .headers(self.headers())
            .json(&body)
            .timeout(Duration::from_secs(30))
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if status.is_success() {
            let result: Value = serde_json::from_str(&text)?;
            if result.get("code").and_then(|v| v.as_i64()) == Some(0) {
                let batch_id = result["data"]["batch_id"].as_str().unwrap_or_default().to_string();
                let urls = result["data"]["file_urls"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                return Ok((batch_id, urls));
            }
        }
        Err(anyhow!(
            "请求上传URL失败: HTTP {} {}",
            status,
            text.chars().take(300).collect::<String>()
        ))
    }

    pub async fn upload_file(&self, file_path: &Path, upload_url: &str) -> Result<()> {
        let data = tokio::fs::read(file_path).await?;
        let resp = self
            .http
            .put(upload_url)
            .body(data)
            .timeout(Duration::from_secs(300))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "上传文件失败: HTTP {} {}",
                status,
                text.chars().take(300).collect::<String>()
            ));
        }
        Ok(())
    }

    /// 带重试的批次状态查询（网络异常 / 408/429/5xx / 响应解析失败可重试）
    pub async fn poll_batch_status(&self, batch_id: &str, cfg: &Config) -> PollInfo {
        let url = format!("{}/extract-results/batch/{}", self.base_url, batch_id);
        let max_attempts = cfg.ocr_fetch_max_retries.max(1) + 1;
        let mut attempt = 0u32;
        let mut last_error = String::new();
        let mut last_retryable = true;
        let mut exhausted = false;

        for attempt_i in 1..=max_attempts {
            attempt = attempt_i;
            match self
                .http
                .get(&url)
                .headers(self.headers())
                .timeout(Duration::from_secs(cfg.ocr_fetch_timeout.max(1)))
                .send()
                .await
            {
                Err(e) => {
                    last_error = format!("查询网络异常: {}", e);
                    last_retryable = is_retryable_req_err(&e);
                }
                Ok(resp) => {
                    let status = resp.status();
                    if !status.is_success() {
                        let text = resp.text().await.unwrap_or_default();
                        last_error = format!(
                            "查询批次状态失败: HTTP {} {}",
                            status,
                            text.chars().take(200).collect::<String>()
                        );
                        last_retryable = is_retryable_status(status);
                    } else {
                        match resp.text().await {
                            Err(e) => {
                                last_error = format!("读取响应失败: {}", e);
                                last_retryable = is_retryable_req_err(&e);
                            }
                            Ok(text) => match serde_json::from_str::<Value>(&text) {
                                Err(e) => {
                                    last_error = format!("查询响应解析失败: {}", e);
                                    last_retryable = true;
                                }
                                Ok(parsed) => {
                                    if parsed.get("code").and_then(|v| v.as_i64()) == Some(0) {
                                        let items = parsed["data"]["extract_result"]
                                            .as_array()
                                            .cloned()
                                            .unwrap_or_default();
                                        let states: Vec<String> = items
                                            .iter()
                                            .filter_map(|i| {
                                                i.get("state")
                                                    .and_then(|v| v.as_str())
                                                    .map(|s| s.to_string())
                                            })
                                            .collect();
                                        let finished = !items.is_empty()
                                            && states
                                                .iter()
                                                .all(|s| s == "done" || s == "failed");
                                        if finished {
                                            return PollInfo {
                                                finished: true,
                                                items,
                                                message: None,
                                                error: None,
                                                retryable: false,
                                                attempts: attempt,
                                                exhausted: false,
                                                in_progress: false,
                                            };
                                        }
                                        return PollInfo {
                                            finished: false,
                                            items,
                                            message: Some(format!("进行中 (状态: {:?})", states)),
                                            error: None,
                                            retryable: false,
                                            attempts: attempt,
                                            exhausted: false,
                                            in_progress: true,
                                        };
                                    }
                                    last_error = format!(
                                        "API返回错误: code={} msg={}",
                                        parsed
                                            .get("code")
                                            .map(|v| v.to_string())
                                            .unwrap_or_default(),
                                        parsed
                                            .get("msg")
                                            .map(|v| v.to_string())
                                            .unwrap_or_default()
                                    );
                                    last_retryable = false;
                                }
                            },
                        }
                    }
                }
            }

            if !last_retryable {
                break;
            }
            if attempt >= max_attempts {
                exhausted = true;
                break;
            }
            let delay = retry_delay(cfg, attempt);
            crate::log_info!(
                "  [MinerU] 批次状态查询失败（第{}次），{:.1}秒后重试: {}",
                attempt,
                delay.as_secs_f64(),
                last_error
            );
            tokio::time::sleep(delay).await;
        }

        PollInfo {
            finished: false,
            items: Vec::new(),
            message: Some(last_error.clone()),
            error: Some(last_error),
            retryable: last_retryable,
            attempts: attempt,
            exhausted,
            in_progress: false,
        }
    }

    /// 单次非阻塞查询批次状态（保留三返回值签名，内部走带重试的 poll）
    pub async fn check_batch_status(
        &self,
        batch_id: &str,
        cfg: &Config,
    ) -> (bool, Vec<Value>, Option<String>) {
        let info = self.poll_batch_status(batch_id, cfg).await;
        if info.finished {
            (true, info.items, None)
        } else {
            (false, info.items, info.message)
        }
    }

    /// 按上传计划逐文件上传（续传也复用同一入口）；每个文件成功后立即回调检查点并删除本地临时分块。
    /// 单文件失败不影响其他文件，返回 (索引, 错误信息) 列表。
    pub async fn upload_planned_files(
        &self,
        items: &[PlannedUpload],
        cfg: &Config,
        on_uploaded: &dyn Fn(usize, &PlannedUpload),
    ) -> Vec<(usize, String)> {
        let mut errors = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let path = Path::new(&item.path);
            match self.upload_file(path, &item.upload_url).await {
                Ok(_) => {
                    on_uploaded(i, item);
                    if is_temp_path(path, cfg) {
                        let _ = tokio::fs::remove_file(path).await;
                    }
                }
                Err(e) => {
                    crate::log_info!("  [MinerU] 上传失败 {}: {}", item.filename, e);
                    errors.push((i, format!("{}: {}", item.filename, e)));
                }
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        errors
    }

    /// 下载并解压批次产物（单文件失败隔离）
    pub async fn extract_batch_results(
        &self,
        items: &[Value],
        name_to_data_id: &HashMap<String, String>,
        cfg: &Config,
    ) -> Vec<MineruResult> {
        let mut results = Vec::new();
        for item in items {
            let name = item
                .get("file_name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let data_id = item
                .get("data_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| name_to_data_id.get(&name).cloned())
                .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
            let state = item.get("state").and_then(|v| v.as_str()).unwrap_or_default();
            if state == "failed" {
                let err = item
                    .get("err_msg")
                    .and_then(|v| v.as_str())
                    .or_else(|| item.get("err_code").and_then(|v| v.as_str()))
                    .unwrap_or("未知解析错误")
                    .to_string();
                results.push(MineruResult {
                    data_id,
                    filename: name,
                    status: "failed".into(),
                    json_path: None,
                    error_message: Some(err),
                    retryable: false,
                    attempts: 0,
                    exhausted: false,
                });
                continue;
            }
            let Some(zip_url) = item.get("full_zip_url").and_then(|v| v.as_str()) else {
                results.push(MineruResult {
                    data_id,
                    filename: name,
                    status: "failed".into(),
                    json_path: None,
                    error_message: Some(format!("结果中缺少 full_zip_url: {}", item)),
                    retryable: false,
                    attempts: 0,
                    exhausted: false,
                });
                continue;
            };
            match self.download_and_extract(zip_url, &data_id, cfg).await {
                Ok(json_path) => results.push(MineruResult {
                    data_id,
                    filename: name,
                    status: "completed".into(),
                    json_path: Some(json_path),
                    error_message: None,
                    retryable: false,
                    attempts: 0,
                    exhausted: false,
                }),
                Err(e) => results.push(MineruResult {
                    data_id,
                    filename: name,
                    status: "failed".into(),
                    json_path: None,
                    error_message: Some(format!("下载解压产物失败: {}", e)),
                    retryable: e.retryable,
                    attempts: e.attempts,
                    exhausted: e.exhausted,
                }),
            }
        }
        results
    }

    pub async fn download_and_extract(
        &self,
        zip_url: &str,
        data_id: &str,
        cfg: &Config,
    ) -> std::result::Result<String, FetchError> {
        let max_attempts = cfg.ocr_download_max_retries.max(1) + 1;
        let mut last_error: Option<FetchError> = None;
        for attempt in 1..=max_attempts {
            match self.download_and_extract_once(zip_url, data_id, cfg).await {
                Ok(path) => return Ok(path),
                Err(mut e) => {
                    if e.retryable && attempt < max_attempts {
                        let delay = retry_delay(cfg, attempt);
                        crate::log_info!(
                            "  [MinerU] 下载结果失败（第{}次），{:.1}秒后重试: {}",
                            attempt,
                            delay.as_secs_f64(),
                            e.message
                        );
                        e.attempts = attempt;
                        last_error = Some(e);
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    e.attempts = attempt;
                    e.exhausted = e.retryable;
                    return Err(e);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| FetchError::new("下载解压产物失败".into(), false)))
    }

    async fn download_and_extract_once(
        &self,
        zip_url: &str,
        data_id: &str,
        cfg: &Config,
    ) -> std::result::Result<String, FetchError> {
        let resp = self
            .http
            .get(zip_url)
            .timeout(Duration::from_secs(cfg.ocr_download_timeout.max(1)))
            .send()
            .await
            .map_err(|e| {
                FetchError::new(
                    format!("下载结果ZIP网络异常: {}", e),
                    is_retryable_req_err(&e),
                )
            })?;
        if !resp.status().is_success() {
            let status = resp.status();
            return Err(FetchError::new(
                format!("下载结果ZIP失败: HTTP {}", status),
                is_retryable_status(status),
            ));
        }
        let bytes = resp.bytes().await.map_err(|e| {
            FetchError::new(
                format!("读取结果ZIP失败: {}", e),
                is_retryable_req_err(&e),
            )
        })?;
        let data_id = data_id.to_string();
        let cfg = cfg.clone();
        let outcome = tokio::task::spawn_blocking(move || -> std::result::Result<String, FetchError> {
            let extract_dir = cfg.temp_folder.join(format!("mineru_{}", data_id));
            if extract_dir.exists() {
                let _ = std::fs::remove_dir_all(&extract_dir);
            }
            if let Err(e) = std::fs::create_dir_all(&extract_dir) {
                return Err(FetchError::new(format!("创建解压目录失败: {}", e), false));
            }

            let result = (|| -> std::result::Result<String, FetchError> {
                let reader = std::io::Cursor::new(bytes);
                let mut archive = zip::ZipArchive::new(reader).map_err(|e| {
                    // 下载不完整 / 连接被重置常表现为 ZIP 损坏，可重试
                    FetchError::new(format!("结果ZIP损坏（可能下载不完整）: {}", e), true)
                })?;
                archive
                    .extract(&extract_dir)
                    .map_err(|e| FetchError::new(format!("解压结果ZIP失败: {}", e), false))?;

                // 优先找 *_middle.json，兜底找包含 pdf_info 的 json
                let mut middle_files: Vec<PathBuf> = Vec::new();
                if let Err(e) = collect_files(&extract_dir, &mut middle_files) {
                    return Err(FetchError::new(format!("遍历解压目录失败: {}", e), false));
                }
                let mut target = middle_files
                    .iter()
                    .find(|p| {
                        p.file_name()
                            .map(|s| s.to_string_lossy().ends_with("_middle.json"))
                            .unwrap_or(false)
                    })
                    .cloned();
                if target.is_none() {
                    for jp in middle_files
                        .iter()
                        .filter(|p| p.extension().map(|e| e == "json").unwrap_or(false))
                    {
                        if let Ok(mut f) = File::open(jp) {
                            let mut raw = Vec::new();
                            if f.read_to_end(&mut raw).is_ok() {
                                if let Ok(v) = serde_json::from_slice::<Value>(&raw) {
                                    if v.get("pdf_info").is_some() {
                                        target = Some(jp.clone());
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                let target = target
                    .ok_or_else(|| FetchError::new("结果ZIP中未找到 middle.json".into(), false))?;

                // 图片与 JSON 同级落盘：图片先进统一目录，再压缩 JSON，最后删临时解压目录
                let saved_images = save_images(&extract_dir, &cfg)
                    .map_err(|e| FetchError::new(format!("保存图片失败: {}", e), false))?;
                if saved_images > 0 {
                    crate::log_info!("  [MinerU] 已保存 {} 张新图片到 {}", saved_images, cfg.image_folder.display());
                }

                // 先写临时文件再 rename，避免失败时覆盖已有产物
                let json_path = cfg.output_json_folder.join(format!("{}.json.gz", data_id));
                let tmp_path = cfg
                    .output_json_folder
                    .join(format!("{}.json.gz.tmp", data_id));
                if let Err(e) = std::fs::create_dir_all(&cfg.output_json_folder) {
                    return Err(FetchError::new(format!("创建产物目录失败: {}", e), false));
                }
                let write_result = (|| -> std::io::Result<()> {
                    let mut fin = File::open(&target)?;
                    let mut fout =
                        GzEncoder::new(File::create(&tmp_path)?, Compression::new(6));
                    std::io::copy(&mut fin, &mut fout)?;
                    fout.finish()?;
                    Ok(())
                })();
                if let Err(e) = write_result {
                    let _ = std::fs::remove_file(&tmp_path);
                    return Err(FetchError::new(format!("写入JSON产物失败: {}", e), false));
                }
                if let Err(e) = std::fs::rename(&tmp_path, &json_path) {
                    let _ = std::fs::remove_file(&tmp_path);
                    return Err(FetchError::new(format!("落盘JSON产物失败: {}", e), false));
                }
                Ok(json_path.to_string_lossy().to_string())
            })();

            let _ = std::fs::remove_dir_all(&extract_dir);
            result
        })
        .await
        .map_err(|e| FetchError::new(format!("解压任务异常: {}", e), false))??;
        Ok(outcome)
    }
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}
