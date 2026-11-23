use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::extract::{Multipart, Path as AxumPath, Query, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::Local;
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db::{iso_from_db, parse_json_paths, Document};
use crate::json_to_md::{
    convert_json_to_markdown, json_basename, load_json, merge_json_to_markdown, resolve_json_path,
};
use crate::state::AppState;

type ApiResult = (StatusCode, Json<Value>);

fn ok_json(value: Value) -> ApiResult {
    (StatusCode::OK, Json(value))
}

fn err_json(status: StatusCode, message: impl Into<String>) -> ApiResult {
    (status, Json(json!({ "error": message.into() })))
}

fn doc_to_item(doc: &Document, tags: Vec<crate::db::TagDto>) -> Value {
    json!({
        "id": doc.id,
        "filename": doc.filename,
        "original_path": doc.original_path,
        "file_size": doc.file_size.unwrap_or(0),
        "page_count": doc.page_count.unwrap_or(0),
        "status": doc.status,
        "progress": doc.progress,
        "error_message": doc.error_message,
        "created_at": iso_from_db(&doc.created_at),
        "updated_at": iso_from_db(&doc.updated_at),
        "markdown_path": doc.markdown_path,
        "json_paths": parse_json_paths(doc.json_paths.as_deref()),
        "ocr_disabled": doc.ocr_disabled,
        "click_count": doc.click_count,
        "rating": doc.rating,
        "tags": tags
    })
}

fn abs_path(p: &str) -> PathBuf {
    crate::config::resolve_path(p)
}

fn same_path(a: &str, b: &str) -> bool {
    abs_path(a)
        .to_string_lossy()
        .to_lowercase()
        == abs_path(b).to_string_lossy().to_lowercase()
}

#[derive(Debug, Deserialize)]
pub struct DocQuery {
    pub page: Option<String>,
    pub page_size: Option<String>,
    pub keyword: Option<String>,
    /// 原始路径独立模糊匹配（与 keyword 取交集）
    pub path_keyword: Option<String>,
    pub status: Option<String>,
    pub tag_id: Option<String>,
    pub ocr_disabled: Option<String>,
    pub sort_by: Option<String>,
    pub sort_order: Option<String>,
}

pub async fn get_documents(State(state): State<AppState>, Query(q): Query<DocQuery>) -> ApiResult {
    let page = match q.page.as_deref().unwrap_or("1").trim().parse::<i64>() {
        Ok(v) => v.max(1),
        Err(_) => return err_json(StatusCode::BAD_REQUEST, "分页参数 page/page_size 必须为整数"),
    };
    let page_size = match q.page_size.as_deref().unwrap_or("10").trim().parse::<i64>() {
        Ok(v) => v.clamp(1, 1000),
        Err(_) => return err_json(StatusCode::BAD_REQUEST, "分页参数 page/page_size 必须为整数"),
    };
    let keyword = q.keyword.as_deref().map(|s| s.trim()).filter(|s| !s.is_empty());
    let path_keyword = q
        .path_keyword
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());

    let allowed_statuses = ["uploaded", "splitting", "ocr_processing", "completed", "failed"];
    let raw_status = q.status.as_deref().unwrap_or("").trim();
    let statuses: Option<Vec<String>> = if raw_status.is_empty() {
        None
    } else {
        let list: Vec<String> = raw_status
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        if let Some(bad) = list.iter().find(|s| !allowed_statuses.contains(&s.as_str())) {
            return err_json(StatusCode::BAD_REQUEST, format!("非法的状态值：{}", bad));
        }
        if list.is_empty() {
            None
        } else {
            Some(list)
        }
    };

    let raw_tag_ids = q.tag_id.as_deref().unwrap_or("").trim();
    let tag_ids: Option<Vec<i64>> = if raw_tag_ids.is_empty() {
        None
    } else {
        let mut ids = Vec::new();
        for part in raw_tag_ids.split(',').filter(|s| !s.is_empty()) {
            match part.parse::<i64>() {
                Ok(v) => ids.push(v),
                Err(_) => return err_json(StatusCode::BAD_REQUEST, "tag_id 参数必须为整数"),
            }
        }
        Some(ids)
    };

    let raw_disabled = q.ocr_disabled.as_deref().unwrap_or("").trim().to_lowercase();
    let ocr_disabled = match raw_disabled.as_str() {
        "" => None,
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => return err_json(StatusCode::BAD_REQUEST, "ocr_disabled 参数必须为 true/false"),
    };

    let allowed_sort = [
        "id",
        "filename",
        "file_size",
        "page_count",
        "status",
        "created_at",
        "updated_at",
        "click_count",
        "rating",
        "ocr_disabled",
    ];
    // 默认按更新时间倒序（取消排序也回落到这里）
    let sort_by = q.sort_by.as_deref().unwrap_or("updated_at").trim();
    let sort_by = if allowed_sort.contains(&sort_by) { sort_by } else { "updated_at" };
    let sort_order = q.sort_order.as_deref().unwrap_or("desc").trim().to_lowercase();
    let sort_order = if sort_order == "asc" { "asc" } else { "desc" };

    let (docs, total) = match state.db.get_documents_paginated(
        page,
        page_size,
        keyword,
        path_keyword,
        statuses.as_deref(),
        tag_ids.as_deref(),
        ocr_disabled,
        sort_by,
        sort_order,
    ) {
        Ok(v) => v,
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let ids: Vec<i64> = docs.iter().map(|d| d.id).collect();
    let tags_map = state
        .db
        .get_tags_grouped_by_document(Some(ids.as_slice()))
        .unwrap_or_default();
    let items: Vec<Value> = docs
        .iter()
        .map(|d| doc_to_item(d, tags_map.get(&d.id).cloned().unwrap_or_default()))
        .collect();
    ok_json(json!({
        "items": items,
        "total": total,
        "page": page,
        "page_size": page_size,
        "processing_count": state.db.count_processing().unwrap_or(0)
    }))
}

#[derive(Debug, Deserialize)]
pub struct FolderQuery {
    pub path: Option<String>,
}

pub async fn browse_folders(Query(q): Query<FolderQuery>) -> ApiResult {
    let path = q.path.as_deref().unwrap_or("").trim().to_string();
    if path.is_empty() {
        let mut roots = Vec::new();
        for letter in b'A'..=b'Z' {
            let drive = format!("{}:\\", letter as char);
            if Path::new(&drive).exists() {
                roots.push(json!({"name": format!("{}:", letter as char), "path": drive}));
            }
        }
        return ok_json(json!({"path": "", "parent": null, "folders": roots}));
    }

    let dir = Path::new(&path);
    if !dir.is_dir() {
        return err_json(StatusCode::BAD_REQUEST, format!("目录不存在或不可访问：{}", path));
    }
    let mut folders = Vec::new();
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('$') || name.starts_with('.') {
                    continue;
                }
                let full = entry.path();
                if full.is_dir() {
                    folders.push(json!({"name": name, "path": full.to_string_lossy()}));
                }
            }
        }
        Err(e) => {
            let status = match e.kind() {
                std::io::ErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
                _ => StatusCode::BAD_REQUEST,
            };
            let prefix = if status == StatusCode::FORBIDDEN {
                "没有权限访问目录"
            } else {
                "读取目录失败"
            };
            return err_json(status, format!("{}：{}", prefix, e));
        }
    }
    folders.sort_by_key(|f| {
        f.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase()
    });

    let abs = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let parent = abs
        .parent()
        .filter(|p| *p != abs && !p.as_os_str().is_empty())
        .map(|p| p.to_string_lossy().to_string());
    ok_json(json!({
        "path": abs.to_string_lossy(),
        "parent": parent,
        "folders": folders
    }))
}

#[derive(Debug, Deserialize)]
pub struct ScanRequest {
    pub directory: Option<String>,
}

pub async fn scan_directory(
    State(state): State<AppState>,
    Json(body): Json<ScanRequest>,
) -> ApiResult {
    let Some(directory) = body.directory.filter(|d| !d.trim().is_empty()) else {
        return err_json(StatusCode::BAD_REQUEST, "请提供目录路径");
    };
    if !Path::new(&directory).is_dir() {
        return err_json(StatusCode::BAD_REQUEST, "目录不存在");
    }
    let db = state.db.clone();
    let cfg = state.cfg();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let mut pdf_files: Vec<PathBuf> = Vec::new();
        collect_pdfs(Path::new(&directory), &mut pdf_files);
        if pdf_files.is_empty() {
            return Ok(json!({"message": "未找到PDF文件"}));
        }
        let conn = db.conn()?;
        let mut stmt = conn.prepare("SELECT original_path, file_size, md5 FROM pdf_documents")?;
        let rows: Vec<(String, Option<i64>, Option<String>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        let existing_paths: HashSet<String> =
            rows.iter().map(|(p, _, _)| p.to_lowercase()).collect();
        let mut size_to_md5s: HashMap<i64, HashSet<String>> = HashMap::new();
        for (_, size, md5) in &rows {
            if let Some(s) = size {
                let set = size_to_md5s.entry(*s).or_default();
                if let Some(m) = md5 {
                    if !m.is_empty() {
                        set.insert(m.clone());
                    }
                }
            }
        }

        let mut added = 0;
        let mut skipped = 0;
        let mut duplicates = 0;
        for pdf_path in &pdf_files {
            let path_str = pdf_path.to_string_lossy().to_string();
            if existing_paths.contains(&path_str.to_lowercase()) {
                skipped += 1;
                continue;
            }
            let Ok(meta) = std::fs::metadata(pdf_path) else {
                skipped += 1;
                continue;
            };
            let file_size = meta.len() as i64;
            let new_md5 = match crate::pdf_utils::file_md5(pdf_path) {
                Ok(v) => v,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            if size_to_md5s
                .get(&file_size)
                .map(|set| set.contains(&new_md5))
                .unwrap_or(false)
            {
                duplicates += 1;
                continue;
            }
            let (page_count, _mb, _bytes) = crate::pdf_utils::get_pdf_info(pdf_path);
            if page_count == 0 {
                skipped += 1;
                continue;
            }
            let filename = pdf_path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            match db.add_document(
                &filename,
                &path_str,
                file_size,
                page_count as i64,
                Some(&new_md5),
                crate::pdf_utils::is_ocr_disabled_name(&path_str),
            ) {
                Ok(_) => {
                    size_to_md5s.entry(file_size).or_default().insert(new_md5);
                    added += 1;
                }
                Err(_) => skipped += 1,
            }
        }
        let _ = cfg;
        Ok(json!({
            "message": "扫描完成",
            "added": added,
            "skipped": skipped,
            "duplicates": duplicates,
            "total_found": pdf_files.len()
        }))
    })
    .await;
    match result {
        Ok(Ok(v)) => ok_json(v),
        Ok(Err(e)) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

fn collect_pdfs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_pdfs(&path, out);
        } else if path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase() == "pdf")
            .unwrap_or(false)
        {
            out.push(path);
        }
    }
}

