//! Markdown → Word（.docx）导出。
//!
//! 设计取舍：
//! - 不引入 docx 生成库，直接用已有 `zip` 依赖写 OOXML（WordprocessingML），
//!   避免为一次导出新增数 MB 依赖、拖慢编译与启动；exe 体积基本不变。
//! - 用已有 `pulldown-cmark` 解析结构：标题、段落、列表、表格、代码块、引用、图片。
//! - 图片从图片目录直接读取并嵌入 `word/media/`，不依赖 127.0.0.1 链接；
//!   缺失图片在正文留下占位文字，并在文末「缺失图片」清单中列出文件名。
//! - 代码块逐行成段、等宽字体 + 底色；表格使用固定布局、等宽列、细边框。

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::config::Config;

pub struct DocxResult {
    pub path: PathBuf,
    pub filename: String,
    pub size: u64,
    pub paragraphs: usize,
    pub tables: usize,
    pub images: usize,
    pub missing_images: usize,
    pub elapsed_ms: u64,
}

/// 导出入口：写入 `output_dir/{标题}__{doc_id}.docx`
pub fn export(
    cfg: &Config,
    title: &str,
    markdown: &str,
    doc_id: i64,
    output_dir: &Path,
) -> Result<DocxResult, String> {
    if markdown.trim().is_empty() {
        return Err("Markdown 内容为空，无法导出".into());
    }
    let started = Instant::now();
    std::fs::create_dir_all(output_dir).map_err(|e| format!("创建导出目录失败：{}", e))?;

    let mut builder = Builder::new(cfg);
    builder.run(markdown);
    let document = builder.finish();

    let filename = docx_filename(title, doc_id);
    let final_path = output_dir.join(&filename);
    let tmp_path = output_dir.join(format!("{}.tmp", filename));
    if let Err(message) = write_docx(&tmp_path, &document, &builder, title) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(message);
    }
    std::fs::rename(&tmp_path, &final_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        format!("写入 Word 文件失败：{}", e)
    })?;

    let size = std::fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0);
    Ok(DocxResult {
        path: final_path,
        filename,
        size,
        paragraphs: document.paragraphs,
        tables: document.tables,
        images: builder.images.len(),
        missing_images: builder.missing.len(),
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

struct Cell {
    xml: String,
}

struct TableState {
    rows: Vec<Vec<Cell>>,
    row: Vec<Cell>,
    cell_paras: String,
    cell_runs: String,
    in_cell: bool,
    head: bool,
}

struct Builder<'a> {
    cfg: &'a Config,
    body: String,
    runs: String,
    ppr: String,
    para_open: bool,
    bold: usize,
    italic: usize,
    strike: usize,
    inline_code: usize,
    quote: usize,
    lists: Vec<bool>,
    code: Option<String>,
    table: Option<TableState>,
    images: BTreeMap<String, Vec<u8>>,
    image_rids: BTreeMap<String, String>,
    missing: Vec<String>,
    docpr: usize,
    paragraphs: usize,
    tables: usize,
}

struct DocumentXml {
    body: String,
    paragraphs: usize,
    tables: usize,
}

impl<'a> Builder<'a> {
    fn new(cfg: &'a Config) -> Self {
        Self {
            cfg,
            body: String::new(),
            runs: String::new(),
            ppr: String::new(),
            para_open: false,
            bold: 0,
            italic: 0,
            strike: 0,
            inline_code: 0,
            quote: 0,
            lists: Vec::new(),
            code: None,
            table: None,
            images: BTreeMap::new(),
            image_rids: BTreeMap::new(),
            missing: Vec::new(),
            docpr: 0,
            paragraphs: 0,
            tables: 0,
        }
    }

    fn run(&mut self, markdown: &str) {
        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        options.insert(Options::ENABLE_STRIKETHROUGH);
        options.insert(Options::ENABLE_TASKLISTS);
        options.insert(Options::ENABLE_FOOTNOTES);
        let parser = Parser::new_ext(markdown, options);
        for event in parser {
            match event {
                Event::Start(tag) => self.start(tag),
                Event::End(tag) => self.end(tag),
                Event::Text(text) => {
                    if let Some(code) = &mut self.code {
                        code.push_str(&text);
                    } else {
                        self.push_text(&text);
                    }
                }
                Event::Code(text) => {
                    self.inline_code += 1;
                    self.push_text(&text);
                    self.inline_code -= 1;
                }
                Event::SoftBreak | Event::HardBreak => {
                    if self.code.is_none() {
                        self.push_break();
                    }
                }
                Event::Rule => {
                    self.flush();
                    self.body.push_str(
                        "<w:p><w:pPr><w:pBdr><w:bottom w:val=\"single\" w:sz=\"6\" w:space=\"1\" w:color=\"BBBBBB\"/></w:pBdr></w:pPr></w:p>",
                    );
                    self.paragraphs += 1;
                }
                Event::TaskListMarker(checked) => {
                    self.push_text(if checked { "☑ " } else { "☐ " });
                }
                Event::FootnoteReference(name) => {
                    self.push_text(&format!("[^{}]", name));
                }
                _ => {}
            }
        }
        self.flush();
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {
                if self.quote > 0 {
                    self.set_style("Quote");
                    self.ppr.push_str("<w:ind w:left=\"567\" w:right=\"283\"/>");
                }
            }
            Tag::Heading { level, .. } => {
                self.flush();
                let lv = (level as usize).clamp(1, 6);
                self.set_style(&format!("Heading{}", lv));
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.quote += 1;
            }
            Tag::CodeBlock(_) => {
                self.flush();
                self.code = Some(String::new());
            }
            Tag::List(ordered) => {
                self.flush();
                self.lists.push(ordered.is_some());
            }
            Tag::Item => {
                self.flush();
                let ordered = *self.lists.last().unwrap_or(&false);
                let level = self.lists.len().saturating_sub(1).min(2) as i32;
                let num_id = if ordered { 2 } else { 1 };
                self.ppr.push_str(&format!(
                    "<w:numPr><w:ilvl w:val=\"{}\"/><w:numId w:val=\"{}\"/></w:numPr>",
                    level, num_id
                ));
                self.para_open = true;
            }
            Tag::Table(_) => {
                self.flush();
                self.table = Some(TableState {
                    rows: Vec::new(),
                    row: Vec::new(),
                    cell_paras: String::new(),
                    cell_runs: String::new(),
                    in_cell: false,
                    head: false,
                });
            }
            Tag::TableHead => {
                if let Some(table) = &mut self.table {
                    table.head = true;
                }
                self.bold += 1;
            }
            Tag::TableRow => {
                if let Some(table) = &mut self.table {
                    table.row.clear();
                }
            }
            Tag::TableCell => {
                if let Some(table) = &mut self.table {
                    table.cell_paras.clear();
                    table.in_cell = true;
                }
            }
            Tag::Emphasis => self.italic += 1,
            Tag::Strong => self.bold += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Image { dest_url, .. } => {
                self.push_image(&dest_url);
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush();
            }
            TagEnd::Heading(_) => {
                self.flush();
            }
            TagEnd::BlockQuote(_) => {
                self.quote = self.quote.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                if let Some(text) = self.code.take() {
                    if text.is_empty() {
                        self.body.push_str(
                            "<w:p><w:pPr><w:pStyle w:val=\"CodeBlock\"/></w:pPr></w:p>",
                        );
                        self.paragraphs += 1;
                    } else {
                        for line in text.lines() {
                            self.body.push_str("<w:p><w:pPr><w:pStyle w:val=\"CodeBlock\"/></w:pPr><w:r><w:t xml:space=\"preserve\">");
                            self.body.push_str(&escape_xml(line));
                            self.body.push_str("</w:t></w:r></w:p>");
                            self.paragraphs += 1;
                        }
                    }
                }
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
            }
            TagEnd::Item => {
                self.flush();
            }
            TagEnd::Table => {
                self.flush();
                self.emit_table();
            }
            TagEnd::TableHead => {
                self.bold = self.bold.saturating_sub(1);
                if let Some(table) = &mut self.table {
                    table.head = false;
                }
            }
            TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    let row = std::mem::take(&mut table.row);
                    if !row.is_empty() {
                        table.rows.push(row);
                    }
                }
            }
            TagEnd::TableCell => {
                self.flush();
                if let Some(table) = &mut self.table {
                    table.in_cell = false;
                    let mut paras = std::mem::take(&mut table.cell_paras);
                    if paras.is_empty() {
                        paras = "<w:p/>".to_string();
                    }
                    table.row.push(Cell { xml: paras });
                }
            }
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            _ => {}
        }
    }

    fn set_style(&mut self, style: &str) {
        if self.ppr.is_empty() || !self.ppr.contains("<w:pStyle") {
            self.ppr.insert_str(0, &format!("<w:pStyle w:val=\"{}\"/>", style));
        }
        self.para_open = true;
    }

    fn push_text(&mut self, text: &str) {
        let mut rpr = String::new();
        if self.inline_code > 0 {
            rpr.push_str("<w:rStyle w:val=\"CodeChar\"/>");
            rpr.push_str("<w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\"/>");
        }
        if self.bold > 0 {
            rpr.push_str("<w:b/><w:bCs/>");
        }
        if self.italic > 0 {
            rpr.push_str("<w:i/><w:iCs/>");
        }
        if self.strike > 0 {
            rpr.push_str("<w:strike/>");
        }
        let rpr = if rpr.is_empty() {
            String::new()
        } else {
            format!("<w:rPr>{}</w:rPr>", rpr)
        };
        let run = format!(
            "<w:r>{}<w:t xml:space=\"preserve\">{}</w:t></w:r>",
            rpr,
            escape_xml(text)
        );
        self.para_open = true;
        if let Some(table) = &mut self.table {
            if table.in_cell {
                table.cell_runs.push_str(&run);
                return;
            }
        }
        self.runs.push_str(&run);
    }

    fn push_break(&mut self) {
        self.para_open = true;
        if let Some(table) = &mut self.table {
            if table.in_cell {
                table.cell_runs.push_str("<w:r><w:br/></w:r>");
                return;
            }
        }
        self.runs.push_str("<w:r><w:br/></w:r>");
    }

    /// 图片嵌入：读取图片目录中的原始文件，按 PNG/JPEG/GIF/BMP 头解析尺寸
    fn push_image(&mut self, src: &str) {
        let Some(name) = local_image_name(src) else {
            return;
        };
        if !crate::mineru::is_safe_image_name(&name) {
            self.push_missing_image(&name);
            return;
        }
        let path = self.cfg.image_folder.join(&name);
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(_) => {
                self.push_missing_image(&name);
                return;
            }
        };
        self.images.entry(name.clone()).or_insert_with(|| data.clone());
        let rid = if let Some(rid) = self.image_rids.get(&name) {
            rid.clone()
        } else {
            let rid = format!("rId{}", 3 + self.image_rids.len());
            self.image_rids.insert(name.clone(), rid.clone());
            rid
        };
        let (px_w, px_h) = image_dimensions(&data, &name).unwrap_or((800, 600));
        let (cx, cy) = fit_size(px_w, px_h);
        self.docpr += 1;
        let drawing = format!(
            "<w:r><w:drawing><wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\">\
<wp:extent cx=\"{cx}\" cy=\"{cy}\"/>\
<wp:docPr id=\"{id}\" name=\"图片 {id}\"/>\
<a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
<pic:pic><pic:nvPicPr><pic:cNvPr id=\"{id}\" name=\"{name}\"/><pic:cNvPicPr/></pic:nvPicPr>\
<pic:blipFill><a:blip r:embed=\"{rid}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>\
<pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm>\
<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr></pic:pic>\
</a:graphicData></a:graphic></wp:inline></w:drawing></w:r>",
            cx = cx,
            cy = cy,
            id = self.docpr,
            name = escape_xml(&name),
            rid = rid
        );
        self.para_open = true;
        if let Some(table) = &mut self.table {
            if table.in_cell {
                table.cell_runs.push_str(&drawing);
                return;
            }
        }
        self.runs.push_str(&drawing);
    }

    fn push_missing_image(&mut self, name: &str) {
        if !self.missing.iter().any(|n| n == name) {
            self.missing.push(name.to_string());
        }
        self.push_text(&format!("[图片缺失：{}]", name));
    }

    fn flush(&mut self) {
        if !self.para_open {
            return;
        }
        let in_cell = self.table.as_ref().map(|t| t.in_cell).unwrap_or(false);
        let runs = if in_cell {
            self.table
                .as_mut()
                .map(|t| std::mem::take(&mut t.cell_runs))
                .unwrap_or_default()
        } else {
            std::mem::take(&mut self.runs)
        };
        let ppr = std::mem::take(&mut self.ppr);
        let ppr = if ppr.is_empty() {
            String::new()
        } else {
            format!("<w:pPr>{}</w:pPr>", ppr)
        };
        let paragraph = format!("<w:p>{}{}</w:p>", ppr, runs);
        self.para_open = false;
        if in_cell {
            if let Some(table) = &mut self.table {
                table.cell_paras.push_str(&paragraph);
            }
            return;
        }
        self.body.push_str(&paragraph);
        self.paragraphs += 1;
    }

    fn emit_table(&mut self) {
        let Some(table) = self.table.take() else {
            return;
        };
        if table.rows.is_empty() {
            return;
        }
        let cols = table.rows.iter().map(|r| r.len()).max().unwrap_or(0).max(1);
        let col_width = (9360 / cols as i32).max(600);
        let mut xml = String::new();
        xml.push_str("<w:tbl><w:tblPr><w:tblW w:type=\"dxa\" w:w=\"9360\"/>");
        xml.push_str("<w:tblBorders>");
        for edge in ["top", "left", "bottom", "right", "insideH", "insideV"] {
            xml.push_str(&format!(
                "<w:{e} w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"999999\"/>",
                e = edge
            ));
        }
        xml.push_str("</w:tblBorders>");
        xml.push_str("<w:tblLayout w:type=\"fixed\"/>");
        xml.push_str("</w:tblPr><w:tblGrid>");
        for _ in 0..cols {
            xml.push_str(&format!("<w:gridCol w:w=\"{}\"/>", col_width));
        }
        xml.push_str("</w:tblGrid>");
        for (row_index, row) in table.rows.iter().enumerate() {
            xml.push_str("<w:tr>");
            for cell in row.iter() {
                xml.push_str("<w:tc><w:tcPr>");
                xml.push_str(&format!(
                    "<w:tcW w:type=\"dxa\" w:w=\"{}\"/>",
                    col_width
                ));
                if row_index == 0 {
                    xml.push_str("<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"F0F4F8\"/>");
                }
                xml.push_str("</w:tcPr>");
                xml.push_str(&cell.xml);
                xml.push_str("</w:tc>");
            }
            xml.push_str("</w:tr>");
        }
        xml.push_str("</w:tbl>");
        self.body.push_str(&xml);
        self.tables += 1;
        // 表格后补一个空段落，避免相邻表格在 Word 中被合并
        self.body.push_str("<w:p/>");
    }

    fn finish(&mut self) -> DocumentXml {
        self.flush();
        if !self.missing.is_empty() {
            let names = std::mem::take(&mut self.missing);
            self.body.push_str(
                "<w:p><w:pPr><w:pStyle w:val=\"Heading2\"/></w:pPr><w:r><w:t>缺失图片</w:t></w:r></w:p>",
            );
            self.paragraphs += 1;
            for name in &names {
                self.body.push_str(
                    "<w:p><w:pPr><w:pStyle w:val=\"Quote\"/></w:pPr><w:r><w:t xml:space=\"preserve\">",
                );
                self.body.push_str(&escape_xml(name));
                self.body.push_str("</w:t></w:r></w:p>");
                self.paragraphs += 1;
            }
            self.missing = names;
        }
        DocumentXml {
            body: std::mem::take(&mut self.body),
            paragraphs: self.paragraphs,
            tables: self.tables,
        }
    }
}

