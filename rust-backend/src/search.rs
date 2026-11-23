//! Markdown 全文检索（Tantivy + jieba 分词）。
//!
//! 设计要点：
//! - 索引粒度是「一篇文档一条」，doc_id 作为唯一键，增量更新用 delete_term + add 实现；
//! - 增量判定以 Markdown 文件内容的 SHA-256 为权威依据，mtime/size 仅用于展示与诊断，
//!   因此「内容变了但 mtime 没变」也能识别；
//! - 全量构建写入独立 staging 目录，成功后整体替换正式目录，旧索引在替换前持续可检索；
//! - 索引带 schema 版本与元数据文件，启动时校验，损坏或版本不兼容会隔离旧目录并自动重建；
//! - 构建在独立线程执行，带进度快照，同一时间只允许一个构建任务（重复触发由上层返回 409）；
//! - 正文只索引不存储，命中片段在检索时按 markdown_path 回读原文生成，索引体积接近倒排本身。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use jieba_rs::{Jieba, TokenizeMode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tantivy::collector::{Count, TopDocs};
use tantivy::query::{BooleanQuery, BoostQuery, Occur, Query, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TantivyDocument, TextFieldIndexing, TextOptions, Value, FAST,
    INDEXED, STORED, STRING,
};
use tantivy::tokenizer::{Token, TokenStream, Tokenizer};
use tantivy::{Index, IndexReader, ReloadPolicy, Term};

use crate::config::Config;
use crate::db::Db;
use crate::log_error;
use crate::log_info;

/// 索引结构版本：字段或分词方式变化时递增，启动校验发现不一致会删除旧索引并自动重建
pub const SCHEMA_VERSION: u64 = 2;
const TOKENIZER_NAME: &str = "jieba";
const META_FILE: &str = "search_meta.json";
/// app_meta 里记录「最近一次成功打开的索引目录」，用于判断配置目录是否变更
const BUILT_FOLDER_KEY: &str = "search.index_built_folder";
const WRITER_HEAP_BYTES: usize = 80 * 1024 * 1024;
const MAX_QUERY_TERMS: usize = 16;

/// 单篇 Markdown 生成片段时的读取上限，避免超大文件被整体读进内存
const MAX_SNIPPET_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// 清洗后参与窗口挑选的正文上限（字符）
const MAX_SNIPPET_TEXT_CHARS: usize = 1_200_000;
const SNIPPET_WINDOW_CHARS: usize = 200;
const SNIPPET_LEAD_CHARS: usize = 30;
const SNIPPET_FALLBACK_CHARS: usize = 160;
/// 每个查询词最多记录的命中位置，避免极端高频词拖慢窗口挑选
const MAX_TERM_OCCURRENCES: usize = 120;

// ---------------------------------------------------------------------------
// 分词
// ---------------------------------------------------------------------------

fn jieba() -> &'static Jieba {
    static JIEBA: OnceLock<Jieba> = OnceLock::new();
    JIEBA.get_or_init(Jieba::new)
}

/// 只保留包含字母、数字或中日韩文字的 token，过滤纯标点与空白
fn has_word_char(text: &str) -> bool {
    text.chars().any(|c| {
        c.is_alphanumeric()
            || ('\u{3040}'..='\u{30ff}').contains(&c)
            || ('\u{3400}'..='\u{4dbf}').contains(&c)
            || ('\u{4e00}'..='\u{9fff}').contains(&c)
            || ('\u{f900}'..='\u{faff}').contains(&c)
    })
}

/// 与索引期一致的分词：Search 模式、小写、丢弃纯标点、按出现顺序编号
fn tokenize_terms(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    for token in jieba().tokenize(text, TokenizeMode::Search, true) {
        let word = token.word.trim();
        if word.is_empty() || !has_word_char(word) {
            continue;
        }
        terms.push(word.to_lowercase());
    }
    terms
}

#[derive(Clone, Default)]
struct JiebaTokenizer;

struct JiebaTokenStream {
    tokens: std::vec::IntoIter<Token>,
    current: Token,
}

impl TokenStream for JiebaTokenStream {
    fn advance(&mut self) -> bool {
        match self.tokens.next() {
            Some(token) => {
                self.current = token;
                true
            }
            None => false,
        }
    }

    fn token(&self) -> &Token {
        &self.current
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.current
    }
}

impl Tokenizer for JiebaTokenizer {
    type TokenStream<'a> = JiebaTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        // jieba-rs 的 start/end 是「字符」下标，Tantivy 需要字节偏移，必须转换，
        // 否则中文正文在生成摘要（按字节切片）时会 panic。
        let char_bytes: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        let byte_at = |char_idx: usize| -> usize {
            char_bytes.get(char_idx).copied().unwrap_or(text.len())
        };
        let mut tokens = Vec::new();
        for (position, token) in jieba()
            .tokenize(text, TokenizeMode::Search, true)
            .into_iter()
            .enumerate()
        {
            let word = token.word.trim();
            if word.is_empty() || !has_word_char(word) {
                continue;
            }
            tokens.push(Token {
                offset_from: byte_at(token.start),
                offset_to: byte_at(token.end),
                position,
                text: word.to_lowercase(),
                position_length: 1,
            });
        }
        JiebaTokenStream {
            tokens: tokens.into_iter(),
            current: Token::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Fields {
    doc_id: Field,
    filename: Field,
    title: Field,
    headings: Field,
    body: Field,
    markdown_path: Field,
}

fn build_schema() -> (Schema, Fields) {
    // 小字段（文件名/标题/小标题）既索引又存储，命中后可直接展示
    let stored_text = || {
        TextOptions::default()
            .set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer(TOKENIZER_NAME)
                    .set_index_option(IndexRecordOption::WithFreqsAndPositions),
            )
            .set_stored()
    };
    // 正文只建倒排、不存储：片段在检索时按 markdown_path 回读文件生成，
    // 这样索引里不再保存一份完整正文，体积接近倒排本身。
    let indexed_only_text = || {
        TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(TOKENIZER_NAME)
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        )
    };

    let mut builder = Schema::builder();
    let doc_id = builder.add_i64_field("doc_id", INDEXED | STORED | FAST);
    let filename = builder.add_text_field("filename", stored_text());
    let title = builder.add_text_field("title", stored_text());
    let headings = builder.add_text_field("headings", stored_text());
    let body = builder.add_text_field("body", indexed_only_text());
    let markdown_path = builder.add_text_field("markdown_path", STRING | STORED);
    (
        builder.build(),
        Fields {
            doc_id,
            filename,
            title,
            headings,
            body,
            markdown_path,
        },
    )
}

// ---------------------------------------------------------------------------
// Markdown 清洗
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct ParsedDoc {
    pub title: String,
    pub headings: String,
    pub body: String,
}

fn is_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &trimmed[hashes..];
    if !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None;
    }
    Some((hashes, rest.trim()))
}

/// 去掉行内 Markdown 标记，保留可检索的文字
fn strip_inline(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '!' && i + 1 < chars.len() && chars[i + 1] == '[' {
            let mut j = i + 2;
            let mut label = String::new();
            while j < chars.len() && chars[j] != ']' {
                label.push(chars[j]);
                j += 1;
            }
            // 跳过图片地址
            if j + 1 < chars.len() && chars[j + 1] == '(' {
                let mut k = j + 2;
                while k < chars.len() && chars[k] != ')' {
                    k += 1;
                }
                i = (k + 1).min(chars.len());
            } else {
                i = (j + 1).min(chars.len());
            }
            out.push_str(&label);
            continue;
        }
        if c == '[' {
            let mut j = i + 1;
            let mut label = String::new();
            while j < chars.len() && chars[j] != ']' {
                label.push(chars[j]);
                j += 1;
            }
            if j + 1 < chars.len() && chars[j + 1] == '(' {
                let mut k = j + 2;
                while k < chars.len() && chars[k] != ')' {
                    k += 1;
                }
                i = (k + 1).min(chars.len());
                out.push_str(&label);
                continue;
            }
        }
        if c == '<' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '>' {
                j += 1;
            }
            if j < chars.len() {
                i = j + 1;
                continue;
            }
        }
        if matches!(c, '*' | '_' | '`' | '~' | '\\' | '|') {
            out.push(' ');
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 把 Markdown 拆成标题、标题集合与正文，正文不包含标题行与代码块
pub fn parse_markdown(markdown: &str, filename: &str) -> ParsedDoc {
    let mut h1: Option<String> = None;
    let mut first_heading: Option<String> = None;
    let mut headings = Vec::new();
    let mut body = Vec::new();
    let mut in_code = false;

    for raw in markdown.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code = !in_code;
            continue;
        }
        if in_code || trimmed.is_empty() {
            continue;
        }
        if let Some((level, text)) = is_heading(line) {
            let cleaned = strip_inline(text);
            if cleaned.is_empty() {
                continue;
            }
            if level == 1 && h1.is_none() {
                h1 = Some(cleaned.clone());
            }
            if first_heading.is_none() {
                first_heading = Some(cleaned.clone());
            }
            headings.push(cleaned);
            continue;
        }
        // 表格分隔行（|---|---|）
        if trimmed.chars().all(|c| matches!(c, '-' | ':' | '|' | ' ')) && trimmed.contains('-') {
            continue;
        }
        // 分隔线
        if trimmed
            .chars()
            .all(|c| matches!(c, '-' | '*' | '_' | '=' | ' '))
            && trimmed.len() >= 3
        {
            continue;
        }
        let mut content = trimmed;
        // 引用、列表前缀
        while content.starts_with('>') {
            content = content[1..].trim_start();
        }
        if let Some(rest) = content.strip_prefix("- ") {
            content = rest.trim_start();
        } else if let Some(rest) = content.strip_prefix("* ") {
            content = rest.trim_start();
        } else if let Some(rest) = content.strip_prefix("+ ") {
            content = rest.trim_start();
        }
        let cleaned = strip_inline(content);
        if !cleaned.is_empty() {
            body.push(cleaned);
        }
    }

    let fallback_title = filename
        .rsplit_once('.')
        .map(|(name, _)| name)
        .unwrap_or(filename)
        .to_string();
    ParsedDoc {
        title: h1.or(first_heading).unwrap_or(fallback_title),
        headings: headings.join(" "),
        body: body.join("\n"),
    }
}

