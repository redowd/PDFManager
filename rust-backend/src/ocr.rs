use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use chrono::{Local, NaiveDateTime};
use serde::Serialize;
use tokio::task::JoinHandle;

use crate::config::{resolve_path, Config};
use crate::db::{now_string, Batch, Chunk, Document, UploadPlanItem};
use crate::json_to_md::{convert_json_to_markdown, merge_json_to_markdown};
use crate::mineru::{MineruClient, PlannedUpload};
use crate::pdf_utils::{
    calculate_split_pages, get_pdf_info, is_temp_path, split_pdf, split_pdf_ranges, ChunkInfo,
};
use crate::state::AppState;

const SCHEDULER_META_KEY: &str = "ocr_auto_scheduler_enabled";

// 分块云端状态常量
const TERMINAL_UPLOAD_STATES: [&str; 4] = ["uploaded", "downloading", "done", "failed"];

pub struct Scheduler {
    pub stop: Arc<AtomicBool>,
    pub submitter: Mutex<Option<JoinHandle<()>>>,
    pub harvester: Mutex<Option<JoinHandle<()>>>,
    /// 批次创建/续传串行化：保证同一时刻只有一个流程在"声明文件 + 上传"。
    /// 采用 tokio Mutex，调用方持锁后禁止再次加锁（非重入）。
    pub upload_lock: tokio::sync::Mutex<()>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            submitter: Mutex::new(None),
            harvester: Mutex::new(None),
            upload_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn is_running(&self) -> bool {
        let sub = self
            .submitter
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|h| !h.is_finished()))
            .unwrap_or(false);
        let harv = self
            .harvester
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|h| !h.is_finished()))
            .unwrap_or(false);
        sub || harv
    }
}

#[derive(Debug, Serialize)]
pub struct SchedulerStatus {
    pub running: bool,
    pub enabled: bool,
    pub completed_count: i64,
    pub pending_count: i64,
    pub processing_count: i64,
    pub active_workers: i64,
    pub active_batches: usize,
    pub uploading_batches: usize,
    pub today_uploaded_files: i64,
    pub daily_limit: i64,
    pub mode: &'static str,
}

#[derive(Debug, Serialize)]
pub struct RecoverySummary {
    pub reset_downloading: usize,
    pub expired_upload_batches: usize,
    pub merge_ready: usize,
    pub merged: usize,
    pub requeued: usize,
}

#[derive(Debug, Serialize)]
pub struct ManualRecoverySummary {
    pub reset_downloading: usize,
    pub expired_upload_batches: usize,
    pub merge_ready: usize,
    pub merged: usize,
    pub requeued: usize,
    pub resumed_batches: usize,
    pub harvested_batches: usize,
}

pub fn get_scheduler_status(state: &AppState) -> SchedulerStatus {
    let cfg = state.cfg();
    let enabled = state
        .db
        .get_meta(SCHEDULER_META_KEY, Some("0"))
        .ok()
        .flatten()
        .map(|v| v == "1")
        .unwrap_or(false);
    let active_batches = state.db.get_active_ocr_batches().unwrap_or_default();
    let processing = state.db.count_processing().unwrap_or(0);
    SchedulerStatus {
        running: state.scheduler.is_running(),
        enabled,
        completed_count: state.db.count_completed_ocr().unwrap_or(0),
        pending_count: state.db.count_pending_ocr().unwrap_or(0),
        processing_count: processing,
        active_workers: processing,
        active_batches: active_batches.len(),
        uploading_batches: active_batches
            .iter()
            .filter(|b| b.status == "uploading")
            .count(),
        today_uploaded_files: state.db.get_today_uploaded_chunk_count().unwrap_or(0),
        daily_limit: cfg.ocr_daily_upload_limit.max(1),
        mode: "producer_consumer_pipeline",
    }
}

pub fn start_scheduler(state: AppState) {
    let _ = state.db.set_meta(SCHEDULER_META_KEY, "1");
    state.scheduler.stop.store(false, Ordering::SeqCst);

    let mut sub = state.scheduler.submitter.lock().unwrap();
    let need_sub = sub.as_ref().map(|h| h.is_finished()).unwrap_or(true);
    if need_sub {
        let st = state.clone();
        *sub = Some(tokio::spawn(async move { submitter_loop(st).await }));
        crate::log_info!("[流水线] 投递生产者线程 (Submitter) 已启动");
    }
    drop(sub);

    let mut harv = state.scheduler.harvester.lock().unwrap();
    let need_harv = harv.as_ref().map(|h| h.is_finished()).unwrap_or(true);
    if need_harv {
        let st = state.clone();
        *harv = Some(tokio::spawn(async move { harvester_loop(st).await }));
        crate::log_info!("[流水线] 收割消费者线程 (Harvester) 已启动");
    }
}

pub fn stop_scheduler(state: &AppState) {
    let _ = state.db.set_meta(SCHEDULER_META_KEY, "0");
    state.scheduler.stop.store(true, Ordering::SeqCst);
    crate::log_info!("[流水线] 已发送停止信号，当前批次将继续处理并收割");
}

pub fn init_scheduler_from_meta(state: AppState) {
    let enabled = state
        .db
        .get_meta(SCHEDULER_META_KEY, Some("0"))
        .ok()
        .flatten()
        .map(|v| v == "1")
        .unwrap_or(false);
    if enabled {
        crate::log_info!("[流水线] 检测到上次关机前流水线处于开启状态，自动恢复运行");
        start_scheduler(state);
    }
}

async fn sleep_checked(stop: &Arc<AtomicBool>, secs: u64) -> bool {
    let mut remaining = secs;
    while remaining > 0 {
        if stop.load(Ordering::SeqCst) {
            return true;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        remaining -= 1;
    }
    stop.load(Ordering::SeqCst)
}

// -------------------------------------------------------------
// 分块级工具：状态判定、临时文件、复用切分
// -------------------------------------------------------------

fn abs_path(p: &str) -> PathBuf {
    resolve_path(p)
}

fn json_file_exists(path: Option<&str>) -> bool {
    let Some(p) = path else { return false };
    if p.is_empty() {
        return false;
    }
    if abs_path(p).exists() {
        return true;
    }
    abs_path(&format!("{}.gz", p)).exists()
}

fn chunk_json_ready(chunk: &Chunk) -> bool {
    json_file_exists(chunk.json_path.as_deref())
}

fn chunk_settled(chunk: &Chunk) -> bool {
    if chunk_json_ready(chunk) {
        return true;
    }
    match chunk.remote_state.as_deref() {
        Some("done") | Some("failed") => true,
        None => chunk.ocr_status == "completed" || chunk.ocr_status == "failed",
        _ => false,
    }
}

fn temp_path(p: &str, cfg: &Config) -> bool {
    if p.is_empty() {
        return false;
    }
    is_temp_path(&abs_path(p), cfg)
}

fn remove_temp_file(path: &str, cfg: &Config) {
    if temp_path(path, cfg) {
        let _ = std::fs::remove_file(abs_path(path));
    }
}

fn remove_temp_paths(paths: &[String], cfg: &Config) {
    for p in paths {
        remove_temp_file(p, cfg);
    }
}

fn truncate500(s: &str) -> String {
    s.chars().take(500).collect()
}

fn parse_datetime(raw: &str) -> Option<NaiveDateTime> {
    for fmt in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(raw, fmt) {
            return Some(dt);
        }
    }
    None
}

fn batch_age_seconds(batch: &Batch) -> i64 {
    let ref_raw = batch
        .submitted_at
        .as_deref()
        .or(batch.created_at.as_deref());
    let Some(raw) = ref_raw else { return 0 };
    let Some(dt) = parse_datetime(raw) else { return 0 };
    (Local::now().naive_local() - dt).num_seconds()
}

fn pending_upload_items(
    state: &AppState,
    cfg: &Config,
    doc_ids: &[i64],
) -> Result<Vec<UploadPlanItem>> {
    let mut items = Vec::new();
    for doc_id in doc_ids {
        for chunk in state.db.get_chunks_by_document(*doc_id)? {
            if chunk_json_ready(&chunk) {
                continue; // 已有可用结果：直接复用，不再上传
            }
            if let Some(state_str) = chunk.remote_state.as_deref() {
                if matches!(state_str, "uploading" | "uploaded" | "downloading" | "done" | "failed")
                {
                    continue;
                }
            }
            if chunk.remote_state.is_none()
                && (chunk.ocr_status == "completed" || chunk.ocr_status == "failed")
            {
                continue;
            }
            if chunk.chunk_path.is_empty() || !abs_path(&chunk.chunk_path).exists() {
                if temp_path(&chunk.chunk_path, cfg) {
                    continue; // 文件已被清理：交由上层重新切分
                }
                return Err(anyhow!("分块文件不存在：{}", chunk.filename));
            }
            items.push(UploadPlanItem {
                chunk_id: chunk.id,
                doc_id: *doc_id,
                filename: chunk.filename.clone(),
                path: chunk.chunk_path.clone(),
                start_page: chunk.start_page,
                end_page: chunk.end_page,
                data_id: chunk
                    .data_id
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string()),
                upload_url: None,
            });
        }
    }
    Ok(items)
}

