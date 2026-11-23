use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use flate2::read::GzDecoder;
use serde_json::Value;

use crate::config::Config;

/// 根据记录路径返回实际存在的 JSON 文件路径。
/// 兼容：path 直接命中 / path+'.gz' 命中 / path 去 '.gz' 命中 /
/// 按 basename 到 OUTPUT_JSON_FOLDER 兜底查找。
pub fn resolve_json_path(path: &str, cfg: &Config) -> Option<PathBuf> {
    if path.is_empty() {
        return None;
    }
    let p = Path::new(path);
    if p.exists() {
        return Some(p.to_path_buf());
    }
    if !path.ends_with(".gz") {
        let gz = PathBuf::from(format!("{}.gz", path));
        if gz.exists() {
            return Some(gz);
        }
    }
    if let Some(stripped) = path.strip_suffix(".gz") {
        let plain = PathBuf::from(stripped);
        if plain.exists() {
            return Some(plain);
        }
    }
    let mut base = p.file_name()?.to_string_lossy().to_string();
    if base.ends_with(".gz") {
        base.truncate(base.len() - 3);
    }
    for cand in [
        cfg.output_json_folder.join(&base),
        cfg.output_json_folder.join(format!("{}.gz", base)),
    ] {
        if cand.exists() {
            return Some(cand);
        }
    }
    None
}

/// 读取 JSON 文件，自动识别 .json / .json.gz
pub fn load_json(path: &str, cfg: &Config) -> Result<Value> {
    let real = resolve_json_path(path, cfg)
        .ok_or_else(|| anyhow!("JSON 文件不存在（含 .gz 兜底）: {}", path))?;
    let mut raw = Vec::new();
    if real.to_string_lossy().ends_with(".gz") {
        let mut decoder = GzDecoder::new(File::open(&real)?);
        decoder.read_to_end(&mut raw)?;
    } else {
        File::open(&real)?.read_to_end(&mut raw)?;
    }
    Ok(serde_json::from_slice(&raw)?)
}

fn extract_text_from_block(block: &Value) -> String {
    let mut text = String::new();
    if let Some(lines) = block.get("lines").and_then(|v| v.as_array()) {
        for line in lines {
            if let Some(spans) = line.get("spans").and_then(|v| v.as_array()) {
                for span in spans {
                    if let Some(content) = span.get("content").and_then(|v| v.as_str()) {
                        text.push_str(content);
                    }
                }
            }
        }
    }
    text
}

/// 递归收集块内的 image_path（MinerU 存的是纯文件名，兜底取 basename 防穿越）
fn collect_image_names(block: &Value, out: &mut Vec<String>) {
    fn walk(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(map) => {
                if let Some(p) = map.get("image_path").and_then(|x| x.as_str()) {
                    let base = p
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or(p)
                        .trim()
                        .to_string();
                    if !base.is_empty() {
                        out.push(base);
                    }
                }
                for (_, child) in map {
                    walk(child, out);
                }
            }
            Value::Array(arr) => {
                for child in arr {
                    walk(child, out);
                }
            }
            _ => {}
        }
    }
    walk(block, out);
}

fn emit_block_images(block: &Value, md: &mut String, cfg: &Config) {
    let mut names = Vec::new();
    collect_image_names(block, &mut names);
    let mut seen = std::collections::HashSet::new();
    for name in names {
        if !seen.insert(name.clone()) {
            continue;
        }
        // 只引用图片目录中确实存在且文件名安全的图片，避免死链与越权
        if !crate::mineru::is_safe_image_name(&name) || !cfg.image_folder.join(&name).is_file() {
            continue;
        }
        md.push_str(&format!("\n![图片](/api/images/{})\n\n", name));
    }
}

/// 递归查找块内的 `<table>` HTML（MinerU 把 html 放在 span 上）
fn collect_table_html(block: &Value) -> Option<String> {
    fn find(v: &Value) -> Option<String> {
        match v {
            Value::Object(map) => {
                if let Some(html) = map.get("html").and_then(|x| x.as_str()) {
                    if html.to_ascii_lowercase().contains("<table") {
                        return Some(html.to_string());
                    }
                }
                for (_, child) in map {
                    if let Some(h) = find(child) {
                        return Some(h);
                    }
                }
                None
            }
            Value::Array(arr) => arr.iter().find_map(find),
            _ => None,
        }
    }
    find(block)
}

