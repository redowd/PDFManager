use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use lopdf::Document as PdfDoc;
use md5::{Digest, Md5};

use crate::config::Config;

/// 计算文件内容 MD5（分块读取，支持大文件）
pub fn file_md5(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// 文件名（去 .pdf 扩展名后）以 _数字 结尾 → 默认禁止 OCR
pub fn is_ocr_disabled_name(filename: &str) -> bool {
    let base = if filename.to_lowercase().ends_with(".pdf") {
        &filename[..filename.len() - 4]
    } else {
        filename
    };
    let bytes = base.as_bytes();
    let mut i = bytes.len();
    while i > 0 && bytes[i - 1].is_ascii_digit() {
        i -= 1;
    }
    if i == bytes.len() {
        return false;
    }
    if i == 0 {
        return false;
    }
    bytes[i - 1] == b'_'
}

#[derive(Debug, Clone)]
pub struct ChunkInfo {
    pub filename: String,
    pub path: PathBuf,
    pub start_page: u32,
    pub end_page: u32,
}

/// 获取 PDF 页数与大小；读取失败返回 (0, 0, 0)，与 Python 版行为一致
pub fn get_pdf_info(path: &Path) -> (u32, f64, u64) {
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let size_mb = file_size as f64 / (1024.0 * 1024.0);
    match PdfDoc::load(path) {
        Ok(doc) => (doc.get_pages().len() as u32, size_mb, file_size),
        Err(e) => {
            crate::log_info!("获取PDF信息失败: {:?} ({})", path, e);
            (0, 0.0, 0)
        }
    }
}

pub fn calculate_split_pages(
    page_count: u32,
    file_size_bytes: u64,
    cfg: &Config,
) -> Option<u32> {
    if page_count <= cfg.max_pages && file_size_bytes <= cfg.max_size_bytes {
        return None;
    }
    let avg_size_per_page = file_size_bytes as f64 / page_count as f64;
    let size_for_max_pages = avg_size_per_page * cfg.max_pages as f64;
    if size_for_max_pages > cfg.max_size_bytes as f64 {
        let target_bytes = cfg.max_size_bytes as f64 * 0.9;
        let split_pages = (target_bytes / avg_size_per_page) as u32;
        Some(split_pages.max(1))
    } else {
        Some(cfg.max_pages)
    }
}

fn split_impl(
    pdf_path: &Path,
    document_id: i64,
    split_pages: u32,
    display_name: Option<&Path>,
    cfg: &Config,
) -> Result<Vec<ChunkInfo>> {
    let total_pages = PdfDoc::load(pdf_path)?.get_pages().len() as u32;
    let num_chunks = (total_pages + split_pages - 1) / split_pages;
    let ranges: Vec<(u32, u32)> = (0..num_chunks)
        .map(|i| {
            let start = i * split_pages + 1;
            (start, (start + split_pages - 1).min(total_pages))
        })
        .collect();
    split_ranges_impl(pdf_path, document_id, &ranges, display_name, cfg)
}

/// 按给定页范围（1-based 闭区间）切分 PDF，仅生成指定范围的分块
pub fn split_pdf_ranges(
    pdf_path: &Path,
    document_id: i64,
    ranges: &[(u32, u32)],
    cfg: &Config,
) -> Result<Vec<ChunkInfo>> {
    split_ranges_impl(pdf_path, document_id, ranges, None, cfg)
}

fn split_ranges_impl(
    pdf_path: &Path,
    document_id: i64,
    ranges: &[(u32, u32)],
    display_name: Option<&Path>,
    cfg: &Config,
) -> Result<Vec<ChunkInfo>> {
    let display = display_name.unwrap_or(pdf_path);
    let original_name = display
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "document".into());
    let extension = display
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_else(|| ".pdf".into());

    let mut chunks = Vec::new();
    for &(start, end) in ranges {
        if start < 1 || end < start {
            continue;
        }
        let mut part = PdfDoc::load(pdf_path)?;
        let page_map = part.get_pages(); // 1-based -> ObjectId
        let remove: Vec<u32> = page_map
            .keys()
            .copied()
            .filter(|n| *n < start || *n > end)
            .collect();
        if !remove.is_empty() {
            part.delete_pages(&remove);
        }
        part.prune_objects();
        part.renumber_objects();

        let chunk_filename = format!(
            "{}_{}_{}_{}{}",
            original_name, document_id, start, end, extension
        );
        let chunk_path = cfg.temp_folder.join(&chunk_filename);
        if chunk_path.exists() {
            let _ = std::fs::remove_file(&chunk_path);
        }
        let mut out = File::create(&chunk_path)?;
        part.save_to(&mut out)?;
        out.flush()?;

        chunks.push(ChunkInfo {
            filename: chunk_filename,
            path: chunk_path,
            start_page: start,
            end_page: end,
        });
    }
    Ok(chunks)
}

/// 分割 PDF：超过 MAX_PAGES 或 MAX_SIZE 时按片切分到 TEMP_FOLDER，否则返回原文件
pub fn split_pdf(pdf_path: &Path, document_id: i64, cfg: &Config) -> Vec<ChunkInfo> {
    let (page_count, _size_mb, file_size_bytes) = get_pdf_info(pdf_path);
    if page_count == 0 {
        return Vec::new();
    }
    let Some(split_pages) = calculate_split_pages(page_count, file_size_bytes, cfg) else {
        return vec![ChunkInfo {
            filename: pdf_path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
            path: pdf_path.to_path_buf(),
            start_page: 1,
            end_page: page_count,
        }];
    };

    match split_impl(pdf_path, document_id, split_pages, None, cfg) {
        Ok(chunks) => chunks,
        Err(e) => {
            crate::log_info!("分割PDF失败({})：{}", e, pdf_path.display());
            Vec::new()
        }
    }
}

/// 判断路径是否位于 TEMP_FOLDER（临时分块才需要在任务结束后清理）
pub fn is_temp_path(path: &Path, cfg: &Config) -> bool {
    path.parent() == Some(cfg.temp_folder.as_path())
}

pub fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

pub fn copy_file(src: &Path, dst: &Path) -> Result<()> {
    ensure_parent(dst)?;
    std::fs::copy(src, dst)?;
    Ok(())
}

pub fn remove_if_exists(path: &Path) {
    let _ = std::fs::remove_file(path);
}

pub fn require_exists(path: &Path) -> Result<()> {
    if path.exists() {
        Ok(())
    } else {
        Err(anyhow!("文件不存在：{}", path.display()))
    }
}