// ---------------------------------------------------------------------------
// 索引状态
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildMode {
    Full,
    Incremental,
}

impl BuildMode {
    fn as_str(self) -> &'static str {
        match self {
            BuildMode::Full => "full",
            BuildMode::Incremental => "incremental",
        }
    }
}

#[derive(Clone, Default)]
struct BuildProgress {
    running: bool,
    mode: String,
    total: usize,
    processed: usize,
    added: usize,
    updated: usize,
    deleted: usize,
    skipped: usize,
    failed: usize,
    last_error: Option<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct IndexStatus {
    /// building | ready | empty | error
    pub state: String,
    /// 索引是否可检索（构建中旧索引也保持可用）
    pub ready: bool,
    pub running: bool,
    pub mode: Option<String>,
    pub total: usize,
    pub processed: usize,
    pub added: usize,
    pub updated: usize,
    pub deleted: usize,
    pub skipped: usize,
    pub failed: usize,
    pub doc_count: usize,
    pub source_doc_count: usize,
    /// 索引状态被标记为待增量更新的文档数（批量改名后置位）
    pub pending_incremental: usize,
    pub index_size: u64,
    pub index_path: String,
    /// 配置目录与已构建索引目录不一致：旧索引不能沿用，需要重建
    pub needs_rebuild: bool,
    pub schema_version: u64,
    pub last_error: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

#[derive(Serialize)]
pub struct SearchHit {
    pub doc_id: i64,
    pub filename: String,
    pub title: String,
    pub score: f32,
    /// 已做 HTML 转义，命中词用 <b> 包裹
    pub snippet: String,
    pub markdown_path: String,
}

#[derive(Serialize)]
pub struct SearchResponse {
    pub query: String,
    pub terms: Vec<String>,
    pub page: usize,
    pub page_size: usize,
    pub total: usize,
    pub took_ms: u64,
    pub results: Vec<SearchHit>,
    /// smart | exact
    pub mode: String,
    /// smart 模式下 AND 无结果退化为 OR 时为 true
    pub relaxed: bool,
    /// 退化时实际命中的词项
    pub matched_terms: Vec<String>,
    /// exact 模式候选超过扫描上限时为 true（total 仅代表已校验范围）
    pub truncated: bool,
}

#[derive(Debug)]
pub enum SearchError {
    NotReady(String),
    BadRequest(String),
    Internal(String),
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchError::NotReady(m) | SearchError::BadRequest(m) | SearchError::Internal(m) => {
                f.write_str(m)
            }
        }
    }
}

#[derive(Debug)]
pub enum StartError {
    AlreadyRunning,
    Failed(String),
}

pub struct BuildStarted {
    pub status: IndexStatus,
}

#[derive(Clone)]
struct StateRow {
    content_hash: String,
    schema_version: i64,
    filename: String,
    markdown_path: String,
}

#[derive(Clone)]
struct Candidate {
    id: i64,
    filename: String,
    markdown_path: String,
    abs_path: PathBuf,
}

struct RuntimeIndex {
    index: Index,
    reader: IndexReader,
}

struct Inner {
    db: Db,
    cfg: Arc<RwLock<Config>>,
    runtime: RwLock<Option<RuntimeIndex>>,
    progress: Mutex<BuildProgress>,
    building: AtomicBool,
    last_error: Mutex<Option<String>>,
    last_doc_count: Mutex<usize>,
}

#[derive(Clone)]
pub struct SearchManager {
    inner: Arc<Inner>,
}

impl SearchManager {
    pub fn new(cfg: Arc<RwLock<Config>>, db: Db) -> Self {
        Self {
            inner: Arc::new(Inner {
                db,
                cfg,
                runtime: RwLock::new(None),
                progress: Mutex::new(BuildProgress::default()),
                building: AtomicBool::new(false),
                last_error: Mutex::new(None),
                last_doc_count: Mutex::new(0),
            }),
        }
    }

    fn cfg_snapshot(&self) -> Config {
        match self.inner.cfg.read() {
            Ok(c) => c.clone(),
            Err(e) => e.into_inner().clone(),
        }
    }

    fn index_dir(&self) -> PathBuf {
        self.cfg_snapshot().search_index_folder
    }

    /// 最近一次成功打开（或构建）的索引目录；没有记录时返回 None
    fn built_folder(&self) -> Option<PathBuf> {
        self.inner
            .db
            .get_meta(BUILT_FOLDER_KEY, None)
            .ok()
            .flatten()
            .filter(|v| !v.trim().is_empty())
            .map(PathBuf::from)
    }

    fn record_built_folder(&self, dir: &Path) {
        if let Err(e) = self
            .inner
            .db
            .set_meta(BUILT_FOLDER_KEY, &dir.to_string_lossy())
        {
            log_error!("[检索] 记录索引目录失败：{e}");
        }
    }

    /// 配置的索引目录与已构建目录不一致，说明旧索引不能沿用
    fn folder_needs_rebuild(&self) -> bool {
        match self.built_folder() {
            Some(built) => !same_dir(&built, &self.index_dir()),
            None => false,
        }
    }

    /// 设置面板切换索引目录后调用：丢弃内存中的旧索引，尝试加载新目录。
    /// 返回 true 表示新目录中已有可用索引并已加载；false 表示需要重建。
    pub fn reload_index_dir(&self) -> bool {
        if let Ok(mut guard) = self.inner.runtime.write() {
            *guard = None;
        }
        let dir = self.index_dir();
        let _ = fs::create_dir_all(&dir);
        if !self.index_built() {
            if let Ok(mut c) = self.inner.last_doc_count.lock() {
                *c = 0;
            }
            return false;
        }
        match self.open_runtime() {
            Ok(rt) => {
                let count = rt.reader.searcher().num_docs() as usize;
                if let Ok(mut c) = self.inner.last_doc_count.lock() {
                    *c = count;
                }
                let _ = self.install_runtime(rt);
                self.record_built_folder(&dir);
                self.set_error(None);
                log_info!("[检索] 已切换索引目录：{}（{} 篇文档）", dir.display(), count);
                true
            }
            Err(e) => {
                log_error!("[检索] 新索引目录不可用：{e}，需要重建");
                if let Ok(mut c) = self.inner.last_doc_count.lock() {
                    *c = 0;
                }
                false
            }
        }
    }

    /// 切换目录后新目录中没有可用索引：记录旧目录，让状态持续显示「需要重建」
    pub fn mark_needs_rebuild(&self, previous_dir: &Path) {
        if !same_dir(previous_dir, &self.index_dir()) {
            self.record_built_folder(previous_dir);
        }
    }

    fn staging_dir(&self) -> PathBuf {
        sibling_dir(&self.index_dir(), "building")
    }

    fn backup_dir(&self) -> PathBuf {
        sibling_dir(&self.index_dir(), "old")
    }

    fn set_error(&self, message: Option<String>) {
        if let Ok(mut e) = self.inner.last_error.lock() {
            *e = message;
        }
    }

    fn last_error(&self) -> Option<String> {
        self.inner
            .last_error
            .lock()
            .ok()
            .and_then(|e| e.clone())
    }

    fn update_progress<F: FnOnce(&mut BuildProgress)>(&self, f: F) {
        if let Ok(mut p) = self.inner.progress.lock() {
            f(&mut p);
        }
    }

