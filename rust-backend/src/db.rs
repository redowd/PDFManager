use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use chrono::{Local, NaiveDateTime};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};

use crate::config::Config;

#[derive(Debug, Clone)]
pub struct Document {
    pub id: i64,
    pub filename: String,
    pub original_path: String,
    pub file_size: Option<i64>,
    pub page_count: Option<i64>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub markdown_path: Option<String>,
    pub json_paths: Option<String>,
    pub error_message: Option<String>,
    pub progress: i64,
    pub md5: Option<String>,
    pub ocr_disabled: bool,
    pub clean_filename: Option<String>,
    pub ocr_phase: Option<String>,
    pub ocr_batch_id: Option<String>,
    pub click_count: i64,
    pub rating: i64,
}

#[derive(Debug, Clone)]
pub struct Chunk {
    pub id: i64,
    pub document_id: i64,
    pub filename: String,
    pub chunk_path: String,
    pub start_page: Option<i64>,
    pub end_page: Option<i64>,
    pub ocr_status: String,
    pub json_path: Option<String>,
    pub batch_id: Option<String>,
    pub data_id: Option<String>,
    pub remote_filename: Option<String>,
    pub remote_state: Option<String>,
    pub error_message: Option<String>,
    pub fetch_attempts: i64,
    pub retry_exhausted: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TagRow {
    pub id: i64,
    pub name: String,
    pub color: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TagDto {
    pub id: i64,
    pub name: String,
    pub color: String,
}

/// 上传计划条目（与 Python 版 upload_plan_json 的字段保持一致，便于双向兼容）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UploadPlanItem {
    pub chunk_id: i64,
    pub doc_id: i64,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub start_page: Option<i64>,
    #[serde(default)]
    pub end_page: Option<i64>,
    #[serde(default)]
    pub data_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Batch {
    pub id: i64,
    pub batch_id: String,
    pub status: String,
    pub doc_ids: Vec<i64>,
    pub file_count: i64,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub error_message: Option<String>,
    pub upload_plan_json: Option<String>,
    pub upload_urls_json: Option<String>,
    pub uploaded_count: i64,
    pub upload_abandoned: bool,
    pub urls_issued_at: Option<String>,
    pub submitted_at: Option<String>,
    pub finished_at: Option<String>,
    pub poll_count: i64,
    pub recovery_attempts: i64,
    pub poll_failures: i64,
    pub poll_error: Option<String>,
}

impl Batch {
    pub fn upload_plan(&self) -> Vec<UploadPlanItem> {
        self.upload_plan_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Vec<UploadPlanItem>>(raw).ok())
            .unwrap_or_default()
    }

    pub fn upload_urls(&self) -> Vec<String> {
        self.upload_urls_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
pub struct PendingDoc {
    pub id: i64,
    pub filename: String,
    pub original_path: String,
    pub page_count: Option<i64>,
    pub file_size: Option<i64>,
}

pub fn now_string() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S%.6f").to_string()
}

pub fn today_string() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

/// 按字符数截断（避免按字节切分多字节字符导致 panic）
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

/// 把数据库中的 DATETIME 文本转成 Python `datetime.isoformat()` 风格的字符串
pub fn iso_from_db(raw: &str) -> String {
    for fmt in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(raw, fmt) {
            if dt.and_utc().timestamp_subsec_micros() == 0 {
                return dt.format("%Y-%m-%dT%H:%M:%S").to_string();
            }
            return dt.format("%Y-%m-%dT%H:%M:%S%.6f").to_string();
        }
    }
    raw.to_string()
}

fn doc_from_row(row: &Row<'_>) -> rusqlite::Result<Document> {
    let ocr_disabled: Option<i64> = row.get("ocr_disabled").unwrap_or(None);
    Ok(Document {
        id: row.get("id")?,
        filename: row.get("filename")?,
        original_path: row.get("original_path")?,
        file_size: row.get("file_size").unwrap_or(None),
        page_count: row.get("page_count").unwrap_or(None),
        status: row.get::<_, Option<String>>("status")?.unwrap_or_else(|| "uploaded".into()),
        created_at: row.get::<_, Option<String>>("created_at")?.unwrap_or_default(),
        updated_at: row.get::<_, Option<String>>("updated_at")?.unwrap_or_default(),
        markdown_path: row.get("markdown_path").unwrap_or(None),
        json_paths: row.get("json_paths").unwrap_or(None),
        error_message: row.get("error_message").unwrap_or(None),
        progress: row.get::<_, Option<i64>>("progress").unwrap_or(Some(0)).unwrap_or(0),
        md5: row.get("md5").unwrap_or(None),
        ocr_disabled: ocr_disabled.unwrap_or(0) != 0,
        clean_filename: row.get("clean_filename").unwrap_or(None),
        ocr_phase: row.get("ocr_phase").unwrap_or(None),
        ocr_batch_id: row.get("ocr_batch_id").unwrap_or(None),
        click_count: row.get::<_, Option<i64>>("click_count").unwrap_or(Some(0)).unwrap_or(0),
        rating: row.get::<_, Option<i64>>("rating").unwrap_or(Some(0)).unwrap_or(0),
    })
}

pub fn parse_json_paths(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw else { return Vec::new() };
    if raw.trim().is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

#[derive(Clone)]
pub struct Db {
    pub path: PathBuf,
}

impl Db {
    pub fn new(cfg: &Config) -> Self {
        Self { path: cfg.database_path.clone() }
    }

    pub fn conn(&self) -> Result<Connection> {
        let conn = Connection::open(&self.path)?;
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        Ok(conn)
    }

    pub fn init(&self) -> Result<()> {
        let conn = self.conn()?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS pdf_documents (
                id INTEGER NOT NULL,
                filename VARCHAR(255) NOT NULL,
                original_path VARCHAR(500) NOT NULL,
                file_size INTEGER,
                page_count INTEGER,
                status VARCHAR(50),
                created_at DATETIME,
                updated_at DATETIME,
                markdown_path VARCHAR(500),
                json_paths TEXT,
                error_message VARCHAR(500),
                progress INTEGER DEFAULT 0,
                md5 VARCHAR(32),
                ocr_disabled BOOLEAN DEFAULT 0,
                clean_filename VARCHAR(500),
                ocr_phase VARCHAR(20),
                ocr_batch_id VARCHAR(100),
                click_count INTEGER NOT NULL DEFAULT 0,
                rating INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (id)
            );
            CREATE TABLE IF NOT EXISTS pdf_chunks (
                id INTEGER NOT NULL,
                document_id INTEGER,
                filename VARCHAR(255) NOT NULL,
                chunk_path VARCHAR(500) NOT NULL,
                start_page INTEGER,
                end_page INTEGER,
                ocr_status VARCHAR(50),
                json_path VARCHAR(500),
                batch_id VARCHAR(100),
                data_id VARCHAR(64),
                remote_filename VARCHAR(255),
                remote_state VARCHAR(20),
                error_message VARCHAR(500),
                fetch_attempts INTEGER DEFAULT 0,
                retry_exhausted BOOLEAN DEFAULT 0,
                PRIMARY KEY (id),
                FOREIGN KEY(document_id) REFERENCES pdf_documents (id)
            );
            CREATE TABLE IF NOT EXISTS tags (
                id INTEGER NOT NULL,
                name VARCHAR(100) NOT NULL,
                color VARCHAR(7),
                PRIMARY KEY (id),
                UNIQUE (name)
            );
            CREATE TABLE IF NOT EXISTS document_tags (
                document_id INTEGER NOT NULL,
                tag_id INTEGER NOT NULL,
                PRIMARY KEY (document_id, tag_id),
                FOREIGN KEY(document_id) REFERENCES pdf_documents (id),
                FOREIGN KEY(tag_id) REFERENCES tags (id)
            );
            CREATE TABLE IF NOT EXISTS ocr_batches (
                id INTEGER NOT NULL,
                batch_id VARCHAR(100) NOT NULL,
                status VARCHAR(50),
                doc_ids_json TEXT NOT NULL,
                file_count INTEGER,
                created_at DATETIME,
                updated_at DATETIME,
                error_message VARCHAR(500),
                upload_plan_json TEXT,
                upload_urls_json TEXT,
                uploaded_count INTEGER DEFAULT 0,
                upload_abandoned BOOLEAN DEFAULT 0,
                urls_issued_at DATETIME,
                submitted_at DATETIME,
                finished_at DATETIME,
                poll_count INTEGER DEFAULT 0,
                recovery_attempts INTEGER DEFAULT 0,
                poll_failures INTEGER DEFAULT 0,
                poll_error VARCHAR(500),
                PRIMARY KEY (id)
            );
            CREATE INDEX IF NOT EXISTS ix_ocr_batches_status ON ocr_batches (status);
            CREATE UNIQUE INDEX IF NOT EXISTS ix_ocr_batches_batch_id ON ocr_batches (batch_id);
            CREATE TABLE IF NOT EXISTS app_meta (key TEXT PRIMARY KEY, value TEXT);
            -- 全文检索增量判定：内容哈希是权威依据，mtime/size 仅用于诊断
            CREATE TABLE IF NOT EXISTS search_index_state (
                doc_id INTEGER PRIMARY KEY,
                content_hash TEXT NOT NULL,
                file_size INTEGER NOT NULL,
                file_mtime INTEGER NOT NULL,
                filename TEXT,
                markdown_path TEXT,
                schema_version INTEGER NOT NULL,
                indexed_at TEXT NOT NULL
            );
            "#,
        )?;
        self.migrate_columns(&conn)?;
        Ok(())
    }

    fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
        let cols: Vec<String> = conn
            .prepare(&format!("PRAGMA table_info({})", table))?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(cols)
    }

