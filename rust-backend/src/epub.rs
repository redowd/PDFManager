//! Markdown → EPUB 3 导出。
//!
//! 设计要点：
//! - 以一级标题（`# `）分章，便于阅读器分页与生成目录；无一级标题时整篇一章。
//! - GFM 表格由 pulldown-cmark 转成 XHTML 表格，配合 style.css 保证边框与表头样式。
//! - 图片从图片目录读取原始文件内嵌进 EPUB（引用重写为 `images/<文件名>`），
//!   找不到或不合法时丢弃图片标签并保留 alt 文本，避免阅读器出现死链。
//! - 输出的 XHTML 严格自闭合空标签（br/hr/img 等），满足 EPUB 的 XML 规范。

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use pulldown_cmark::{html, Event, Options, Parser};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::config::Config;

pub struct EpubResult {
    pub path: PathBuf,
    pub filename: String,
    pub size: u64,
    pub chapters: usize,
    pub images: usize,
    pub missing_images: usize,
}

/// 导出并打包 EPUB：写入 `output_dir/{标题}__{doc_id}.epub`
pub fn export(
    cfg: &Config,
    title: &str,
    markdown: &str,
    doc_id: i64,
    output_dir: &Path,
) -> Result<EpubResult, String> {
    if markdown.trim().is_empty() {
        return Err("Markdown 内容为空，无法导出".into());
    }
    std::fs::create_dir_all(output_dir)
        .map_err(|e| format!("创建 EPUB 导出目录失败：{}", e))?;

    // 1. 分章 → 渲染 HTML → 重写并收集图片
    let mut images: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut missing_images = 0usize;
    let mut chapters: Vec<(String, String)> = Vec::new();
    for (chapter_title, chapter_md) in split_chapters(markdown, title) {
        let rendered = render_markdown(&chapter_md);
        let (rendered, missing) = rewrite_images(&rendered, cfg, &mut images);
        missing_images += missing;
        let rendered = normalize_void_tags(&rendered).replace("&nbsp;", "&#160;");
        chapters.push((chapter_title, rendered));
    }
    if chapters.is_empty() {
        return Err("没有可导出的内容".into());
    }

    // 2. 组装 EPUB 文件清单
    let book_id = format!("urn:uuid:{}", uuid::Uuid::new_v4());
    let modified = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut chapter_files: Vec<(String, String, String)> = Vec::new();
    for (index, (chapter_title, body)) in chapters.iter().enumerate() {
        let file_name = format!("chapter-{:03}.xhtml", index + 1);
        chapter_files.push((
            file_name.clone(),
            chapter_title.clone(),
            chapter_document(chapter_title, body),
        ));
    }

    let nav = nav_document(title, &chapter_files);
    let ncx = ncx_document(title, &book_id, &chapter_files);
    let opf = opf_document(title, &book_id, &modified, &chapter_files, &images);

    // 3. 写临时文件后原子改名，避免打包中断留下半个 EPUB
    let filename = epub_filename(title, doc_id);
    let final_path = output_dir.join(&filename);
    let tmp_path = output_dir.join(format!("{}.tmp", filename));
    let write_result = write_epub(
        &tmp_path,
        &opf,
        &nav,
        &ncx,
        &chapter_files,
        &images,
    );
    if let Err(message) = write_result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(message);
    }
    std::fs::rename(&tmp_path, &final_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        format!("写入 EPUB 文件失败：{}", e)
    })?;

    let size = std::fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0);
    Ok(EpubResult {
        path: final_path,
        filename,
        size,
        chapters: chapter_files.len(),
        images: images.len(),
        missing_images,
    })
}

/// 按一级标题分章；代码块内的 `#` 不参与切分。
fn split_chapters(markdown: &str, fallback_title: &str) -> Vec<(String, String)> {
    let mut chapters: Vec<(String, String)> = Vec::new();
    let mut current_title = fallback_title.trim().to_string();
    let mut buffer = String::new();
    let mut in_fence = false;

    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        }
        if !in_fence && (line.starts_with("# ") || line == "#") {
            if !buffer.trim().is_empty() {
                chapters.push((current_title.clone(), std::mem::take(&mut buffer)));
            }
            let heading = line.trim_start_matches('#').trim().trim_end_matches('#').trim();
            if !heading.is_empty() {
                current_title = heading.to_string();
            }
            // 标题保留在章节正文里，阅读器目录与正文层级一致
            buffer.push_str("# ");
            buffer.push_str(heading);
            buffer.push('\n');
            continue;
        }
        buffer.push_str(line);
        buffer.push('\n');
    }
    if !buffer.trim().is_empty() {
        chapters.push((current_title, buffer));
    }
    if chapters.is_empty() {
        chapters.push((fallback_title.trim().to_string(), markdown.to_string()));
    }
    chapters
}