fn reset_failed_chunks_for_retry(state: &AppState, doc_id: i64) -> Result<()> {
    for chunk in state.db.get_chunks_by_document(doc_id)? {
        if chunk.remote_state.as_deref() == Some("failed")
            || (chunk.remote_state.is_none() && chunk.ocr_status == "failed")
        {
            if chunk_json_ready(&chunk) {
                state.db.update_chunk_remote_state(
                    chunk.id,
                    "done",
                    chunk.json_path.as_deref(),
                    None,
                )?;
            } else {
                state.db.reset_chunk_for_retry(chunk.id)?;
            }
        }
    }
    Ok(())
}

/// 从历史分块文件名解析页范围：{basename}_{doc_id}_{start}_{end}.pdf
fn range_from_filename(filename: &str) -> Option<(u32, u32)> {
    if filename.is_empty() {
        return None;
    }
    let mut base = filename;
    if let Some(dot) = base.rfind('.') {
        let ext = &base[dot + 1..];
        if !ext.is_empty()
            && ext
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            base = &base[..dot];
        }
    }
    let idx_end = base.rfind('_')?;
    let end_raw = &base[idx_end + 1..];
    if end_raw.is_empty() || !end_raw.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let idx_start = base[..idx_end].rfind('_')?;
    let start_raw = &base[idx_start + 1..idx_end];
    if start_raw.is_empty() || !start_raw.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let start: u32 = start_raw.parse().ok()?;
    let end: u32 = end_raw.parse().ok()?;
    if start >= 1 && end >= start {
        Some((start, end))
    } else {
        None
    }
}

/// 按 split_pdf 的确定性切分规则，计算整篇文档的预期页范围（1-based 闭区间）
fn expected_split_ranges(doc: &Document, cfg: &Config) -> Vec<(u32, u32)> {
    let page_count = doc.page_count.unwrap_or(0);
    if page_count <= 0 {
        return Vec::new();
    }
    let mut file_size_bytes = doc.file_size.unwrap_or(0).max(0) as u64;
    if file_size_bytes == 0 && !doc.original_path.is_empty() {
        if let Ok(meta) = std::fs::metadata(abs_path(&doc.original_path)) {
            file_size_bytes = meta.len();
        }
    }
    let page_count = page_count as u32;
    let split_pages = if file_size_bytes > 0 {
        calculate_split_pages(page_count, file_size_bytes, cfg)
    } else {
        None
    };
    let Some(split_pages) = split_pages else {
        return vec![(1, page_count)];
    };
    if split_pages == 0 {
        return vec![(1, page_count)];
    }
    let mut ranges = Vec::new();
    let total_chunks = (page_count + split_pages - 1) / split_pages;
    for i in 0..total_chunks {
        let start = i * split_pages + 1;
        let end = (start + split_pages - 1).min(page_count);
        ranges.push((start, end));
    }
    ranges
}

/// 确保每个历史分块都有可用页范围：优先记录自带 start/end，其次解析文件名，
/// 最后按确定性切分计划按顺序补齐；无法确定时明确报错，绝不静默整篇重跑。
fn resolve_chunk_ranges(
    state: &AppState,
    cfg: &Config,
    doc: &Document,
    chunks: &[Chunk],
) -> Result<HashMap<i64, (u32, u32)>> {
    let mut ranges: HashMap<i64, (u32, u32)> = HashMap::new();
    let mut unknown: Vec<&Chunk> = Vec::new();
    for c in chunks {
        if let (Some(s), Some(e)) = (c.start_page, c.end_page) {
            if s > 0 && e >= s {
                ranges.insert(c.id, (s as u32, e as u32));
                continue;
            }
        }
        let parsed =
            range_from_filename(&c.filename).or_else(|| range_from_filename(&c.chunk_path));
        if let Some((s, e)) = parsed {
            ranges.insert(c.id, (s, e));
            state.db.update_chunk_range(c.id, s as i64, e as i64)?;
        } else {
            unknown.push(c);
        }
    }

    if !unknown.is_empty() {
        let expected = expected_split_ranges(doc, cfg);
        let mut used: HashSet<(u32, u32)> = ranges.values().cloned().collect();
        unknown.sort_by_key(|c| c.id);
        for c in unknown {
            let mut assigned: Option<(u32, u32)> = None;
            for r in &expected {
                if !used.contains(r) {
                    assigned = Some(*r);
                    break;
                }
            }
            match assigned {
                Some((s, e)) => {
                    ranges.insert(c.id, (s, e));
                    used.insert((s, e));
                    state.db.update_chunk_range(c.id, s as i64, e as i64)?;
                }
                None => {
                    return Err(anyhow!(
                        "历史分块缺少页范围信息，且无法按确定性切分规则安全映射；\
                         已保留全部分块记录，请人工确认后再重跑"
                    ))
                }
            }
        }
    }
    Ok(ranges)
}

/// 找出没有被任何分块覆盖的页范围（历史记录整块丢失时用于补建记录）
fn uncovered_ranges(page_count: u32, ranges: &HashMap<i64, (u32, u32)>) -> Vec<(u32, u32)> {
    if page_count == 0 {
        return Vec::new();
    }
    let mut intervals: Vec<(u32, u32)> = ranges
        .values()
        .filter(|(s, e)| s <= e)
        .cloned()
        .collect();
    intervals.sort();
    let mut gaps = Vec::new();
    let mut cursor: u32 = 1;
    for (start, end) in intervals {
        if start > cursor {
            gaps.push((cursor, (start - 1).min(page_count)));
        }
        cursor = cursor.max(end + 1);
        if cursor > page_count {
            break;
        }
    }
    if cursor <= page_count {
        gaps.push((cursor, page_count));
    }
    gaps.into_iter().filter(|(s, e)| s <= e).collect()
}

fn assert_source_pdf_usable(
    doc: &Document,
    ranges: &HashMap<i64, (u32, u32)>,
) -> Result<()> {
    let path = abs_path(&doc.original_path);
    if doc.original_path.is_empty() || !path.exists() {
        return Err(anyhow!(
            "源PDF文件不存在，无法补切缺失分块；已完成分块结果已保留"
        ));
    }
    let (actual_pages, _size_mb, _size_bytes) = get_pdf_info(&path);
    if actual_pages == 0 {
        return Err(anyhow!(
            "无法读取源PDF页数（文件可能损坏）；已完成分块结果已保留"
        ));
    }
    if let Some(pc) = doc.page_count {
        if pc > 0 && actual_pages as i64 != pc {
            return Err(anyhow!(
                "源PDF页数({})与历史记录({})不一致，无法安全续跑；已完成分块结果已保留",
                actual_pages,
                pc
            ));
        }
    }
    let max_end = ranges.values().map(|(_s, e)| *e).max().unwrap_or(0);
    if max_end > actual_pages {
        return Err(anyhow!(
            "历史分块页范围({})超出源PDF实际页数({})，无法安全续跑；已完成分块结果已保留",
            max_end,
            actual_pages
        ));
    }
    Ok(())
}

/// 确保分块记录与本地文件就绪，尽量复用既有结果
async fn ensure_chunks(state: &AppState, cfg: &Config, doc: &Document) -> Result<Vec<Chunk>> {
    let existing = state.db.get_chunks_by_document(doc.id)?;
    if existing.is_empty() {
        let path = abs_path(&doc.original_path);
        let doc_id = doc.id;
        let cfg2 = cfg.clone();
        let descriptors =
            tokio::task::spawn_blocking(move || split_pdf(&path, doc_id, &cfg2)).await?;
        if descriptors.is_empty() {
            return Err(anyhow!("PDF分割失败"));
        }
        for desc in &descriptors {
            state.db.add_chunk(
                doc.id,
                &desc.filename,
                &desc.path.to_string_lossy(),
                desc.start_page as i64,
                desc.end_page as i64,
            )?;
        }
        return state.db.get_chunks_by_document(doc.id);
    }

    let ranges = resolve_chunk_ranges(state, cfg, doc, &existing)?;

    let mut need_cut: Vec<Chunk> = Vec::new();
    for chunk in &existing {
        if chunk_settled(chunk)
            || matches!(
                chunk.remote_state.as_deref(),
                Some("uploading") | Some("uploaded") | Some("downloading")
            )
        {
            continue;
        }
        if !chunk.chunk_path.is_empty() && abs_path(&chunk.chunk_path).exists() {
            continue; // 本地文件仍在：直接复用，不重切
        }
        need_cut.push(chunk.clone());
    }

    let page_count = doc.page_count.unwrap_or(0).max(0) as u32;
    let gaps = uncovered_ranges(page_count, &ranges);
    if need_cut.is_empty() && gaps.is_empty() {
        return Ok(existing);
    }

    assert_source_pdf_usable(doc, &ranges)?;

    let mut cut_ranges: Vec<(u32, u32)> = Vec::new();
    for c in &need_cut {
        let r = ranges
            .get(&c.id)
            .ok_or_else(|| anyhow!("历史分块缺少页范围信息：{}", c.filename))?;
        cut_ranges.push(*r);
    }
    cut_ranges.extend(gaps.iter().cloned());

    let path = abs_path(&doc.original_path);
    let doc_id = doc.id;
    let cfg2 = cfg.clone();
    let descriptors = tokio::task::spawn_blocking(move || {
        split_pdf_ranges(&path, doc_id, &cut_ranges, &cfg2)
    })
    .await??;
    let by_range: HashMap<(u32, u32), ChunkInfo> = descriptors
        .into_iter()
        .map(|d| ((d.start_page, d.end_page), d))
        .collect();

    for chunk in &need_cut {
        let r = ranges
            .get(&chunk.id)
            .ok_or_else(|| anyhow!("历史分块缺少页范围信息：{}", chunk.filename))?;
        let desc = by_range.get(r).ok_or_else(|| {
            anyhow!("补切分块失败：页范围 {}-{} 未生成", r.0, r.1)
        })?;
        state.db.update_chunk_source(
            chunk.id,
            if chunk.filename.is_empty() {
                &desc.filename
            } else {
                &chunk.filename
            },
            &desc.path.to_string_lossy(),
            Some(desc.start_page as i64),
            Some(desc.end_page as i64),
        )?;
    }

    for gap in &gaps {
        let desc = by_range.get(gap).ok_or_else(|| {
            anyhow!("补切分块失败：页范围 {}-{} 未生成", gap.0, gap.1)
        })?;
        state.db.add_chunk(
            doc.id,
            &desc.filename,
            &desc.path.to_string_lossy(),
            desc.start_page as i64,
            desc.end_page as i64,
        )?;
    }

    state.db.get_chunks_by_document(doc.id)
}

