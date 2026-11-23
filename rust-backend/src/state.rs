use std::sync::{Arc, RwLock};

use crate::config::Config;
use crate::db::Db;
use crate::ocr::Scheduler;
use crate::search::SearchManager;

#[derive(Clone)]
pub struct AppState {
    /// 可热更新的运行配置：设置面板保存后立即替换，正在运行的任务使用启动时的快照
    pub cfg: Arc<RwLock<Config>>,
    pub db: Db,
    pub scheduler: Arc<Scheduler>,
    /// Markdown 全文检索索引管理（独立线程构建，不阻塞 OCR）
    pub search: SearchManager,
}

impl AppState {
    /// 取当前配置快照，避免长时间持锁
    pub fn cfg(&self) -> Config {
        self.cfg
            .read()
            .map(|c| c.clone())
            .unwrap_or_else(|e| e.into_inner().clone())
    }

    pub fn set_cfg(&self, cfg: Config) {
        match self.cfg.write() {
            Ok(mut guard) => *guard = cfg,
            Err(e) => *e.into_inner() = cfg,
        }
    }
}