    /// 尝试打开并安装索引；调用方需保证不会与目录替换并发
    fn open_runtime(&self) -> Result<RuntimeIndex> {
        let dir = self.index_dir();
        open_validated(&dir)
    }

    fn index_built(&self) -> bool {
        self.index_dir().join(META_FILE).exists()
    }

    fn ensure_open(&self) -> Result<()> {
        // 配置目录已变更时不能静默打开旧目录，否则检索仍命中旧索引
        if self.folder_needs_rebuild() {
            return Err(anyhow!("索引目录已变更，需要重建索引"));
        }
        {
            let guard = self
                .inner
                .runtime
                .read()
                .map_err(|_| anyhow!("索引读锁已损坏"))?;
            if guard.is_some() {
                return Ok(());
            }
        }
        // 从未构建过索引不算故障，不写入 last_error，前端显示「未构建」
        if !self.index_built() {
            return Err(anyhow!("索引尚未构建"));
        }
        let mut guard = self
            .inner
            .runtime
            .write()
            .map_err(|_| anyhow!("索引写锁已损坏"))?;
        if guard.is_none() {
            match self.open_runtime() {
                Ok(rt) => {
                    *guard = Some(rt);
                    self.set_error(None);
                }
                Err(e) => {
                    let msg = format!("索引不可用：{e}");
                    self.set_error(Some(msg.clone()));
                    return Err(anyhow!(msg));
                }
            }
        }
        Ok(())
    }

    fn install_runtime(&self, rt: RuntimeIndex) -> Result<()> {
        let mut guard = self
            .inner
            .runtime
            .write()
            .map_err(|_| anyhow!("索引写锁已损坏"))?;
        *guard = Some(rt);
        Ok(())
    }

    /// 启动校验：索引缺失视为未构建；版本不兼容或损坏则隔离旧目录并自动全量重建
    pub fn startup_check(&self) {
        let dir = self.index_dir();
        let _ = fs::create_dir_all(&dir);
        let meta_path = dir.join(META_FILE);

        if !meta_path.exists() {
            // 空目录=从未构建，保持未构建状态；非空但缺元数据=损坏或旧版残留
            let has_files = fs::read_dir(&dir)
                .map(|mut it| it.next().is_some())
                .unwrap_or(false);
            if has_files {
                log_error!("[检索] 索引目录缺少版本信息，隔离并重建：{}", dir.display());
                self.quarantine();
                self.auto_rebuild();
            }
            return;
        }

        let meta = match read_meta(&dir) {
            Ok(meta) => meta,
            Err(e) => {
                log_error!("[检索] 读取索引元数据失败：{e}，隔离并重建");
                self.quarantine();
                self.auto_rebuild();
                return;
            }
        };

        if meta.schema_version != SCHEMA_VERSION {
            // 版本不兼容的旧索引无法复用：直接删掉再重建，避免它与新索引长期各占一份磁盘
            log_error!(
                "[检索] 索引版本不兼容（{} != {}），删除旧索引并重建",
                meta.schema_version,
                SCHEMA_VERSION
            );
            self.discard_index();
            self.auto_rebuild();
            return;
        }

        match self.open_runtime() {
            Ok(rt) => {
                let count = rt.reader.searcher().num_docs() as usize;
                if let Ok(mut c) = self.inner.last_doc_count.lock() {
                    *c = count;
                }
                let _ = self.install_runtime(rt);
                self.record_built_folder(&dir);
                log_info!(
                    "[检索] 索引校验通过：{} 篇文档，{}",
                    count,
                    dir.display()
                );
            }
            Err(e) => {
                log_error!("[检索] 索引打开失败：{e}，隔离并重建");
                self.quarantine();
                self.auto_rebuild();
            }
        }
    }

    /// 版本不兼容时直接清理旧索引目录（内容已无法复用，留着只会占磁盘）
    fn discard_index(&self) {
        let dir = self.index_dir();
        if let Ok(mut guard) = self.inner.runtime.write() {
            *guard = None;
        }
        if dir.exists() {
            if let Err(e) = fs::remove_dir_all(&dir) {
                log_error!("[检索] 删除旧索引目录失败：{e}");
            }
        }
        let _ = fs::create_dir_all(&dir);
    }

    /// 把损坏的索引目录改名隔离，避免下次启动继续命中坏数据
    fn quarantine(&self) {
        let dir = self.index_dir();
        if !dir.exists() {
            return;
        }
        if let Ok(mut guard) = self.inner.runtime.write() {
            *guard = None;
        }
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let broken = PathBuf::from(format!("{}.broken-{}", dir.display(), stamp));
        if let Err(e) = fs::rename(&dir, &broken) {
            log_error!("[检索] 隔离损坏索引失败：{e}，尝试直接清理");
            let _ = fs::remove_dir_all(&dir);
        } else {
            log_error!("[检索] 损坏索引已隔离到 {}", broken.display());
        }
        let _ = fs::create_dir_all(&dir);
    }

    fn auto_rebuild(&self) {
        if self.source_doc_count().unwrap_or(0) == 0 {
            log_info!("[检索] 没有可索引文档，跳过自动重建");
            return;
        }
        match self.start_build(BuildMode::Full) {
            Ok(_) => log_info!("[检索] 已自动触发索引全量重建"),
            Err(StartError::AlreadyRunning) => {}
            Err(StartError::Failed(e)) => log_error!("[检索] 自动重建启动失败：{e}"),
        }
    }