// -------------------------------------------------------------
// 批次投递（计划先落库 → 逐文件上传检查点 → 即传即焚）
// -------------------------------------------------------------

/// 按已持久化的上传计划逐个上传，每个成功立即写检查点并删除本地临时副本。
/// 与 Python upload_planned_files 语义一致：单文件失败不中断整批。
async fn upload_items(
    state: &AppState,
    cfg: &Config,
    mineru: &MineruClient,
    batch_id: &str,
    items: &[PlannedUpload],
) -> Vec<(usize, String)> {
    let mut errors = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let outcome: Result<(), String> = async {
            if item.upload_url.is_empty() {
                return Err("缺少预签名上传URL".to_string());
            }
            let path = abs_path(&item.path);
            if item.path.is_empty() || !path.exists() {
                return Err(format!("本地分块文件不存在：{}", item.path));
            }
            mineru
                .upload_file(&path, &item.upload_url)
                .await
                .map_err(|e| e.to_string())?;
            // 即传即焚：位于 TEMP_FOLDER 的分块上传成功立刻删除
            if is_temp_path(&path, cfg) {
                let _ = std::fs::remove_file(&path);
            }
            state
                .db
                .mark_chunk_uploaded(item.chunk_id)
                .map_err(|e| e.to_string())?;
            state
                .db
                .increment_batch_counter(batch_id, "uploaded_count")
                .map_err(|e| e.to_string())?;
            Ok(())
        }
        .await;
        if let Err(e) = outcome {
            errors.push((index, e));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    errors
}

/// 创建批次并上传分块：
/// 1) 先申请上传 URL，并把计划/URL 落库（status=uploading），任何时刻崩溃都可续传；
/// 2) 逐文件上传，每成功一个立即写检查点并删除本地临时副本；
/// 3) 全部成功 → 批次进入 processing，交由 Harvester 收割。
async fn create_and_upload_batch(
    state: &AppState,
    cfg: &Config,
    mineru: &MineruClient,
    doc_ids: &[i64],
    items: &[UploadPlanItem],
) -> Result<(String, Vec<(usize, String)>)> {
    let files_info: Vec<(String, String)> = items
        .iter()
        .map(|it| (it.filename.clone(), it.data_id.clone()))
        .collect();
    let (batch_id, upload_urls) = mineru.request_upload_urls(&files_info).await?;

    let issued_at = now_string();
    state.db.add_ocr_batch(
        &batch_id,
        doc_ids,
        items.len(),
        Some(items),
        Some(&upload_urls),
        Some(&issued_at),
    )?;

    for it in items {
        state.db.attach_chunk_to_batch(
            it.chunk_id,
            &batch_id,
            &it.data_id,
            &it.filename,
        )?;
    }
    for doc_id in doc_ids {
        state
            .db
            .update_document_progress(*doc_id, "ocr_processing", Some(40), Some(""))?;
        state
            .db
            .set_document_phase(*doc_id, "uploading", Some(batch_id.as_str()))?;
    }

    let planned: Vec<PlannedUpload> = items
        .iter()
        .enumerate()
        .map(|(idx, it)| PlannedUpload {
            chunk_id: it.chunk_id,
            doc_id: it.doc_id,
            filename: it.filename.clone(),
            path: it.path.clone(),
            data_id: it.data_id.clone(),
            upload_url: upload_urls.get(idx).cloned().unwrap_or_default(),
        })
        .collect();

    let errors = upload_items(state, cfg, mineru, &batch_id, &planned).await;
    if !errors.is_empty() {
        let msg = truncate500(&format!(
            "{} 个分块上传失败：{}",
            errors.len(),
            errors[0].1
        ));
        state.db.update_ocr_batch_error(&batch_id, &msg)?;
        crate::log_info!(
            "[流水线] 批次 {} 有 {} 个分块上传失败，保留批次待续传",
            batch_id,
            errors.len()
        );
    } else {
        state.db.mark_batch_submitted(&batch_id)?;
        for doc_id in doc_ids {
            state
                .db
                .set_document_phase(*doc_id, "submitted", Some(batch_id.as_str()))?;
        }
        crate::log_info!(
            "[流水线] 批次 {} 投递成功（{} 个分块），本地临时文件已即传即焚",
            batch_id,
            items.len()
        );
    }
    Ok((batch_id, errors))
}

/// 放弃 uploading 批次的剩余分块：解绑并重新排队（保留已上传分块的所有权）
fn abandon_uploading_batch(
    state: &AppState,
    cfg: &Config,
    batch: &Batch,
) -> Result<()> {
    let plan = batch.upload_plan();
    let mut uploaded_any = false;
    for item in &plan {
        let Some(chunk) = state.db.get_chunk(item.chunk_id)? else {
            continue;
        };
        if let Some(bid) = chunk.batch_id.as_deref() {
            if bid != batch.batch_id.as_str() {
                continue;
            }
        }
        if chunk
            .remote_state
            .as_deref()
            .map(|s| TERMINAL_UPLOAD_STATES.contains(&s))
            .unwrap_or(false)
        {
            uploaded_any = true;
            continue;
        }
        state.db.reset_chunk_for_retry(chunk.id)?;
        remove_temp_file(&chunk.chunk_path, cfg);
    }
    let status = if uploaded_any { "processing" } else { "failed" };
    state.db.mark_batch_upload_abandoned(
        &batch.batch_id,
        status,
        "上传链接过期/中断，剩余分块已重新排队",
    )?;
    for doc_id in &batch.doc_ids {
        state.db.requeue_document_for_resume(
            *doc_id,
            "上传中断，已保留已完成进度并重新排队",
        )?;
    }
    crate::log_info!(
        "[流水线] 批次 {} 上传已放弃，剩余分块重新排队",
        batch.batch_id
    );
    Ok(())
}

/// 续传 uploading 批次：URL 未过期则用原 URL 补传剩余分块；
/// 过期/超次数则放弃剩余分块（重新排队），已上传分块继续由该批次收割。
/// 调用方必须已持有 upload_lock。
async fn resume_uploading_batch(
    state: &AppState,
    cfg: &Config,
    mineru: &MineruClient,
    batch: &Batch,
) -> Result<&'static str> {
    let plan = batch.upload_plan();
    let urls = batch.upload_urls();
    let mut remaining: Vec<(usize, UploadPlanItem, String)> = Vec::new();
    for (idx, item) in plan.iter().enumerate() {
        let Some(chunk) = state.db.get_chunk(item.chunk_id)? else {
            continue;
        };
        if chunk
            .remote_state
            .as_deref()
            .map(|s| TERMINAL_UPLOAD_STATES.contains(&s))
            .unwrap_or(false)
        {
            continue;
        }
        // batch_id 为 NULL 表示计划落库后尚未绑定（崩溃窗口）或已脱离：均视为仍需本批上传
        if let Some(bid) = chunk.batch_id.as_deref() {
            if bid != batch.batch_id.as_str() {
                continue;
            }
        }
        remaining.push((
            idx,
            item.clone(),
            urls.get(idx).cloned().unwrap_or_default(),
        ));
    }

    if remaining.is_empty() {
        state.db.mark_batch_submitted(&batch.batch_id)?;
        return Ok("submitted");
    }

    let age = batch_age_seconds(batch);
    if age > cfg.resume_upload_url_ttl as i64
        || batch.recovery_attempts >= cfg.resume_max_upload_attempts
    {
        abandon_uploading_batch(state, cfg, batch)?;
        return Ok("abandoned");
    }

    for (_idx, item, _url) in &remaining {
        state.db.attach_chunk_to_batch(
            item.chunk_id,
            &batch.batch_id,
            &item.data_id,
            &item.filename,
        )?;
    }
    state
        .db
        .increment_batch_counter(&batch.batch_id, "recovery_attempts")?;

    let planned: Vec<PlannedUpload> = remaining
        .iter()
        .map(|(_idx, item, url)| PlannedUpload {
            chunk_id: item.chunk_id,
            doc_id: item.doc_id,
            filename: item.filename.clone(),
            path: item.path.clone(),
            data_id: item.data_id.clone(),
            upload_url: url.clone(),
        })
        .collect();

    let errors = upload_items(state, cfg, mineru, &batch.batch_id, &planned).await;
    if !errors.is_empty() {
        let msg = truncate500(&format!(
            "续传仍有 {} 个分块失败：{}",
            errors.len(),
            errors[0].1
        ));
        state.db.update_ocr_batch_error(&batch.batch_id, &msg)?;
        crate::log_info!(
            "[流水线] 批次 {} 续传 {} 个分块，仍失败 {} 个",
            batch.batch_id,
            remaining.len(),
            errors.len()
        );
        return Ok("partial");
    }

    state.db.mark_batch_submitted(&batch.batch_id)?;
    for doc_id in &batch.doc_ids {
        state
            .db
            .set_document_phase(*doc_id, "submitted", Some(batch.batch_id.as_str()))?;
    }
    crate::log_info!(
        "[流水线] 批次 {} 续传完成，共 {} 个分块",
        batch.batch_id,
        remaining.len()
    );
    Ok("resumed")
}