/// 查找 html 中的指定标签（大小写不敏感），返回 (标签起始, 标签结束后的位置)
fn next_tag(html: &str, name: &str, from: usize, closing: bool) -> Option<(usize, usize)> {
    let bytes = html.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let mut j = i + 1;
            if closing {
                if j < bytes.len() && bytes[j] == b'/' {
                    j += 1;
                } else {
                    i += 1;
                    continue;
                }
            } else if j < bytes.len() && bytes[j] == b'/' {
                i += 1;
                continue;
            }
            let start = j;
            while j < bytes.len()
                && !bytes[j].is_ascii_whitespace()
                && bytes[j] != b'>'
                && bytes[j] != b'/'
            {
                j += 1;
            }
            if html[start..j].eq_ignore_ascii_case(name) {
                let end = html[i..].find('>')? + i;
                return Some((i, end + 1));
            }
        }
        i += 1;
    }
    None
}

fn parse_span(tag: &str, attr: &str) -> usize {
    let lower = tag.to_ascii_lowercase();
    let Some(pos) = lower.find(attr) else {
        return 1;
    };
    let rest = lower[pos + attr.len()..].trim_start();
    let Some(rest) = rest.strip_prefix('=') else {
        return 1;
    };
    let rest = rest.trim_start().trim_start_matches(['"', '\'']);
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits
        .parse::<usize>()
        .ok()
        .filter(|v| *v >= 1)
        .unwrap_or(1)
        .min(50)
}