/// Markdown → HTML：开启 GFM 表格，并把软换行当作硬换行（与预览一致）
fn render_markdown(markdown: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);
    let parser = Parser::new_ext(markdown, options).map(|event| match event {
        Event::SoftBreak => Event::HardBreak,
        other => other,
    });
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

/// 把 HTML 中的本地图片引用替换为 EPUB 内部路径，并收集图片字节。
/// 返回值第二项为缺失/非法图片数量（这些图片标签会被移除，只保留 alt 文本）。
fn rewrite_images(
    html: &str,
    cfg: &Config,
    images: &mut BTreeMap<String, Vec<u8>>,
) -> (String, usize) {
    let bytes = html.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    let mut missing = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'<' && starts_with_ignore_ascii_case(&bytes[i..], b"<img") {
            if let Some(rel_end) = bytes[i..].iter().position(|&b| b == b'>') {
                let end = i + rel_end + 1;
                let tag = &html[i..end];
                let src = extract_attr(tag, "src");
                if let Some(src) = src {
                    if let Some(name) = local_image_name(&src) {
                        if crate::mineru::is_safe_image_name(&name) {
                            if let Ok(data) = std::fs::read(cfg.image_folder.join(&name)) {
                                let local_src = format!("images/{}", name);
                                let new_tag = tag
                                    .replace(&format!("src=\"{}\"", src), &format!("src=\"{}\"", local_src))
                                    .replace(&format!("src='{}'", src), &format!("src='{}'", local_src));
                                out.extend_from_slice(new_tag.as_bytes());
                                images.entry(name).or_insert(data);
                                i = end;
                                continue;
                            }
                        }
                        missing += 1;
                        push_alt_text(&mut out, tag);
                        i = end;
                        continue;
                    }
                    // 外链图片与 data URI 原样保留，交给阅读器自行加载
                    out.extend_from_slice(tag.as_bytes());
                    i = end;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }

    (String::from_utf8_lossy(&out).into_owned(), missing)
}

/// 提取图片地址并映射为图片目录中的文件名；外链/data URI 返回 None
fn local_image_name(src: &str) -> Option<String> {
    let src = src.trim();
    if src.is_empty()
        || src.starts_with("http://")
        || src.starts_with("https://")
        || src.starts_with("data:")
        || src.starts_with("//")
    {
        return None;
    }
    let name = src.rsplit('/').next()?.split('?').next()?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// 图片缺失时保留 alt 文本，避免整段语义丢失
fn push_alt_text(out: &mut Vec<u8>, tag: &str) {
    let alt = extract_attr(tag, "alt").unwrap_or_default();
    let alt = alt.trim();
    if alt.is_empty() {
        return;
    }
    let span = format!("<span class=\"missing-image\">[{}]</span>", escape_xml(alt));
    out.extend_from_slice(span.as_bytes());
}

/// 读取标签属性值（支持双引号与单引号）
fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needle = format!("{}=", attr);
    let mut search_from = 0usize;
    while let Some(pos) = lower[search_from..].find(&needle) {
        let start = search_from + pos + needle.len();
        let rest = &tag[start..];
        let mut chars = rest.chars();
        match chars.next() {
            Some(quote @ ('"' | '\'')) => {
                let value: String = rest[1..].chars().take_while(|&c| c != quote).collect();
                return Some(value);
            }
            Some(_) => {
                let value: String = rest
                    .chars()
                    .take_while(|c| !c.is_whitespace() && *c != '>')
                    .collect();
                if value.is_empty() {
                    return None;
                }
                return Some(value);
            }
            None => return None,
        }
    }
    None
}

/// 空元素（void）标签统一写成 ` />`，保证 XHTML 可被 XML 解析
fn normalize_void_tags(html: &str) -> String {
    const VOID_TAGS: [&[u8]; 12] = [
        b"br", b"hr", b"img", b"col", b"input", b"meta", b"link", b"source", b"wbr", b"area",
        b"base", b"param",
    ];
    let bytes = html.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len() + 32);
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let mut matched = false;
            for tag in VOID_TAGS {
                if starts_with_ignore_ascii_case(&bytes[i + 1..], tag) {
                    let next = bytes.get(i + 1 + tag.len()).copied();
                    if matches!(next, Some(c) if c == b' ' || c == b'>' || c == b'/' || c == b'\t' || c == b'\n' || c == b'\r') {
                        matched = true;
                        break;
                    }
                }
            }
            if matched {
                if let Some(rel_end) = bytes[i..].iter().position(|&b| b == b'>') {
                    let end = i + rel_end + 1;
                    let mut trim_end = end - 1;
                    while trim_end > i
                        && matches!(bytes[trim_end - 1], b' ' | b'/' | b'\t' | b'\r' | b'\n')
                    {
                        trim_end -= 1;
                    }
                    out.extend_from_slice(&bytes[i..trim_end]);
                    out.extend_from_slice(b" />");
                    i = end;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn starts_with_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len()
        && haystack[..needle.len()].eq_ignore_ascii_case(needle)
}

fn chapter_document(title: &str, body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<!DOCTYPE html>\n<html xmlns=\"http://www.w3.org/1999/xhtml\" xml:lang=\"zh-CN\" lang=\"zh-CN\">\n<head>\n<meta charset=\"utf-8\"/>\n<title>{}</title>\n<link rel=\"stylesheet\" type=\"text/css\" href=\"style.css\"/>\n</head>\n<body>\n{}</body>\n</html>\n",
        escape_xml(title),
        body
    )
}

fn nav_document(title: &str, chapters: &[(String, String, String)]) -> String {
    let mut items = String::new();
    for (file, chapter_title, _) in chapters {
        items.push_str(&format!(
            "<li><a href=\"{}\">{}</a></li>\n",
            escape_xml(file),
            escape_xml(chapter_title)
        ));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<!DOCTYPE html>\n<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\" xml:lang=\"zh-CN\" lang=\"zh-CN\">\n<head>\n<meta charset=\"utf-8\"/>\n<title>目录</title>\n<link rel=\"stylesheet\" type=\"text/css\" href=\"style.css\"/>\n</head>\n<body>\n<nav epub:type=\"toc\" id=\"toc\">\n<h1>目录</h1>\n<ol>\n{}</ol>\n</nav>\n</body>\n</html>\n",
        items
    )
}

fn ncx_document(title: &str, book_id: &str, chapters: &[(String, String, String)]) -> String {
    let mut points = String::new();
    for (index, (file, chapter_title, _)) in chapters.iter().enumerate() {
        let order = index + 1;
        points.push_str(&format!(
            "<navPoint id=\"nav-{}\" playOrder=\"{}\"><navLabel><text>{}</text></navLabel><content src=\"{}\"/></navPoint>\n",
            order,
            order,
            escape_xml(chapter_title),
            escape_xml(file)
        ));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ncx xmlns=\"http://www.daisy.org/z3986/2005/ncx/\" version=\"2005-1\">\n<head>\n<meta name=\"dtb:uid\" content=\"{}\"/>\n<meta name=\"dtb:depth\" content=\"1\"/>\n<meta name=\"dtb:totalPageCount\" content=\"0\"/>\n<meta name=\"dtb:maxPageNumber\" content=\"0\"/>\n</head>\n<docTitle><text>{}</text></docTitle>\n<navMap>\n{}</navMap>\n</ncx>\n",
        escape_xml(book_id),
        escape_xml(title),
        points
    )
}

fn opf_document(
    title: &str,
    book_id: &str,
    modified: &str,
    chapters: &[(String, String, String)],
    images: &BTreeMap<String, Vec<u8>>,
) -> String {
    let mut manifest = String::new();
    manifest.push_str("<item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/>\n");
    manifest.push_str("<item id=\"ncx\" href=\"toc.ncx\" media-type=\"application/x-dtbncx+xml\"/>\n");
    manifest.push_str("<item id=\"css\" href=\"style.css\" media-type=\"text/css\"/>\n");
    for (index, (file, _, _)) in chapters.iter().enumerate() {
        manifest.push_str(&format!(
            "<item id=\"chapter-{}\" href=\"{}\" media-type=\"application/xhtml+xml\"/>\n",
            index + 1,
            escape_xml(file)
        ));
    }
    for (index, (name, _)) in images.iter().enumerate() {
        manifest.push_str(&format!(
            "<item id=\"img-{}\" href=\"images/{}\" media-type=\"{}\"/>\n",
            index + 1,
            escape_xml(name),
            image_media_type(name)
        ));
    }

    let mut spine = String::new();
    for index in 0..chapters.len() {
        spine.push_str(&format!("<itemref idref=\"chapter-{}\"/>\n", index + 1));
    }

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"book-id\">\n<metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n<dc:identifier id=\"book-id\">{}</dc:identifier>\n<dc:title>{}</dc:title>\n<dc:language>zh-CN</dc:language>\n<dc:creator>PDF Manager</dc:creator>\n<meta property=\"dcterms:modified\">{}</meta>\n</metadata>\n<manifest>\n{}</manifest>\n<spine toc=\"ncx\">\n{}</spine>\n</package>\n",
        escape_xml(book_id),
        escape_xml(title),
        modified,
        manifest,
        spine
    )
}

fn style_css() -> &'static str {
    r#"@charset "utf-8";
body { font-family: "Songti SC", SimSun, serif; line-height: 1.75; margin: 0 0.6em; color: #111; font-size: 1em; }
h1, h2, h3, h4, h5, h6 { font-family: "Heiti SC", SimHei, sans-serif; line-height: 1.35; margin: 1.2em 0 0.6em; }
h1 { font-size: 1.5em; border-bottom: 1px solid #ccc; padding-bottom: 0.3em; }
h2 { font-size: 1.3em; }
h3 { font-size: 1.15em; }
p { margin: 0.5em 0; text-align: justify; }
img { max-width: 100%; height: auto; display: block; margin: 0.8em auto; }
table { border-collapse: collapse; width: 100%; margin: 0.8em 0; font-size: 0.92em; }
th, td { border: 1px solid #999; padding: 4px 6px; vertical-align: top; text-align: left; }
th { background: #f0f0f0; font-weight: 600; }
blockquote { margin: 0.8em 0; padding: 0.2em 0.8em; border-left: 3px solid #bbb; color: #444; }
pre { background: #f5f5f5; padding: 0.6em; overflow-x: auto; font-size: 0.85em; white-space: pre-wrap; }
code { font-family: Consolas, "Courier New", monospace; background: #f5f5f5; padding: 0 0.2em; }
pre code { background: none; padding: 0; }
ul, ol { padding-left: 1.4em; }
li { margin: 0.2em 0; }
hr { border: none; border-top: 1px solid #ccc; margin: 1em 0; }
.missing-image { color: #999; font-size: 0.9em; }
"#
}

fn image_media_type(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()) {
        Some(ext) if ext == "png" => "image/png",
        Some(ext) if ext == "gif" => "image/gif",
        Some(ext) if ext == "webp" => "image/webp",
        Some(ext) if ext == "bmp" => "image/bmp",
        Some(ext) if ext == "svg" => "image/svg+xml",
        _ => "image/jpeg",
    }
}

fn write_epub(
    path: &Path,
    opf: &str,
    nav: &str,
    ncx: &str,
    chapters: &[(String, String, String)],
    images: &BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("创建 EPUB 文件失败：{}", e))?;
    let mut zip = ZipWriter::new(file);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    // mimetype 必须是第一个条目且不压缩，否则部分阅读器拒绝识别
    zip.start_file("mimetype", stored)
        .map_err(|e| format!("写入 mimetype 失败：{}", e))?;
    zip.write_all(b"application/epub+zip")
        .map_err(|e| format!("写入 mimetype 失败：{}", e))?;

    let container = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\">\n<rootfiles>\n<rootfile full-path=\"OEBPS/content.opf\" media-type=\"application/oebps-package+xml\"/>\n</rootfiles>\n</container>\n";
    for (name, content) in [
        ("META-INF/container.xml", container),
        ("OEBPS/content.opf", opf),
        ("OEBPS/nav.xhtml", nav),
        ("OEBPS/toc.ncx", ncx),
        ("OEBPS/style.css", style_css()),
    ] {
        zip.start_file(name, deflated)
            .map_err(|e| format!("写入 {} 失败：{}", name, e))?;
        zip.write_all(content.as_bytes())
            .map_err(|e| format!("写入 {} 失败：{}", name, e))?;
    }

    for (file_name, _, document) in chapters {
        let entry = format!("OEBPS/{}", file_name);
        zip.start_file(&entry, deflated)
            .map_err(|e| format!("写入 {} 失败：{}", entry, e))?;
        zip.write_all(document.as_bytes())
            .map_err(|e| format!("写入 {} 失败：{}", entry, e))?;
    }

    for (name, data) in images {
        let entry = format!("OEBPS/images/{}", name);
        // 图片本身已压缩，直接存储可避免二次压缩耗时
        zip.start_file(&entry, stored)
            .map_err(|e| format!("写入图片 {} 失败：{}", name, e))?;
        zip.write_all(data)
            .map_err(|e| format!("写入图片 {} 失败：{}", name, e))?;
    }

    zip.finish().map_err(|e| format!("完成 EPUB 打包失败：{}", e))?;
    Ok(())
}

/// 导出文件名：去掉 .pdf、替换 Windows 非法字符、按 ID 去重
fn epub_filename(title: &str, doc_id: i64) -> String {
    let base = title.trim();
    let base = base
        .strip_suffix(".pdf")
        .or_else(|| base.strip_suffix(".PDF"))
        .unwrap_or(base);
    let mut cleaned: String = base
        .chars()
        .map(|c| {
            if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    cleaned = cleaned.trim().trim_end_matches('.').trim().to_string();
    if cleaned.is_empty() {
        cleaned = "document".to_string();
    }
    let truncated: String = cleaned.chars().take(80).collect();
    format!("{}__{}.epub", truncated, doc_id)
}

fn escape_xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}