/// Submitter 每轮优先续传一个 uploading 批次；返回是否做了续传工作
async fn resume_uploading_batches_once(
    state: &AppState,
    cfg: &Config,
    mineru: &MineruClient,
) -> bool {
    let batches: Vec<Batch> = state
        .db
        .get_active_ocr_batches()
        .unwrap_or_default()
        .into_iter()
        .filter(|b| b.status == "uploading" && !b.upload_abandoned)
        .collect();
    if batches.is_empty() {
        return false;
    }
    let _guard = state.scheduler.upload_lock.lock().await;
    for batch in batches {
        match resume_uploading_batch(state, cfg, mineru, &batch).await {
            Ok(outcome) => {
                if outcome != "skip" {
                    return true;
                }
            }
            Err(e) => {
                crate::log_info!("[流水线] 续传批次 {} 异常：{}", batch.batch_id, e);
                continue;
            }
        }
    }
    false
}

// -------------------------------------------------------------
// 收割：逐分块下载（已有 JSON 直接复用）→ 文档仅合并
// -------------------------------------------------------------

fn match_legacy_result<'a>(
    cloud_map: &'a [serde_json::Value],
    chunk_filename: &str,
) -> Option<&'a serde_json::Value> {
    for item in cloud_map {
        let name = item.get("file_name").and_then(|v| v.as_str()).unwrap_or("");
        if !name.is_empty() && (name.contains(chunk_filename) || chunk_filename.contains(name)) {
            return Some(item);
        }
    }
    None
}

fn match_downloaded_result<'a>(
    results: &'a [crate::mineru::MineruResult],
    chunk_filename: &str,
) -> Option<&'a crate::mineru::MineruResult> {
    for result in results {
        if !result.filename.is_empty()
            && (result.filename.contains(chunk_filename)
                || chunk_filename.contains(&result.filename))
        {
            return Some(result);
        }
    }
    None
}

/// 文档落盘：只有全部分块到达终态才合并。返回 'completed' | 'failed' | 'pending'
fn finalize_document(state: &AppState, cfg: &Config, doc_id: i64) -> Result<&'static str> {
    let Some(doc) = state.db.get_document(doc_id)? else {
        return Ok("failed");
    };
    if doc.status == "completed" {
        return Ok("completed");
    }
    let chunks = state.db.get_chunks_by_document(doc_id)?;
    if chunks.is_empty() {
        return Ok("pending");
    }
    if !chunks.iter().all(chunk_settled) {
        return Ok("pending");
    }
    if !state.db.try_claim_finalize(doc_id)? {
        let latest = state.db.get_document(doc_id)?;
        return Ok(
            if latest.map(|d| d.status == "completed").unwrap_or(false) {
                "completed"
            } else {
                "pending"
            },
        );
    }

    let mut ordered = chunks;
    ordered.sort_by_key(|c| c.start_page.unwrap_or(0));
    let json_paths: Vec<String> = ordered
        .iter()
        .filter(|c| chunk_json_ready(c))
        .filter_map(|c| c.json_path.clone())
        .collect();
    let failed: Vec<&Chunk> = ordered
        .iter()
        .filter(|c| !chunk_json_ready(c))
        .collect();

    if !failed.is_empty() || json_paths.is_empty() {
        let ranges = if failed.is_empty() {
            "无有效产物".to_string()
        } else {
            failed
                .iter()
                .map(|c| {
                    format!(
                        "{}-{}",
                        c.start_page
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "None".to_string()),
                        c.end_page
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "None".to_string())
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        match doc.markdown_path.as_deref() {
            Some(md) => state
                .db
                .update_document_markdown_path(doc_id, md, &json_paths)?,
            None => state.db.update_document_json_paths(doc_id, &json_paths)?,
        }
        let msg = truncate500(&format!("分块缺失或解析失败：{}", ranges));
        state
            .db
            .update_document_progress(doc_id, "failed", Some(40), Some(&msg))?;
        crate::log_info!(
            "[收割] 文档 {} 存在失败分块（{}），保留其余 {} 个结果",
            doc_id,
            ranges,
            json_paths.len()
        );
        return Ok("failed");
    }

    let base_name = Path::new(&doc.filename)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| doc.filename.clone());
    let md_path = cfg
        .output_md_folder
        .join(format!("{}__{}.md", base_name, doc_id));
    let tmp_path = PathBuf::from(format!("{}.tmp", md_path.to_string_lossy()));
    let convert_result = if json_paths.len() == 1 {
        convert_json_to_markdown(&json_paths[0], &tmp_path, cfg)
    } else {
        merge_json_to_markdown(&json_paths, &tmp_path, cfg)
    };
    let write_result = convert_result.and_then(|_| {
        std::fs::rename(&tmp_path, &md_path)?;
        Ok(())
    });
    match write_result {
        Ok(()) => {
            state.db.update_document_markdown_path(
                doc_id,
                &md_path.to_string_lossy(),
                &json_paths,
            )?;
            state
                .db
                .update_document_progress(doc_id, "completed", Some(100), Some(""))?;
            crate::log_info!(
                "[收割] 文档 {} ({}) 合并 Markdown 成功",
                doc_id,
                doc.filename
            );
            Ok("completed")
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            let msg = truncate500(&format!("合并Markdown失败：{}", e));
            state
                .db
                .update_document_progress(doc_id, "failed", None, Some(&msg))?;
            crate::log_info!("[收割] 文档 {} 合并失败：{}", doc_id, e);
            Ok("failed")
        }
    }
}