fn decode_entities(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// 单元格文本清洗：去标签、换行转 `<br>`、转义竖线
fn clean_cell_text(fragment: &str) -> String {
    let mut out = String::new();
    let mut rest = fragment;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('>') else {
            break;
        };
        let tag = rest[start + 1..start + end].trim().to_ascii_lowercase();
        if tag.starts_with("br")
            || tag.starts_with("/p")
            || tag.starts_with("/div")
            || tag.starts_with("/li")
        {
            out.push('\n');
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    let decoded = decode_entities(&out);
    let lines: Vec<String> = decoded
        .split('\n')
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect();
    lines.join("<br>").replace('|', "\\|")
}

type RawCell = (String, usize, usize, bool);

fn parse_cells(row_html: &str) -> Vec<RawCell> {
    let mut cells = Vec::new();
    let mut pos = 0usize;
    loop {
        let td = next_tag(row_html, "td", pos, false);
        let th = next_tag(row_html, "th", pos, false);
        let (name, start, end, is_header) = match (td, th) {
            (Some(a), Some(b)) => {
                if a.0 < b.0 {
                    ("td", a.0, a.1, false)
                } else {
                    ("th", b.0, b.1, true)
                }
            }
            (Some(a), None) => ("td", a.0, a.1, false),
            (None, Some(b)) => ("th", b.0, b.1, true),
            (None, None) => break,
        };
        let tag_text = &row_html[start..end];
        let rowspan = parse_span(tag_text, "rowspan");
        let colspan = parse_span(tag_text, "colspan");
        let close = next_tag(row_html, name, end, true);
        let content_end = close.map(|(s, _)| s).unwrap_or(row_html.len());
        cells.push((
            clean_cell_text(&row_html[end..content_end]),
            rowspan,
            colspan,
            is_header,
        ));
        pos = close.map(|(_, e)| e).unwrap_or(row_html.len());
    }
    cells
}

fn parse_html_table(html: &str) -> Vec<Vec<RawCell>> {
    let mut rows = Vec::new();
    let mut pos = 0usize;
    loop {
        let Some((_, row_start)) = next_tag(html, "tr", pos, false) else {
            break;
        };
        let Some((row_close, row_end)) = next_tag(html, "tr", row_start, true) else {
            break;
        };
        rows.push(parse_cells(&html[row_start..row_close]));
        pos = row_end;
    }
    rows
}

/// HTML 表格 → Markdown 表格。
/// rowspan/colspan 无法在 Markdown 中表达：内容只放在锚点单元格，被合并覆盖的格子留空；
/// 多行表头按列用 " / " 压平成单行表头；单元格内换行转 `<br>`，竖线转义。
fn html_table_to_markdown(html: &str) -> Option<String> {
    let rows = parse_html_table(html);
    if rows.is_empty() {
        return None;
    }
    let mut grid: Vec<Vec<String>> = Vec::new();
    let mut filled: Vec<Vec<bool>> = Vec::new();
    // 先按首行 rowspan 预估表头行数：表头里的 colspan 会把分组名铺满被覆盖列，便于压平表头
    let header_span = rows
        .first()
        .map(|row| row.iter().map(|cell| cell.1).max().unwrap_or(1))
        .unwrap_or(1);
    let header_hint = if header_span > 1 { header_span } else { 1 };
    for (r, cells) in rows.iter().enumerate() {
        while grid.len() <= r {
            grid.push(Vec::new());
            filled.push(Vec::new());
        }
        let mut c = 0usize;
        for (text, rowspan, colspan, _) in cells {
            while filled[r].get(c).copied().unwrap_or(false) {
                c += 1;
            }
            let in_header = r < header_hint;
            for dr in 0..*rowspan {
                let rr = r + dr;
                while grid.len() <= rr {
                    grid.push(Vec::new());
                    filled.push(Vec::new());
                }
                while grid[rr].len() < c + colspan {
                    grid[rr].push(String::new());
                    filled[rr].push(false);
                }
                for dc in 0..*colspan {
                    if dr == 0 && (dc == 0 || in_header) {
                        grid[rr][c + dc] = text.clone();
                    }
                    filled[rr][c + dc] = true;
                }
            }
            c += colspan;
        }
    }
    let width = grid.iter().map(|row| row.len()).max().unwrap_or(0);
    if width == 0 {
        return None;
    }
    for row in grid.iter_mut() {
        row.resize(width, String::new());
    }

    // 多行表头：首行出现 rowspan>1 时，把它跨的行一起压平成一行表头
    let mut header_rows = 1usize;
    if let Some(first) = rows.first() {
        let span = first.iter().map(|cell| cell.1).max().unwrap_or(1);
        if span > 1 {
            header_rows = span.min(grid.len());
        }
    }
    header_rows = header_rows.min(grid.len()).max(1);

    let mut header: Vec<String> = Vec::with_capacity(width);
    for c in 0..width {
        let mut parts: Vec<String> = Vec::new();
        for r in 0..header_rows {
            let v = grid[r][c].trim();
            if !v.is_empty() && parts.last().map(|p| p != v).unwrap_or(true) {
                parts.push(v.to_string());
            }
        }
        let text = parts.join(" / ");
        header.push(if text.is_empty() { " ".to_string() } else { text });
    }

    let mut md = String::from("\n");
    md.push_str(&format!("| {} |\n", header.join(" | ")));
    md.push_str(&format!(
        "|{}|\n",
        (0..width).map(|_| " --- ").collect::<Vec<_>>().join("|")
    ));
    for row in grid.iter().skip(header_rows) {
        let cells: Vec<String> = row
            .iter()
            .map(|v| {
                let t = v.trim();
                if t.is_empty() {
                    " ".to_string()
                } else {
                    t.to_string()
                }
            })
            .collect();
        md.push_str(&format!("| {} |\n", cells.join(" | ")));
    }
    md.push('\n');
    Some(md)
}

fn process_block(block: &Value, md: &mut String, cfg: &Config) -> bool {
    let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("text");
    let mut contain_footnote = false;
    match block_type {
        "title" => {
            let text = extract_text_from_block(block);
            md.push_str(&format!("\n# {}\n\n", text));
        }
        "text" => {
            let text = extract_text_from_block(block);
            if !text.is_empty() {
                md.push_str(&format!("{}\n", text));
            }
        }
        "list" => {
            if let Some(sub_blocks) = block.get("blocks").and_then(|v| v.as_array()) {
                for sub in sub_blocks {
                    let text = extract_text_from_block(sub);
                    if !text.is_empty() {
                        md.push_str(&format!("- {}\n", text));
                    }
                }
            }
        }
        "page_footnote" => {
            let text = extract_text_from_block(block);
            if !text.is_empty() {
                md.push_str(&format!("{}\n", text));
                contain_footnote = true;
            }
        }
        "image" | "image_body" => {
            emit_block_images(block, md, cfg);
        }
        "table" | "table_body" => {
            // 优先用 MinerU 的 html 还原表格；没有 html 时退化为表格区域截图
            match collect_table_html(block).and_then(|html| html_table_to_markdown(&html)) {
                Some(table_md) => md.push_str(&table_md),
                None => emit_block_images(block, md, cfg),
            }
        }
        "table_caption" | "table_footnote" => {
            let text = extract_text_from_block(block);
            if !text.is_empty() {
                md.push_str(&format!("{}\n", text));
            }
        }
        _ => {}
    }
    contain_footnote
}

fn append_pages(data: &Value, md: &mut String, cfg: &Config) {
    let Some(pages) = data.get("pdf_info").and_then(|v| v.as_array()) else {
        return;
    };
    let mut pages: Vec<&Value> = pages.iter().collect();
    pages.sort_by_key(|p| p.get("page_idx").and_then(|v| v.as_i64()).unwrap_or(0));

    for page in pages {
        let mut contain_footnote = false;
        if let Some(blocks) = page.get("para_blocks").and_then(|v| v.as_array()) {
            let mut blocks: Vec<&Value> = blocks.iter().collect();
            blocks.sort_by_key(|b| b.get("index").and_then(|v| v.as_i64()).unwrap_or(0));
            for block in blocks {
                contain_footnote = process_block(block, md, cfg) || contain_footnote;
            }
        }
        if let Some(blocks) = page.get("discarded_blocks").and_then(|v| v.as_array()) {
            for block in blocks {
                contain_footnote = process_block(block, md, cfg) || contain_footnote;
            }
        }
        if contain_footnote {
            md.push_str("\n---\n\n");
        }
    }
}

fn write_md(output_md: &Path, content: &str) -> Result<()> {
    if let Some(parent) = output_md.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = File::create(output_md)?;
    f.write_all(content.as_bytes())?;
    Ok(())
}

/// 将单个 middle.json 转换为 Markdown
pub fn convert_json_to_markdown(input_json: &str, output_md: &Path, cfg: &Config) -> Result<()> {
    let data = load_json(input_json, cfg)?;
    let mut md = String::new();
    append_pages(&data, &mut md, cfg);
    write_md(output_md, &md)?;
    crate::log_info!("成功转换 {} 到 {}", input_json, output_md.display());
    Ok(())
}

/// 合并多个分片 JSON（分片内部按 page_idx 排序，分片之间按传入顺序拼接）
pub fn merge_json_to_markdown(json_paths: &[String], output_md: &Path, cfg: &Config) -> Result<()> {
    let mut md = String::new();
    for path in json_paths {
        let data = load_json(path, cfg)?;
        append_pages(&data, &mut md, cfg);
    }
    write_md(output_md, &md)?;
    crate::log_info!("成功合并 {} 个JSON文件到 {}", json_paths.len(), output_md.display());
    Ok(())
}

pub fn json_basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod table_tests {
    use super::html_table_to_markdown;

    #[test]
    fn converts_rowspan_colspan_table() {
        let html = r#"<table><tr><td rowspan="2">年龄别</td><td colspan="3">人口数(人)</td><td colspan="3">占总人口数合计的百分比(%)</td><td rowspan="2">性别比(女=100)</td></tr><tr><td>合计</td><td>男</td><td>女</td><td>合计</td><td>男</td><td>女</td></tr><tr><td>总计</td><td>100,379,045</td><td>51,522,177</td><td>48,856,868</td><td>100.00</td><td>51.33</td><td>48.67</td><td>105.46</td></tr></table>"#;
        let md = html_table_to_markdown(html).expect("table");
        println!("{}", md);
        assert!(md.contains("| 年龄别 | 人口数(人) / 合计 | 人口数(人) / 男 | 人口数(人) / 女 |"));
        assert!(md.contains("| 总计 | 100,379,045 | 51,522,177 | 48,856,868 | 100.00 | 51.33 | 48.67 | 105.46 |"));
    }

    #[test]
    fn escapes_pipe_and_br() {
        let html = r#"<table><tr><th>a|b</th><th>c</th></tr><tr><td>x<br>y</td><td>z</td></tr></table>"#;
        let md = html_table_to_markdown(html).expect("table");
        println!("{}", md);
        assert!(md.contains("a\\|b"));
        assert!(md.contains("x<br>y"));
    }
}