/// 提取图片文件名；外链 / data URI 返回 None
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

/// 解析常见图片格式的像素尺寸（PNG / JPEG / GIF / BMP），失败时按 4:3 占位
fn image_dimensions(data: &[u8], name: &str) -> Option<(u32, u32)> {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => {
            if data.len() >= 24 && &data[12..16] == b"IHDR" {
                let w = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
                let h = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
                return Some((w, h));
            }
        }
        "gif" => {
            if data.len() >= 10 {
                let w = u16::from_le_bytes([data[6], data[7]]) as u32;
                let h = u16::from_le_bytes([data[8], data[9]]) as u32;
                return Some((w, h));
            }
        }
        "bmp" => {
            if data.len() >= 26 {
                let w = i32::from_le_bytes([data[18], data[19], data[20], data[21]]).unsigned_abs();
                let h = i32::from_le_bytes([data[22], data[23], data[24], data[25]]).unsigned_abs();
                return Some((w, h));
            }
        }
        "jpg" | "jpeg" => {
            let mut i = 2usize;
            while i + 9 < data.len() {
                if data[i] != 0xFF {
                    i += 1;
                    continue;
                }
                let marker = data[i + 1];
                if (0xC0..=0xCF).contains(&marker)
                    && marker != 0xC4
                    && marker != 0xC8
                    && marker != 0xCC
                {
                    let h = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
                    let w = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
                    return Some((w, h));
                }
                let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
                if len < 2 {
                    break;
                }
                i += 2 + len;
            }
        }
        _ => {}
    }
    None
}