/// 历史批次（无上传计划）兼容收割：保持"全终态才落盘 + 文件名模糊匹配"语义
async fn harvest_legacy_batch(
    state: &AppState,
    cfg: &Config,
    mineru: &MineruClient,
    batch: &Batch,
) -> Result<bool> {
    let batch_id = batch.batch_id.as_str();
    let status = mineru.poll_batch_status(batch_id, cfg).await;
    if let Some(err) = status.error.as_deref() {
        state.db.record_batch_poll_failure(batch_id, err)?;
        crate::log_info!(
            "[收割] 历史批次 {} 状态查询失败（已查询 {} 次{}）：{}",
            batch_id,
            status.attempts,
            if status.exhausted { "，重试耗尽" } else { "" },
            err
        );
    } else {
        state.db.clear_batch_poll_failure(batch_id)?;
    }
    if !status.finished {
        return Ok(false);
    }
    let extract_results = status.items;

    let raw_map = state
        .db
        .get_meta(&format!("batch_data_map_{}", batch_id), Some("{}"))?
        .unwrap_or_else(|| "{}".to_string());
    let name_map: HashMap<String, String> = serde_json::from_str(&raw_map).unwrap_or_default();
    let cloud_items: Vec<serde_json::Value> = extract_results
        .into_iter()
        .filter(|item| {
            item.get("file_name")
                .and_then(|v| v.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false)
        })
        .collect();

    // 先筛出真正需要下载的云端结果：已有可用 JSON 的分块一律跳过
    let mut needed_cloud: Vec<serde_json::Value> = Vec::new();
    let mut seen_names: HashSet<String> = HashSet::new();
    for doc_id in &batch.doc_ids {
        for chunk in state.db.get_chunks_by_document(*doc_id)? {
            if chunk_json_ready(&chunk) {
                continue;
            }
            let Some(matched) = match_legacy_result(&cloud_items, &chunk.filename) else {
                continue;
            };
            if matched.get("state").and_then(|v| v.as_str()) == Some("failed") {
                continue;
            }
            let name = matched
                .get("file_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if seen_names.contains(&name) {
                continue;
            }
            seen_names.insert(name);
            needed_cloud.push(matched.clone());
        }
    }

    let results = mineru
        .extract_batch_results(&needed_cloud, &name_map, cfg)
        .await;

    for doc_id in &batch.doc_ids {
        let doc_result: Result<()> = (|| {
            for chunk in state.db.get_chunks_by_document(*doc_id)? {
                if chunk_json_ready(&chunk) {
                    continue; // 已下载结果原样保留，不覆盖
                }
                let Some(matched) = match_legacy_result(&cloud_items, &chunk.filename) else {
                    state.db.update_chunk_remote_state(
                        chunk.id,
                        "failed",
                        None,
                        Some("缺少解析产物"),
                    )?;
                    continue;
                };
                if matched.get("state").and_then(|v| v.as_str()) == Some("failed") {
                    let reason = matched
                        .get("err_msg")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .or_else(|| {
                            matched
                                .get("err_code")
                                .and_then(|v| v.as_str())
                                .filter(|s| !s.is_empty())
                        })
                        .unwrap_or("云端解析失败");
                    let msg = truncate500(reason);
                    state.db.update_chunk_remote_state(
                        chunk.id,
                        "failed",
                        None,
                        Some(&msg),
                    )?;
                    continue;
                }
                let Some(downloaded) =
                    match_downloaded_result(&results, &chunk.filename)
                else {
                    state.db.update_chunk_remote_state(
                        chunk.id,
                        "failed",
                        None,
                        Some("云端结果未返回"),
                    )?;
                    continue;
                };
                if downloaded.status == "failed" {
                    let tag = if downloaded.exhausted {
                        "重试耗尽"
                    } else if !downloaded.retryable {
                        "不可重试"
                    } else {
                        "批次已收尾，未再重试"
                    };
                    let msg = truncate500(&format!(
                        "{}（本次请求 {} 次）：{}",
                        tag,
                        downloaded.attempts.max(1),
                        downloaded
                            .error_message
                            .as_deref()
                            .unwrap_or("下载解压产物失败")
                    ));
                    state.db.mark_chunk_fetch_failure(
                        chunk.id,
                        &msg,
                        downloaded.retryable,
                        downloaded.exhausted,
                        "failed",
                    )?;
                } else if let Some(jp) = downloaded
                    .json_path
                    .as_deref()
                    .filter(|p| !p.is_empty())
                {
                    state
                        .db
                        .update_chunk_remote_state(chunk.id, "done", Some(jp), None)?;
                } else {
                    state.db.update_chunk_remote_state(
                        chunk.id,
                        "failed",
                        None,
                        Some("下载结果缺少JSON路径"),
                    )?;
                }
            }
            let _ = finalize_document(state, cfg, *doc_id)?;
            Ok(())
        })();
        if let Err(e) = doc_result {
            let msg = truncate500(&e.to_string());
            state
                .db
                .update_document_progress(*doc_id, "failed", None, Some(&msg))?;
            crate::log_info!("[收割] 历史批次文档 {} 收割异常：{}", doc_id, e);
        }
    }

    state.db.close_ocr_batch(batch_id, "completed", None)?;
    crate::log_info!("[收割] 历史批次 {} 收割完毕（已下载分块直接复用）", batch_id);
    Ok(true)
}

/// 单轮收割一个批次：逐分块轮询云端、下载落盘，文档级别只做合并。
async fn harvest_batch_once(
    state: &AppState,
    cfg: &Config,
    mineru: &MineruClient,
    batch: &Batch,
) -> Result<bool> {
    let batch_id = batch.batch_id.clone();
    if batch.upload_plan_json.is_none() {
        return harvest_legacy_batch(state, cfg, mineru, batch).await;
    }

    let plan = batch.upload_plan();
    // 上传尚未完成且未放弃：先确认是否其实已全部上传（崩溃在最后一刻），否则等待 Submitter 续传
    if batch.status == "uploading" && !batch.upload_abandoned {
        let mut all_terminal = true;
        for item in &plan {
            let chunk = state.db.get_chunk(item.chunk_id)?;
            let state_str = chunk.as_ref().and_then(|c| c.remote_state.as_deref());
            let terminal = state_str
                .map(|s| TERMINAL_UPLOAD_STATES.contains(&s))
                .unwrap_or(false);
            if !terminal {
                all_terminal = false;
            }
        }
        if all_terminal {
            state.db.mark_batch_submitted(&batch_id)?;
        } else {
            return Ok(false);
        }
    }

    let status = mineru.poll_batch_status(&batch_id, cfg).await;
    if let Some(err) = status.error.as_deref() {
        state.db.record_batch_poll_failure(&batch_id, err)?;
        crate::log_info!(
            "[收割] 批次 {} 状态查询失败（已查询 {} 次{}）：{}",
            batch_id,
            status.attempts,
            if status.exhausted { "，重试耗尽" } else { "" },
            err
        );
    } else {
        state.db.clear_batch_poll_failure(&batch_id)?;
    }
    let items = status.items;
    if !items.is_empty() {
        state.db.increment_batch_counter(&batch_id, "poll_count")?;
    }
    let mut items_by_data: HashMap<String, &serde_json::Value> = HashMap::new();
    let mut items_by_name: HashMap<String, &serde_json::Value> = HashMap::new();
    for item in &items {
        if let Some(data_id) = item.get("data_id").and_then(|v| v.as_str()) {
            if !data_id.is_empty() {
                items_by_data.insert(data_id.to_string(), item);
            }
        }
        if let Some(name) = item.get("file_name").and_then(|v| v.as_str()) {
            if !name.is_empty() {
                items_by_name.insert(name.to_string(), item);
            }
        }
    }

    let mut settled_all = true;
    for item in &plan {
        let Some(chunk) = state.db.get_chunk(item.chunk_id)? else {
            continue;
        };
        let remote = chunk.remote_state.as_deref();
        if matches!(remote, Some("done") | Some("failed")) {
            continue;
        }
        if remote == Some("pending") && chunk.batch_id.as_deref() != Some(batch_id.as_str()) {
            continue; // 已被放弃并脱离本批次，交由新批次处理
        }
        if matches!(remote, Some("pending") | Some("uploading")) {
            settled_all = false;
            continue;
        }

        let cloud_item = items_by_data
            .get(&item.data_id)
            .copied()
            .or_else(|| items_by_name.get(&item.filename).copied());
        let Some(cloud_item) = cloud_item else {
            settled_all = false;
            continue;
        };
        let cloud_state = cloud_item.get("state").and_then(|v| v.as_str());
        if cloud_state == Some("failed") {
            let reason = cloud_item
                .get("err_msg")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    cloud_item
                        .get("err_code")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                })
                .unwrap_or("云端解析失败");
            let msg = truncate500(reason);
            state
                .db
                .update_chunk_remote_state(chunk.id, "failed", None, Some(&msg))?;
            continue;
        }
        if cloud_state != Some("done") {
            settled_all = false;
            continue;
        }

        // 云端已完成：原子认领下载，已有 JSON 直接复用
        if !state.db.claim_chunk_download(chunk.id)? {
            let latest = state.db.get_chunk(chunk.id)?;
            if latest
                .as_ref()
                .and_then(|c| c.remote_state.as_deref())
                .map(|s| s == "downloading" || s == "uploaded")
                .unwrap_or(false)
            {
                settled_all = false;
            }
            continue;
        }
        let zip_url = cloud_item
            .get("full_zip_url")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let Some(zip_url) = zip_url else {
            let cycles = chunk.fetch_attempts;
            let msg = truncate500(&format!(
                "不可重试（第{}轮，本次请求 1 次）：结果中缺少 full_zip_url",
                cycles + 1
            ));
            state
                .db
                .mark_chunk_fetch_failure(chunk.id, &msg, false, false, "uploaded")?;
            continue;
        };
        match mineru
            .download_and_extract(zip_url, &item.data_id, cfg)
            .await
        {
            Ok(json_path) => {
                state
                    .db
                    .update_chunk_remote_state(chunk.id, "done", Some(&json_path), None)?;
            }
            Err(e) => {
                // 外层收割轮次：之前轮次的失败次数 + 本次 = 第几轮
                let cycles = state
                    .db
                    .get_chunk(chunk.id)?
                    .map(|c| c.fetch_attempts)
                    .unwrap_or(chunk.fetch_attempts);
                let will_exhaust = e.retryable
                    && (cycles + 1) >= cfg.ocr_download_max_cycles.max(1) as i64;
                let attempts = e.attempts.max(1);
                if e.retryable && !will_exhaust {
                    let msg = truncate500(&format!(
                        "下载解压产物失败（第{}轮，本次请求 {} 次）：{}",
                        cycles + 1,
                        attempts,
                        e
                    ));
                    state.db.mark_chunk_fetch_failure(
                        chunk.id,
                        &msg,
                        true,
                        false,
                        "uploaded",
                    )?;
                    settled_all = false;
                    crate::log_info!(
                        "[收割] 分块 {} 下载失败（第{}轮，本次已请求 {} 次），下一轮继续重试：{}",
                        chunk.filename,
                        cycles + 1,
                        attempts,
                        e
                    );
                } else {
                    let reason = if will_exhaust { "重试耗尽" } else { "不可重试" };
                    let msg = truncate500(&format!(
                        "{}（第{}轮，本次请求 {} 次）：{}",
                        reason,
                        cycles + 1,
                        attempts,
                        e
                    ));
                    state.db.mark_chunk_fetch_failure(
                        chunk.id,
                        &msg,
                        e.retryable,
                        will_exhaust,
                        "uploaded",
                    )?;
                    crate::log_info!(
                        "[收割] 分块 {} {}，已标记失败：{}",
                        chunk.filename,
                        reason,
                        e
                    );
                }
            }
        }
    }

    // 超时兜底：明确失败的才落 failed，仍可重试的保留状态交给人工/下一次恢复
    if !settled_all && batch_age_seconds(batch) > cfg.mineru_poll_timeout as i64 {
        for item in &plan {
            let Some(chunk) = state.db.get_chunk(item.chunk_id)? else {
                continue;
            };
            if matches!(chunk.remote_state.as_deref(), Some("done") | Some("failed")) {
                continue;
            }
            if chunk.batch_id.as_deref() != Some(batch_id.as_str()) {
                continue;
            }
            state.db.update_chunk_remote_state(
                chunk.id,
                "failed",
                None,
                Some("云端解析超时"),
            )?;
        }
        state
            .db
            .close_ocr_batch(&batch_id, "failed", Some("云端解析超时"))?;
        crate::log_info!(
            "[收割] 批次 {} 轮询超时，未完成分块已标记失败（已完成结果已保留）",
            batch_id
        );
    } else if settled_all {
        state.db.close_ocr_batch(&batch_id, "completed", None)?;
    }

    // 文档级别只做合并：本批次涉及的文档，若全部分块已落定则立即出 Markdown
    for doc_id in &batch.doc_ids {
        let _ = finalize_document(state, cfg, *doc_id)?;
    }
    Ok(settled_all)
}