pub async fn upload_document(State(state): State<AppState>, mut multipart: Multipart) -> ApiResult {
    let mut file_name: Option<String> = None;
    let mut file_bytes: Option<Vec<u8>> = None;
    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => {
                if field.name() == Some("file") {
                    file_name = Some(field.file_name().unwrap_or("").to_string());
                    match field.bytes().await {
                        Ok(b) => file_bytes = Some(b.to_vec()),
                        Err(e) => {
                            return err_json(StatusCode::BAD_REQUEST, format!("读取上传文件失败：{}", e))
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(e) => return err_json(StatusCode::BAD_REQUEST, format!("解析上传请求失败：{}", e)),
        }
    }
    let Some(original_name) = file_name else {
        return err_json(StatusCode::BAD_REQUEST, "没有上传文件");
    };
    if original_name.is_empty() {
        return err_json(StatusCode::BAD_REQUEST, "文件名为空");
    }
    if !original_name.to_lowercase().ends_with(".pdf") {
        return err_json(StatusCode::BAD_REQUEST, "只支持PDF文件");
    }
    let Some(bytes) = file_bytes else {
        return err_json(StatusCode::BAD_REQUEST, "没有上传文件");
    };
    let upload_cfg = state.cfg();
    if bytes.len() as u64 > upload_cfg.max_size_bytes {
        return err_json(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("文件超过大小限制（最大 {}MB）", upload_cfg.max_file_size_mb),
        );
    }

    let stored_name = format!("{}.pdf", uuid::Uuid::new_v4().simple());
    let stored_path = upload_cfg.upload_folder.join(stored_name);
    if let Err(e) = std::fs::create_dir_all(&upload_cfg.upload_folder) {
        return err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    if let Err(e) = std::fs::write(&stored_path, &bytes) {
        return err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }

    let stored_str = stored_path.to_string_lossy().to_string();
    let info = crate::pdf_utils::get_pdf_info(&stored_path);
    if info.0 == 0 {
        let _ = std::fs::remove_file(&stored_path);
        return err_json(StatusCode::BAD_REQUEST, "无法读取PDF文件");
    }
    let md5 = match crate::pdf_utils::file_md5(&stored_path) {
        Ok(v) => v,
        Err(_) => {
            let _ = std::fs::remove_file(&stored_path);
            return err_json(StatusCode::INTERNAL_SERVER_ERROR, "计算文件校验值失败");
        }
    };
    let basename = Path::new(&original_name)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| original_name.clone());
    match state.db.add_document(
        &basename,
        &stored_str,
        info.2 as i64,
        info.0 as i64,
        Some(&md5),
        crate::pdf_utils::is_ocr_disabled_name(&basename),
    ) {
        Ok(id) => (
            StatusCode::CREATED,
            Json(json!({"id": id, "message": "文件上传成功"})),
        ),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn get_document(
    State(state): State<AppState>,
    AxumPath(doc_id): AxumPath<i64>,
) -> ApiResult {
    let doc = match state.db.get_document(doc_id) {
        Ok(Some(d)) => d,
        Ok(None) => return err_json(StatusCode::NOT_FOUND, "文档不存在"),
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    // 与列表接口同一份字段结构（含 rating / tags / click_count），
    // 预览页需要用它在弹窗内直接打分、选标签
    let tags = state
        .db
        .get_tags_grouped_by_document(Some(&[doc_id]))
        .ok()
        .and_then(|m| m.get(&doc_id).cloned())
        .unwrap_or_default();
    ok_json(doc_to_item(&doc, tags))
}

/// 预览打开计数：由前端在每次打开预览时显式调用，GET markdown 不计入
pub async fn bump_document_click(
    State(state): State<AppState>,
    AxumPath(doc_id): AxumPath<i64>,
) -> ApiResult {
    match state.db.bump_click_count(doc_id) {
        Ok(Some(count)) => ok_json(json!({ "id": doc_id, "click_count": count })),
        Ok(None) => err_json(StatusCode::NOT_FOUND, "文档不存在"),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Debug, Deserialize)]
pub struct RatingBody {
    pub rating: i64,
}

/// 评分：0 表示未评分，1~10 为有效分值
pub async fn set_document_rating(
    State(state): State<AppState>,
    AxumPath(doc_id): AxumPath<i64>,
    Json(body): Json<RatingBody>,
) -> ApiResult {
    if !(0..=10).contains(&body.rating) {
        return err_json(StatusCode::BAD_REQUEST, "评分必须是 0~10 的整数");
    }
    match state.db.set_rating(doc_id, body.rating) {
        Ok(true) => ok_json(json!({ "id": doc_id, "rating": body.rating })),
        Ok(false) => err_json(StatusCode::NOT_FOUND, "文档不存在"),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// 解析 Markdown 产物路径：优先记录路径；目录被设置面板改到新位置时，
/// 按文件名为新 Markdown 目录兜底（不迁移旧文件，只保证还能读到最后一次产物）。
fn resolve_md_file(doc: &Document, cfg: &crate::config::Config) -> Option<(PathBuf, String)> {
    let md_path = doc.markdown_path.as_deref()?;
    let recorded = abs_path(md_path);
    if recorded.is_file() {
        return Some((recorded, md_path.to_string()));
    }
    let base = Path::new(md_path).file_name()?;
    let candidate = cfg.output_md_folder.join(base);
    if candidate.is_file() {
        return Some((candidate.clone(), candidate.to_string_lossy().to_string()));
    }
    None
}

pub async fn get_document_markdown(
    State(state): State<AppState>,
    AxumPath(doc_id): AxumPath<i64>,
) -> ApiResult {
    let doc = match state.db.get_document(doc_id) {
        Ok(Some(d)) => d,
        Ok(None) => return err_json(StatusCode::NOT_FOUND, "文档不存在"),
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let cfg = state.cfg();
    let Some((path, md_path)) = resolve_md_file(&doc, &cfg) else {
        return err_json(StatusCode::NOT_FOUND, "该文档尚未生成Markdown，请先执行OCR识别");
    };
    match std::fs::read_to_string(&path) {
        Ok(content) => ok_json(json!({
            "id": doc.id,
            "filename": doc.filename,
            "markdown_path": md_path,
            "json_paths": parse_json_paths(doc.json_paths.as_deref()),
            "has_pdf": !doc.original_path.is_empty() && abs_path(&doc.original_path).exists(),
            "content": content
        })),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, format!("读取Markdown文件失败：{}", e)),
    }
}

#[derive(Deserialize)]
pub struct ExportEpubRequest {
    /// 预览里有未保存的编辑时，用屏幕上的内容导出；缺省读取磁盘上的 Markdown
    #[serde(default)]
    pub content: Option<String>,
}

/// POST /api/documents/{id}/export-epub：把 Markdown 导出为 EPUB
/// （按一级标题分章、GFM 表格转 XHTML、图片从图片目录内嵌）
pub async fn export_document_epub(
    State(state): State<AppState>,
    AxumPath(doc_id): AxumPath<i64>,
    Json(body): Json<ExportEpubRequest>,
) -> ApiResult {
    let doc = match state.db.get_document(doc_id) {
        Ok(Some(d)) => d,
        Ok(None) => return err_json(StatusCode::NOT_FOUND, "文档不存在"),
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let cfg = state.cfg();
    let markdown = match body.content.filter(|c| !c.trim().is_empty()) {
        Some(content) => content,
        None => {
            let Some((path, _)) = resolve_md_file(&doc, &cfg) else {
                return err_json(
                    StatusCode::NOT_FOUND,
                    "该文档尚未生成Markdown，请先执行OCR识别",
                );
            };
            match std::fs::read_to_string(&path) {
                Ok(content) => content,
                Err(e) => {
                    return err_json(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("读取Markdown文件失败：{}", e),
                    )
                }
            }
        }
    };
    let export_title = doc.filename.clone();
    // EPUB 打包涉及大量文件读写，放到阻塞线程，避免拖住其他接口
    let result = tokio::task::spawn_blocking(move || {
        crate::epub::export(&cfg, &export_title, &markdown, doc_id, &cfg.epub_folder)
    })
    .await;
    match result {
        Ok(Ok(out)) => ok_json(json!({
            "id": doc_id,
            "filename": out.filename,
            "path": out.path.to_string_lossy(),
            "size": out.size,
            "chapters": out.chapters,
            "images": out.images,
            "missing_images": out.missing_images,
            "message": "EPUB 导出成功"
        })),
        Ok(Err(message)) => err_json(StatusCode::INTERNAL_SERVER_ERROR, message),
        Err(e) => err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("导出任务执行失败：{}", e),
        ),
    }
}

/// POST /api/documents/{id}/export-docx：把 Markdown 导出为 Word（.docx）
/// （标题层级、列表、表格、代码块、引用与正文图片均转成 Word 原生结构）
pub async fn export_document_docx(
    State(state): State<AppState>,
    AxumPath(doc_id): AxumPath<i64>,
    Json(body): Json<ExportEpubRequest>,
) -> ApiResult {
    let doc = match state.db.get_document(doc_id) {
        Ok(Some(d)) => d,
        Ok(None) => return err_json(StatusCode::NOT_FOUND, "文档不存在"),
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let cfg = state.cfg();
    let markdown = match body.content.filter(|c| !c.trim().is_empty()) {
        Some(content) => content,
        None => {
            let Some((path, _)) = resolve_md_file(&doc, &cfg) else {
                return err_json(
                    StatusCode::NOT_FOUND,
                    "该文档尚未生成Markdown，请先执行OCR识别",
                );
            };
            match std::fs::read_to_string(&path) {
                Ok(content) => content,
                Err(e) => {
                    return err_json(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("读取Markdown文件失败：{}", e),
                    )
                }
            }
        }
    };
    let export_title = doc.filename.clone();
    // 逐段解析与图片打包都在阻塞线程执行，避免拖住其他接口
    let export_dir = cfg.epub_folder.clone();
    let result = tokio::task::spawn_blocking(move || {
        crate::docx::export(&cfg, &export_title, &markdown, doc_id, &export_dir)
    })
    .await;
    match result {
        Ok(Ok(out)) => ok_json(json!({
            "id": doc_id,
            "filename": out.filename,
            "path": out.path.to_string_lossy(),
            "size": out.size,
            "paragraphs": out.paragraphs,
            "tables": out.tables,
            "images": out.images,
            "missing_images": out.missing_images,
            "elapsed_ms": out.elapsed_ms,
            "message": "Word 导出成功"
        })),
        Ok(Err(message)) => err_json(StatusCode::INTERNAL_SERVER_ERROR, message),
        Err(e) => err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("导出任务执行失败：{}", e),
        ),
    }
}

/// 提供原始 PDF 文件流，供前端预览弹窗内嵌对比展示
pub async fn get_document_pdf(
    State(state): State<AppState>,
    AxumPath(doc_id): AxumPath<i64>,
) -> Response {
    let doc = match state.db.get_document(doc_id) {
        Ok(Some(d)) => d,
        Ok(None) => return err_json(StatusCode::NOT_FOUND, "文档不存在").into_response(),
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    if doc.original_path.is_empty() {
        return err_json(StatusCode::NOT_FOUND, "该文档没有原始PDF文件").into_response();
    }
    let path = abs_path(&doc.original_path);
    if !path.exists() {
        return err_json(StatusCode::NOT_FOUND, "原始PDF文件不存在或已被移动").into_response();
    }
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            return err_json(StatusCode::INTERNAL_SERVER_ERROR, format!("读取PDF文件失败：{}", e))
                .into_response()
        }
    };
    Response::builder()
        .header(header::CONTENT_TYPE, "application/pdf")
        .header(header::CONTENT_LENGTH, data.len())
        .header(header::CACHE_CONTROL, "private, max-age=300")
        .header(
            header::CONTENT_DISPOSITION,
            format!("inline; filename*=UTF-8''{}", urlencoding_encode(&doc.filename)),
        )
        .body(Body::from(data))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// 文件名转 URL 编码（Content-Disposition 扩展格式），仅保留安全字符
fn urlencoding_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[derive(Deserialize)]
pub struct SaveMarkdownRequest {
    pub content: Option<String>,
}

/// 保存用户在线编辑后的 Markdown 内容（直接写回产物文件）
pub async fn save_document_markdown(
    State(state): State<AppState>,
    AxumPath(doc_id): AxumPath<i64>,
    Json(body): Json<SaveMarkdownRequest>,
) -> ApiResult {
    let doc = match state.db.get_document(doc_id) {
        Ok(Some(d)) => d,
        Ok(None) => return err_json(StatusCode::NOT_FOUND, "文档不存在"),
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let cfg = state.cfg();
    let Some((path, md_path)) = resolve_md_file(&doc, &cfg) else {
        return err_json(StatusCode::NOT_FOUND, "Markdown 文件不存在，可能已被移动或删除");
    };
    let Some(content) = body.content else {
        return err_json(StatusCode::BAD_REQUEST, "content 必须为字符串");
    };
    if let Err(e) = std::fs::write(&path, content.as_bytes()) {
        return err_json(StatusCode::INTERNAL_SERVER_ERROR, format!("保存Markdown失败：{}", e));
    }
    let json_paths = parse_json_paths(doc.json_paths.as_deref());
    let _ = state.db.update_document_markdown_path(doc_id, &md_path, &json_paths);
    ok_json(json!({
        "success": true,
        "id": doc.id,
        "markdown_path": md_path,
        "size": content.len()
    }))
}

// ---------------- 从本地 JSON 重建 Markdown ----------------

enum RebuildKind {
    Success,
    Skipped,
    Failed,
}

struct RebuildOutcome {
    doc_id: i64,
    filename: String,
    kind: RebuildKind,
    reason: Option<String>,
    markdown_path: Option<String>,
    backup_path: Option<String>,
    bytes: usize,
}

impl RebuildOutcome {
    fn skipped(doc_id: i64, filename: &str, reason: impl Into<String>) -> Self {
        Self {
            doc_id,
            filename: filename.to_string(),
            kind: RebuildKind::Skipped,
            reason: Some(reason.into()),
            markdown_path: None,
            backup_path: None,
            bytes: 0,
        }
    }

    fn failed(doc_id: i64, filename: &str, reason: impl Into<String>) -> Self {
        Self {
            doc_id,
            filename: filename.to_string(),
            kind: RebuildKind::Failed,
            reason: Some(reason.into()),
            markdown_path: None,
            backup_path: None,
            bytes: 0,
        }
    }

    fn to_json(&self) -> Value {
        let result = match self.kind {
            RebuildKind::Success => "success",
            RebuildKind::Skipped => "skipped",
            RebuildKind::Failed => "failed",
        };
        json!({
            "doc_id": self.doc_id,
            "filename": self.filename,
            "result": result,
            "reason": self.reason,
            "markdown_path": self.markdown_path,
            "backup_path": self.backup_path,
            "bytes": self.bytes
        })
    }
}

/// 旧 Markdown 备份到同目录：`{原名}.bak.{YYYYMMDD-HHMMSS}`；同一秒重名时追加序号。
/// 备份不自动清理，需要用户自行按时间删除。
fn backup_markdown(path: &Path) -> std::io::Result<String> {
    let Some(file_name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Markdown 文件名为空",
        ));
    };
    let stamp = Local::now().format("%Y%m%d-%H%M%S");
    let mut candidate = path.with_file_name(format!("{}.bak.{}", file_name, stamp));
    let mut seq = 1;
    while candidate.exists() {
        candidate = path.with_file_name(format!("{}.bak.{}-{}", file_name, stamp, seq));
        seq += 1;
    }
    std::fs::copy(path, &candidate)?;
    Ok(candidate.to_string_lossy().to_string())
}

/// 用本地 OCR JSON 重建单篇 Markdown：不调用 MinerU，不触碰分块检查点/批次/OCR 状态字段。
fn rebuild_markdown_for_document(state: &AppState, doc_id: i64) -> RebuildOutcome {
    let doc = match state.db.get_document(doc_id) {
        Ok(Some(d)) => d,
        Ok(None) => return RebuildOutcome::skipped(doc_id, "", "文档不存在"),
        Err(e) => return RebuildOutcome::failed(doc_id, "", format!("读取文档失败：{}", e)),
    };
    let filename = doc.filename.clone();

    // 重建只读本地产物，OCR 仍在跑时结果随时会被流水线覆盖，直接跳过
    if doc.status == "splitting" || doc.status == "ocr_processing" {
        return RebuildOutcome::skipped(
            doc_id,
            &filename,
            "文档正在 OCR 处理中，完成后再重建",
        );
    }

    let json_paths = parse_json_paths(doc.json_paths.as_deref());
    if json_paths.is_empty() {
        return RebuildOutcome::skipped(doc_id, &filename, "缺少 OCR JSON（json_paths 为空）");
    }
    let cfg = state.cfg();
    let missing: Vec<String> = json_paths
        .iter()
        .filter(|p| resolve_json_path(p, &cfg).is_none())
        .map(|p| json_basename(p))
        .collect();
    if !missing.is_empty() {
        return RebuildOutcome::skipped(
            doc_id,
            &filename,
            format!("JSON 文件不存在或不可读：{}", missing.join(", ")),
        );
    }

    let base_name = Path::new(&filename)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| filename.clone());
    let md_path = cfg
        .output_md_folder
        .join(format!("{}__{}.md", base_name, doc_id));
    let tmp_path = PathBuf::from(format!("{}.rebuild.tmp", md_path.to_string_lossy()));

    // 先写临时文件，成功后再替换旧 Markdown，转换失败不会破坏原文件
    let convert_result = if json_paths.len() == 1 {
        convert_json_to_markdown(&json_paths[0], &tmp_path, &cfg)
    } else {
        merge_json_to_markdown(&json_paths, &tmp_path, &cfg)
    };
    if let Err(e) = convert_result {
        let _ = std::fs::remove_file(&tmp_path);
        return RebuildOutcome::failed(doc_id, &filename, format!("生成 Markdown 失败：{}", e));
    }
    let bytes = std::fs::metadata(&tmp_path)
        .map(|m| m.len() as usize)
        .unwrap_or(0);

    let backup_path = if md_path.is_file() {
        match backup_markdown(&md_path) {
            Ok(p) => Some(p),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp_path);
                return RebuildOutcome::failed(
                    doc_id,
                    &filename,
                    format!("备份旧 Markdown 失败：{}", e),
                );
            }
        }
    } else {
        None
    };

    if let Err(e) = std::fs::rename(&tmp_path, &md_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return RebuildOutcome::failed(
            doc_id,
            &filename,
            format!("写入 Markdown 失败：{}", e),
        );
    }

    // 入库用绝对路径：cfg 里的目录可能是相对路径，而数据库读取时按 backend/ 兜底解析
    let stored_path = std::path::absolute(&md_path)
        .unwrap_or_else(|_| md_path.clone())
        .to_string_lossy()
        .to_string();
    if let Err(e) = state
        .db
        .update_document_markdown_path(doc_id, &stored_path, &json_paths)
    {
        return RebuildOutcome {
            doc_id,
            filename,
            kind: RebuildKind::Failed,
            reason: Some(format!("Markdown 已写入，但更新数据库记录失败：{}", e)),
            markdown_path: Some(stored_path),
            backup_path,
            bytes,
        };
    }

    RebuildOutcome {
        doc_id,
        filename,
        kind: RebuildKind::Success,
        reason: None,
        markdown_path: Some(stored_path),
        backup_path,
        bytes,
    }
}

/// POST /api/documents/{id}/rebuild-markdown：单篇从本地 JSON 重建
pub async fn rebuild_document_markdown(
    State(state): State<AppState>,
    AxumPath(doc_id): AxumPath<i64>,
) -> ApiResult {
    let outcome = tokio::task::spawn_blocking(move || rebuild_markdown_for_document(&state, doc_id))
        .await;
    match outcome {
        Ok(out) => {
            let value = out.to_json();
            match out.kind {
                RebuildKind::Success => ok_json(value),
                RebuildKind::Skipped => (StatusCode::CONFLICT, Json(value)),
                RebuildKind::Failed => (StatusCode::INTERNAL_SERVER_ERROR, Json(value)),
            }
        }
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// POST /api/documents/batch/rebuild-markdown：逐篇重建，单篇失败不中断整批
pub async fn batch_rebuild_markdown(
    State(state): State<AppState>,
    Json(body): Json<BatchOcrRequest>,
) -> ApiResult {
    let doc_ids = body.doc_ids.unwrap_or_default();
    if doc_ids.is_empty() {
        return err_json(StatusCode::BAD_REQUEST, "请选择文档");
    }
    let result = tokio::task::spawn_blocking(move || {
        let mut success = 0usize;
        let mut skipped = 0usize;
        let mut failed = 0usize;
        let mut details = Vec::with_capacity(doc_ids.len());
        for doc_id in doc_ids {
            let out = rebuild_markdown_for_document(&state, doc_id);
            match out.kind {
                RebuildKind::Success => success += 1,
                RebuildKind::Skipped => skipped += 1,
                RebuildKind::Failed => failed += 1,
            }
            details.push(out.to_json());
        }
        json!({
            "total": details.len(),
            "success": success,
            "skipped": skipped,
            "failed": failed,
            "details": details
        })
    })
    .await;
    match result {
        Ok(value) => ok_json(value),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn get_document_json(
    State(state): State<AppState>,
    AxumPath((doc_id, index)): AxumPath<(i64, i64)>,
) -> ApiResult {
    let doc = match state.db.get_document(doc_id) {
        Ok(Some(d)) => d,
        Ok(None) => return err_json(StatusCode::NOT_FOUND, "文档不存在"),
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let json_paths = parse_json_paths(doc.json_paths.as_deref());
    if json_paths.is_empty() {
        return err_json(StatusCode::NOT_FOUND, "该文档没有OCR原始JSON结果");
    }
    if index < 0 || index as usize >= json_paths.len() {
        return err_json(
            StatusCode::BAD_REQUEST,
            format!("JSON文件序号超出范围（共{}个）", json_paths.len()),
        );
    }
    let json_path = &json_paths[index as usize];
    let cfg = state.cfg();
    let Some(real) = resolve_json_path(json_path, &cfg) else {
        return err_json(StatusCode::NOT_FOUND, format!("JSON文件不存在：{}", json_path));
    };
    match load_json(&real.to_string_lossy(), &cfg) {
        Ok(data) => {
            let content = serde_json::to_string_pretty(&data).unwrap_or_default();
            ok_json(json!({
                "id": doc.id,
                "index": index,
                "json_filename": json_basename(json_path),
                "json_path": json_path,
                "total": json_paths.len(),
                "content": content
            }))
        }
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, format!("读取JSON文件失败：{}", e)),
    }
}

#[derive(Debug, Deserialize)]
pub struct RenameRequest {
    pub filename: Option<String>,
}

pub async fn rename_document(
    State(state): State<AppState>,
    AxumPath(doc_id): AxumPath<i64>,
    Json(body): Json<RenameRequest>,
) -> ApiResult {
    let doc = match state.db.get_document(doc_id) {
        Ok(Some(d)) => d,
        Ok(None) => return err_json(StatusCode::NOT_FOUND, "文档不存在"),
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    // 与批量重命名共用同一套规则：只校验主名，扩展名统一补齐为 .pdf
    let new_pdf_name = match crate::batch_rename::normalize_new_name(
        body.filename.as_deref().unwrap_or(""),
        false,
    ) {
        Ok(n) => n.name,
        Err(e) => return err_json(StatusCode::BAD_REQUEST, e),
    };
    let base = new_pdf_name
        .strip_suffix(".pdf")
        .unwrap_or(&new_pdf_name)
        .to_string();

    if doc.original_path.is_empty() || !abs_path(&doc.original_path).exists() {
        return err_json(StatusCode::BAD_REQUEST, "原始PDF文件不存在或已移动，无法重命名");
    }
    let original = abs_path(&doc.original_path);
    let Some(parent) = original.parent() else {
        return err_json(StatusCode::BAD_REQUEST, "原始PDF文件不存在或已移动，无法重命名");
    };
    let new_pdf_path = parent.join(&new_pdf_name);

    let md_path = doc.markdown_path.clone().filter(|p| abs_path(p).exists());
    let md_needs_rename = md_path.is_some();
    let mut new_md_path = doc.markdown_path.clone();
    if let Some(md) = &md_path {
        let md_file = abs_path(md);
        let md_name = if md_file
            .file_name()
            .map(|n| n.to_string_lossy().contains(&format!("__{}.md", doc_id)))
            .unwrap_or(false)
        {
            format!("{}__{}.md", base, doc_id)
        } else {
            format!("{}.md", base)
        };
        new_md_path = md_file
            .parent()
            .map(|p| p.join(&md_name).to_string_lossy().to_string())
            .or_else(|| Some(md_name));
    }

    let new_pdf_str = new_pdf_path.to_string_lossy().to_string();
    if !same_path(&new_pdf_str, &doc.original_path) && new_pdf_path.exists() {
        return err_json(StatusCode::BAD_REQUEST, format!("同目录下已存在文件：{}", new_pdf_name));
    }
    if let (Some(new_md), Some(old_md)) = (&new_md_path, &md_path) {
        if !same_path(new_md, old_md) && abs_path(new_md).exists() {
            return err_json(
                StatusCode::BAD_REQUEST,
                format!("Markdown文件已存在：{}", json_basename(new_md)),
            );
        }
    }

    let mut pdf_renamed = false;
    if !same_path(&new_pdf_str, &doc.original_path) {
        if let Err(e) = std::fs::rename(abs_path(&doc.original_path), &new_pdf_str) {
            return err_json(StatusCode::INTERNAL_SERVER_ERROR, format!("文件重命名失败：{}", e));
        }
        pdf_renamed = true;
    }
    if let (Some(new_md), Some(old_md)) = (&new_md_path, &md_path) {
        if !same_path(new_md, old_md) {
            if let Err(e) = std::fs::rename(abs_path(old_md), abs_path(new_md)) {
                if pdf_renamed {
                    let _ = std::fs::rename(&new_pdf_str, abs_path(&doc.original_path));
                }
                return err_json(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("文件重命名失败：{}", e),
                );
            }
        }
    }

    let md_to_store = if md_needs_rename { new_md_path.as_deref() } else { None };
    if let Err(e) = state
        .db
        .rename_document(doc_id, &new_pdf_name, &new_pdf_str, md_to_store)
    {
        return err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    ok_json(json!({
        "message": "重命名成功",
        "filename": new_pdf_name,
        "original_path": new_pdf_str,
        "markdown_path": md_to_store.map(|s| s.to_string()).or(doc.markdown_path)
    }))
}

// ---------------- 批量重命名 ----------------

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct BatchRenameRequest {
    #[serde(default)]
    pub doc_ids: Vec<i64>,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub find: String,
    #[serde(default)]
    pub replace: String,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default = "default_true")]
    pub main_name_only: bool,
    #[serde(default)]
    pub clean_whitespace: bool,
    /// 手动指定某几行的新文件名（优先于规则结果）
    #[serde(default)]
    pub overrides: Vec<RenameOverride>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RenameOverride {
    pub doc_id: i64,
    pub new_filename: String,
}

struct RenamePlanRow {
    doc_id: i64,
    old_filename: String,
    new_filename: String,
    status: &'static str,
    reason: String,
    pdf_old: Option<PathBuf>,
    pdf_new: Option<PathBuf>,
    md_old: Option<PathBuf>,
    md_new: Option<PathBuf>,
}

impl RenamePlanRow {
    fn to_json(&self) -> Value {
        json!({
            "doc_id": self.doc_id,
            "old_filename": self.old_filename,
            "new_filename": self.new_filename,
            "status": self.status,
            "reason": self.reason
        })
    }
}

/// 计算改名后的 Markdown 目标路径，沿用 `{basename}__{doc_id}.md` 约定
fn rename_md_target(doc: &Document, base: &str) -> Option<(PathBuf, PathBuf)> {
    let md = doc.markdown_path.as_deref().filter(|p| !p.trim().is_empty())?;
    let md_file = abs_path(md);
    if !md_file.is_file() {
        return None;
    }
    let name = if md_file
        .file_name()
        .map(|n| n.to_string_lossy().contains(&format!("__{}.md", doc.id)))
        .unwrap_or(false)
    {
        format!("{}__{}.md", base, doc.id)
    } else {
        format!("{}.md", base)
    };
    let target = md_file
        .parent()
        .map(|p| p.join(&name))
        .unwrap_or_else(|| PathBuf::from(&name));
    Some((md_file, target))
}

fn path_readonly(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.permissions().readonly())
        .unwrap_or(false)
}

/// 预览与执行共用同一套计划计算，执行前重新计算以避免预览后文件被外部改动
fn build_rename_plan(
    state: &AppState,
    req: &BatchRenameRequest,
) -> Result<Vec<RenamePlanRow>, String> {
    if req.doc_ids.is_empty() {
        return Err("请先勾选需要重命名的文档".to_string());
    }
    let rule = crate::batch_rename::RenameRule {
        mode: req.mode.clone(),
        find: req.find.clone(),
        replace: req.replace.clone(),
        case_sensitive: req.case_sensitive,
        main_name_only: req.main_name_only,
        clean_whitespace: req.clean_whitespace,
    };
    let re = crate::batch_rename::build_regex(&rule)?;
    let overrides: std::collections::HashMap<i64, String> = req
        .overrides
        .iter()
        .map(|o| (o.doc_id, o.new_filename.clone()))
        .collect();
    let mut rows: Vec<RenamePlanRow> = Vec::with_capacity(req.doc_ids.len());

    for doc_id in &req.doc_ids {
        let doc = match state.db.get_document(*doc_id) {
            Ok(Some(d)) => d,
            Ok(None) => {
                rows.push(RenamePlanRow {
                    doc_id: *doc_id,
                    old_filename: String::new(),
                    new_filename: String::new(),
                    status: "missing",
                    reason: "文档不存在或已被删除".to_string(),
                    pdf_old: None,
                    pdf_new: None,
                    md_old: None,
                    md_new: None,
                });
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };

        let old_pdf = if doc.original_path.trim().is_empty() {
            None
        } else {
            let p = abs_path(&doc.original_path);
            p.is_file().then_some(p)
        };
        let (mut new_filename, mut status, mut reason) = match &re {
            None => (doc.filename.clone(), "unchanged", "未填写查找内容".to_string()),
            Some(_) => match crate::batch_rename::renamed_raw(&doc.filename, &rule) {
                Err(e) => (doc.filename.clone(), "invalid", e),
                // 校验失败时把“替换后的候选名”带进表格，方便定位是哪个字符有问题
                Ok(raw) => match crate::batch_rename::normalize_new_name(&raw, rule.clean_whitespace) {
                    Err(e) => (raw, "invalid", e),
                    Ok(n) if n.name == doc.filename => {
                        (n.name, "unchanged", "名称未发生变化".to_string())
                    }
                    Ok(n) => (n.name, "ok", n.note),
                },
            },
        };
        // 手动指定的新名优先于规则结果，按与预览一致的规则校验（不带 .pdf 自动补全）
        if let Some(manual) = overrides.get(&doc.id) {
            match crate::batch_rename::normalize_new_name(manual, false) {
                Ok(n) if n.name == doc.filename => {
                    new_filename = n.name;
                    status = "unchanged";
                    reason = "手动指定：名称未发生变化".to_string();
                }
                Ok(n) => {
                    new_filename = n.name;
                    status = "ok";
                    reason = if n.note.is_empty() {
                        "手动指定".to_string()
                    } else {
                        format!("手动指定；{}", n.note)
                    };
                }
                Err(e) => {
                    new_filename = manual.clone();
                    status = "invalid";
                    reason = format!("手动指定被拒绝：{}", e);
                }
            }
        }

        let mut row = RenamePlanRow {
            doc_id: doc.id,
            old_filename: doc.filename.clone(),
            new_filename: new_filename.clone(),
            status,
            reason,
            pdf_old: old_pdf.clone(),
            pdf_new: None,
            md_old: None,
            md_new: None,
        };

        if row.status == "ok" {
            let Some(pdf_old) = old_pdf.clone() else {
                row.status = "missing";
                row.reason = "原始 PDF 不存在或已被移动".to_string();
                rows.push(row);
                continue;
            };
            let Some(parent) = pdf_old.parent() else {
                row.status = "missing";
                row.reason = "原始 PDF 路径无效".to_string();
                rows.push(row);
                continue;
            };
            if path_readonly(&pdf_old) || path_readonly(parent) {
                row.status = "readonly";
                row.reason = "文件或所在目录不可写".to_string();
                rows.push(row);
                continue;
            }
            let base = new_filename
                .strip_suffix(".pdf")
                .unwrap_or(&new_filename)
                .to_string();
            let (md_old, md_new) = match rename_md_target(&doc, &base) {
                Some((a, b)) => (Some(a), Some(b)),
                None => (None, None),
            };
            row.pdf_new = Some(parent.join(&new_filename));
            row.md_old = md_old;
            row.md_new = md_new;
        }
        rows.push(row);
    }

    // 批内重名：同名（大小写不敏感）的所有行都标记冲突，并指出撞的是第几行
    let mut counts: std::collections::HashMap<String, Vec<usize>> = std::collections::HashMap::new();
    for (idx, row) in rows.iter().enumerate().filter(|(_, r)| r.status == "ok") {
        counts
            .entry(row.new_filename.to_lowercase())
            .or_default()
            .push(idx);
    }
    for idx in 0..rows.len() {
        if rows[idx].status != "ok" {
            continue;
        }
        let lower = rows[idx].new_filename.to_lowercase();
        if let Some(list) = counts.get(&lower) {
            if list.len() > 1 {
                let other = list.iter().copied().find(|i| *i != idx).unwrap_or(idx);
                rows[idx].status = "conflict";
                rows[idx].reason = format!(
                    "与本批第 {} 行重名（同名：{}）",
                    other + 1,
                    rows[idx].new_filename
                );
                continue;
            }
        }
        match state.db.document_ids_by_filename(&rows[idx].new_filename) {
            Ok(ids) => {
                if let Some(other_id) = ids.iter().find(|id| **id != rows[idx].doc_id) {
                    let other_name = state
                        .db
                        .get_document(*other_id)
                        .ok()
                        .flatten()
                        .map(|d| d.filename)
                        .unwrap_or_default();
                    rows[idx].status = "conflict";
                    rows[idx].reason = format!(
                        "库内已有同名文档：{}（ID {}）",
                        other_name, other_id
                    );
                    continue;
                }
            }
            Err(e) => return Err(e.to_string()),
        }
        if let Some(pdf_new) = &rows[idx].pdf_new {
            let exists = pdf_new.exists();
            let is_self = rows[idx]
                .pdf_old
                .as_ref()
                .map(|p| p.to_string_lossy().to_lowercase() == pdf_new.to_string_lossy().to_lowercase())
                .unwrap_or(false);
            if exists && !is_self {
                rows[idx].status = "conflict";
                rows[idx].reason = "同目录下已存在同名文件".to_string();
                continue;
            }
        }
        if let (Some(md_old), Some(md_new)) = (&rows[idx].md_old, &rows[idx].md_new) {
            if md_old != md_new && md_new.exists() {
                rows[idx].status = "conflict";
                rows[idx].reason = "目标 Markdown 文件已存在".to_string();
            }
        }
    }

    Ok(rows)
}

fn summarize_rename_rows(rows: &[RenamePlanRow]) -> Value {
    let count = |s: &str| rows.iter().filter(|r| r.status == s).count();
    json!({
        "total": rows.len(),
        "ok": count("ok"),
        "unchanged": count("unchanged"),
        "conflict": count("conflict"),
        "invalid": count("invalid"),
        "missing": count("missing"),
        "readonly": count("readonly")
    })
}

pub async fn batch_rename_preview(
    State(state): State<AppState>,
    Json(body): Json<BatchRenameRequest>,
) -> ApiResult {
    match build_rename_plan(&state, &body) {
        Ok(rows) => ok_json(json!({
            "items": rows.iter().map(|r| r.to_json()).collect::<Vec<_>>(),
            "summary": summarize_rename_rows(&rows)
        })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, e),
    }
}

/// 单条执行：先改 PDF，再改 Markdown，最后事务更新数据库；任一步失败回滚已做的文件改名
fn execute_rename_row(state: &AppState, row: &RenamePlanRow) -> Result<(), String> {
    let pdf_old = row.pdf_old.as_ref().ok_or_else(|| "缺少原文件路径".to_string())?;
    let pdf_new = row.pdf_new.as_ref().ok_or_else(|| "缺少目标路径".to_string())?;
    let pdf_moved = pdf_old.to_string_lossy().to_lowercase() != pdf_new.to_string_lossy().to_lowercase();
    if pdf_moved {
        std::fs::rename(pdf_old, pdf_new).map_err(|e| format!("重命名 PDF 失败：{}", e))?;
    }

    let mut md_moved = false;
    if let (Some(md_old), Some(md_new)) = (&row.md_old, &row.md_new) {
        if md_old != md_new {
            if let Err(e) = std::fs::rename(md_old, md_new) {
                if pdf_moved {
                    let _ = std::fs::rename(pdf_new, pdf_old);
                }
                return Err(format!("重命名 Markdown 失败：{}", e));
            }
            md_moved = true;
        }
    }

    let md_store = if row.md_old.is_some() {
        row.md_new
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
    } else {
        None
    };
    if let Err(e) = state.db.rename_document_with_index(
        row.doc_id,
        &row.new_filename,
        &pdf_new.to_string_lossy(),
        md_store.as_deref(),
    ) {
        if md_moved {
            if let (Some(a), Some(b)) = (&row.md_old, &row.md_new) {
                let _ = std::fs::rename(b, a);
            }
        }
        if pdf_moved {
            let _ = std::fs::rename(pdf_new, pdf_old);
        }
        return Err(format!("更新数据库失败：{}", e));
    }
    Ok(())
}

pub async fn batch_rename(
    State(state): State<AppState>,
    Json(body): Json<BatchRenameRequest>,
) -> ApiResult {
    let rows = match build_rename_plan(&state, &body) {
        Ok(rows) => rows,
        Err(e) => return err_json(StatusCode::BAD_REQUEST, e),
    };
    let mut items = Vec::with_capacity(rows.len());
    let (mut success, mut failed, mut skipped) = (0usize, 0usize, 0usize);
    let mut skipped_by: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for row in &rows {
        if row.status != "ok" {
            skipped += 1;
            *skipped_by.entry(row.status).or_insert(0) += 1;
            items.push(json!({
                "doc_id": row.doc_id,
                "old_filename": row.old_filename,
                "new_filename": row.new_filename,
                "status": row.status,
                "ok": false,
                "error": row.reason
            }));
            continue;
        }
        match execute_rename_row(&state, row) {
            Ok(()) => {
                success += 1;
                items.push(json!({
                    "doc_id": row.doc_id,
                    "old_filename": row.old_filename,
                    "new_filename": row.new_filename,
                    "status": "renamed",
                    "ok": true,
                    "error": ""
                }));
            }
            Err(e) => {
                failed += 1;
                crate::log_error!("[批量重命名] 文档 {} 失败：{}", row.doc_id, e);
                items.push(json!({
                    "doc_id": row.doc_id,
                    "old_filename": row.old_filename,
                    "new_filename": row.new_filename,
                    "status": "failed",
                    "ok": false,
                    "error": e
                }));
            }
        }
    }
    let pending = state.db.pending_index_count().unwrap_or(0);
    let n = |k: &str| *skipped_by.get(k).unwrap_or(&0);
    ok_json(json!({
        "items": items,
        "summary": {
            "success": success,
            "failed": failed,
            "skipped": skipped,
            "skipped_conflict": n("conflict"),
            "skipped_invalid": n("invalid"),
            "skipped_missing": n("missing"),
            "skipped_readonly": n("readonly"),
            "skipped_unchanged": n("unchanged"),
            "total": rows.len()
        },
        "index_pending": pending
    }))
}

#[derive(Debug, Deserialize)]
pub struct MoveRequest {
    pub path: Option<String>,
}pub async fn move_document(
    State(state): State<AppState>,
    AxumPath(doc_id): AxumPath<i64>,
    Json(body): Json<MoveRequest>,
) -> ApiResult {
    let doc = match state.db.get_document(doc_id) {
        Ok(Some(d)) => d,
        Ok(None) => return err_json(StatusCode::NOT_FOUND, "文档不存在"),
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let target_raw = body.path.as_deref().unwrap_or("").trim().to_string();
    if target_raw.is_empty() {
        return err_json(StatusCode::BAD_REQUEST, "目标路径不能为空");
    }
    let target_path = PathBuf::from(&target_raw);
    if !target_raw.to_lowercase().ends_with(".pdf") {
        return err_json(StatusCode::BAD_REQUEST, "目标路径必须以 .pdf 结尾");
    }
    let Some(new_pdf_name) = target_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
    else {
        return err_json(StatusCode::BAD_REQUEST, "目标路径缺少文件名");
    };
    if new_pdf_name.chars().any(|c| "*?\"<>|".contains(c)) {
        return err_json(StatusCode::BAD_REQUEST, "文件名不能包含 * ? \" < > | 字符");
    }
    let Some(target_dir) = abs_path(&target_raw).parent().map(|p| p.to_path_buf()) else {
        return err_json(StatusCode::BAD_REQUEST, "目标路径缺少目录部分");
    };
    if target_dir.as_os_str().is_empty() {
        return err_json(StatusCode::BAD_REQUEST, "目标路径缺少目录部分");
    }
    if let Err(e) = std::fs::create_dir_all(&target_dir) {
        return err_json(StatusCode::INTERNAL_SERVER_ERROR, format!("创建目标目录失败：{}", e));
    }
    if doc.original_path.is_empty() || !abs_path(&doc.original_path).exists() {
        return err_json(StatusCode::BAD_REQUEST, "原始PDF文件不存在或已移动，无法移动");
    }
    let abs_target = abs_path(&target_raw);
    let abs_original = abs_path(&doc.original_path);
    if abs_target != abs_original && abs_target.exists() {
        return err_json(StatusCode::BAD_REQUEST, format!("目标路径已存在文件：{}", new_pdf_name));
    }

    let base = &new_pdf_name[..new_pdf_name.len() - 4];
    let md_path = doc.markdown_path.clone().filter(|p| abs_path(p).exists());
    let md_needs_rename = md_path.is_some();
    let mut new_md_path = doc.markdown_path.clone();
    if let Some(md) = &md_path {
        let md_file = abs_path(md);
        let md_name = if md_file
            .file_name()
            .map(|n| n.to_string_lossy().contains(&format!("__{}.md", doc_id)))
            .unwrap_or(false)
        {
            format!("{}__{}.md", base, doc_id)
        } else {
            format!("{}.md", base)
        };
        new_md_path = md_file
            .parent()
            .map(|p| p.join(&md_name).to_string_lossy().to_string())
            .or_else(|| Some(md_name));
        if let Some(new_md) = &new_md_path {
            if !same_path(new_md, md) && abs_path(new_md).exists() {
                return err_json(
                    StatusCode::BAD_REQUEST,
                    format!("Markdown文件已存在：{}", json_basename(new_md)),
                );
            }
        }
    }

    let same_drive = abs_target
        .to_string_lossy()
        .chars()
        .take(2)
        .collect::<String>()
        .to_lowercase()
        == abs_original
            .to_string_lossy()
            .chars()
            .take(2)
            .collect::<String>()
            .to_lowercase();
    let target_str = abs_target.to_string_lossy().to_string();
    let original_abs = abs_original.to_string_lossy().to_string();
    let mut moved = false;
    if abs_target != abs_original {
        let result = if same_drive {
            std::fs::rename(&original_abs, &target_str)
        } else {
            std::fs::copy(&original_abs, &target_str).and_then(|_| {
                std::fs::remove_file(&original_abs)
            })
        };
        if let Err(e) = result {
            return err_json(StatusCode::INTERNAL_SERVER_ERROR, format!("文件移动失败：{}", e));
        }
        moved = true;
    }

    if let (Some(new_md), Some(old_md)) = (&new_md_path, &md_path) {
        if !same_path(new_md, old_md) {
            if let Err(e) = std::fs::rename(abs_path(old_md), abs_path(new_md)) {
                if moved {
                    let rollback = if same_drive {
                        std::fs::rename(&target_str, &original_abs)
                    } else {
                        std::fs::copy(&target_str, &original_abs)
                            .and_then(|_| std::fs::remove_file(&target_str))
                    };
                    let _ = rollback;
                }
                return err_json(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("文件移动失败：{}", e),
                );
            }
        }
    }

    let md_to_store = if md_needs_rename { new_md_path.as_deref() } else { None };
    if let Err(e) = state
        .db
        .rename_document(doc_id, &new_pdf_name, &target_str, md_to_store)
    {
        return err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    ok_json(json!({
        "message": "移动成功",
        "filename": new_pdf_name,
        "original_path": target_str,
        "markdown_path": md_to_store.map(|s| s.to_string()).or(doc.markdown_path)
    }))
}

pub async fn process_ocr(
    State(state): State<AppState>,
    AxumPath(doc_id): AxumPath<i64>,
) -> ApiResult {
    let doc = match state.db.get_document(doc_id) {
        Ok(Some(d)) => d,
        Ok(None) => return err_json(StatusCode::NOT_FOUND, "文档不存在"),
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    if doc.ocr_disabled {
        return err_json(
            StatusCode::FORBIDDEN,
            "该文档已禁止OCR（ocr_disabled），如需识别请先在列表中开启OCR开关",
        );
    }
    match crate::ocr::submit_ocr(&state, doc_id) {
        Ok((true, _)) => (
            StatusCode::ACCEPTED,
            Json(json!({"message": "OCR任务已提交", "doc_id": doc_id, "status": "splitting"})),
        ),
        Ok((false, Some("processing"))) => {
            err_json(StatusCode::CONFLICT, "文档正在处理中，请勿重复提交")
        }
        Ok((false, _)) => err_json(StatusCode::BAD_REQUEST, "任务提交失败"),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Debug, Deserialize)]
pub struct OcrDisabledRequest {
    pub ocr_disabled: Option<Value>,
}

pub async fn set_ocr_disabled(
    State(state): State<AppState>,
    AxumPath(doc_id): AxumPath<i64>,
    Json(body): Json<OcrDisabledRequest>,
) -> ApiResult {
    let doc = match state.db.get_document(doc_id) {
        Ok(Some(d)) => d,
        Ok(None) => return err_json(StatusCode::NOT_FOUND, "文档不存在"),
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let Some(disabled) = body.ocr_disabled.as_ref().and_then(|v| v.as_bool()) else {
        return err_json(StatusCode::BAD_REQUEST, "ocr_disabled 必须为布尔值");
    };
    if doc.status == "splitting" || doc.status == "ocr_processing" {
        return err_json(StatusCode::CONFLICT, "文档正在处理中，无法更改OCR开关");
    }
    match state.db.set_ocr_disabled(doc_id, disabled) {
        Ok(_) => ok_json(json!({"id": doc_id, "ocr_disabled": disabled, "message": "已更新"})),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn delete_document(
    State(state): State<AppState>,
    AxumPath(doc_id): AxumPath<i64>,
) -> ApiResult {
    let doc = match state.db.get_document(doc_id) {
        Ok(Some(d)) => d,
        Ok(None) => return err_json(StatusCode::NOT_FOUND, "文档不存在"),
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    if !doc.original_path.is_empty() && abs_path(&doc.original_path).exists() {
        let _ = std::fs::remove_file(abs_path(&doc.original_path));
    }
    if let Some(md) = &doc.markdown_path {
        if abs_path(md).exists() {
            let _ = std::fs::remove_file(abs_path(md));
        }
    }
    let cfg = state.cfg();
    for jp in parse_json_paths(doc.json_paths.as_deref()) {
        if let Some(real) = resolve_json_path(&jp, &cfg) {
            let _ = std::fs::remove_file(real);
        }
    }
    if let Ok(chunks) = state.db.get_chunks_by_document(doc_id) {
        for chunk in chunks {
            if abs_path(&chunk.chunk_path).exists() {
                let _ = std::fs::remove_file(abs_path(&chunk.chunk_path));
            }
            if let Some(jp) = &chunk.json_path {
                if let Some(real) = resolve_json_path(jp, &cfg) {
                    let _ = std::fs::remove_file(real);
                }
            }
        }
    }
    match state.db.delete_document(doc_id) {
        Ok(_) => ok_json(json!({"message": "删除成功"})),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Debug, Deserialize)]
pub struct BatchOcrRequest {
    pub doc_ids: Option<Vec<i64>>,
}

pub async fn batch_ocr(State(state): State<AppState>, Json(body): Json<BatchOcrRequest>) -> ApiResult {
    let doc_ids = body.doc_ids.unwrap_or_default();
    if doc_ids.is_empty() {
        return err_json(StatusCode::BAD_REQUEST, "请选择文档");
    }
    let mut details = Vec::new();
    let mut accepted = 0;
    let mut skipped = 0;
    for doc_id in doc_ids {
        match crate::ocr::submit_ocr(&state, doc_id) {
            Ok((true, _)) => {
                accepted += 1;
                details.push(json!({"doc_id": doc_id, "result": "accepted"}));
            }
            Ok((false, reason)) => {
                skipped += 1;
                let reason_text = if reason == Some("not_found") {
                    "文档不存在"
                } else {
                    "文档正在处理中"
                };
                details.push(json!({"doc_id": doc_id, "result": "skipped", "reason": reason_text}));
            }
            Err(e) => {
                skipped += 1;
                details.push(json!({"doc_id": doc_id, "result": "skipped", "reason": e.to_string()}));
            }
        }
    }
    ok_json(json!({"accepted": accepted, "skipped": skipped, "details": details}))
}

#[derive(Debug, Deserialize)]
pub struct BatchOcrDisabledRequest {
    pub doc_ids: Option<Vec<i64>>,
    /// true=批量禁用 OCR，false=批量启用 OCR
    pub disabled: Option<bool>,
}

/// 批量设置 OCR 开关：直接写目标值，不因同值拒绝；仅文档不存在时跳过
pub async fn batch_set_ocr_disabled(
    State(state): State<AppState>,
    Json(body): Json<BatchOcrDisabledRequest>,
) -> ApiResult {
    let doc_ids = body.doc_ids.unwrap_or_default();
    if doc_ids.is_empty() {
        return err_json(StatusCode::BAD_REQUEST, "请选择文档");
    }
    let Some(disabled) = body.disabled else {
        return err_json(StatusCode::BAD_REQUEST, "缺少 disabled 参数（true=禁用，false=启用）");
    };
    let (mut success, mut failed, mut skipped_missing) = (0usize, 0usize, 0usize);
    let mut details = Vec::with_capacity(doc_ids.len());
    for doc_id in doc_ids {
        match state.db.get_document(doc_id) {
            Ok(None) => {
                skipped_missing += 1;
                details.push(json!({"doc_id": doc_id, "result": "skipped", "reason": "文档不存在"}));
                continue;
            }
            Err(e) => {
                failed += 1;
                details.push(json!({"doc_id": doc_id, "result": "failed", "reason": e.to_string()}));
                continue;
            }
            Ok(Some(_)) => {}
        }
        // 直接写目标值：同值重复执行幂等；处理中的文档只改标志，不打断在跑任务
        match state.db.set_ocr_disabled(doc_id, disabled) {
            Ok(_) => {
                success += 1;
                details.push(json!({"doc_id": doc_id, "result": "updated", "disabled": disabled}));
            }
            Err(e) => {
                failed += 1;
                details.push(json!({"doc_id": doc_id, "result": "failed", "reason": e.to_string()}));
            }
        }
    }
    ok_json(json!({
        "success": success,
        "failed": failed,
        "skipped": skipped_missing,
        "skipped_missing": skipped_missing,
        "disabled": disabled,
        "details": details
    }))
}

#[derive(Debug, Deserialize)]
pub struct BatchRatingRequest {
    pub doc_ids: Option<Vec<i64>>,
    pub rating: i64,
}

/// 批量评分：取值范围与单篇接口一致（0~10，0 表示未评分）
pub async fn batch_set_rating(
    State(state): State<AppState>,
    Json(body): Json<BatchRatingRequest>,
) -> ApiResult {
    let doc_ids = body.doc_ids.unwrap_or_default();
    if doc_ids.is_empty() {
        return err_json(StatusCode::BAD_REQUEST, "请选择文档");
    }
    if !(0..=10).contains(&body.rating) {
        return err_json(StatusCode::BAD_REQUEST, "评分必须是 0~10 的整数");
    }
    let (mut success, mut failed, mut missing) = (0usize, 0usize, 0usize);
    let mut details = Vec::with_capacity(doc_ids.len());
    for doc_id in doc_ids {
        match state.db.set_rating(doc_id, body.rating) {
            Ok(true) => {
                success += 1;
                details.push(json!({"doc_id": doc_id, "result": "ok", "rating": body.rating}));
            }
            Ok(false) => {
                missing += 1;
                details.push(json!({"doc_id": doc_id, "result": "skipped", "reason": "文档不存在"}));
            }
            Err(e) => {
                failed += 1;
                details.push(json!({"doc_id": doc_id, "result": "failed", "reason": e.to_string()}));
            }
        }
    }
    ok_json(json!({
        "success": success,
        "failed": failed,
        "skipped": missing,
        "skipped_missing": missing,
        "rating": body.rating,
        "details": details
    }))
}

pub async fn scheduler_status(State(state): State<AppState>) -> ApiResult {    ok_json(serde_json::to_value(crate::ocr::get_scheduler_status(&state)).unwrap_or_default())
}

pub async fn scheduler_start(State(state): State<AppState>) -> ApiResult {
    crate::ocr::start_scheduler(state.clone());
    let mut value =
        serde_json::to_value(crate::ocr::get_scheduler_status(&state)).unwrap_or_default();
    if let Some(obj) = value.as_object_mut() {
        obj.insert("message".into(), json!("解耦流水线已启动"));
    }
    ok_json(value)
}

pub async fn scheduler_stop(State(state): State<AppState>) -> ApiResult {
    crate::ocr::stop_scheduler(&state);
    let mut value =
        serde_json::to_value(crate::ocr::get_scheduler_status(&state)).unwrap_or_default();
    if let Some(obj) = value.as_object_mut() {
        obj.insert("message".into(), json!("解耦流水线已暂停"));
    }
    ok_json(value)
}

/// GET /api/settings：读取配置（token 仅返回掩码 + 目录占用统计）
pub async fn get_settings(State(state): State<AppState>) -> ApiResult {
    let cfg = state.cfg();
    match serde_json::to_value(crate::settings::describe(&cfg)) {
        Ok(value) => ok_json(value),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// PUT /api/settings：校验后持久化到 app_meta 并热更新运行配置
pub async fn update_settings(
    State(state): State<AppState>,
    Json(req): Json<crate::settings::SettingsUpdate>,
) -> ApiResult {
    let current = state.cfg();
    // 构建线程会持续使用索引目录，构建期间禁止切换，避免新旧目录交叉
    if let Some(raw) = req.search_index_folder.as_deref() {
        if !raw.trim().is_empty() {
            if let Ok(path) = crate::settings::validate_folder(raw) {
                if !same_path(
                    &path.to_string_lossy(),
                    &current.search_index_folder.to_string_lossy(),
                ) && state.search.is_running()
                {
                    return err_json(
                        StatusCode::CONFLICT,
                        "索引正在构建，请等待完成后再修改索引目录",
                    );
                }
            }
        }
    }
    match crate::settings::save(&state.db, &current, &req) {
        Ok(next) => {
            let index_dir_changed = !same_path(
                &current.search_index_folder.to_string_lossy(),
                &next.search_index_folder.to_string_lossy(),
            );
            state.set_cfg(next.clone());
            // 目录变了立即丢弃旧索引：若新目录已有可用索引则加载，否则状态变为需重建
            let index_loaded = if index_dir_changed {
                let loaded = state.search.reload_index_dir();
                if !loaded {
                    state
                        .search
                        .mark_needs_rebuild(&current.search_index_folder);
                }
                Some(loaded)
            } else {
                None
            };
            let status = state.search.status();
            let mut value = serde_json::to_value(crate::settings::describe(&next))
                .unwrap_or_else(|_| json!({}));
            if let Some(obj) = value.as_object_mut() {
                let message = if index_dir_changed {
                    if index_loaded == Some(true) {
                        "设置已保存并生效；索引目录已变更，已加载新目录中的索引"
                    } else {
                        "设置已保存并生效；索引目录已变更，需要重建索引"
                    }
                } else {
                    "设置已保存并生效"
                };
                obj.insert("message".into(), json!(message));
                obj.insert("index_dir_changed".into(), json!(index_dir_changed));
                obj.insert("needs_rebuild".into(), json!(status.needs_rebuild));
                obj.insert("index_state".into(), json!(status.state));
            }
            ok_json(value)
        }
        Err(message) => err_json(StatusCode::BAD_REQUEST, message),
    }
}

#[derive(Deserialize)]
pub struct ConvertZhRequest {
    pub content: String,
    /// t2s = 繁→简（应用自定义映射），s2t = 简→繁
    pub direction: String,
}

/// POST /api/convert-zh：繁简中文转换（预览弹窗工具栏调用）
/// 词库来自 zhconv（MediaWiki/OpenCC 合并表），繁→简时优先应用用户自定义映射
pub async fn convert_zh(
    State(state): State<AppState>,
    Json(body): Json<ConvertZhRequest>,
) -> ApiResult {
    if body.content.is_empty() {
        return err_json(StatusCode::BAD_REQUEST, "内容不能为空");
    }
    let direction = match body.direction.as_str() {
        "t2s" | "s2t" => body.direction,
        _ => return err_json(StatusCode::BAD_REQUEST, "direction 仅支持 t2s 或 s2t"),
    };
    // 大文档转换在阻塞线程执行，避免占用异步调度
    let mapping = state.cfg().zh_t2s_mapping;
    let content = body.content;
    let dir_for_task = direction.clone();
    match tokio::task::spawn_blocking(move || {
        crate::zh_convert::convert(&content, &dir_for_task, &mapping)
    })
    .await
    {
        Ok(Ok(converted)) => ok_json(json!({
            "content": converted,
            "direction": direction,
            "chars": converted.chars().count()
        })),
        Ok(Err(message)) => err_json(StatusCode::BAD_REQUEST, message),
        Err(e) => err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("转换任务执行失败：{}", e),
        ),
    }
}

fn image_content_type(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()) {
        Some(e) if e == "png" => "image/png",
        Some(e) if e == "gif" => "image/gif",
        Some(e) if e == "webp" => "image/webp",
        Some(e) if e == "bmp" => "image/bmp",
        Some(e) if e == "svg" => "image/svg+xml",
        _ => "image/jpeg",
    }
}

/// GET /api/images/{name}：只读图片目录内的单个文件，文件名白名单 + 目录校验
pub async fn get_image(State(state): State<AppState>, AxumPath(name): AxumPath<String>) -> Response {
    if !crate::mineru::is_safe_image_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid image name").into_response();
    }
    let cfg = state.cfg();
    let Ok(base) = std::fs::canonicalize(&cfg.image_folder) else {
        return (StatusCode::NOT_FOUND, "image folder not ready").into_response();
    };
    let Ok(real) = std::fs::canonicalize(base.join(&name)) else {
        return (StatusCode::NOT_FOUND, "image not found").into_response();
    };
    // 只允许图片目录下的直接子文件，杜绝路径穿越
    if real.parent() != Some(base.as_path()) {
        return (StatusCode::NOT_FOUND, "image not found").into_response();
    }
    match std::fs::read(&real) {
        Ok(bytes) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, image_content_type(&name)),
                (
                    header::CACHE_CONTROL,
                    "public, max-age=31536000, immutable",
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "image not found").into_response(),
    }
}

/// POST /api/ocr/recover：手动触发断点续跑（续传上传中批次并收割一次）
pub async fn ocr_recover(State(state): State<AppState>) -> ApiResult {
    match crate::ocr::resume_interrupted_ocr(state.clone()).await {
        Ok(summary) => {
            let mut value = serde_json::to_value(summary).unwrap_or_default();
            let status =
                serde_json::to_value(crate::ocr::get_scheduler_status(&state)).unwrap_or_default();
            if let (Some(obj), Some(status_obj)) = (value.as_object_mut(), status.as_object()) {
                for (k, v) in status_obj {
                    obj.insert(k.clone(), v.clone());
                }
                obj.insert("message".into(), json!("断点续跑恢复完成"));
            }
            ok_json(value)
        }
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn get_tags(State(state): State<AppState>) -> ApiResult {
    match state.db.get_all_tags() {
        Ok(tags) => ok_json(serde_json::to_value(tags).unwrap_or_default()),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateTagRequest {
    pub name: Option<String>,
    pub color: Option<String>,
}

pub async fn create_tag(State(state): State<AppState>, Json(body): Json<CreateTagRequest>) -> ApiResult {
    let Some(name) = body.name.filter(|n| !n.trim().is_empty()) else {
        return err_json(StatusCode::BAD_REQUEST, "标签名称不能为空");
    };
    let color = body.color.unwrap_or_else(|| "#1890ff".to_string());
    match state.db.add_tag(&name, &color) {
        Ok(tag_id) => (
            StatusCode::CREATED,
            Json(json!({"id": tag_id, "name": name, "color": color})),
        ),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn delete_tag(
    State(state): State<AppState>,
    AxumPath(tag_id): AxumPath<i64>,
) -> ApiResult {
    match state.db.delete_tag(tag_id) {
        Ok(_) => ok_json(json!({"message": "删除成功"})),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Debug, Deserialize)]
pub struct AddTagRequest {
    pub tag_name: Option<String>,
}

pub async fn add_tag_to_document(
    State(state): State<AppState>,
    AxumPath(doc_id): AxumPath<i64>,
    Json(body): Json<AddTagRequest>,
) -> ApiResult {
    let Some(tag_name) = body.tag_name.filter(|n| !n.trim().is_empty()) else {
        return err_json(StatusCode::BAD_REQUEST, "标签名称不能为空");
    };
    let tag_id = match state.db.get_tag_by_name(&tag_name) {
        Ok(Some(tag)) => tag.id,
        Ok(None) => match state.db.add_tag(&tag_name, "#1890ff") {
            Ok(id) => id,
            Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        },
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    match state.db.add_tag_to_document(doc_id, tag_id) {
        Ok(_) => ok_json(json!({"message": "标签添加成功"})),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn remove_tag_from_document(
    State(state): State<AppState>,
    AxumPath((doc_id, tag_id)): AxumPath<(i64, i64)>,
) -> ApiResult {
    match state.db.remove_tag_from_document(doc_id, tag_id) {
        Ok(_) => ok_json(json!({"message": "标签移除成功"})),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ---------------- 全文检索 ----------------

#[derive(Debug, Deserialize)]
pub struct SearchParams {
    pub q: Option<String>,
    pub page: Option<String>,
    pub page_size: Option<String>,
    pub doc_id: Option<String>,
    /// smart（默认，分词+加权+AND/OR）| exact（原文连续子串）
    pub mode: Option<String>,
    /// 查询期是否过滤单字噪声（默认开启，传 0/false 关闭）
    pub filter_singles: Option<String>,
}

fn parse_search_usize(raw: Option<&str>, default: usize) -> Option<usize> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Some(default),
        Some(v) => v.parse::<usize>().ok(),
    }
}

pub async fn search_documents(
    State(state): State<AppState>,
    Query(p): Query<SearchParams>,
) -> ApiResult {
    let page = match parse_search_usize(p.page.as_deref(), 1) {
        Some(v) => v.max(1),
        None => return err_json(StatusCode::BAD_REQUEST, "分页参数 page 必须为整数"),
    };
    let page_size = match parse_search_usize(p.page_size.as_deref(), 10) {
        Some(v) => v.clamp(1, 50),
        None => return err_json(StatusCode::BAD_REQUEST, "分页参数 page_size 必须为整数"),
    };
    let doc_id = match p.doc_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(v) => match v.parse::<i64>() {
            Ok(id) => Some(id),
            Err(_) => return err_json(StatusCode::BAD_REQUEST, "doc_id 必须为整数"),
        },
    };

    let mode_raw = p.mode.as_deref().unwrap_or("smart").trim().to_lowercase();
    let exact = match mode_raw.as_str() {
        "" | "smart" => false,
        "exact" => true,
        _ => return err_json(StatusCode::BAD_REQUEST, "mode 仅支持 smart 或 exact"),
    };
    let filter_singles = match p.filter_singles.as_deref().unwrap_or("").trim().to_lowercase().as_str() {
        "" | "1" | "true" => true,
        "0" | "false" => false,
        other => return err_json(StatusCode::BAD_REQUEST, format!("filter_singles 参数非法：{}", other)),
    };

    match state.search.search(
        p.q.as_deref().unwrap_or(""),
        page,
        page_size,
        doc_id,
        exact,
        filter_singles,
    ) {
        Ok(resp) => ok_json(json!(resp)),
        Err(crate::search::SearchError::NotReady(msg)) => {
            err_json(StatusCode::SERVICE_UNAVAILABLE, msg)
        }
        Err(crate::search::SearchError::BadRequest(msg)) => err_json(StatusCode::BAD_REQUEST, msg),
        Err(crate::search::SearchError::Internal(msg)) => {
            err_json(StatusCode::INTERNAL_SERVER_ERROR, msg)
        }
    }
}

pub async fn search_index_status(State(state): State<AppState>) -> ApiResult {
    ok_json(json!(state.search.status()))
}

fn start_search_build(state: &AppState, mode: crate::search::BuildMode) -> ApiResult {
    match state.search.start_build(mode) {
        Ok(started) => ok_json(json!(started.status)),
        Err(crate::search::StartError::AlreadyRunning) => err_json(
            StatusCode::CONFLICT,
            "索引构建正在进行中，请等待当前任务结束",
        ),
        Err(crate::search::StartError::Failed(msg)) => {
            err_json(StatusCode::INTERNAL_SERVER_ERROR, msg)
        }
    }
}

pub async fn search_index_rebuild(State(state): State<AppState>) -> ApiResult {
    start_search_build(&state, crate::search::BuildMode::Full)
}

pub async fn search_index_update(State(state): State<AppState>) -> ApiResult {
    start_search_build(&state, crate::search::BuildMode::Incremental)
}

#[derive(RustEmbed)]
#[folder = "../frontend/build/"]
struct Assets;

// ---------------- 字体 ----------------

#[derive(Debug, Deserialize)]
pub struct FontQuery {
    pub path: Option<String>,
}

/// 读取本机字体文件，供前端用 FontFace API 加载
pub async fn get_font_file(Query(q): Query<FontQuery>) -> Response {
    let Some(path) = q.path.as_deref().map(str::trim).filter(|p| !p.is_empty()) else {
        return err_json(StatusCode::BAD_REQUEST, "请提供字体文件路径").into_response();
    };
    let file = PathBuf::from(path);
    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let mime = match ext.as_str() {
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttc" => "font/collection",
        _ => {
            return err_json(
                StatusCode::BAD_REQUEST,
                "仅支持 .ttf/.otf/.woff/.woff2/.ttc 字体文件",
            )
            .into_response()
        }
    };
    let data = match std::fs::read(&file) {
        Ok(d) => d,
        Err(e) => {
            return err_json(StatusCode::NOT_FOUND, format!("读取字体文件失败：{}", e))
                .into_response()
        }
    };
    if data.is_empty() {
        return err_json(StatusCode::BAD_REQUEST, "字体文件为空").into_response();
    }
    Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, data.len())
        .header(header::CACHE_CONTROL, "public, max-age=3600")
        .body(Body::from(data))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    if let Some(content) = Assets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        let mut response = Response::new(Body::from(content.data.into_owned()));
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_str(&mime.to_string())
                .unwrap_or(header::HeaderValue::from_static("application/octet-stream")),
        );
        return response;
    }
    // SPA 路由兜底
    if let Some(content) = Assets::get("index.html") {
        let mut response = Response::new(Body::from(content.data.into_owned()));
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("text/html; charset=utf-8"),
        );
        return response;
    }
    (StatusCode::NOT_FOUND, "Not Found").into_response()
}