/// 像素 → EMU，并按正文宽度（6 英寸）与最大高度等比缩放
fn fit_size(px_w: u32, px_h: u32) -> (i64, i64) {
    const EMU_PER_PX: i64 = 9525;
    const MAX_W: i64 = 5_486_400; // 6 英寸
    const MAX_H: i64 = 6_858_000; // 7.5 英寸
    let w = (px_w.max(1) as i64) * EMU_PER_PX;
    let h = (px_h.max(1) as i64) * EMU_PER_PX;
    let scale = (MAX_W as f64 / w as f64)
        .min(MAX_H as f64 / h as f64)
        .min(1.0);
    (((w as f64) * scale) as i64, ((h as f64) * scale) as i64)
}

fn document_xml(document: &DocumentXml) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" \
xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
<w:body>{}<w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/>\
<w:pgMar w:top=\"1134\" w:right=\"1134\" w:bottom=\"1134\" w:left=\"1134\" w:header=\"851\" w:footer=\"992\" w:gutter=\"0\"/>\
</w:sectPr></w:body></w:document>",
        document.body
    )
}

fn styles_xml() -> String {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
    out.push_str("<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">");
    out.push_str("<w:docDefaults><w:rPrDefault><w:rPr>");
    out.push_str("<w:rFonts w:ascii=\"Calibri\" w:hAnsi=\"Calibri\" w:eastAsia=\"宋体\"/>");
    out.push_str("<w:sz w:val=\"21\"/><w:szCs w:val=\"21\"/>");
    out.push_str("<w:lang w:val=\"zh-CN\" w:eastAsia=\"zh-CN\"/>");
    out.push_str("</w:rPr></w:rPrDefault>");
    out.push_str("<w:pPrDefault><w:pPr><w:spacing w:after=\"120\" w:line=\"300\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault>");
    out.push_str("</w:docDefaults>");
    out.push_str("<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:qFormat/></w:style>");
    for (level, size, before, after) in [
        (1, 36, 360, 180),
        (2, 30, 280, 140),
        (3, 26, 240, 120),
        (4, 23, 200, 100),
        (5, 21, 180, 80),
        (6, 21, 160, 80),
    ] {
        out.push_str(&format!(
            "<w:style w:type=\"paragraph\" w:styleId=\"Heading{lv}\"><w:name w:val=\"heading {lv}\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/>\
<w:pPr><w:keepNext/><w:spacing w:before=\"{before}\" w:after=\"{after}\"/><w:outlineLvl w:val=\"{ol}\"/></w:pPr>\
<w:rPr><w:rFonts w:ascii=\"Calibri\" w:hAnsi=\"Calibri\" w:eastAsia=\"黑体\"/><w:b/><w:sz w:val=\"{size}\"/><w:szCs w:val=\"{size}\"/><w:color w:val=\"1F2933\"/></w:rPr></w:style>",
            lv = level, before = before, after = after, ol = level - 1, size = size
        ));
    }
    out.push_str("<w:style w:type=\"paragraph\" w:styleId=\"Quote\"><w:name w:val=\"Quote\"/><w:basedOn w:val=\"Normal\"/>\
<w:pPr><w:ind w:left=\"567\" w:right=\"283\"/><w:spacing w:before=\"60\" w:after=\"60\"/></w:pPr>\
<w:rPr><w:color w:val=\"52606D\"/><w:i/></w:rPr></w:style>");
    out.push_str("<w:style w:type=\"paragraph\" w:styleId=\"CodeBlock\"><w:name w:val=\"Code Block\"/><w:basedOn w:val=\"Normal\"/>\
<w:pPr><w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"F5F5F5\"/><w:spacing w:before=\"0\" w:after=\"0\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr>\
<w:rPr><w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\"/><w:sz w:val=\"18\"/><w:szCs w:val=\"18\"/></w:rPr></w:style>");
    out.push_str("<w:style w:type=\"character\" w:styleId=\"CodeChar\"><w:name w:val=\"Code Char\"/>\
<w:rPr><w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\"/><w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"F5F5F5\"/><w:sz w:val=\"19\"/></w:rPr></w:style>");
    out.push_str("</w:styles>");
    out
}

fn numbering_xml() -> String {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
    out.push_str("<w:numbering xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">");
    // 无序列表三种符号
    out.push_str("<w:abstractNum w:abstractNumId=\"0\"><w:multiLevelType w:val=\"hybridMultilevel\"/>");
    for (lvl, ch) in [(0, "\u{2022}"), (1, "o"), (2, "\u{25AA}")] {
        out.push_str(&format!(
            "<w:lvl w:ilvl=\"{lvl}\"><w:start w:val=\"1\"/><w:numFmt w:val=\"bullet\"/><w:lvlText w:val=\"{ch}\"/>\
<w:lvlJc w:val=\"left\"/><w:pPr><w:ind w:left=\"{ind}\" w:hanging=\"420\"/></w:pPr>\
<w:rPr><w:rFonts w:ascii=\"Symbol\" w:hAnsi=\"Symbol\" w:hint=\"default\"/></w:rPr></w:lvl>",
            lvl = lvl,
            ch = ch,
            ind = 720 + 420 * lvl
        ));
    }
    out.push_str("</w:abstractNum>");
    // 有序列表
    out.push_str("<w:abstractNum w:abstractNumId=\"1\"><w:multiLevelType w:val=\"hybridMultilevel\"/>");
    for lvl in 0..3 {
        out.push_str(&format!(
            "<w:lvl w:ilvl=\"{lvl}\"><w:start w:val=\"1\"/><w:numFmt w:val=\"{fmt}\"/><w:lvlText w:val=\"{text}\"/>\
<w:lvlJc w:val=\"left\"/><w:pPr><w:ind w:left=\"{ind}\" w:hanging=\"420\"/></w:pPr></w:lvl>",
            lvl = lvl,
            fmt = if lvl == 0 { "decimal" } else if lvl == 1 { "lowerLetter" } else { "lowerRoman" },
            text = if lvl == 0 { "%1." } else if lvl == 1 { "%2)" } else { "%3." },
            ind = 720 + 420 * lvl
        ));
    }
    out.push_str("</w:abstractNum>");
    out.push_str("<w:num w:numId=\"1\"><w:abstractNumId w:val=\"0\"/></w:num>");
    out.push_str("<w:num w:numId=\"2\"><w:abstractNumId w:val=\"1\"/></w:num>");
    out.push_str("</w:numbering>");
    out
}

fn content_types_xml(images: &BTreeMap<String, String>) -> String {
    let mut exts: BTreeMap<String, &'static str> = BTreeMap::new();
    for name in images.keys() {
        let ext = name
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_else(|| "png".to_string());
        // Content_Types 的 Extension 必须与实际媒体文件扩展名一致（jpg 不能写成 jpeg）
        let (ext, ct) = match ext.as_str() {
            "png" => ("png", "image/png"),
            "gif" => ("gif", "image/gif"),
            "bmp" => ("bmp", "image/bmp"),
            "webp" => ("webp", "image/webp"),
            "jpg" => ("jpg", "image/jpeg"),
            "jpeg" => ("jpeg", "image/jpeg"),
            _ => ("png", "image/png"),
        };
        exts.insert(ext.to_string(), ct);
    }
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
    out.push_str("<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">");
    out.push_str("<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>");
    out.push_str("<Default Extension=\"xml\" ContentType=\"application/xml\"/>");
    for (ext, ct) in exts {
        out.push_str(&format!("<Default Extension=\"{}\" ContentType=\"{}\"/>", ext, ct));
    }
    out.push_str("<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>");
    out.push_str("<Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>");
    out.push_str("<Override PartName=\"/word/numbering.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml\"/>");
    out.push_str("<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>");
    out.push_str("<Override PartName=\"/docProps/app.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.extended-properties+xml\"/>");
    out.push_str("</Types>");
    out
}

fn root_rels_xml() -> &'static str {
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/>\
<Relationship Id=\"rId3\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties\" Target=\"docProps/app.xml\"/>\
</Relationships>"
}

fn document_rels_xml(images: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
    out.push_str("<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">");
    out.push_str("<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>");
    out.push_str("<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering\" Target=\"numbering.xml\"/>");
    for (name, rid) in images {
        out.push_str(&format!(
            "<Relationship Id=\"{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/{}\"/>",
            rid,
            escape_xml(name)
        ));
    }
    out.push_str("</Relationships>");
    out
}

fn core_xml(title: &str) -> String {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" \
xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" \
xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">\
<dc:title>{}</dc:title><dc:creator>PDF Manager</dc:creator>\
<dcterms:created xsi:type=\"dcterms:W3CDTF\">{}</dcterms:created>\
<dcterms:modified xsi:type=\"dcterms:W3CDTF\">{}</dcterms:modified>\
</cp:coreProperties>",
        escape_xml(title),
        now,
        now
    )
}

fn app_xml() -> &'static str {
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties\" \
xmlns:vt=\"http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes\">\
<Application>PDF Manager</Application></Properties>"
}

fn write_docx(
    path: &Path,
    document: &DocumentXml,
    builder: &Builder,
    title: &str,
) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("创建 Word 文件失败：{}", e))?;
    let mut zip = ZipWriter::new(file);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

    let parts: Vec<(&str, String)> = vec![
        ("[Content_Types].xml", content_types_xml(&builder.image_rids)),
        ("_rels/.rels", root_rels_xml().to_string()),
        ("word/document.xml", document_xml(document)),
        ("word/styles.xml", styles_xml()),
        ("word/numbering.xml", numbering_xml()),
        ("word/_rels/document.xml.rels", document_rels_xml(&builder.image_rids)),
        ("docProps/core.xml", core_xml(title)),
        ("docProps/app.xml", app_xml().to_string()),
    ];
    for (name, content) in parts {
        zip.start_file(name, deflated)
            .map_err(|e| format!("写入 {} 失败：{}", name, e))?;
        zip.write_all(content.as_bytes())
            .map_err(|e| format!("写入 {} 失败：{}", name, e))?;
    }
    for (name, data) in &builder.images {
        zip.start_file(format!("word/media/{}", name), stored)
            .map_err(|e| format!("写入图片 {} 失败：{}", name, e))?;
        zip.write_all(data)
            .map_err(|e| format!("写入图片 {} 失败：{}", name, e))?;
    }
    zip.finish()
        .map_err(|e| format!("完成 Word 打包失败：{}", e))?;
    Ok(())
}

/// 导出文件名：去扩展名、替换 Windows 非法字符、按 ID 去重
fn docx_filename(title: &str, doc_id: i64) -> String {
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
    format!("{}__{}.docx", truncated, doc_id)
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