fn active_batch_ids_for_doc(state: &AppState, doc_id: i64) -> Result<Vec<String>> {
    let mut ids: Vec<String> = Vec::new();
    if let Some(doc) = state.db.get_document(doc_id)? {
        if let Some(bid) = doc.ocr_batch_id.filter(|s| !s.is_empty()) {
            ids.push(bid);
        }
    }
    for chunk in state.db.get_chunks_by_document(doc_id)? {
        if let Some(bid) = chunk.batch_id.filter(|s| !s.is_empty()) {
            if !ids.contains(&bid) {
                ids.push(bid);
            }
        }
    }
    // 历史批次的分块没有 batch_id：按 ocr_batches.doc_ids_json 重连仍活跃的批次
    for batch_id in state.db.get_active_batch_ids_for_document(doc_id)? {
        if !ids.contains(&batch_id) {
            ids.push(batch_id);
        }
    }
    Ok(ids)
}

fn has_active_batch_for_doc(state: &AppState, doc_id: i64) -> Result<bool> {
    for batch_id in active_batch_ids_for_doc(state, doc_id)? {
        if let Some(batch) = state.db.get_ocr_batch(&batch_id)? {
            if batch.status == "uploading" || batch.status == "processing" {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

// -------------------------------------------------------------
// Submitter / Harvester 背景任务
// -------------------------------------------------------------

async fn submitter_loop(state: AppState) {
    let stop = state.scheduler.stop.clone();
    crate::log_info!("[生产者 Submitter] 开始按频控巡检并投递任务...");
    while !stop.load(Ordering::SeqCst) {
        // 每轮重新取配置快照，设置面板改动的 Token/目录可即时生效
        let cfg = state.cfg();
        let mineru = MineruClient::new(&cfg);
        if let Err(e) = submitter_round(&state, &cfg, &mineru).await {
            crate::log_info!("[生产者 Submitter] 投递异常: {}", e);
            if sleep_checked(&stop, 10).await {
                break;
            }
        }
    }
    crate::log_info!("[生产者 Submitter] 线程已退出");
}

async fn submitter_round(
    state: &AppState,
    cfg: &Config,
    mineru: &MineruClient,
) -> Result<()> {
    let stop = state.scheduler.stop.clone();
    if stop.load(Ordering::SeqCst) {
        return Ok(());
    }

    // 1. 今日上传限额（留 1% 余量，避免正好卡在额度边界上）
    let today_count = state.db.get_today_uploaded_chunk_count()?;
    let daily_limit = cfg.ocr_daily_upload_limit.max(1);
    let daily_threshold = (daily_limit * 99 / 100).max(1);
    if today_count >= daily_threshold {
        crate::log_info!(
            "[生产者 Submitter] 今日上传已达限额 ({}/{})，暂停投递",
            today_count,
            daily_limit
        );
        sleep_checked(&stop, 60).await;
        return Ok(());
    }

    // 2. 优先续传中断的上传批次（不新增批次）
    if resume_uploading_batches_once(state, cfg, mineru).await {
        let wait = cfg.ocr_upload_interval.max(10);
        crate::log_info!("[生产者 Submitter] 已续传中断批次，遵守频控休眠 {} 秒...", wait);
        sleep_checked(&stop, wait).await;
        return Ok(());
    }

    // 3. 检查云端活跃排队批次
    let active_batches = state.db.get_active_ocr_batches()?;
    if active_batches.len() >= 5 {
        crate::log_info!(
            "[生产者 Submitter] 云端活跃批次达上限 ({}/5)，等待收割释放...",
            active_batches.len()
        );
        sleep_checked(&stop, 15).await;
        return Ok(());
    }

    // 4. 拉取待处理文档并按"仍需上传的分块"贪心聚合
    let candidates = state
        .db
        .get_next_pending_ocr_docs(cfg.batch_doc_limit, &cfg.scheduler_sort_mode, false)?;
    if candidates.is_empty() {
        sleep_checked(&stop, 10).await;
        return Ok(());
    }

    let mut prepared: Vec<(i64, Vec<UploadPlanItem>)> = Vec::new();
    let mut total_items = 0usize;
    for d in &candidates {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let prepared_doc: Result<Option<(i64, Vec<UploadPlanItem>)>> = async {
            let Some(doc) = state.db.get_document(d.id)? else {
                return Ok(None);
            };
            ensure_chunks(state, cfg, &doc).await?;
            let items = pending_upload_items(state, cfg, &[d.id])?;
            if items.is_empty() {
                // 无待上传分块：若仍有活跃批次则交给收割线程，否则说明批次已终结
                if has_active_batch_for_doc(state, d.id)? {
                    state.db.update_document_progress(
                        d.id,
                        "ocr_processing",
                        Some(40),
                        Some(""),
                    )?;
                    state.db.set_document_phase(d.id, "submitted", None)?;
                } else {
                    state.db.update_document_progress(
                        d.id,
                        "failed",
                        None,
                        Some("云端批次已结束但分块未落定，可点击OCR断点续跑"),
                    )?;
                }
                return Ok(None);
            }
            Ok(Some((d.id, items)))
        }
        .await;
        match prepared_doc {
            Ok(None) => continue,
            Ok(Some((doc_id, items))) => {
                if !prepared.is_empty() && total_items + items.len() > cfg.batch_chunk_limit
                {
                    let paths: Vec<String> =
                        items.iter().map(|it| it.path.clone()).collect();
                    remove_temp_paths(&paths, cfg);
                    break;
                }
                total_items += items.len();
                prepared.push((doc_id, items));
                if total_items >= cfg.batch_chunk_limit.saturating_sub(3) {
                    break;
                }
            }
            Err(e) => {
                crate::log_info!("[生产者 Submitter] 文档 {} 准备失败: {}", d.id, e);
            }
        }
    }

    if prepared.is_empty() {
        sleep_checked(&stop, 10).await;
        return Ok(());
    }

    // 5. 原子锁定（只抢占 uploaded）；整个"抢占→建批次→上传"持锁，避免恢复流程误判中断
    let _guard = state.scheduler.upload_lock.lock().await;
    let requested: Vec<i64> = prepared.iter().map(|(id, _)| *id).collect();
    let claimed = state.db.try_mark_batch_processing(&requested, false)?;
    if claimed.is_empty() {
        for (_doc_id, items) in &prepared {
            let paths: Vec<String> = items.iter().map(|it| it.path.clone()).collect();
            remove_temp_paths(&paths, cfg);
        }
        return Ok(());
    }
    // 未抢占成功的文档：清理刚切出的临时分块，下次重切
    for (doc_id, items) in &prepared {
        if !claimed.contains(doc_id) {
            let paths: Vec<String> = items.iter().map(|it| it.path.clone()).collect();
            remove_temp_paths(&paths, cfg);
        }
    }
    let all_items: Vec<UploadPlanItem> = prepared
        .iter()
        .filter(|(doc_id, _)| claimed.contains(doc_id))
        .flat_map(|(_doc_id, items)| items.iter().cloned())
        .collect();
    if all_items.is_empty() {
        return Ok(());
    }
    let doc_ids: Vec<i64> = prepared
        .iter()
        .filter(|(doc_id, _)| claimed.contains(doc_id))
        .map(|(doc_id, _)| *doc_id)
        .collect();
    crate::log_info!(
        "[生产者 Submitter] 正在投递批次：{} 本书，{} 个分块...",
        doc_ids.len(),
        all_items.len()
    );
    let (batch_id, errors) =
        create_and_upload_batch(state, cfg, mineru, &doc_ids, &all_items).await?;
    if !errors.is_empty() {
        crate::log_info!(
            "[生产者 Submitter] 批次 {} 部分上传失败，下轮将尝试续传",
            batch_id
        );
    }
    drop(_guard);

    let wait = cfg.ocr_upload_interval.max(10);
    crate::log_info!("[生产者 Submitter] 遵守 50个/分钟 频控，安全休眠 {} 秒...", wait);
    sleep_checked(&stop, wait).await;
    Ok(())
}

async fn harvester_loop(state: AppState) {
    let stop = state.scheduler.stop.clone();
    crate::log_info!("[消费者 Harvester] 开始巡检活跃批次并收割结果...");
    while !stop.load(Ordering::SeqCst) {
        // 每轮重新取配置快照，设置面板改动的 Token/重试参数可即时生效
        let cfg = state.cfg();
        let mineru = MineruClient::new(&cfg);
        let active_batches = match state.db.get_active_ocr_batches() {
            Ok(v) => v,
            Err(e) => {
                crate::log_info!("[消费者 Harvester] 收割异常: {}", e);
                if sleep_checked(&stop, 15).await {
                    break;
                }
                continue;
            }
        };
        if active_batches.is_empty() {
            if sleep_checked(&stop, 10).await {
                break;
            }
            continue;
        }
        for batch in &active_batches {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if let Err(e) = harvest_batch_once(&state, &cfg, &mineru, batch).await {
                crate::log_info!(
                    "[消费者 Harvester] 批次 {} 收割异常: {}",
                    batch.batch_id,
                    e
                );
            }
        }
        if sleep_checked(&stop, 15).await {
            break;
        }
    }
    crate::log_info!("[消费者 Harvester] 线程已退出");
}

// -------------------------------------------------------------
// 手动提交 / 单文档断点续跑
// -------------------------------------------------------------

/// 提交OCR任务：先通过CAS抢占状态，成功后异步执行。
/// 若文档此前已是 completed（用户主动重跑），按整篇重做处理。
pub fn submit_ocr(state: &AppState, doc_id: i64) -> Result<(bool, Option<&'static str>)> {
    let prior_status = state
        .db
        .get_document(doc_id)?
        .map(|d| d.status)
        .unwrap_or_default();
    let (accepted, reason) = state.db.try_mark_processing(doc_id)?;
    if !accepted {
        return Ok((false, reason));
    }
    let force_refresh = prior_status == "completed";
    let st = state.clone();
    tokio::spawn(async move {
        run_ocr_job(st, doc_id, force_refresh).await;
    });
    Ok((true, None))
}

async fn run_ocr_job(state: AppState, doc_id: i64, force_refresh: bool) {
    let cfg = state.cfg();
    let mut temp_to_clean: Vec<PathBuf> = Vec::new();
    let result =
        run_ocr_job_inner(&state, &cfg, doc_id, force_refresh, &mut temp_to_clean).await;
    if let Err(e) = result {
        crate::log_info!("[OCR任务] doc={} 失败: {}", doc_id, e);
        let msg = truncate500(&e.to_string());
        let _ = state
            .db
            .update_document_progress(doc_id, "failed", None, Some(&msg));
    }
    // 保持原有磁盘清理策略：任务结束后清理本任务残留的 temp 分块副本（原文件不删）
    for p in &temp_to_clean {
        if is_temp_path(p, &cfg) {
            let _ = std::fs::remove_file(p);
        }
    }
    let prefix = "mineru_";
    if let Ok(entries) = std::fs::read_dir(&cfg.temp_folder) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir()
                && path
                    .file_name()
                    .map(|n| n.to_string_lossy().starts_with(prefix))
                    .unwrap_or(false)
            {
                let _ = std::fs::remove_dir_all(&path);
            }
        }
    }
}

async fn run_ocr_job_inner(
    state: &AppState,
    cfg: &Config,
    doc_id: i64,
    force_refresh: bool,
    temp_to_clean: &mut Vec<PathBuf>,
) -> Result<()> {
    let Some(doc) = state.db.get_document(doc_id)? else {
        return Ok(()); // 文档已被删除，终止任务
    };

    if force_refresh {
        state.db.delete_chunks_by_document(doc_id)?;
    } else {
        // 云端失败的分块解绑回 pending，下一步只重切/重传缺失页范围
        reset_failed_chunks_for_retry(state, doc_id)?;
    }

    // 1. 先续传该文档仍在 uploading 的批次（URL 未过期时代价最低）
    let mineru = MineruClient::new(cfg);
    for batch_id in active_batch_ids_for_doc(state, doc_id)? {
        if let Some(batch) = state.db.get_ocr_batch(&batch_id)? {
            if batch.status == "uploading" && !batch.upload_abandoned {
                let outcome = {
                    let _guard = state.scheduler.upload_lock.lock().await;
                    resume_uploading_batch(state, cfg, &mineru, &batch).await
                };
                match outcome {
                    Ok("partial") => {
                        // 续传仍失败：立即放弃剩余分块，随后在本任务内重新切分投递
                        if let Some(latest) = state.db.get_ocr_batch(&batch_id)? {
                            if let Err(e) = abandon_uploading_batch(state, cfg, &latest) {
                                crate::log_info!(
                                    "[OCR任务] 放弃批次 {} 剩余分块失败：{}",
                                    batch_id,
                                    e
                                );
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        crate::log_info!("[OCR任务] 续传批次 {} 失败：{}", batch_id, e);
                    }
                }
            }
        }
    }

    // 2. 复用已完成分块，只为缺失/待传分块重新切分
    let chunks = ensure_chunks(state, cfg, &doc).await?;
    for c in &chunks {
        if is_temp_path(&abs_path(&c.chunk_path), cfg) {
            temp_to_clean.push(abs_path(&c.chunk_path));
        }
    }
    state
        .db
        .update_document_progress(doc_id, "ocr_processing", Some(40), Some(""))?;

    // 3. 仍有待上传分块则创建批次（计划先落库，逐文件检查点）
    let items = pending_upload_items(state, cfg, &[doc_id])?;
    if !items.is_empty() {
        // 历史批次（无上传计划）可能仍在云端解析：先收割其已有结果，绝不重复上传
        let mut legacy_active: Vec<String> = Vec::new();
        for bid in active_batch_ids_for_doc(state, doc_id)? {
            if let Some(batch) = state.db.get_ocr_batch(&bid)? {
                if batch.status == "processing"
                    && batch.upload_plan_json.is_none()
                    && !batch.upload_abandoned
                {
                    legacy_active.push(bid);
                }
            }
        }
        if !legacy_active.is_empty() {
            crate::log_info!(
                "[OCR任务] 文档 {} 仍挂在历史批次 {:?} 上，先收割云端结果，避免重复上传",
                doc_id,
                legacy_active
            );
        } else {
            let (batch_id, errors) = {
                let _guard = state.scheduler.upload_lock.lock().await;
                create_and_upload_batch(state, cfg, &mineru, &[doc_id], &items).await?
            };
            if !errors.is_empty() {
                // 上传失败：放弃该批次剩余分块（已完成分块保留），避免无意义长时间轮询
                if let Some(batch) = state.db.get_ocr_batch(&batch_id)? {
                    let _guard = state.scheduler.upload_lock.lock().await;
                    abandon_uploading_batch(state, cfg, &batch)?;
                }
                return Err(anyhow!(
                    "{} 个分块上传失败，已完成进度已保留，可再次提交续跑",
                    errors.len()
                ));
            }
        }
    }

    // 4. 接管收割：轮询该文档涉及的活跃批次，直到全部落定
    let deadline = Instant::now() + Duration::from_secs(cfg.mineru_poll_timeout);
    loop {
        for batch_id in active_batch_ids_for_doc(state, doc_id)? {
            if let Some(batch) = state.db.get_ocr_batch(&batch_id)? {
                if (batch.status == "uploading" || batch.status == "processing")
                    && !batch.upload_abandoned
                {
                    harvest_batch_once(state, cfg, &mineru, &batch).await?;
                } else if batch.upload_abandoned && batch.status == "processing" {
                    harvest_batch_once(state, cfg, &mineru, &batch).await?;
                }
            }
        }

        let result = finalize_document(state, cfg, doc_id)?;
        if result == "completed" || result == "failed" {
            break;
        }
        let mut live_batches = false;
        for bid in active_batch_ids_for_doc(state, doc_id)? {
            if let Some(batch) = state.db.get_ocr_batch(&bid)? {
                if batch.status == "uploading" || batch.status == "processing" {
                    live_batches = true;
                    break;
                }
            }
        }
        if !live_batches {
            // 无在途批次却仍未落定：说明需要重新投递，交由调度器/再次提交
            return Err(anyhow!("没有可继续的云端批次，已完成进度已保留"));
        }
        if Instant::now() > deadline {
            return Err(anyhow!(
                "OCR任务轮询超时，云端批次记录已保留，可稍后恢复"
            ));
        }
        tokio::time::sleep(Duration::from_secs(cfg.mineru_poll_interval)).await;
    }

    Ok(())
}

// -------------------------------------------------------------
// 启动恢复 / 手动恢复入口
// -------------------------------------------------------------

/// 启动恢复：URL 已确定过期的 uploading 批次不联网，直接放弃剩余分块并重新排队
fn abandon_expired_uploading_batches(state: &AppState, cfg: &Config) -> usize {
    let mut count = 0usize;
    let batches = state.db.get_active_ocr_batches().unwrap_or_default();
    for batch in batches {
        if batch.status != "uploading" || batch.upload_abandoned {
            continue;
        }
        if batch_age_seconds(&batch) > cfg.resume_upload_url_ttl as i64 {
            match abandon_uploading_batch(state, cfg, &batch) {
                Ok(_) => count += 1,
                Err(e) => {
                    crate::log_info!("[启动恢复] 放弃批次 {} 失败：{}", batch.batch_id, e)
                }
            }
        }
    }
    count
}

/// 启动恢复：重整残留的 splitting/ocr_processing 文档（移植自 Python dao.recover_interrupted_documents）
fn recover_interrupted_documents(state: &AppState) -> Result<(Vec<i64>, usize)> {
    let live_doc_ids = state.db.get_active_batch_doc_ids()?;
    let docs = state.db.get_documents_in_progress()?;
    let mut merge_ready: Vec<i64> = Vec::new();
    let mut requeued = 0usize;
    for d in docs {
        if live_doc_ids.contains(&d.id) {
            continue; // 仍挂在活跃批次上：Harvester 会按 batch_id 继续收割
        }
        let chunks = state.db.get_chunks_by_document(d.id)?;
        if !chunks.is_empty() {
            let mut settled = true;
            let mut has_json = false;
            let mut has_failed = false;
            for c in &chunks {
                match c.remote_state.as_deref() {
                    Some("done") | Some("failed") => {
                        if c.remote_state.as_deref() == Some("failed") {
                            has_failed = true;
                        } else if json_file_exists(c.json_path.as_deref()) {
                            has_json = true;
                        }
                        continue;
                    }
                    None if c.ocr_status == "completed" || c.ocr_status == "failed" => {
                        if c.ocr_status == "failed" {
                            has_failed = true;
                        } else if json_file_exists(c.json_path.as_deref()) {
                            has_json = true;
                        }
                        continue;
                    }
                    _ => {
                        settled = false;
                        break;
                    }
                }
            }
            if settled {
                if has_failed {
                    // 部分分块已有结果、部分失败：保留已下载结果，等待用户显式续跑
                    state.db.update_document_progress(
                        d.id,
                        "failed",
                        None,
                        Some("存在解析失败的分块，可点击OCR断点续跑"),
                    )?;
                } else if has_json {
                    merge_ready.push(d.id);
                } else {
                    state.db.update_document_progress(
                        d.id,
                        "failed",
                        None,
                        Some("全部分块解析失败，可点击OCR断点续跑"),
                    )?;
                }
                continue;
            }
            if has_failed {
                // 已有明确失败分块：不自动重投（避免烧额度），交给用户显式恢复
                state.db.update_document_progress(
                    d.id,
                    "failed",
                    None,
                    Some("存在解析失败的分块，可点击OCR断点续跑"),
                )?;
                continue;
            }
        }
        state.db.requeue_document_for_resume(d.id, "")?;
        requeued += 1;
    }
    Ok((merge_ready, requeued))
}

/// 服务启动时的恢复入口：不联网，仅做状态重整与本地合并
pub fn recover_pipeline_on_startup(state: &AppState) -> RecoverySummary {
    let cfg = state.cfg();
    let reset_downloading = state.db.reset_stuck_downloading_chunks().unwrap_or(0);
    let expired_upload_batches = abandon_expired_uploading_batches(state, &cfg);
    let (merge_ready, requeued) = match recover_interrupted_documents(state) {
        Ok(v) => v,
        Err(e) => {
            crate::log_info!("[启动恢复] 重整中断文档失败：{}", e);
            (Vec::new(), 0)
        }
    };
    let mut merged = 0usize;
    for doc_id in &merge_ready {
        match finalize_document(state, &cfg, *doc_id) {
            Ok("completed") => merged += 1,
            Ok(_) => {}
            Err(e) => {
                crate::log_info!("[启动恢复] 文档 {} 仅合并失败：{}", doc_id, e)
            }
        }
    }
    let summary = RecoverySummary {
        reset_downloading,
        expired_upload_batches,
        merge_ready: merge_ready.len(),
        merged,
        requeued,
    };
    crate::log_info!(
        "[启动恢复] 断点续跑状态重整完成：reset_downloading={} expired_upload_batches={} merge_ready={} merged={} requeued={}",
        summary.reset_downloading,
        summary.expired_upload_batches,
        summary.merge_ready,
        summary.merged,
        summary.requeued
    );
    summary
}

/// 手动恢复入口：重整中断状态 → 续传同一批次剩余分块（沿用原 batch_id/URL，不新增额度）
/// → 立即收割一轮活跃批次；随后仍未完成的分块留给调度器或单文档"OCR"按钮继续。
pub async fn resume_interrupted_ocr(state: AppState) -> Result<ManualRecoverySummary> {
    let cfg = state.cfg();
    let (reset_downloading, expired_upload_batches, merged, merge_ready_len, requeued) = {
        let _guard = state.scheduler.upload_lock.lock().await;
        let reset_downloading = state.db.reset_stuck_downloading_chunks()?;
        let expired_upload_batches = abandon_expired_uploading_batches(&state, &cfg);
        let (merge_ready, requeued) = recover_interrupted_documents(&state)?;
        let mut merged = 0usize;
        for doc_id in &merge_ready {
            match finalize_document(&state, &cfg, *doc_id) {
                Ok("completed") => merged += 1,
                Ok(_) => {}
                Err(e) => {
                    crate::log_info!("[手动恢复] 文档 {} 仅合并失败：{}", doc_id, e)
                }
            }
        }
        (reset_downloading, expired_upload_batches, merged, merge_ready.len(), requeued)
    };

    let mineru = MineruClient::new(&cfg);
    let mut resumed_batches = 0usize;
    for _ in 0..5 {
        // 每轮续传一个批次，最多处理 5 个
        if !resume_uploading_batches_once(&state, &cfg, &mineru).await {
            break;
        }
        resumed_batches += 1;
    }

    let mut harvested_batches = 0usize;
    for batch in state.db.get_active_ocr_batches()? {
        match harvest_batch_once(&state, &cfg, &mineru, &batch).await {
            Ok(_) => harvested_batches += 1,
            Err(e) => {
                crate::log_info!(
                    "[手动恢复] 批次 {} 收割异常：{}",
                    batch.batch_id,
                    e
                )
            }
        }
    }

    Ok(ManualRecoverySummary {
        reset_downloading,
        expired_upload_batches,
        merge_ready: merge_ready_len,
        merged,
        requeued,
        resumed_batches,
        harvested_batches,
    })
}

/// 调度器未开启时，为崩溃前遗留的活跃批次启动一次性恢复任务：
/// - 续传 uploading 批次（沿用同一 batch_id/URL，不新增额度）；
/// - 收割 processing 批次，直到没有活跃批次或云端超时。
/// 只做"恢复已有批次"，不投递新文档，因此不会自动烧 MinerU 额度。
pub fn start_recovery_worker(state: &AppState) -> bool {
    if state.scheduler.is_running() {
        return false;
    }
    let active = state.db.get_active_ocr_batches().unwrap_or_default();
    if active.is_empty() {
        return false;
    }

    let st = state.clone();
    tokio::spawn(async move {
        crate::log_info!("[恢复线程] 检测到遗留活跃批次，开始断点续跑...");
        let cfg = st.cfg();
        let mineru = MineruClient::new(&cfg);
        loop {
            let active_batches = match st.db.get_active_ocr_batches() {
                Ok(v) => v,
                Err(e) => {
                    crate::log_info!("[恢复线程] 异常退出：{}", e);
                    break;
                }
            };
            if active_batches.is_empty() {
                break;
            }
            for batch in active_batches {
                let outcome: Result<()> = if batch.status == "uploading"
                    && !batch.upload_abandoned
                {
                    let _guard = st.scheduler.upload_lock.lock().await;
                    resume_uploading_batch(&st, &cfg, &mineru, &batch)
                        .await
                        .map(|_| ())
                } else {
                    harvest_batch_once(&st, &cfg, &mineru, &batch)
                        .await
                        .map(|_| ())
                };
                if let Err(e) = outcome {
                    crate::log_info!("[恢复线程] 批次 {} 恢复异常：{}", batch.batch_id, e);
                }
            }
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
        crate::log_info!("[恢复线程] 遗留批次已全部收尾/等待人工处理");
    });
    crate::log_info!("[恢复线程] 已启动遗留批次断点续跑线程");
    true
}