    fn source_doc_count(&self) -> Result<usize> {
        let conn = self.inner.db.conn()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pdf_documents WHERE markdown_path IS NOT NULL AND TRIM(markdown_path) <> ''",
            [],
            |r| r.get(0),
        )?;
        Ok(count as usize)
    }

    fn index_size(&self) -> u64 {
        dir_size(&self.index_dir())
    }

    fn doc_count(&self) -> usize {
        if let Ok(guard) = self.inner.runtime.read() {
            if let Some(rt) = guard.as_ref() {
                return rt.reader.searcher().num_docs() as usize;
            }
        }
        self.inner
            .last_doc_count
            .lock()
            .map(|c| *c)
            .unwrap_or(0)
    }

    pub fn status(&self) -> IndexStatus {
        // 状态查询只做轻量探测：索引已安装时直接读；构建过但未安装时尝试打开一次
        if !self.is_running() && self.index_built() {
            let installed = self
                .inner
                .runtime
                .read()
                .map(|g| g.is_some())
                .unwrap_or(false);
            if !installed {
                let _ = self.ensure_open();
            }
        }

        let progress = self
            .inner
            .progress
            .lock()
            .map(|p| p.clone())
            .unwrap_or_default();
        let ready = self
            .inner
            .runtime
            .read()
            .map(|g| g.is_some())
            .unwrap_or(false);
        let needs_rebuild = self.folder_needs_rebuild();
        let doc_count = self.doc_count();
        let last_error = self.last_error();

        let state = if progress.running {
            "building".to_string()
        } else if last_error.is_some() {
            // 构建失败/索引不可用：即使旧索引仍可检索也要把失败原因暴露到状态区
            "error".to_string()
        } else if needs_rebuild {
            // 配置目录已变更且新目录没有可用索引：明确提示需要重建
            "needs_rebuild".to_string()
        } else if !ready || doc_count == 0 {
            "empty".to_string()
        } else {
            "ready".to_string()
        };

        IndexStatus {
            state,
            ready: ready && !needs_rebuild,
            running: progress.running,
            mode: if progress.mode.is_empty() {
                None
            } else {
                Some(progress.mode.clone())
            },
            total: progress.total,
            processed: progress.processed,
            added: progress.added,
            updated: progress.updated,
            deleted: progress.deleted,
            skipped: progress.skipped,
            failed: progress.failed,
            doc_count,
            source_doc_count: self.source_doc_count().unwrap_or(0),
            pending_incremental: self
                .inner
                .db
                .pending_index_count()
                .unwrap_or(0)
                .max(0) as usize,
            index_size: self.index_size(),
            index_path: self.index_dir().display().to_string(),
            needs_rebuild,
            schema_version: SCHEMA_VERSION,
            last_error: last_error.or(progress.last_error),
            started_at: progress.started_at,
            finished_at: progress.finished_at,
        }
    }

    pub fn is_running(&self) -> bool {
        self.inner.building.load(Ordering::SeqCst)
    }

    /// 启动构建；已在构建时返回 AlreadyRunning（由路由层映射为 409）
    pub fn start_build(&self, mode: BuildMode) -> std::result::Result<BuildStarted, StartError> {
        if self
            .inner
            .building
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(StartError::AlreadyRunning);
        }

        let dir = self.index_dir();
        if let Err(e) = fs::create_dir_all(&dir) {
            self.inner.building.store(false, Ordering::SeqCst);
            return Err(StartError::Failed(format!("创建索引目录失败：{e}")));
        }

        self.set_error(None);
        let started_at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        self.update_progress(|p| {
            *p = BuildProgress {
                running: true,
                mode: mode.as_str().to_string(),
                started_at: Some(started_at),
                ..BuildProgress::default()
            };
        });

        let this = self.clone();
        std::thread::spawn(move || {
            let began = Instant::now();
            let result = match mode {
                BuildMode::Full => this.run_full(),
                BuildMode::Incremental => this.run_incremental(),
            };
            let elapsed = began.elapsed().as_secs_f64();
            match result {
                Ok(summary) => {
                    log_info!(
                        "[检索] 索引构建完成（{}）：新增 {}，更新 {}，删除 {}，跳过 {}，失败 {}，文档 {}，耗时 {:.1}s",
                        mode.as_str(),
                        summary.added,
                        summary.updated,
                        summary.deleted,
                        summary.skipped,
                        summary.failed,
                        summary.doc_count,
                        elapsed
                    );
                    this.update_progress(|p| {
                        p.running = false;
                        p.total = summary.total;
                        p.processed = summary.processed;
                        p.added = summary.added;
                        p.updated = summary.updated;
                        p.deleted = summary.deleted;
                        p.skipped = summary.skipped;
                        p.failed = summary.failed;
                        p.finished_at = Some(
                            chrono::Local::now()
                                .format("%Y-%m-%d %H:%M:%S")
                                .to_string(),
                        );
                    });
                    if let Ok(mut c) = this.inner.last_doc_count.lock() {
                        *c = summary.doc_count;
                    }
                    this.set_error(None);
                }
                Err(e) => {
                    let msg = e.to_string();
                    log_error!("[检索] 索引构建失败（{}）：{}", mode.as_str(), msg);
                    this.update_progress(|p| {
                        p.running = false;
                        p.last_error = Some(msg.clone());
                        p.finished_at = Some(
                            chrono::Local::now()
                                .format("%Y-%m-%d %H:%M:%S")
                                .to_string(),
                        );
                    });
                    this.set_error(Some(msg));
                }
            }
            this.inner.building.store(false, Ordering::SeqCst);
        });

        Ok(BuildStarted {
            status: self.status(),
        })
    }

    // -----------------------------------------------------------------------
    // 全量构建
    // -----------------------------------------------------------------------

    fn run_full(&self) -> Result<BuildSummary> {
        let dir = self.index_dir();
        let staging = self.staging_dir();
        let backup = self.backup_dir();

        if staging.exists() {
            fs::remove_dir_all(&staging)
                .with_context(|| format!("清理旧 staging 目录失败：{}", staging.display()))?;
        }
        if backup.exists() {
            let _ = fs::remove_dir_all(&backup);
        }
        fs::create_dir_all(&staging)
            .with_context(|| format!("创建 staging 目录失败：{}", staging.display()))?;

        let candidates = self.load_candidates()?;
        self.update_progress(|p| {
            p.total = candidates.len();
            p.processed = 0;
        });

        let (schema, fields) = build_schema();
        let index = Index::create_in_dir(&staging, schema)
            .with_context(|| format!("创建索引失败：{}", staging.display()))?;
        index
            .tokenizers()
            .register(TOKENIZER_NAME, JiebaTokenizer);
        let mut writer = index
            .writer(WRITER_HEAP_BYTES)
            .context("创建索引写入器失败")?;

        let mut summary = BuildSummary {
            total: candidates.len(),
            ..BuildSummary::default()
        };
        let mut states: Vec<(i64, StateRow)> = Vec::new();
        let began = Instant::now();

        for candidate in &candidates {
            summary.processed += 1;
            if !candidate.abs_path.is_file() {
                // Markdown 已被移动/删除：全量构建里算跳过，不算失败
                summary.skipped += 1;
                self.update_progress(|p| p.processed = summary.processed);
                continue;
            }
            match self.index_one(&writer, fields, candidate) {
                Ok(row) => {
                    summary.added += 1;
                    states.push((candidate.id, row));
                }
                Err(e) => {
                    summary.failed += 1;
                    log_error!(
                        "[检索] 全量构建跳过文档 {}（{}）：{}",
                        candidate.id,
                        candidate.filename,
                        e
                    );
                }
            }
            self.update_progress(|p| {
                p.processed = summary.processed;
                p.added = summary.added;
                p.updated = summary.updated;
                p.skipped = summary.skipped;
                p.failed = summary.failed;
            });
            if summary.processed % 200 == 0 {
                log_info!(
                    "[检索] 全量构建进度 {}/{}，已用 {:.1}s，失败 {}",
                    summary.processed,
                    summary.total,
                    began.elapsed().as_secs_f64(),
                    summary.failed
                );
            }
        }

        writer.commit().context("提交全量索引失败")?;
        let _ = writer.wait_merging_threads();
        drop(index);

        write_meta(&staging, &IndexMeta {
            schema_version: SCHEMA_VERSION,
            built_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            doc_count: states.len(),
        })?;

        // 目录整体替换：先摘掉内存中的旧索引，再改名，最后安装新索引
        {
            let mut guard = self
                .inner
                .runtime
                .write()
                .map_err(|_| anyhow!("索引写锁已损坏"))?;
            // 释放旧索引的 mmap 句柄，否则 Windows 上目录改名可能失败
            *guard = None;

            if dir.exists() {
                if let Err(e) = fs::rename(&dir, &backup) {
                    // 旧目录被占用时恢复旧索引，避免检索能力凭空消失
                    *guard = open_validated(&dir).ok();
                    return Err(anyhow!(
                        "旧索引目录无法替换（可能仍被占用）：{} -> {}：{e}",
                        dir.display(),
                        backup.display()
                    ));
                }
            }
            if let Err(e) = fs::rename(&staging, &dir) {
                // 尝试回滚，保证旧索引还能继续服务
                if backup.exists() && fs::rename(&backup, &dir).is_ok() {
                    *guard = open_validated(&dir).ok();
                }
                return Err(anyhow!("新索引目录替换失败：{e}"));
            }
            let runtime = match open_validated(&dir) {
                Ok(rt) => rt,
                Err(e) => {
                    // 新索引不可用：回退到旧索引，避免整体不可检索
                    let _ = fs::remove_dir_all(&dir);
                    if backup.exists() && fs::rename(&backup, &dir).is_ok() {
                        *guard = open_validated(&dir).ok();
                    }
                    return Err(anyhow!("打开新索引失败：{e}"));
                }
            };
            *guard = Some(runtime);
        }

        self.replace_state_table(&states)?;
        summary.doc_count = states.len();
        self.record_built_folder(&dir);

        if backup.exists() {
            let _ = fs::remove_dir_all(&backup);
        }
        Ok(summary)
    }

    // -----------------------------------------------------------------------
    // 增量构建
    // -----------------------------------------------------------------------

    fn run_incremental(&self) -> Result<BuildSummary> {
        if self.ensure_open().is_err() {
            log_info!("[检索] 增量更新时索引不可用，自动降级为全量重建");
            return self.run_full();
        }

        let candidates = self.load_candidates()?;
        self.update_progress(|p| {
            p.total = candidates.len();
            p.processed = 0;
        });

        let previous = self.load_state_table()?;
        let mut seen: HashSet<i64> = HashSet::new();
        let mut summary = BuildSummary {
            total: candidates.len(),
            ..BuildSummary::default()
        };
        let mut changed: Vec<(i64, StateRow)> = Vec::new();
        let mut removed_ids: Vec<i64> = Vec::new();

        // 取索引写入器与字段；短时间持有写锁获取 Index 克隆即可
        let (index, fields) = {
            let guard = self
                .inner
                .runtime
                .read()
                .map_err(|_| anyhow!("索引读锁已损坏"))?;
            let rt = guard.as_ref().ok_or_else(|| anyhow!("索引尚未就绪"))?;
            (rt.index.clone(), {
                let schema = rt.index.schema();
                Fields {
                    doc_id: schema.get_field("doc_id")?,
                    filename: schema.get_field("filename")?,
                    title: schema.get_field("title")?,
                    headings: schema.get_field("headings")?,
                    body: schema.get_field("body")?,
                    markdown_path: schema.get_field("markdown_path")?,
                }
            })
        };
        let mut writer = index
            .writer(WRITER_HEAP_BYTES)
            .context("创建索引写入器失败")?;

        for candidate in &candidates {
            seen.insert(candidate.id);
            summary.processed += 1;
            let prev = previous.get(&candidate.id);

            if !candidate.abs_path.is_file() {
                // 文件不存在：从索引与状态表中移除
                if prev.is_some() {
                    writer.delete_term(Term::from_field_i64(fields.doc_id, candidate.id));
                    removed_ids.push(candidate.id);
                    summary.deleted += 1;
                } else {
                    summary.skipped += 1;
                }
                self.touch_progress(&summary);
                continue;
            }

            let bytes = match fs::read(&candidate.abs_path) {
                Ok(b) => b,
                Err(e) => {
                    summary.failed += 1;
                    log_error!(
                        "[检索] 增量读取失败 {}（{}）：{}",
                        candidate.id,
                        candidate.abs_path.display(),
                        e
                    );
                    self.touch_progress(&summary);
                    continue;
                }
            };
            let hash = sha256_hex(&bytes);

            // 内容未变且元数据未变：跳过
            if let Some(row) = prev {
                if row.content_hash == hash
                    && row.schema_version == SCHEMA_VERSION as i64
                    && row.filename == candidate.filename
                    && row.markdown_path == candidate.markdown_path
                {
                    summary.skipped += 1;
                    self.touch_progress(&summary);
                    continue;
                }
            }

            let text = String::from_utf8_lossy(&bytes);
            let parsed = parse_markdown(&text, &candidate.filename);
            let mut doc = TantivyDocument::new();
            doc.add_i64(fields.doc_id, candidate.id);
            doc.add_text(fields.filename, &candidate.filename);
            doc.add_text(fields.title, &parsed.title);
            doc.add_text(fields.headings, &parsed.headings);
            doc.add_text(fields.body, &parsed.body);
            doc.add_text(fields.markdown_path, &candidate.markdown_path);

            writer.delete_term(Term::from_field_i64(fields.doc_id, candidate.id));
            if let Err(e) = writer.add_document(doc) {
                summary.failed += 1;
                log_error!("[检索] 增量写入失败 {}：{}", candidate.id, e);
                self.touch_progress(&summary);
                continue;
            }
            if prev.is_some() {
                summary.updated += 1;
            } else {
                summary.added += 1;
            }
            changed.push((
                candidate.id,
                StateRow {
                    content_hash: hash,
                    schema_version: SCHEMA_VERSION as i64,
                    filename: candidate.filename.clone(),
                    markdown_path: candidate.markdown_path.clone(),
                },
            ));
            self.touch_progress(&summary);
        }

        // 数据库里已删除的文档：清掉索引条目与状态
        for id in previous.keys() {
            if !seen.contains(id) {
                writer.delete_term(Term::from_field_i64(fields.doc_id, *id));
                removed_ids.push(*id);
                summary.deleted += 1;
            }
        }

        writer.commit().context("提交增量索引失败")?;
        drop(writer);

        {
            let guard = self
                .inner
                .runtime
                .read()
                .map_err(|_| anyhow!("索引读锁已损坏"))?;
            if let Some(rt) = guard.as_ref() {
                rt.reader.reload().context("重新加载索引失败")?;
            }
        }

        self.apply_state_changes(&changed, &removed_ids)?;

        summary.doc_count = self.doc_count();
        Ok(summary)
    }

    fn touch_progress(&self, summary: &BuildSummary) {
        self.update_progress(|p| {
            p.processed = summary.processed;
            p.added = summary.added;
            p.updated = summary.updated;
            p.deleted = summary.deleted;
            p.skipped = summary.skipped;
            p.failed = summary.failed;
        });
    }

    fn index_one(
        &self,
        writer: &tantivy::IndexWriter,
        fields: Fields,
        candidate: &Candidate,
    ) -> Result<StateRow> {
        let bytes = fs::read(&candidate.abs_path)
            .with_context(|| format!("读取失败：{}", candidate.abs_path.display()))?;
        let hash = sha256_hex(&bytes);
        let text = String::from_utf8_lossy(&bytes);
        let parsed = parse_markdown(&text, &candidate.filename);

        let mut doc = TantivyDocument::new();
        doc.add_i64(fields.doc_id, candidate.id);
        doc.add_text(fields.filename, &candidate.filename);
        doc.add_text(fields.title, &parsed.title);
        doc.add_text(fields.headings, &parsed.headings);
        doc.add_text(fields.body, &parsed.body);
        doc.add_text(fields.markdown_path, &candidate.markdown_path);

        writer.delete_term(Term::from_field_i64(fields.doc_id, candidate.id));
        writer
            .add_document(doc)
            .with_context(|| format!("写入索引失败：{}", candidate.filename))?;

        Ok(StateRow {
            content_hash: hash,
            schema_version: SCHEMA_VERSION as i64,
            filename: candidate.filename.clone(),
            markdown_path: candidate.markdown_path.clone(),
        })
    }

    // -----------------------------------------------------------------------
    // 数据库交互
    // -----------------------------------------------------------------------

    fn load_candidates(&self) -> Result<Vec<Candidate>> {
        let conn = self.inner.db.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, filename, markdown_path FROM pdf_documents \
             WHERE markdown_path IS NOT NULL AND TRIM(markdown_path) <> '' ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, filename, markdown_path) = row?;
            let abs_path = crate::config::resolve_path(&markdown_path);
            out.push(Candidate {
                id,
                filename,
                markdown_path,
                abs_path,
            });
        }
        Ok(out)
    }

    fn load_state_table(&self) -> Result<HashMap<i64, StateRow>> {
        let conn = self.inner.db.conn()?;
        let mut stmt = conn.prepare(
            "SELECT doc_id, content_hash, schema_version, filename, markdown_path \
             FROM search_index_state",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                StateRow {
                    content_hash: r.get::<_, String>(1)?,
                    schema_version: r.get::<_, i64>(2)?,
                    filename: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    markdown_path: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                },
            ))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (id, state) = row?;
            out.insert(id, state);
        }
        Ok(out)
    }

    fn replace_state_table(&self, rows: &[(i64, StateRow)]) -> Result<()> {
        let mut conn = self.inner.db.conn()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM search_index_state", [])?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO search_index_state \
                 (doc_id, content_hash, file_size, file_mtime, filename, markdown_path, schema_version, indexed_at) \
                 VALUES (?1, ?2, 0, 0, ?3, ?4, ?5, ?6)",
            )?;
            let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
            for (id, row) in rows {
                stmt.execute(rusqlite::params![
                    id,
                    row.content_hash,
                    row.filename,
                    row.markdown_path,
                    row.schema_version,
                    now
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn apply_state_changes(
        &self,
        changed: &[(i64, StateRow)],
        removed: &[i64],
    ) -> Result<()> {
        let mut conn = self.inner.db.conn()?;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO search_index_state \
                 (doc_id, content_hash, file_size, file_mtime, filename, markdown_path, schema_version, indexed_at) \
                 VALUES (?1, ?2, 0, 0, ?3, ?4, ?5, ?6)",
            )?;
            let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
            for (id, row) in changed {
                stmt.execute(rusqlite::params![
                    id,
                    row.content_hash,
                    row.filename,
                    row.markdown_path,
                    row.schema_version,
                    now
                ])?;
            }
        }
        for id in removed {
            tx.execute("DELETE FROM search_index_state WHERE doc_id = ?1", [id])?;
        }
        tx.commit()?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 检索
    // -----------------------------------------------------------------------

    pub fn search(
        &self,
        q: &str,
        page: usize,
        page_size: usize,
        doc_id: Option<i64>,
        exact: bool,
        filter_singles: bool,
    ) -> std::result::Result<SearchResponse, SearchError> {
        let query_text = q.trim();
        if query_text.is_empty() {
            return Err(SearchError::BadRequest("检索关键词不能为空".into()));
        }
        let mut terms = dedupe_terms(tokenize_terms(query_text), MAX_QUERY_TERMS);
        // 查询期单字噪声过滤：存在多字词项时丢弃单字词项（如“的”“了”）
        if filter_singles && terms.iter().any(|t| t.chars().count() >= 2) {
            terms.retain(|t| t.chars().count() >= 2);
        }
        // 精确模式还要用原始串做字面校验，规范化后为空说明只有标记符号
        let needle = if exact { normalize_exact(query_text) } else { String::new() };
        if exact && needle.is_empty() {
            return Err(SearchError::BadRequest(
                "关键词中没有可检索的文字（请避免只输入标点符号）".into(),
            ));
        }
        if terms.is_empty() {
            return Err(SearchError::BadRequest(
                "关键词中没有可检索的文字（请避免只输入标点符号）".into(),
            ));
        }

        if let Err(e) = self.ensure_open() {
            return Err(SearchError::NotReady(e.to_string()));
        }
        if self.folder_needs_rebuild() {
            return Err(SearchError::NotReady(
                "索引目录已变更，需要重建索引".into(),
            ));
        }

        let snippet_md_folder = self.cfg_snapshot().output_md_folder;
        let began = Instant::now();
        let page = page.max(1);
        let page_size = page_size.clamp(1, 50);
        let offset = (page - 1) * page_size;

        let guard = match self.inner.runtime.read() {
            Ok(g) => g,
            Err(_) => return Err(SearchError::Internal("索引读锁已损坏".into())),
        };
        let rt = match guard.as_ref() {
            Some(rt) => rt,
            None => {
                return Err(SearchError::NotReady(
                    "索引尚未构建，请先执行全量重建".into(),
                ))
            }
        };
        let searcher = rt.reader.searcher();
        let schema = rt.index.schema();
        let fields = match fields_from_schema(&schema) {
            Ok(f) => f,
            Err(e) => return Err(SearchError::Internal(e.to_string())),
        };

        /// 精确模式的候选扫描上限：在此范围内校验并统计 total
        const EXACT_SCAN_CAP: usize = 500;

        if exact {
            let and_query = build_query(&terms, fields, doc_id, true);
            let mut candidates = match searcher.search(&and_query, &TopDocs::with_limit(EXACT_SCAN_CAP)) {
                Ok(v) => v,
                Err(e) => return Err(SearchError::Internal(format!("检索失败：{e}"))),
            };
            // 全词 AND 召回不足时用 OR 扩大候选，再由原文校验把关
            if candidates.is_empty() {
                let or_query = build_query(&terms, fields, doc_id, false);
                candidates = match searcher.search(&or_query, &TopDocs::with_limit(EXACT_SCAN_CAP)) {
                    Ok(v) => v,
                    Err(e) => return Err(SearchError::Internal(format!("检索失败：{e}"))),
                };
            }
            let truncated = candidates.len() >= EXACT_SCAN_CAP;

            let mut verified: Vec<(f32, TantivyDocument, String, String, String)> = Vec::new();
            for (score, addr) in candidates {
                let doc: TantivyDocument = match searcher.doc(addr) {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                let get_str = |field: Field| -> String {
                    doc.get_first(field)
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                };
                let filename = get_str(fields.filename);
                let markdown_path = get_str(fields.markdown_path);
                let mut title = get_str(fields.title);
                if title.trim().is_empty() {
                    title = filename.clone();
                }
                let source = read_exact_source(&markdown_path, &filename, &snippet_md_folder);
                if !normalize_exact(&source).contains(&needle) {
                    continue;
                }
                verified.push((score, doc, filename, title, markdown_path));
            }

            let total = verified.len();
            let results = verified
                .into_iter()
                .skip(offset)
                .take(page_size)
                .map(|(score, doc, filename, title, markdown_path)| SearchHit {
                    doc_id: doc
                        .get_first(fields.doc_id)
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0),
                    snippet: snippet_for_document(
                        &markdown_path,
                        &filename,
                        std::slice::from_ref(&query_text.to_string()),
                        &snippet_md_folder,
                    ),
                    filename,
                    title,
                    score,
                    markdown_path,
                })
                .collect();

            return Ok(SearchResponse {
                query: query_text.to_string(),
                terms,
                page,
                page_size,
                total,
                took_ms: began.elapsed().as_millis() as u64,
                results,
                mode: "exact".to_string(),
                relaxed: false,
                matched_terms: Vec::new(),
                truncated,
            });
        }

        let and_query = build_query(&terms, fields, doc_id, true);
        let or_query = build_query(&terms, fields, doc_id, false);

        let limit = offset.saturating_add(page_size).max(1);
        let (mut top, mut total) = match searcher.search(
            &and_query,
            &(TopDocs::with_limit(limit), Count),
        ) {
            Ok(v) => v,
            Err(e) => return Err(SearchError::Internal(format!("检索失败：{e}"))),
        };

        // 全词 AND 无结果时退化为 OR，兼顾长句与多个关键词
        let mut relaxed = false;
        if total == 0 && terms.len() > 1 {
            match searcher.search(&or_query, &(TopDocs::with_limit(limit), Count)) {
                Ok((t, c)) => {
                    top = t;
                    total = c;
                    relaxed = true;
                }
                Err(e) => return Err(SearchError::Internal(format!("检索失败：{e}"))),
            }
        }

        // 退化时报告实际命中的词，供前端说明“已放宽匹配”
        let matched_terms: Vec<String> = if relaxed {
            terms
                .iter()
                .filter(|term| {
                    let q = build_query(std::slice::from_ref(term), fields, doc_id, true);
                    searcher.search(&q, &Count).map(|c| c > 0).unwrap_or(false)
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        };

        // 正文不再存储：只为当前页的命中回读 Markdown 生成片段，total 不依赖文件
        let mut results = Vec::new();
        for (score, addr) in top.into_iter().skip(offset).take(page_size) {
            let doc: TantivyDocument = match searcher.doc(addr) {
                Ok(d) => d,
                Err(_) => continue,
            };
            let get_str = |field: Field| -> String {
                doc.get_first(field)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            let filename = get_str(fields.filename);
            let markdown_path = get_str(fields.markdown_path);
            let mut title = get_str(fields.title);
            if title.trim().is_empty() {
                title = filename.clone();
            }
            results.push(SearchHit {
                doc_id: doc
                    .get_first(fields.doc_id)
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0),
                snippet: snippet_for_document(
                    &markdown_path,
                    &filename,
                    &terms,
                    &snippet_md_folder,
                ),
                filename,
                title,
                score,
                markdown_path,
            });
        }

        Ok(SearchResponse {
            query: query_text.to_string(),
            terms,
            page,
            page_size,
            total,
            took_ms: began.elapsed().as_millis() as u64,
            results,
            mode: "smart".to_string(),
            relaxed,
            matched_terms,
            truncated: false,
        })
    }
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

#[derive(Default)]
struct BuildSummary {
    total: usize,
    processed: usize,
    added: usize,
    updated: usize,
    deleted: usize,
    skipped: usize,
    failed: usize,
    doc_count: usize,
}

#[derive(Serialize, Deserialize)]
struct IndexMeta {
    schema_version: u64,
    built_at: String,
    doc_count: usize,
}

fn sibling_dir(dir: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}.{}", dir.display(), suffix))
}

/// 路径比较：忽略大小写与末尾分隔符，避免 `E:\x` 与 `E:\x\` 被当成不同目录
fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        p.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    norm(a) == norm(b)
}

fn read_meta(dir: &Path) -> Result<IndexMeta> {
    let raw = fs::read(dir.join(META_FILE)).context("读取索引元数据失败")?;
    let meta: IndexMeta = serde_json::from_slice(&raw).context("解析索引元数据失败")?;
    Ok(meta)
}

fn write_meta(dir: &Path, meta: &IndexMeta) -> Result<()> {
    let raw = serde_json::to_vec_pretty(meta)?;
    fs::write(dir.join(META_FILE), raw).context("写入索引元数据失败")?;
    Ok(())
}

/// 打开索引并校验 schema 版本与必需字段
fn open_validated(dir: &Path) -> Result<RuntimeIndex> {
    if !dir.join(META_FILE).exists() {
        return Err(anyhow!("索引目录缺少版本信息（{}）", dir.display()));
    }
    let meta = read_meta(dir)?;
    if meta.schema_version != SCHEMA_VERSION {
        return Err(anyhow!(
            "索引版本不兼容（{} != {}）",
            meta.schema_version,
            SCHEMA_VERSION
        ));
    }
    let index = Index::open_in_dir(dir)
        .with_context(|| format!("打开索引目录失败：{}", dir.display()))?;
    index
        .tokenizers()
        .register(TOKENIZER_NAME, JiebaTokenizer);
    let schema = index.schema();
    fields_from_schema(&schema)?;
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()
        .context("创建索引读取器失败")?;
    // 触发一次 searcher，确认段文件可读（损坏时这里报错）
    let _ = reader.searcher().num_docs();
    Ok(RuntimeIndex { index, reader })
}

fn fields_from_schema(schema: &Schema) -> Result<Fields> {
    Ok(Fields {
        doc_id: schema.get_field("doc_id").context("索引缺少 doc_id 字段")?,
        filename: schema
            .get_field("filename")
            .context("索引缺少 filename 字段")?,
        title: schema.get_field("title").context("索引缺少 title 字段")?,
        headings: schema
            .get_field("headings")
            .context("索引缺少 headings 字段")?,
        body: schema.get_field("body").context("索引缺少 body 字段")?,
        markdown_path: schema
            .get_field("markdown_path")
            .context("索引缺少 markdown_path 字段")?,
    })
}

fn dir_size(dir: &Path) -> u64 {
    fn walk(path: &Path, total: &mut u64) {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                walk(&entry.path(), total);
            } else {
                *total += meta.len();
            }
        }
    }
    let mut total = 0;
    walk(dir, &mut total);
    total
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

fn dedupe_terms(terms: Vec<String>, max: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for term in terms {
        if term.is_empty() || !seen.insert(term.clone()) {
            continue;
        }
        out.push(term);
        if out.len() >= max {
            break;
        }
    }
    out
}

/// 每个关键词在 filename/title/headings/body 上以 SHOULD 组合并加权；
/// 外层按 AND（或 OR 兜底）合并，doc_id 过滤作为 MUST。
fn build_query(
    terms: &[String],
    fields: Fields,
    doc_id: Option<i64>,
    conjunction: bool,
) -> BooleanQuery {
    let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();

    for term in terms {
        let mut per_term: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for (field, boost) in [
            (fields.title, 3.0f32),
            (fields.headings, 2.0f32),
            (fields.filename, 2.0f32),
            (fields.body, 1.0f32),
        ] {
            let term_query = TermQuery::new(
                Term::from_field_text(field, term),
                IndexRecordOption::WithFreqs,
            );
            per_term.push((
                Occur::Should,
                Box::new(BoostQuery::new(Box::new(term_query), boost)),
            ));
        }
        clauses.push((
            if conjunction {
                Occur::Must
            } else {
                Occur::Should
            },
            Box::new(BooleanQuery::new(per_term)),
        ));
    }

    if let Some(id) = doc_id {
        clauses.push((
            Occur::Must,
            Box::new(TermQuery::new(
                Term::from_field_i64(fields.doc_id, id),
                IndexRecordOption::Basic,
            )),
        ));
    }

    BooleanQuery::new(clauses)
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 逐字符小写，保证下标与原文一一对应（中文不受影响，英文可大小写不敏感匹配）
fn lower_chars(chars: &[char]) -> Vec<char> {
    chars
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect()
}

fn is_ascii_word(needle: &[char]) -> bool {
    !needle.is_empty()
        && needle.iter().all(|c| c.is_ascii())
        && needle.iter().any(|c| c.is_ascii_alphanumeric())
}

fn is_word_boundary(hay: &[char], start: usize, len: usize) -> bool {
    let before_ok = start == 0 || !hay[start - 1].is_ascii_alphanumeric();
    let after_ok = start + len >= hay.len() || !hay[start + len].is_ascii_alphanumeric();
    before_ok && after_ok
}

/// 在字符数组里查找词的所有出现位置；英文词优先整词匹配，找不到再退化为子串匹配
fn find_occurrences(hay: &[char], needle: &[char], whole_word: bool) -> Vec<usize> {
    let mut out = Vec::new();
    if needle.is_empty() || hay.len() < needle.len() {
        return out;
    }
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        let matched = hay[i..i + needle.len()] == *needle;
        if matched && (!whole_word || is_word_boundary(hay, i, needle.len())) {
            out.push(i);
            if out.len() >= MAX_TERM_OCCURRENCES {
                break;
            }
            i += needle.len();
            continue;
        }
        i += 1;
    }
    out
}

/// 窗口文字把换行折成空格，便于单行展示
fn window_text(chars: &[char]) -> String {
    chars
        .iter()
        .map(|c| if *c == '\n' || *c == '\r' { ' ' } else { *c })
        .collect()
}

/// 在正文中挑选命中词覆盖最多的窗口，输出带 `<b>` 高亮的 HTML 片段。
/// 只对原文做 HTML 转义，`<b>` 标签由本函数拼接，输出可直接交给前端渲染。
fn build_snippet(body: &str, terms: &[String]) -> String {
    let text = body.trim();
    if text.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = text.chars().collect();
    let lower = lower_chars(&chars);
    let needles: Vec<Vec<char>> = terms
        .iter()
        .map(|t| lower_chars(&t.chars().collect::<Vec<_>>()))
        .filter(|n| !n.is_empty())
        .collect();

    // 每个查询词的所有命中位置（升序），用于窗口打分与高亮
    let mut per_term: Vec<Vec<usize>> = Vec::with_capacity(needles.len());
    for needle in &needles {
        let whole_word = is_ascii_word(needle);
        let mut occ = find_occurrences(&lower, needle, whole_word);
        if occ.is_empty() && whole_word {
            // 英文词嵌在连写里（如 OCR 文本 gdp2020）时退化为子串匹配
            occ = find_occurrences(&lower, needle, false);
        }
        per_term.push(occ);
    }

    let mut candidates: Vec<usize> = per_term
        .iter()
        .flat_map(|occ| occ.iter().map(|&s| s.saturating_sub(SNIPPET_LEAD_CHARS)))
        .collect();
    if candidates.is_empty() {
        // 词只命中标题/文件名：仍给一段正文开头作为上下文
        let plain = window_text(&chars[..chars.len().min(SNIPPET_FALLBACK_CHARS)]);
        let html = escape_html(plain.trim());
        return if html.is_empty() {
            html
        } else if chars.len() > SNIPPET_FALLBACK_CHARS {
            format!("{html}…")
        } else {
            html
        };
    }
    candidates.sort_unstable();
    candidates.dedup();

    // 评分：先看窗口覆盖的不同查询词数量，再看命中总次数，最后取更靠前的位置
    let mut best: Option<(usize, usize, usize)> = None;
    for &start in &candidates {
        let end = (start + SNIPPET_WINDOW_CHARS).min(chars.len());
        let mut distinct = 0;
        let mut total = 0;
        for occ in &per_term {
            let left = occ.partition_point(|&p| p < start);
            let right = occ.partition_point(|&p| p < end);
            if right > left {
                distinct += 1;
                total += right - left;
            }
        }
        let better = match best {
            None => true,
            Some((best_distinct, best_total, best_start)) => {
                distinct > best_distinct
                    || (distinct == best_distinct
                        && (total > best_total || (total == best_total && start < best_start)))
            }
        };
        if better {
            best = Some((distinct, total, start));
        }
    }
    let win_start = best.map(|(_, _, s)| s).unwrap_or(0);
    let win_end = (win_start + SNIPPET_WINDOW_CHARS).min(chars.len());

    // 收集窗口内完整命中的区间并合并重叠，避免同一处被重复包裹；
    // 跨出窗口边界的词不参与高亮，否则会出现 `<b>人</b>…` 这种半截词
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for (idx, occ) in per_term.iter().enumerate() {
        for &p in occ {
            let end = p + needles[idx].len();
            if p >= win_start && end <= win_end {
                ranges.push((p, end));
            }
        }
    }
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        if let Some(last) = merged.last_mut() {
            if start <= last.1 {
                last.1 = last.1.max(end);
                continue;
            }
        }
        merged.push((start, end));
    }

    let mut html = String::new();
    let mut cursor = win_start;
    for (start, end) in merged {
        if start > cursor {
            html.push_str(&escape_html(&window_text(&chars[cursor..start])));
        }
        html.push_str("<b>");
        html.push_str(&escape_html(&window_text(&chars[start..end])));
        html.push_str("</b>");
        cursor = end;
    }
    if cursor < win_end {
        html.push_str(&escape_html(&window_text(&chars[cursor..win_end])));
    }

    let mut out = String::new();
    if win_start > 0 {
        out.push('…');
    }
    out.push_str(html.trim());
    if win_end < chars.len() {
        out.push('…');
    }
    out
}