    fn add_column_if_missing(
        conn: &Connection,
        table: &str,
        column: &str,
        definition: &str,
    ) -> Result<()> {
        let cols = Self::table_columns(conn, table)?;
        if !cols.iter().any(|c| c == column) {
            conn.execute(&format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, definition), [])?;
        }
        Ok(())
    }

    fn migrate_columns(&self, conn: &Connection) -> Result<()> {
        // pdf_documents
        for (name, definition) in [
            ("error_message", "VARCHAR(500)"),
            ("progress", "INTEGER DEFAULT 0"),
            ("json_paths", "TEXT"),
            ("md5", "VARCHAR(32)"),
            ("ocr_disabled", "BOOLEAN DEFAULT 0"),
            ("clean_filename", "VARCHAR(500)"),
            ("ocr_phase", "VARCHAR(20)"),
            ("ocr_batch_id", "VARCHAR(100)"),
            ("click_count", "INTEGER NOT NULL DEFAULT 0"),
            ("rating", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            Self::add_column_if_missing(conn, "pdf_documents", name, definition)?;
        }
        conn.execute("CREATE INDEX IF NOT EXISTS ix_pdf_documents_md5 ON pdf_documents (md5)", [])?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS ix_pdf_documents_ocr_disabled ON pdf_documents (ocr_disabled)",
            [],
        )?;

        // pdf_chunks：断点续跑检查点列
        for (name, definition) in [
            ("batch_id", "VARCHAR(100)"),
            ("data_id", "VARCHAR(64)"),
            ("remote_filename", "VARCHAR(255)"),
            ("remote_state", "VARCHAR(20)"),
            ("error_message", "VARCHAR(500)"),
            ("fetch_attempts", "INTEGER DEFAULT 0"),
            ("retry_exhausted", "BOOLEAN DEFAULT 0"),
        ] {
            Self::add_column_if_missing(conn, "pdf_chunks", name, definition)?;
        }
        conn.execute(
            "CREATE INDEX IF NOT EXISTS ix_pdf_chunks_batch_id ON pdf_chunks (batch_id)",
            [],
        )?;

        // ocr_batches：上传计划 / 预签名 URL / 恢复计数
        for (name, definition) in [
            ("upload_plan_json", "TEXT"),
            ("upload_urls_json", "TEXT"),
            ("uploaded_count", "INTEGER DEFAULT 0"),
            ("upload_abandoned", "BOOLEAN DEFAULT 0"),
            ("urls_issued_at", "DATETIME"),
            ("submitted_at", "DATETIME"),
            ("finished_at", "DATETIME"),
            ("poll_count", "INTEGER DEFAULT 0"),
            ("recovery_attempts", "INTEGER DEFAULT 0"),
            ("poll_failures", "INTEGER DEFAULT 0"),
            ("poll_error", "VARCHAR(500)"),
        ] {
            Self::add_column_if_missing(conn, "ocr_batches", name, definition)?;
        }
        Ok(())
    }

    // ---------------- 文档 ----------------

    pub fn add_document(
        &self,
        filename: &str,
        original_path: &str,
        file_size: i64,
        page_count: i64,
        md5: Option<&str>,
        ocr_disabled: bool,
    ) -> Result<i64> {
        let conn = self.conn()?;
        let now = now_string();
        conn.execute(
            "INSERT INTO pdf_documents
             (filename, original_path, file_size, page_count, status, created_at, updated_at,
              md5, ocr_disabled, progress)
             VALUES (?1, ?2, ?3, ?4, 'uploaded', ?5, ?5, ?6, ?7, 0)",
            params![filename, original_path, file_size, page_count, now, md5, ocr_disabled as i64],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_document(&self, doc_id: i64) -> Result<Option<Document>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row("SELECT * FROM pdf_documents WHERE id=?1", params![doc_id], doc_from_row)
            .optional()?)
    }

    /// 预览打开计数：只加 click_count，不改 updated_at（避免打开预览就改变默认排序）
    pub fn bump_click_count(&self, doc_id: i64) -> Result<Option<i64>> {
        let conn = self.conn()?;
        let affected = conn.execute(
            "UPDATE pdf_documents SET click_count = COALESCE(click_count, 0) + 1 WHERE id=?1",
            params![doc_id],
        )?;
        if affected == 0 {
            return Ok(None);
        }
        Ok(Some(conn.query_row(
            "SELECT COALESCE(click_count, 0) FROM pdf_documents WHERE id=?1",
            params![doc_id],
            |r| r.get(0),
        )?))
    }

    /// 评分保存：0 表示未评分，1~10 为有效分值；同样不改 updated_at
    pub fn set_rating(&self, doc_id: i64, rating: i64) -> Result<bool> {
        let conn = self.conn()?;
        let affected = conn.execute(
            "UPDATE pdf_documents SET rating=?1 WHERE id=?2",
            params![rating, doc_id],
        )?;
        Ok(affected > 0)
    }

    pub fn rename_document(
        &self,
        doc_id: i64,
        filename: &str,
        original_path: &str,
        markdown_path: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn()?;
        let now = now_string();
        if let Some(md) = markdown_path {
            conn.execute(
                "UPDATE pdf_documents SET filename=?1, original_path=?2, markdown_path=?3, updated_at=?4 WHERE id=?5",
                params![filename, original_path, md, now, doc_id],
            )?;
        } else {
            conn.execute(
                "UPDATE pdf_documents SET filename=?1, original_path=?2, updated_at=?3 WHERE id=?4",
                params![filename, original_path, now, doc_id],
            )?;
        }
        Ok(())
    }

    /// 批量重命名的数据库更新：单文档一个事务，同时把检索索引状态标记为待增量更新。
    /// 将 search_index_state.content_hash 置空作为“需重建该文档索引”的哨兵，
    /// 增量构建时会因哈希不匹配重新写入新路径，避免索引里的 markdown_path 指向旧文件。
    pub fn rename_document_with_index(
        &self,
        doc_id: i64,
        filename: &str,
        original_path: &str,
        markdown_path: Option<&str>,
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let now = now_string();
        if let Some(md) = markdown_path {
            tx.execute(
                "UPDATE pdf_documents SET filename=?1, original_path=?2, markdown_path=?3, updated_at=?4 WHERE id=?5",
                params![filename, original_path, md, now, doc_id],
            )?;
            tx.execute(
                "UPDATE search_index_state SET filename=?1, markdown_path=?2, content_hash='' WHERE doc_id=?3",
                params![filename, md, doc_id],
            )?;
        } else {
            tx.execute(
                "UPDATE pdf_documents SET filename=?1, original_path=?2, updated_at=?3 WHERE id=?4",
                params![filename, original_path, now, doc_id],
            )?;
            tx.execute(
                "UPDATE search_index_state SET filename=?1, content_hash='' WHERE doc_id=?2",
                params![filename, doc_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 库内同名文档（大小写不敏感），用于批量重命名的冲突检测
    pub fn document_ids_by_filename(&self, filename: &str) -> Result<Vec<i64>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT id FROM pdf_documents WHERE LOWER(filename)=LOWER(?1)")?;
        let ids = stmt
            .query_map(params![filename], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    /// 索引状态中被标记为待增量更新的文档数（content_hash 为空）
    pub fn pending_index_count(&self) -> Result<i64> {
        let conn = self.conn()?;
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM search_index_state WHERE content_hash=''",
            [],
            |r| r.get::<_, i64>(0),
        )?)
    }

    pub fn set_ocr_disabled(&self, doc_id: i64, disabled: bool) -> Result<bool> {
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE pdf_documents SET ocr_disabled=?1, updated_at=?2 WHERE id=?3",
            params![disabled as i64, now_string(), doc_id],
        )?;
        Ok(n > 0)
    }

    pub fn update_document_progress(
        &self,
        doc_id: i64,
        status: &str,
        progress: Option<i64>,
        error_message: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn()?;
        let now = now_string();
        if let Some(p) = progress {
            conn.execute(
                "UPDATE pdf_documents SET status=?1, progress=?2, updated_at=?3 WHERE id=?4",
                params![status, p, now, doc_id],
            )?;
        } else {
            conn.execute(
                "UPDATE pdf_documents SET status=?1, updated_at=?2 WHERE id=?3",
                params![status, now, doc_id],
            )?;
        }
        if let Some(msg) = error_message {
            conn.execute(
                "UPDATE pdf_documents SET error_message=?1 WHERE id=?2",
                params![msg, doc_id],
            )?;
        }
        Ok(())
    }

    pub fn update_document_markdown_path(
        &self,
        doc_id: i64,
        markdown_path: &str,
        json_paths: &[String],
    ) -> Result<()> {
        let conn = self.conn()?;
        let json_raw = if json_paths.is_empty() {
            None
        } else {
            Some(serde_json::to_string(json_paths)?)
        };
        conn.execute(
            "UPDATE pdf_documents SET markdown_path=?1, json_paths=?2, updated_at=?3 WHERE id=?4",
            params![markdown_path, json_raw, now_string(), doc_id],
        )?;
        Ok(())
    }

    /// 仅更新 json_paths（失败落盘时保留原 markdown_path，可能是 NULL）
    pub fn update_document_json_paths(&self, doc_id: i64, json_paths: &[String]) -> Result<()> {
        let conn = self.conn()?;
        let json_raw = if json_paths.is_empty() {
            None
        } else {
            Some(serde_json::to_string(json_paths)?)
        };
        conn.execute(
            "UPDATE pdf_documents SET json_paths=?1, updated_at=?2 WHERE id=?3",
            params![json_raw, now_string(), doc_id],
        )?;
        Ok(())
    }

    pub fn delete_document(&self, doc_id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM document_tags WHERE document_id=?1", params![doc_id])?;
        conn.execute("DELETE FROM pdf_chunks WHERE document_id=?1", params![doc_id])?;
        conn.execute("DELETE FROM pdf_documents WHERE id=?1", params![doc_id])?;
        Ok(())
    }

    pub fn get_documents_paginated(
        &self,
        page: i64,
        page_size: i64,
        keyword: Option<&str>,
        path_keyword: Option<&str>,
        statuses: Option<&[String]>,
        tag_ids: Option<&[i64]>,
        ocr_disabled: Option<bool>,
        sort_by: &str,
        sort_order: &str,
    ) -> Result<(Vec<Document>, i64)> {
        let conn = self.conn()?;
        let mut where_sql = String::from(" WHERE 1=1");
        let mut values: Vec<Value> = Vec::new();

        if let Some(kw) = keyword {
            where_sql.push_str(
                " AND (d.filename LIKE ? OR d.id IN (SELECT dt.document_id FROM document_tags dt \
                 JOIN tags t ON t.id = dt.tag_id WHERE t.name LIKE ?))",
            );
            let pat = format!("%{}%", kw);
            values.push(Value::Text(pat.clone()));
            values.push(Value::Text(pat));
        }
        // 原始路径独立条件：与 keyword 取交集，单独填写时只按路径过滤
        if let Some(pk) = path_keyword {
            where_sql.push_str(" AND d.original_path LIKE ?");
            values.push(Value::Text(format!("%{}%", pk)));
        }
        if let Some(list) = statuses {
            if !list.is_empty() {
                let marks = vec!["?"; list.len()].join(",");
                where_sql.push_str(&format!(" AND d.status IN ({})", marks));
                for s in list {
                    values.push(Value::Text(s.clone()));
                }
            }
        }
        if let Some(ids) = tag_ids {
            if !ids.is_empty() {
                let marks = vec!["?"; ids.len()].join(",");
                where_sql.push_str(&format!(
                    " AND d.id IN (SELECT document_id FROM document_tags WHERE tag_id IN ({}))",
                    marks
                ));
                for id in ids {
                    values.push(Value::Integer(*id));
                }
            }
        }
        if let Some(flag) = ocr_disabled {
            where_sql.push_str(" AND d.ocr_disabled = ?");
            values.push(Value::Integer(if flag { 1 } else { 0 }));
        }

        let total: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM pdf_documents d{}", where_sql),
            params_from_iter(values.iter()),
            |r| r.get(0),
        )?;

        let order_col = match sort_by {
            "id" => "d.id",
            "filename" => "d.filename",
            "file_size" => "d.file_size",
            "page_count" => "d.page_count",
            "status" => "d.status",
            "updated_at" => "d.updated_at",
            "click_count" => "d.click_count",
            "rating" => "d.rating",
            "ocr_disabled" => "d.ocr_disabled",
            _ => "d.updated_at",
        };
        let dir = if sort_order.eq_ignore_ascii_case("asc") { "ASC" } else { "DESC" };
        let sql = format!(
            "SELECT * FROM pdf_documents d{} ORDER BY {} {} LIMIT ? OFFSET ?",
            where_sql, order_col, dir
        );
        let mut vals = values;
        vals.push(Value::Integer(page_size));
        vals.push(Value::Integer((page - 1) * page_size));
        let mut stmt = conn.prepare(&sql)?;
        let docs = stmt
            .query_map(params_from_iter(vals.iter()), doc_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((docs, total))
    }

    pub fn count_processing(&self) -> Result<i64> {
        let conn = self.conn()?;
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM pdf_documents WHERE status IN ('splitting','ocr_processing')",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn count_completed_ocr(&self) -> Result<i64> {
        let conn = self.conn()?;
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM pdf_documents WHERE status='completed'",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn count_pending_ocr(&self) -> Result<i64> {
        let conn = self.conn()?;
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM pdf_documents WHERE ocr_disabled=0 AND status='uploaded'",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn get_next_pending_ocr_docs(
        &self,
        limit: usize,
        sort_mode: &str,
        include_failed: bool,
    ) -> Result<Vec<PendingDoc>> {
        let conn = self.conn()?;
        let order = if sort_mode == "shortest_first" {
            "ORDER BY page_count IS NULL, page_count ASC, id ASC"
        } else {
            "ORDER BY id ASC"
        };
        let statuses = if include_failed { "'uploaded','failed'" } else { "'uploaded'" };
        let sql = format!(
            "SELECT id, filename, original_path, page_count, file_size FROM pdf_documents \
             WHERE ocr_disabled=0 AND status IN ({}) {} LIMIT ?1",
            statuses, order
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![limit as i64], |r| {
                Ok(PendingDoc {
                    id: r.get(0)?,
                    filename: r.get(1)?,
                    original_path: r.get(2)?,
                    page_count: r.get(3).unwrap_or(None),
                    file_size: r.get(4).unwrap_or(None),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// 原子状态抢占：单条条件 UPDATE，rowcount 决定是否成功
    pub fn try_mark_processing(&self, doc_id: i64) -> Result<(bool, Option<&'static str>)> {
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE pdf_documents SET status='splitting', progress=10,
                    error_message=NULL, updated_at=?1
             WHERE id=?2 AND status NOT IN ('splitting','ocr_processing')",
            params![now_string(), doc_id],
        )?;
        if n == 1 {
            return Ok((true, None));
        }
        let exists: Option<i64> = conn
            .query_row("SELECT id FROM pdf_documents WHERE id=?1", params![doc_id], |r| r.get(0))
            .optional()?;
        Ok((false, Some(if exists.is_none() { "not_found" } else { "processing" })))
    }

    pub fn try_mark_batch_processing(&self, doc_ids: &[i64], include_failed: bool) -> Result<Vec<i64>> {
        if doc_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn()?;
        let mut claimed = Vec::new();
        let now = now_string();
        let statuses = if include_failed { "'uploaded','failed'" } else { "'uploaded'" };
        for id in doc_ids {
            let n = conn.execute(
                &format!(
                    "UPDATE pdf_documents SET status='splitting', progress=10, updated_at=?1
                     WHERE id=?2 AND status IN ({})",
                    statuses
                ),
                params![now, id],
            )?;
            if n > 0 {
                claimed.push(*id);
            }
        }
        Ok(claimed)
    }

    // ---------------- 分块 ----------------

    pub fn add_chunk(
        &self,
        doc_id: i64,
        filename: &str,
        chunk_path: &str,
        start_page: i64,
        end_page: i64,
    ) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO pdf_chunks (document_id, filename, chunk_path, start_page, end_page, ocr_status)
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
            params![doc_id, filename, chunk_path, start_page, end_page],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_chunks_by_document(&self, doc_id: i64) -> Result<Vec<Chunk>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, document_id, filename, chunk_path, start_page, end_page, ocr_status, json_path, \
             batch_id, data_id, remote_filename, remote_state, error_message, fetch_attempts, retry_exhausted
             FROM pdf_chunks WHERE document_id=?1 ORDER BY start_page",
        )?;
        let rows = stmt
            .query_map(params![doc_id], |r| {
                Ok(Chunk {
                    id: r.get(0)?,
                    document_id: r.get(1)?,
                    filename: r.get(2)?,
                    chunk_path: r.get(3)?,
                    start_page: r.get(4).unwrap_or(None),
                    end_page: r.get(5).unwrap_or(None),
                    ocr_status: r.get::<_, Option<String>>(6)?.unwrap_or_else(|| "pending".into()),
                    json_path: r.get(7).unwrap_or(None),
                    batch_id: r.get(8).unwrap_or(None),
                    data_id: r.get(9).unwrap_or(None),
                    remote_filename: r.get(10).unwrap_or(None),
                    remote_state: r.get(11).unwrap_or(None),
                    error_message: r.get(12).unwrap_or(None),
                    fetch_attempts: r.get::<_, Option<i64>>(13).unwrap_or(None).unwrap_or(0),
                    retry_exhausted: r.get::<_, Option<i64>>(14).unwrap_or(None).unwrap_or(0) != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get_chunk(&self, chunk_id: i64) -> Result<Option<Chunk>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT id, document_id, filename, chunk_path, start_page, end_page, ocr_status, json_path, \
                 batch_id, data_id, remote_filename, remote_state, error_message, fetch_attempts, retry_exhausted
                 FROM pdf_chunks WHERE id=?1",
                params![chunk_id],
                |r| {
                    Ok(Chunk {
                        id: r.get(0)?,
                        document_id: r.get(1)?,
                        filename: r.get(2)?,
                        chunk_path: r.get(3)?,
                        start_page: r.get(4).unwrap_or(None),
                        end_page: r.get(5).unwrap_or(None),
                        ocr_status: r.get::<_, Option<String>>(6)?.unwrap_or_else(|| "pending".into()),
                        json_path: r.get(7).unwrap_or(None),
                        batch_id: r.get(8).unwrap_or(None),
                        data_id: r.get(9).unwrap_or(None),
                        remote_filename: r.get(10).unwrap_or(None),
                        remote_state: r.get(11).unwrap_or(None),
                        error_message: r.get(12).unwrap_or(None),
                        fetch_attempts: r.get::<_, Option<i64>>(13).unwrap_or(None).unwrap_or(0),
                        retry_exhausted: r.get::<_, Option<i64>>(14).unwrap_or(None).unwrap_or(0) != 0,
                    })
                },
            )
            .optional()?)
    }

    pub fn get_chunks_by_batch(&self, batch_id: &str) -> Result<Vec<Chunk>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, document_id, filename, chunk_path, start_page, end_page, ocr_status, json_path, \
             batch_id, data_id, remote_filename, remote_state, error_message, fetch_attempts, retry_exhausted
             FROM pdf_chunks WHERE batch_id=?1 ORDER BY start_page",
        )?;
        let rows = stmt
            .query_map(params![batch_id], |r| {
                Ok(Chunk {
                    id: r.get(0)?,
                    document_id: r.get(1)?,
                    filename: r.get(2)?,
                    chunk_path: r.get(3)?,
                    start_page: r.get(4).unwrap_or(None),
                    end_page: r.get(5).unwrap_or(None),
                    ocr_status: r.get::<_, Option<String>>(6)?.unwrap_or_else(|| "pending".into()),
                    json_path: r.get(7).unwrap_or(None),
                    batch_id: r.get(8).unwrap_or(None),
                    data_id: r.get(9).unwrap_or(None),
                    remote_filename: r.get(10).unwrap_or(None),
                    remote_state: r.get(11).unwrap_or(None),
                    error_message: r.get(12).unwrap_or(None),
                    fetch_attempts: r.get::<_, Option<i64>>(13).unwrap_or(None).unwrap_or(0),
                    retry_exhausted: r.get::<_, Option<i64>>(14).unwrap_or(None).unwrap_or(0) != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// 更新分块本地来源（PDF 改名后重切会生成新的文件名/路径，页范围不变）
    pub fn update_chunk_source(
        &self,
        chunk_id: i64,
        filename: &str,
        chunk_path: &str,
        start_page: Option<i64>,
        end_page: Option<i64>,
    ) -> Result<()> {
        let conn = self.conn()?;
        if let (Some(s), Some(e)) = (start_page, end_page) {
            conn.execute(
                "UPDATE pdf_chunks SET filename=?1, chunk_path=?2, start_page=?3, end_page=?4 WHERE id=?5",
                params![filename, chunk_path, s, e, chunk_id],
            )?;
        } else {
            conn.execute(
                "UPDATE pdf_chunks SET filename=?1, chunk_path=?2 WHERE id=?3",
                params![filename, chunk_path, chunk_id],
            )?;
        }
        Ok(())
    }

    /// 历史分块仅修正页范围（文件名/路径保持不变，避免把已完成的远端产物改坏）
    pub fn update_chunk_range(&self, chunk_id: i64, start_page: i64, end_page: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE pdf_chunks SET start_page=?1, end_page=?2 WHERE id=?3",
            params![start_page, end_page, chunk_id],
        )?;
        Ok(())
    }

    pub fn delete_chunk(&self, chunk_id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM pdf_chunks WHERE id=?1", params![chunk_id])?;
        Ok(())
    }

    /// 原子绑定：仅当分块尚未上传/未完成时才可绑定到批次
    pub fn attach_chunk_to_batch(
        &self,
        chunk_id: i64,
        batch_id: &str,
        data_id: &str,
        remote_filename: &str,
    ) -> Result<bool> {
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE pdf_chunks
             SET batch_id=?1, data_id=?2, remote_filename=?3,
                 remote_state='uploading', error_message=NULL
             WHERE id=?4
               AND (remote_state IS NULL OR remote_state IN ('pending', 'uploading'))",
            params![batch_id, data_id, remote_filename, chunk_id],
        )?;
        Ok(n > 0)
    }

    /// 上传成功检查点：uploading/uploaded → uploaded
    pub fn mark_chunk_uploaded(&self, chunk_id: i64) -> Result<bool> {
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE pdf_chunks SET remote_state='uploaded', error_message=NULL
             WHERE id=?1 AND remote_state IN ('uploading', 'uploaded')",
            params![chunk_id],
        )?;
        Ok(n > 0)
    }

    /// 原子认领下载：uploaded → downloading
    pub fn claim_chunk_download(&self, chunk_id: i64) -> Result<bool> {
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE pdf_chunks SET remote_state='downloading' WHERE id=?1 AND remote_state='uploaded'",
            params![chunk_id],
        )?;
        Ok(n > 0)
    }

    pub fn update_chunk_remote_state(
        &self,
        chunk_id: i64,
        state: &str,
        json_path: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn()?;
        let ocr_status = match state {
            "done" => Some("completed"),
            "failed" => Some("failed"),
            "pending" | "uploading" | "uploaded" => Some("pending"),
            _ => None,
        };
        conn.execute(
            "UPDATE pdf_chunks SET remote_state=?1 WHERE id=?2",
            params![state, chunk_id],
        )?;
        if let Some(p) = json_path {
            conn.execute("UPDATE pdf_chunks SET json_path=?1 WHERE id=?2", params![p, chunk_id])?;
        }
        if let Some(msg) = error_message {
            conn.execute(
                "UPDATE pdf_chunks SET error_message=?1 WHERE id=?2",
                params![msg, chunk_id],
            )?;
        }
        if let Some(status) = ocr_status {
            conn.execute(
                "UPDATE pdf_chunks SET ocr_status=?1 WHERE id=?2",
                params![status, chunk_id],
            )?;
        }
        if state == "done" {
            // 重试成功后清空失败计数与错误
            conn.execute(
                "UPDATE pdf_chunks SET fetch_attempts=0, retry_exhausted=0, error_message=NULL WHERE id=?1",
                params![chunk_id],
            )?;
        }
        Ok(())
    }

    /// 记录一次取结果失败：自增 fetch_attempts 并写最后错误。
    /// 可重试且未耗尽 → 回退 fallback_state（默认 uploaded），下一轮可再次认领；
    /// 不可重试或耗尽 → failed，retry_exhausted 仅在“重试耗尽”时置 1。
    pub fn mark_chunk_fetch_failure(
        &self,
        chunk_id: i64,
        error_message: &str,
        retryable: bool,
        exhausted: bool,
        fallback_state: &str,
    ) -> Result<()> {
        let conn = self.conn()?;
        let state = if retryable && !exhausted { fallback_state } else { "failed" };
        let ocr_status = if state == "failed" { "failed" } else { "pending" };
        conn.execute(
            "UPDATE pdf_chunks
             SET remote_state=?1, error_message=?2,
                 fetch_attempts=COALESCE(fetch_attempts, 0) + 1,
                 retry_exhausted=?3, ocr_status=?4
             WHERE id=?5",
            params![
                state,
                truncate_chars(error_message, 500),
                exhausted as i64,
                ocr_status,
                chunk_id
            ],
        )?;
        Ok(())
    }

    /// 把分块从云端批次中解绑并置回 pending（保留页范围，便于只重切/重传该分块）
    pub fn reset_chunk_for_retry(&self, chunk_id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE pdf_chunks
             SET remote_state='pending', batch_id=NULL, data_id=NULL,
                 remote_filename=NULL, error_message=NULL, ocr_status='pending',
                 fetch_attempts=0, retry_exhausted=0
             WHERE id=?1",
            params![chunk_id],
        )?;
        Ok(())
    }

    /// 兼容旧启动入口：中断任务的状态重整已由 `ocr::recover_pipeline_on_startup` 接管，
    /// 这里不再一刀切置为 failed，避免破坏活跃批次的断点续跑。
    pub fn reset_interrupted_jobs(&self) -> Result<usize> {
        Ok(0)
    }

    /// 启动恢复：下载中断的 downloading 分块回退为 uploaded
    pub fn reset_stuck_downloading_chunks(&self) -> Result<usize> {
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE pdf_chunks SET remote_state='uploaded' WHERE remote_state='downloading'",
            [],
        )?;
        Ok(n)
    }


    pub fn update_chunk_ocr_status(
        &self,
        chunk_id: i64,
        status: &str,
        json_path: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn()?;
        if let Some(p) = json_path {
            conn.execute(
                "UPDATE pdf_chunks SET ocr_status=?1, json_path=?2 WHERE id=?3",
                params![status, p, chunk_id],
            )?;
        } else {
            conn.execute(
                "UPDATE pdf_chunks SET ocr_status=?1 WHERE id=?2",
                params![status, chunk_id],
            )?;
        }
        Ok(())
    }

    pub fn delete_chunks_by_document(&self, doc_id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM pdf_chunks WHERE document_id=?1", params![doc_id])?;
        Ok(())
    }

    // ---------------- 批次 ----------------

    fn batch_from_row(row: &Row<'_>) -> rusqlite::Result<Batch> {
        let doc_ids_raw: Option<String> = row.get("doc_ids_json").unwrap_or(None);
        let upload_abandoned: Option<i64> = row.get("upload_abandoned").unwrap_or(None);
        Ok(Batch {
            id: row.get("id")?,
            batch_id: row.get("batch_id")?,
            status: row.get::<_, Option<String>>("status")?.unwrap_or_else(|| "processing".into()),
            doc_ids: doc_ids_raw
                .as_deref()
                .and_then(|raw| serde_json::from_str::<Vec<i64>>(raw).ok())
                .unwrap_or_default(),
            file_count: row.get::<_, Option<i64>>("file_count").unwrap_or(Some(0)).unwrap_or(0),
            created_at: row.get("created_at").unwrap_or(None),
            updated_at: row.get("updated_at").unwrap_or(None),
            error_message: row.get("error_message").unwrap_or(None),
            upload_plan_json: row.get("upload_plan_json").unwrap_or(None),
            upload_urls_json: row.get("upload_urls_json").unwrap_or(None),
            uploaded_count: row
                .get::<_, Option<i64>>("uploaded_count")
                .unwrap_or(Some(0))
                .unwrap_or(0),
            upload_abandoned: upload_abandoned.unwrap_or(0) != 0,
            urls_issued_at: row.get("urls_issued_at").unwrap_or(None),
            submitted_at: row.get("submitted_at").unwrap_or(None),
            finished_at: row.get("finished_at").unwrap_or(None),
            poll_count: row.get::<_, Option<i64>>("poll_count").unwrap_or(Some(0)).unwrap_or(0),
            recovery_attempts: row
                .get::<_, Option<i64>>("recovery_attempts")
                .unwrap_or(Some(0))
                .unwrap_or(0),
            poll_failures: row
                .get::<_, Option<i64>>("poll_failures")
                .unwrap_or(Some(0))
                .unwrap_or(0),
            poll_error: row.get("poll_error").unwrap_or(None),
        })
    }

    /// 记录已提交/正在提交的批次。plan 非空时先以 uploading 落库（崩溃可续传）
    pub fn add_ocr_batch(
        &self,
        batch_id: &str,
        doc_ids: &[i64],
        file_count: usize,
        plan: Option<&[UploadPlanItem]>,
        upload_urls: Option<&[String]>,
        urls_issued_at: Option<&str>,
    ) -> Result<i64> {
        let conn = self.conn()?;
        let now = now_string();
        let status = if plan.is_some() { "uploading" } else { "processing" };
        conn.execute(
            "INSERT INTO ocr_batches (batch_id, status, doc_ids_json, file_count, created_at, updated_at,
                                      upload_plan_json, upload_urls_json, uploaded_count,
                                      upload_abandoned, urls_issued_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7, 0, 0, ?8)",
            params![
                batch_id,
                status,
                serde_json::to_string(doc_ids)?,
                file_count as i64,
                now,
                plan.map(|p| serde_json::to_string(p)).transpose()?,
                upload_urls.map(|u| serde_json::to_string(u)).transpose()?,
                urls_issued_at,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_ocr_batch(&self, batch_id: &str) -> Result<Option<Batch>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT * FROM ocr_batches WHERE batch_id=?1",
                params![batch_id],
                Self::batch_from_row,
            )
            .optional()?)
    }

    /// 所有尚未收尾的批次（uploading + processing）
    pub fn get_active_ocr_batches(&self) -> Result<Vec<Batch>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT * FROM ocr_batches WHERE status IN ('uploading','processing') ORDER BY id ASC",
        )?;
        let rows = stmt
            .query_map([], Self::batch_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// 已提交、等待收割的批次
    pub fn get_pending_ocr_batches(&self) -> Result<Vec<Batch>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT * FROM ocr_batches WHERE status='processing' ORDER BY id ASC",
        )?;
        let rows = stmt
            .query_map([], Self::batch_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn count_active_ocr_batches(&self) -> Result<i64> {
        let conn = self.conn()?;
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM ocr_batches WHERE status IN ('uploading','processing')",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn update_ocr_batch_error(&self, batch_id: &str, error_message: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE ocr_batches SET error_message=?1, updated_at=?2 WHERE batch_id=?3",
            params![error_message, now_string(), batch_id],
        )?;
        Ok(())
    }

    /// 批次状态查询失败：累计连续失败次数并落库最后错误（重启后仍可追溯）
    pub fn record_batch_poll_failure(&self, batch_id: &str, error_message: &str) -> Result<bool> {
        let conn = self.conn()?;
        let err = truncate_chars(error_message, 500);
        let n = conn.execute(
            "UPDATE ocr_batches
             SET poll_failures=COALESCE(poll_failures, 0) + 1, poll_error=?1, updated_at=?2
             WHERE batch_id=?3",
            params![err, now_string(), batch_id],
        )?;
        Ok(n > 0)
    }

    /// 状态查询恢复正常：清零连续失败计数与最后错误
    pub fn clear_batch_poll_failure(&self, batch_id: &str) -> Result<bool> {
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE ocr_batches
             SET poll_failures=0, poll_error=NULL, updated_at=?1
             WHERE batch_id=?2 AND (COALESCE(poll_failures, 0) <> 0 OR poll_error IS NOT NULL)",
            params![now_string(), batch_id],
        )?;
        Ok(n > 0)
    }

    /// 批次进入云端解析阶段
    pub fn mark_batch_submitted(&self, batch_id: &str) -> Result<()> {
        let conn = self.conn()?;
        let now = now_string();
        conn.execute(
            "UPDATE ocr_batches SET status='processing', submitted_at=?1, error_message=NULL, updated_at=?1
             WHERE batch_id=?2",
            params![now, batch_id],
        )?;
        Ok(())
    }

    /// 上传被放弃：标记 abandoned 并按是否已有文件上传决定后续状态
    pub fn mark_batch_upload_abandoned(
        &self,
        batch_id: &str,
        status: &str,
        error_message: &str,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE ocr_batches SET status=?1, upload_abandoned=1, error_message=?2, updated_at=?3
             WHERE batch_id=?4",
            params![status, error_message, now_string(), batch_id],
        )?;
        Ok(())
    }

    pub fn increment_batch_counter(&self, batch_id: &str, field: &str) -> Result<bool> {
        if !matches!(field, "uploaded_count" | "poll_count" | "recovery_attempts") {
            return Ok(false);
        }
        let conn = self.conn()?;
        let n = conn.execute(
            &format!(
                "UPDATE ocr_batches SET {} = COALESCE({}, 0) + 1, updated_at=?1 WHERE batch_id=?2",
                field, field
            ),
            params![now_string(), batch_id],
        )?;
        Ok(n > 0)
    }

    /// 批次收尾：completed / failed，记录结束时间
    pub fn close_ocr_batch(&self, batch_id: &str, status: &str, error_message: Option<&str>) -> Result<()> {
        let conn = self.conn()?;
        let now = now_string();
        conn.execute(
            "UPDATE ocr_batches SET status=?1, error_message=?2, finished_at=?3, updated_at=?3
             WHERE batch_id=?4",
            params![status, error_message, now, batch_id],
        )?;
        Ok(())
    }

    pub fn complete_ocr_batch(&self, batch_id: &str, error_message: Option<&str>) -> Result<()> {
        let status = if error_message.is_some() { "failed" } else { "completed" };
        self.close_ocr_batch(batch_id, status, error_message)
    }

    pub fn get_today_uploaded_chunk_count(&self) -> Result<i64> {
        let conn = self.conn()?;
        let total: Option<i64> = conn.query_row(
            "SELECT SUM(file_count) FROM ocr_batches WHERE created_at >= ?1",
            params![today_string()],
            |r| r.get(0),
        )?;
        Ok(total.unwrap_or(0))
    }

    // ---------------- meta ----------------

    pub fn get_meta(&self, key: &str, default: Option<&str>) -> Result<Option<String>> {
        let conn = self.conn()?;
        conn.execute("CREATE TABLE IF NOT EXISTS app_meta (key TEXT PRIMARY KEY, value TEXT)", [])?;
        let val: Option<String> = conn
            .query_row("SELECT value FROM app_meta WHERE key=?1", params![key], |r| r.get(0))
            .optional()?;
        Ok(val.or_else(|| default.map(|s| s.to_string())))
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("CREATE TABLE IF NOT EXISTS app_meta (key TEXT PRIMARY KEY, value TEXT)", [])?;
        conn.execute(
            "INSERT OR REPLACE INTO app_meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    // ---------------- 标签 ----------------

    pub fn add_tag(&self, name: &str, color: &str) -> Result<i64> {
        let conn = self.conn()?;
        let existing: Option<i64> = conn
            .query_row("SELECT id FROM tags WHERE name=?1", params![name], |r| r.get(0))
            .optional()?;
        if let Some(id) = existing {
            return Ok(id);
        }
        conn.execute("INSERT INTO tags (name, color) VALUES (?1, ?2)", params![name, color])?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_tag_by_name(&self, name: &str) -> Result<Option<TagRow>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row("SELECT id, name, color FROM tags WHERE name=?1", params![name], |r| {
                Ok(TagRow { id: r.get(0)?, name: r.get(1)?, color: r.get(2)? })
            })
            .optional()?)
    }

    pub fn get_all_tags(&self) -> Result<Vec<TagRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT id, name, color FROM tags ORDER BY id")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(TagRow { id: r.get(0)?, name: r.get(1)?, color: r.get(2)? })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn delete_tag(&self, tag_id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM document_tags WHERE tag_id=?1", params![tag_id])?;
        conn.execute("DELETE FROM tags WHERE id=?1", params![tag_id])?;
        Ok(())
    }

    pub fn add_tag_to_document(&self, doc_id: i64, tag_id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT OR IGNORE INTO document_tags (document_id, tag_id) VALUES (?1, ?2)",
            params![doc_id, tag_id],
        )?;
        Ok(())
    }

    pub fn remove_tag_from_document(&self, doc_id: i64, tag_id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM document_tags WHERE document_id=?1 AND tag_id=?2",
            params![doc_id, tag_id],
        )?;
        Ok(())
    }

    pub fn get_tags_grouped_by_document(
        &self,
        doc_ids: Option<&[i64]>,
    ) -> Result<HashMap<i64, Vec<TagDto>>> {
        let conn = self.conn()?;
        let mut grouped: HashMap<i64, Vec<TagDto>> = HashMap::new();
        if let Some(ids) = doc_ids {
            if ids.is_empty() {
                return Ok(grouped);
            }
            let marks = vec!["?"; ids.len()].join(",");
            let sql = format!(
                "SELECT dt.document_id, t.id, t.name, t.color FROM document_tags dt
                 JOIN tags t ON t.id = dt.tag_id WHERE dt.document_id IN ({})",
                marks
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(ids.iter()), |r| {
                Ok((r.get::<_, i64>(0)?, TagDto { id: r.get(1)?, name: r.get(2)?, color: r.get(3)? }))
            })?;
            for row in rows {
                let (doc_id, tag) = row?;
                grouped.entry(doc_id).or_default().push(tag);
            }
        }
        Ok(grouped)
    }

    // ---------------- 启动维护 ----------------

    /// 更新断点续跑阶段标记（不改变前端可见的 status）
    pub fn set_document_phase(&self, doc_id: i64, phase: &str, batch_id: Option<&str>) -> Result<()> {
        let conn = self.conn()?;
        if let Some(bid) = batch_id {
            conn.execute(
                "UPDATE pdf_documents SET ocr_phase=?1, ocr_batch_id=?2, updated_at=?3 WHERE id=?4",
                params![phase, bid, now_string(), doc_id],
            )?;
        } else {
            conn.execute(
                "UPDATE pdf_documents SET ocr_phase=?1, updated_at=?2 WHERE id=?3",
                params![phase, now_string(), doc_id],
            )?;
        }
        Ok(())
    }

    /// 把中断文档放回待提交队列；保留 pdf_chunks（已完成分块结果不会被重传）
    pub fn requeue_document_for_resume(&self, doc_id: i64, error_message: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE pdf_documents SET status='uploaded', progress=0, error_message=?1,
                    ocr_phase=NULL, updated_at=?2
             WHERE id=?3 AND status != 'completed'",
            params![error_message, now_string(), doc_id],
        )?;
        Ok(())
    }

    /// 原子认领“合并落盘”阶段，避免重复合并
    pub fn try_claim_finalize(&self, doc_id: i64) -> Result<bool> {
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE pdf_documents SET ocr_phase='merging', updated_at=?1
             WHERE id=?2 AND status IN ('splitting','ocr_processing','failed')",
            params![now_string(), doc_id],
        )?;
        Ok(n > 0)
    }

    /// 所有活跃批次关联的文档 ID 集合
    pub fn get_active_batch_doc_ids(&self) -> Result<std::collections::HashSet<i64>> {
        let batches = self.get_active_ocr_batches()?;
        let mut ids = std::collections::HashSet::new();
        for b in batches {
            for id in &b.doc_ids {
                ids.insert(*id);
            }
            for item in b.upload_plan() {
                ids.insert(item.doc_id);
            }
        }
        Ok(ids)
    }

    /// 包含指定文档的活跃批次 ID（兼容无上传计划的历史批次）
    pub fn get_active_batch_ids_for_document(&self, doc_id: i64) -> Result<Vec<String>> {
        Ok(self
            .get_active_ocr_batches()?
            .into_iter()
            .filter(|b| b.doc_ids.contains(&doc_id))
            .map(|b| b.batch_id)
            .collect())
    }

    /// 启动恢复用：读取残留的 splitting/ocr_processing 文档
    pub fn get_documents_in_progress(&self) -> Result<Vec<Document>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT * FROM pdf_documents WHERE status IN ('splitting','ocr_processing')",
        )?;
        let rows = stmt
            .query_map([], doc_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn backfill_json_paths(&self) -> Result<usize> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id FROM pdf_documents
             WHERE status='completed' AND (json_paths IS NULL OR json_paths='')",
        )?;
        let ids: Vec<i64> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        let mut count = 0;
        for doc_id in ids {
            let mut chunk_stmt = conn.prepare(
                "SELECT json_path FROM pdf_chunks WHERE document_id=?1 AND json_path IS NOT NULL
                 ORDER BY start_page",
            )?;
            let paths_raw: Vec<String> = chunk_stmt
                .query_map(params![doc_id], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(chunk_stmt);
            let mut paths: Vec<String> = Vec::new();
            for p in paths_raw {
                if p.is_empty() || paths.contains(&p) {
                    continue;
                }
                if Path::new(&p).exists() || Path::new(&format!("{}.gz", p)).exists() {
                    paths.push(p);
                }
            }
            if !paths.is_empty() {
                conn.execute(
                    "UPDATE pdf_documents SET json_paths=?1, updated_at=?2 WHERE id=?3",
                    params![serde_json::to_string(&paths)?, now_string(), doc_id],
                )?;
                count += 1;
            }
        }
        Ok(count)
    }

    pub fn migrate_output_layout(&self, cfg: &Config) -> Result<(usize, usize)> {
        fs::create_dir_all(&cfg.output_md_folder)?;
        fs::create_dir_all(&cfg.output_json_folder)?;
        // 数据库中的历史路径可能是相对旧版后端目录的，统一解析为绝对路径再比较/移动
        let out_root = crate::config::resolve_path(&cfg.output_folder.to_string_lossy());
        let mut moved_md = 0usize;
        let mut moved_json = 0usize;

        let mut relocate = |old_path: &str| -> Option<PathBuf> {
            let p = crate::config::resolve_path(old_path);
            if p.parent() != Some(out_root.as_path()) {
                return None;
            }
            let name = p.file_name()?.to_string_lossy().to_string();
            let low = name.to_lowercase();
            let (target_dir, kind) = if low.ends_with(".md") {
                (&cfg.output_md_folder, 0)
            } else if low.ends_with(".json") || low.ends_with(".json.gz") {
                (&cfg.output_json_folder, 1)
            } else {
                return None;
            };
            let new_path = target_dir.join(&name);
            if p.exists() {
                let _ = fs::create_dir_all(target_dir);
                if fs::rename(&p, &new_path).is_err() {
                    // 跨盘时 rename 失败，退回“复制+删除”
                    if fs::copy(&p, &new_path).is_ok() {
                        let _ = fs::remove_file(&p);
                    } else {
                        return None;
                    }
                }
                if kind == 0 {
                    moved_md += 1;
                } else {
                    moved_json += 1;
                }
            } else if !new_path.exists() {
                return None;
            }
            Some(new_path)
        };

        let mut conn = self.conn()?;
        // 1) pdf_documents
        let docs: Vec<(i64, Option<String>, Option<String>)> = {
            let mut stmt = conn.prepare("SELECT id, markdown_path, json_paths FROM pdf_documents")?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        // 2) pdf_chunks（先取完再开事务，避免 conn 同时被借用）
        let chunks: Vec<(i64, String)> = {
            let mut stmt =
                conn.prepare("SELECT id, json_path FROM pdf_chunks WHERE json_path IS NOT NULL")?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let tx = conn.transaction()?;
        for (doc_id, md, json_paths) in docs {
            if let Some(md_path) = md {
                if let Some(new_md) = relocate(&md_path) {
                    tx.execute(
                        "UPDATE pdf_documents SET markdown_path=?1 WHERE id=?2",
                        params![new_md.to_string_lossy().to_string(), doc_id],
                    )?;
                }
            }
            if let Some(raw) = json_paths {
                if let Ok(paths) = serde_json::from_str::<Vec<String>>(&raw) {
                    let mut changed = false;
                    let new_paths: Vec<String> = paths
                        .iter()
                        .map(|p| match relocate(p) {
                            Some(np) => {
                                changed = true;
                                np.to_string_lossy().to_string()
                            }
                            None => p.clone(),
                        })
                        .collect();
                    if changed {
                        tx.execute(
                            "UPDATE pdf_documents SET json_paths=?1 WHERE id=?2",
                            params![serde_json::to_string(&new_paths)?, doc_id],
                        )?;
                    }
                }
            }
        }
        // 2) pdf_chunks 回写
        for (chunk_id, jp) in chunks {
            if let Some(new_jp) = relocate(&jp) {
                tx.execute(
                    "UPDATE pdf_chunks SET json_path=?1 WHERE id=?2",
                    params![new_jp.to_string_lossy().to_string(), chunk_id],
                )?;
            }
        }
        tx.commit()?;

        // 3) 兜底：output 根目录下未被 DB 引用的散落文件
        if let Ok(entries) = fs::read_dir(&cfg.output_folder) {
            for entry in entries.flatten() {
                if entry.path().is_file() {
                    let _ = relocate(&entry.path().to_string_lossy());
                }
            }
        }
        Ok((moved_md, moved_json))
    }

    pub fn migrate_json_to_gzip(&self, cfg: &Config) -> Result<(usize, usize, usize)> {
        let json_dir = cfg.output_json_folder.clone();
        fs::create_dir_all(&json_dir)?;
        let mut stats = (0usize, 0usize, 0usize); // migrated, skipped, failed

        fn gzip_to_json_dir(src: &Path, json_dir: &Path) -> Option<PathBuf> {
            let name = src.file_name()?.to_string_lossy().to_string();
            if !name.to_lowercase().ends_with(".json") {
                return None;
            }
            let dst = json_dir.join(format!("{}.gz", name));
            if dst.exists() {
                return Some(dst);
            }
            let mut fin = fs::File::open(src).ok()?;
            let mut fout = GzEncoder::new(fs::File::create(&dst).ok()?, Compression::new(6));
            std::io::copy(&mut fin, &mut fout).ok()?;
            fout.finish().ok()?;
            // 校验可解压
            let check = fs::File::open(&dst).ok()?;
            let mut dec = GzDecoder::new(check);
            let mut buf = [0u8; 1];
            if dec.read(&mut buf).is_err() {
                let _ = fs::remove_file(&dst);
                return None;
            }
            Some(dst)
        }

        fn resolve_src(p: &str) -> Option<PathBuf> {
            for cand in [p.to_string(), format!("{}.gz", p)] {
                let path = PathBuf::from(&cand);
                if path.exists() {
                    return Some(path);
                }
            }
            None
        }

        let mut conn = self.conn()?;
        let docs: Vec<(i64, String)> = {
            let mut stmt = conn
                .prepare("SELECT id, json_paths FROM pdf_documents WHERE json_paths IS NOT NULL")?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let chunks: Vec<(i64, String)> = {
            let mut stmt =
                conn.prepare("SELECT id, json_path FROM pdf_chunks WHERE json_path IS NOT NULL")?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let tx = conn.transaction()?;
        for (doc_id, raw) in docs {
            let Ok(paths) = serde_json::from_str::<Vec<String>>(&raw) else { continue };
            let mut new_paths = Vec::new();
            let mut changed = false;
            for p in paths {
                let path = PathBuf::from(&p);
                if p.ends_with(".gz") && path.exists() {
                    new_paths.push(p);
                    continue;
                }
                let Some(src) = resolve_src(&p) else {
                    new_paths.push(p);
                    stats.1 += 1;
                    continue;
                };
                if src.to_string_lossy().ends_with(".gz") {
                    if src.parent() == Some(json_dir.as_path()) {
                        if src.to_string_lossy() != p {
                            new_paths.push(src.to_string_lossy().to_string());
                            changed = true;
                        } else {
                            new_paths.push(p);
                        }
                        continue;
                    }
                    let dst = json_dir.join(src.file_name().unwrap_or_default());
                    match fs::rename(&src, &dst) {
                        Ok(_) => {
                            new_paths.push(dst.to_string_lossy().to_string());
                            changed = true;
                            stats.0 += 1;
                        }
                        Err(_) => {
                            new_paths.push(p);
                            stats.2 += 1;
                        }
                    }
                    continue;
                }
                match gzip_to_json_dir(&src, &json_dir) {
                    Some(dst) => {
                        new_paths.push(dst.to_string_lossy().to_string());
                        changed = true;
                        stats.0 += 1;
                        if src != dst {
                            let _ = fs::remove_file(&src);
                        }
                    }
                    None => {
                        new_paths.push(p);
                        stats.2 += 1;
                    }
                }
            }
            if changed {
                tx.execute(
                    "UPDATE pdf_documents SET json_paths=?1 WHERE id=?2",
                    params![serde_json::to_string(&new_paths)?, doc_id],
                )?;
            }
        }

        for (chunk_id, p) in chunks {
            let path = PathBuf::from(&p);
            if p.ends_with(".gz") && path.exists() {
                continue;
            }
            let Some(src) = resolve_src(&p) else {
                stats.1 += 1;
                continue;
            };
            if src.to_string_lossy().ends_with(".gz") {
                if src.parent() == Some(json_dir.as_path()) {
                    if src.to_string_lossy() != p {
                        tx.execute(
                            "UPDATE pdf_chunks SET json_path=?1 WHERE id=?2",
                            params![src.to_string_lossy().to_string(), chunk_id],
                        )?;
                    }
                    continue;
                }
                let dst = json_dir.join(src.file_name().unwrap_or_default());
                if fs::rename(&src, &dst).is_ok() {
                    tx.execute(
                        "UPDATE pdf_chunks SET json_path=?1 WHERE id=?2",
                        params![dst.to_string_lossy().to_string(), chunk_id],
                    )?;
                    stats.0 += 1;
                } else {
                    stats.2 += 1;
                }
                continue;
            }
            match gzip_to_json_dir(&src, &json_dir) {
                Some(dst) => {
                    tx.execute(
                        "UPDATE pdf_chunks SET json_path=?1 WHERE id=?2",
                        params![dst.to_string_lossy().to_string(), chunk_id],
                    )?;
                    stats.0 += 1;
                    if src != dst {
                        let _ = fs::remove_file(&src);
                    }
                }
                None => stats.2 += 1,
            }
        }
        tx.commit()?;

        // 兜底：旧 output/json 下未被 DB 引用的散落文件
        let old_dir = cfg.output_folder.join("json");
        if old_dir.is_dir() && old_dir != json_dir {
            if let Ok(entries) = fs::read_dir(&old_dir) {
                for entry in entries.flatten() {
                    let src = entry.path();
                    if !src.is_file() {
                        continue;
                    }
                    let low = src.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
                    if low.ends_with(".json") {
                        if let Some(_dst) = gzip_to_json_dir(&src, &json_dir) {
                            stats.0 += 1;
                            let _ = fs::remove_file(&src);
                        } else {
                            stats.2 += 1;
                        }
                    } else if low.ends_with(".json.gz") {
                        let dst = json_dir.join(src.file_name().unwrap_or_default());
                        if fs::rename(&src, &dst).is_ok() {
                            stats.0 += 1;
                        } else {
                            stats.2 += 1;
                        }
                    }
                }
            }
        }
        Ok(stats)
    }

    /// 后台回填 md5（逐文档独立事务，文件缺失则跳过）
    pub fn backfill_md5(&self) -> Result<usize> {
        let conn = self.conn()?;
        let rows: Vec<(i64, String)> = {
            let mut stmt = conn.prepare(
                "SELECT id, original_path FROM pdf_documents WHERE md5 IS NULL OR md5=''",
            )?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let mut updated = 0;
        for (doc_id, path) in rows {
            if path.is_empty() || !Path::new(&path).exists() {
                continue;
            }
            let Ok(digest) = crate::pdf_utils::file_md5(Path::new(&path)) else { continue };
            for attempt in 0..5 {
                match conn.execute(
                    "UPDATE pdf_documents SET md5=?1 WHERE id=?2",
                    params![digest, doc_id],
                ) {
                    Ok(_) => {
                        updated += 1;
                        break;
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(
                        500 * (attempt + 1) as u64,
                    )),
                }
            }
        }
        Ok(updated)
    }

    /// 按文件名规则回填 ocr_disabled（一次性 v1 标记 + NULL 兜底）
    pub fn backfill_ocr_disabled(&self) -> Result<()> {
        let key = "ocr_disabled_backfill_v1";
        let conn = self.conn()?;
        conn.execute("CREATE TABLE IF NOT EXISTS app_meta (key TEXT PRIMARY KEY, value TEXT)", [])?;
        let done: Option<String> = conn
            .query_row("SELECT value FROM app_meta WHERE key=?1", params![key], |r| r.get(0))
            .optional()?;
        if done.is_none() {
            let rows: Vec<(i64, String, Option<i64>)> = {
                let mut stmt =
                    conn.prepare("SELECT id, filename, ocr_disabled FROM pdf_documents")?;
                let rows = stmt
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2).unwrap_or(None))))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            let mut to_disable = Vec::new();
            let mut to_enable = Vec::new();
            for (id, filename, flag) in rows {
                if crate::pdf_utils::is_ocr_disabled_name(&filename) {
                    if flag.unwrap_or(0) == 0 {
                        to_disable.push(id);
                    }
                } else if flag.is_none() {
                    to_enable.push(id);
                }
            }
            for (ids, value) in [(&to_disable, 1i64), (&to_enable, 0i64)] {
                for chunk in ids.chunks(500) {
                    let marks = vec!["?"; chunk.len()].join(",");
                    let sql =
                        format!("UPDATE pdf_documents SET ocr_disabled=? WHERE id IN ({})", marks);
                    let mut vals: Vec<Value> = vec![Value::Integer(value)];
                    for id in chunk {
                        vals.push(Value::Integer(*id));
                    }
                    conn.execute(&sql, params_from_iter(vals.iter()))?;
                }
            }
            conn.execute(
                "INSERT OR REPLACE INTO app_meta (key, value) VALUES (?1, ?2)",
                params![key, (to_disable.len() + to_enable.len()).to_string()],
            )?;
            crate::log_info!(
                "[启动] ocr_disabled v1 规则回填完成：命中规则置禁止 {} 个，NULL 补允许 {} 个",
                to_disable.len(),
                to_enable.len()
            );
        } else {
            let rows: Vec<(i64, String)> = {
                let mut stmt = conn.prepare(
                    "SELECT id, filename FROM pdf_documents WHERE ocr_disabled IS NULL",
                )?;
                let rows = stmt
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            let mut updated = 0;
            for (id, filename) in rows {
                let flag = if crate::pdf_utils::is_ocr_disabled_name(&filename) { 1 } else { 0 };
                conn.execute(
                    "UPDATE pdf_documents SET ocr_disabled=?1 WHERE id=?2",
                    params![flag, id],
                )?;
                updated += 1;
            }
            if updated > 0 {
                crate::log_info!("[启动] ocr_disabled NULL 兜底回填：{} 个文档", updated);
            }
        }
        Ok(())
    }
}

pub fn err(msg: impl Into<String>) -> anyhow::Error {
    anyhow!(msg.into())
}