/// 读取 Markdown（有上限）、做与索引期一致的清洗，再生成高亮片段。
/// 文件被移动/删除时返回空片段，不影响检索结果本身。
/// 索引里存的旧路径失效时，用当前 Markdown 目录中的同名文件兜底（迁移目录后无需重建也能出片段）。
/// 精确模式规范化：小写、去掉全部空白与 Markdown 结构字符，保留文字与常规标点。
/// 这样跨 `**加粗**`、表格竖线、链接括号、换行都能匹配，但不会把不同字词拼成假命中。
fn normalize_exact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_whitespace() {
            continue;
        }
        if matches!(
            ch,
            '#' | '*' | '_' | '`' | '>' | '|' | '~' | '[' | ']' | '(' | ')' | '!'
        ) {
            continue;
        }
        for lower in ch.to_lowercase() {
            out.push(lower);
        }
    }
    out
}

/// 读取用于精确校验的正文（与片段读取同源）：Markdown 缺失时退回文件名
fn read_exact_source(markdown_path: &str, filename: &str, md_folder: &Path) -> String {
    let mut path = crate::config::resolve_path(markdown_path);
    if !path.is_file() {
        if let Some(name) = Path::new(markdown_path).file_name() {
            let alt = md_folder.join(name);
            if alt.is_file() {
                path = alt;
            }
        }
    }
    let file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return filename.to_string(),
    };
    let mut buf = Vec::new();
    if file
        .take(MAX_SNIPPET_FILE_BYTES)
        .read_to_end(&mut buf)
        .is_err()
    {
        return filename.to_string();
    }
    let raw = String::from_utf8_lossy(&buf);
    if raw.trim().is_empty() {
        return filename.to_string();
    }
    raw.into_owned()
}

fn snippet_for_document(
    markdown_path: &str,
    filename: &str,
    terms: &[String],
    md_folder: &Path,
) -> String {
    let mut path = crate::config::resolve_path(markdown_path);
    if !path.is_file() {
        if let Some(name) = Path::new(markdown_path).file_name() {
            let alt = md_folder.join(name);
            if alt.is_file() {
                path = alt;
            }
        }
    }
    let file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let mut buf = Vec::new();
    if file
        .take(MAX_SNIPPET_FILE_BYTES)
        .read_to_end(&mut buf)
        .is_err()
    {
        return String::new();
    }
    let raw = String::from_utf8_lossy(&buf);
    let capped: String = if raw.chars().count() > MAX_SNIPPET_TEXT_CHARS {
        raw.chars().take(MAX_SNIPPET_TEXT_CHARS).collect()
    } else {
        raw.into_owned()
    };
    let parsed = parse_markdown(&capped, filename);
    let source = if parsed.body.trim().is_empty() {
        parsed.headings
    } else {
        parsed.body
    };
    build_snippet(&source, terms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_is_split_into_fields() {
        let md = "# 中国经济年鉴\n\n## 人口\n\n1984 年**总人口**约为 10 亿。\n\n| 年份 | 人口 |\n| --- | --- |\n| 1984 | 10 亿 |\n\n![图](a.png)\n";
        let parsed = parse_markdown(md, "yearbook.md");
        assert_eq!(parsed.title, "中国经济年鉴");
        assert!(parsed.headings.contains("人口"));
        assert!(parsed.body.contains("总人口"));
        assert!(parsed.body.contains("1984"));
        assert!(!parsed.body.contains('|'));
        assert!(!parsed.body.contains("##"));
    }

    #[test]
    fn title_falls_back_to_filename() {
        let parsed = parse_markdown("正文没有标题", "中国经济年鉴 1984 1.pdf");
        assert_eq!(parsed.title, "中国经济年鉴 1984 1");
    }

    #[test]
    fn chinese_tokenizer_keeps_words() {
        let terms = tokenize_terms("中国人口统计");
        assert!(terms.iter().any(|t| t == "人口"));
        assert!(terms.iter().any(|t| t == "中国"));
    }

    #[test]
    fn body_is_indexed_but_not_stored() {
        let (schema, fields) = build_schema();
        let body = schema.get_field_entry(fields.body);
        assert!(body.is_indexed(), "body 必须建倒排");
        assert!(!body.is_stored(), "body 不应存储，避免索引里再存一份正文");
        let doc_id = schema.get_field_entry(fields.doc_id);
        assert!(doc_id.is_fast(), "doc_id 需要 FAST 用于过滤与删除");
        assert!(doc_id.is_stored());
        assert!(schema.get_field_entry(fields.title).is_stored());
        assert!(schema.get_field_entry(fields.markdown_path).is_stored());
    }

    #[test]
    fn snippet_picks_window_covering_most_terms() {
        let mut body = String::new();
        for i in 0..40 {
            body.push_str(&format!("第{i}段内容是无关的统计表格说明。"));
        }
        body.push_str("本段讨论城市规划与人口普查的关系。");
        let terms = vec!["城市规划".to_string(), "人口普查".to_string()];
        let html = build_snippet(&body, &terms);
        assert!(html.contains("<b>城市规划</b>"), "缺少整词高亮：{html}");
        assert!(html.contains("<b>人口普查</b>"), "缺少整词高亮：{html}");
        assert!(html.starts_with('…'), "窗口应位于正文中部：{html}");
    }

    #[test]
    fn snippet_matches_english_case_insensitively() {
        let body = "The GDP figure is reported annually in the yearbook.";
        let html = build_snippet(body, &["gdp".to_string()]);
        assert!(html.contains("<b>GDP</b>"), "英文应大小写不敏感：{html}");
        // 已有整词命中时不把连写（GDPX）也算进去
        let body2 = "GDP growth is reported, GDPX is a typo.";
        let html2 = build_snippet(body2, &["gdp".to_string()]);
        assert!(html2.contains("<b>GDP</b>"), "应高亮整词：{html2}");
        assert!(!html2.contains("<b>GDPX</b>"), "不应命中 GDPX：{html2}");
    }

    #[test]
    fn snippet_matches_chinese_single_char_and_escapes_html() {
        let body = "文件中出现 <script> 标签，同时提到人字。";
        let html = build_snippet(body, &["人".to_string()]);
        assert!(html.contains("<b>人</b>"), "中文单字应命中：{html}");
        assert!(html.contains("&lt;script&gt;"), "原文必须转义：{html}");
        assert!(!html.contains("<script>"), "不应输出原始标签：{html}");
    }

    #[test]
    fn snippet_falls_back_when_term_only_in_title() {
        let body = "这里没有任何查询词，只有一段普通正文。";
        let html = build_snippet(body, &["城市规划".to_string()]);
        assert!(!html.contains("<b>"), "无正文命中时不应伪造高亮：{html}");
        assert!(html.contains("普通正文"), "应给出正文开头上下文：{html}");
    }

    /// 真实数据库验证：需要手动指定路径后运行
    /// `cargo test search::tests::real_data_probe -- --ignored --nocapture`
    #[test]
    #[ignore = "依赖真实数据库与 Markdown 产物，按需手动运行"]
    fn real_data_probe() {
        // 模拟 exe 从项目根目录启动：数据库里的相对路径按 backend/ 解析
        if let Ok(root) = std::env::var("PROBE_ROOT") {
            std::env::set_current_dir(root).expect("切换工作目录失败");
        }
        let db_path = std::env::var("PROBE_DB").expect("PROBE_DB 未设置");
        let output = std::env::var("PROBE_OUTPUT").expect("PROBE_OUTPUT 未设置");
        let index_dir = std::env::var("PROBE_INDEX").expect("PROBE_INDEX 未设置");
        std::env::set_var("DATABASE_PATH", &db_path);
        std::env::set_var("OUTPUT_FOLDER", &output);
        std::env::set_var("UPLOAD_FOLDER", format!("{output}/uploads"));
        std::env::set_var("TEMP_FOLDER", format!("{output}/temp"));
        std::env::set_var("SEARCH_INDEX_FOLDER", &index_dir);

        let cfg = Config::load();
        let db = Db::new(&cfg);
        db.init().expect("初始化数据库失败");
        let mgr = SearchManager::new(Arc::new(RwLock::new(cfg)), db);

        fn wait(mgr: &SearchManager) -> IndexStatus {
            loop {
                let status = mgr.status();
                if !status.running {
                    return status;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }

        let began = Instant::now();
        match mgr.start_build(BuildMode::Full) {
            Ok(_) => {}
            Err(_) => panic!("启动全量构建失败"),
        }
        let status = wait(&mgr);
        println!(
            "全量构建：{:.2}s，文档 {}，大小 {} 字节，失败 {}",
            began.elapsed().as_secs_f64(),
            status.doc_count,
            status.index_size,
            status.failed
        );
        assert!(status.doc_count > 0, "索引文档数为 0");

        let began = Instant::now();
        mgr.start_build(BuildMode::Incremental).expect("启动增量失败");
        let status = wait(&mgr);
        println!(
            "增量更新：{:.2}s，跳过 {}，新增 {}，更新 {}，删除 {}",
            began.elapsed().as_secs_f64(),
            status.skipped,
            status.added,
            status.updated,
            status.deleted
        );
        assert_eq!(status.added, 0, "增量更新不应新增文档");
        assert_eq!(status.updated, 0, "增量更新不应重复写入文档");
        assert_eq!(status.failed, 0, "增量更新不应有失败文档");

        let began = Instant::now();
        let result = mgr.search("人口", 1, 10, None).expect("检索失败");
        println!(
            "检索「人口」：{} 条，用时 {}ms（接口 {}ms）",
            result.total,
            began.elapsed().as_millis(),
            result.took_ms
        );
        for hit in result.results.iter().take(5) {
            println!(
                "  - doc {} {} score={:.2} snippet={}",
                hit.doc_id, hit.filename, hit.score, hit.snippet
            );
        }
        assert!(result.total > 0, "检索「人口」没有结果");
        assert!(
            result.results.iter().any(|h| h.snippet.contains("<b>")),
            "片段中没有高亮"
        );

        // 同一批查询用于改造前后对比（两字词、多词、英文）
        for query in [
            "人口",
            "人口 统计",
            "城市规划",
            "人口普查",
            "改革开放 经济 发展",
            "GDP",
            "population",
        ] {
            let began = Instant::now();
            match mgr.search(query, 1, 10, None) {
                Ok(res) => {
                    println!(
                        "查询「{}」：{} 条，总耗时 {}ms（接口 {}ms）",
                        query,
                        res.total,
                        began.elapsed().as_millis(),
                        res.took_ms
                    );
                    for hit in res.results.iter().take(2) {
                        let snippet = hit
                            .snippet
                            .chars()
                            .take(120)
                            .collect::<String>()
                            .replace('\n', " ");
                        println!("    doc {} score={:.2}：{}", hit.doc_id, hit.score, snippet);
                    }
                }
                Err(e) => println!("查询「{}」失败：{e}", query),
            }
        }
    }
}
