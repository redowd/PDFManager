#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod batch_rename;
mod config;
mod db;
mod desktop;
mod docx;
mod epub;
mod json_to_md;
mod logging;
mod mineru;
mod ocr;
mod pdf_utils;
mod routes;
mod search;
mod settings;
mod state;
mod zh_convert;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post, put};
use axum::Router;
use tower_http::cors::{Any, CorsLayer};

use config::Config;
use db::Db;
use ocr::Scheduler;
use state::AppState;

fn build_router(state: AppState, cfg: &Config) -> Router {
    let origins: Vec<axum::http::HeaderValue> = cfg
        .cors_origins
        .iter()
        .filter_map(|o| o.parse().ok())
        .collect();
    let cors = CorsLayer::new()
        .allow_origin(origins)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route(
            "/api/documents",
            get(routes::get_documents).post(routes::upload_document),
        )
        .route("/api/documents/scan", post(routes::scan_directory))
        .route("/api/documents/batch/ocr", post(routes::batch_ocr))
        .route(
            "/api/documents/batch/ocr-disabled",
            post(routes::batch_set_ocr_disabled),
        )
        .route(
            "/api/documents/batch/rating",
            post(routes::batch_set_rating),
        )
        .route(
            "/api/documents/batch/rebuild-markdown",
            post(routes::batch_rebuild_markdown),
        )
        .route(
            "/api/documents/batch/rename/preview",
            post(routes::batch_rename_preview),
        )
        .route(
            "/api/documents/batch/rename",
            post(routes::batch_rename),
        )
        .route(
            "/api/documents/{id}",
            get(routes::get_document).delete(routes::delete_document),
        )
        .route(
            "/api/documents/{id}/markdown",
            get(routes::get_document_markdown).put(routes::save_document_markdown),
        )
        .route("/api/documents/{id}/json/{index}", get(routes::get_document_json))
        .route("/api/documents/{id}/pdf", get(routes::get_document_pdf))
        .route("/api/documents/{id}/click", post(routes::bump_document_click))
        .route(
            "/api/documents/{id}/rating",
            axum::routing::put(routes::set_document_rating),
        )
        .route("/api/documents/{id}/rename", put(routes::rename_document))
        .route("/api/documents/{id}/move", put(routes::move_document))
        .route("/api/documents/{id}/ocr", post(routes::process_ocr))
        .route(
            "/api/documents/{id}/rebuild-markdown",
            post(routes::rebuild_document_markdown),
        )
        // 预览弹窗导出 EPUB：表格转 XHTML、图片内嵌
        .route(
            "/api/documents/{id}/export-epub",
            post(routes::export_document_epub),
        )
        .route(
            "/api/documents/{id}/export-docx",
            post(routes::export_document_docx),
        )
        .route(
            "/api/documents/{id}/ocr-disabled",
            put(routes::set_ocr_disabled),
        )
        .route(
            "/api/documents/{id}/tags",
            post(routes::add_tag_to_document),
        )
        .route(
            "/api/documents/{id}/tags/{tag_id}",
            axum::routing::delete(routes::remove_tag_from_document),
        )
        // 全文检索：索引状态/构建与查询
        .route("/api/search", get(routes::search_documents))
        .route("/api/search/index/status", get(routes::search_index_status))
        .route(
            "/api/search/index/rebuild",
            post(routes::search_index_rebuild),
        )
        .route(
            "/api/search/index/update",
            post(routes::search_index_update),
        )
        .route("/api/folders", get(routes::browse_folders))
        .route("/api/scheduler/status", get(routes::scheduler_status))
        .route("/api/scheduler/start", post(routes::scheduler_start))
        .route("/api/scheduler/stop", post(routes::scheduler_stop))
        .route("/api/ocr/recover", post(routes::ocr_recover))
        .route("/api/tags", get(routes::get_tags).post(routes::create_tag))
        .route("/api/tags/{id}", axum::routing::delete(routes::delete_tag))
        .route("/api/fonts/file", get(routes::get_font_file))
        // 图片与设置接口必须早于静态资源兜底，否则会被前端 SPA 路由吞掉
        .route("/api/images/{name}", get(routes::get_image))
        .route(
            "/api/settings",
            get(routes::get_settings).put(routes::update_settings),
        )
        // 预览弹窗繁简转换：zhconv 词库 + 用户自定义繁→简映射
        .route("/api/convert-zh", post(routes::convert_zh))
        .fallback(routes::static_handler)
        .layer(DefaultBodyLimit::max(
            (cfg.max_size_bytes as usize).saturating_add(10 * 1024 * 1024),
        ))
        .layer(cors)
        .with_state(state)
}

/// 优先监听配置端口；端口被占用时退回系统分配的随机端口，避免第二个实例启动失败
fn bind_listener(preferred: u16) -> std::io::Result<std::net::TcpListener> {
    match std::net::TcpListener::bind(("127.0.0.1", preferred)) {
        Ok(l) => Ok(l),
        Err(e) => {
            log_error!("监听端口 {} 失败（{}），改用随机端口", preferred, e);
            std::net::TcpListener::bind(("127.0.0.1", 0))
        }
    }
}

fn main() {
    let mut cfg = Config::load();
    let log_path = cfg
        .database_path
        .parent()
        .map(|p| p.join("pdf_manager.log"))
        .unwrap_or_else(|| PathBuf::from("pdf_manager.log"));
    logging::init(&log_path);

    // WebView2 的缓存与配置统一放在数据目录，避免在 exe 旁边生成 PDFManager.exe.WebView2
    let webview_data_dir = cfg
        .database_path
        .parent()
        .map(|p| p.join("webview2"))
        .unwrap_or_else(|| PathBuf::from("webview2"));

    if let Err(e) = cfg.init_directories() {
        log_error!("创建数据目录失败：{}", e);
    }

    let db = Db::new(&cfg);
    if let Err(e) = db.init() {
        log_error!("初始化数据库失败：{}", e);
        std::process::exit(1);
    }

    // 设置面板保存的覆盖项优先于 .env；目录可能被改到新位置，需要重新创建
    if let Err(e) = settings::load_overrides(&db, &mut cfg) {
        log_error!("[设置] 读取 app_meta 覆盖项失败：{}", e);
    }
    if let Err(e) = cfg.init_directories() {
        log_error!("创建数据目录失败：{}", e);
    }

    // 启动维护（与 Python 版一致）
    match db.backfill_json_paths() {
        Ok(n) if n > 0 => log_info!("[启动] 已回填 {} 个历史文档的 json_paths", n),
        Ok(_) => {}
        Err(e) => log_error!("[启动] 回填 json_paths 失败：{}", e),
    }
    match db.migrate_output_layout(&cfg) {
        Ok((md, js)) if md > 0 || js > 0 => log_info!(
            "[启动] OCR产物目录迁移：移动 Markdown {} 个、JSON {} 个",
            md,
            js
        ),
        Ok(_) => {}
        Err(e) => log_error!("[启动] OCR产物目录迁移失败：{}", e),
    }
    match db.migrate_json_to_gzip(&cfg) {
        Ok((m, s, f)) if m > 0 || f > 0 => log_info!(
            "[启动] JSON压缩迁移：{} 个压缩并迁移，{} 个源文件缺失跳过，{} 个失败",
            m,
            s,
            f
        ),
        Ok(_) => {}
        Err(e) => log_error!("[启动] JSON压缩迁移失败：{}", e),
    }

    // 后台回填（不阻塞服务启动）
    {
        let db = db.clone();
        std::thread::spawn(move || {
            let _ = db.backfill_ocr_disabled();
        });
    }
    {
        let db = db.clone();
        std::thread::spawn(move || match db.backfill_md5() {
            Ok(n) => log_info!("[启动] md5 后台回填完成：{} 个文档", n),
            Err(e) => log_error!("[启动] md5 回填失败：{}", e),
        });
    }

    let cfg_shared = Arc::new(RwLock::new(cfg.clone()));
    let state = AppState {
        cfg: cfg_shared.clone(),
        db: db.clone(),
        scheduler: Arc::new(Scheduler::new()),
        search: search::SearchManager::new(cfg_shared, db),
    };

    // 索引校验放在后台线程：缺索引保持未构建，损坏/版本不兼容则自动全量重建，不阻塞窗口启动
    {
        let search = state.search.clone();
        std::thread::spawn(move || search.startup_check());
    }

    let listener = match bind_listener(cfg.port) {
        Ok(l) => l,
        Err(e) => {
            log_error!("无法监听本机端口：{}", e);
            std::process::exit(1);
        }
    };
    let port = listener
        .local_addr()
        .map(|a| a.port())
        .unwrap_or(cfg.port);
    let _ = listener.set_nonblocking(true);
    let url = format!("http://127.0.0.1:{}", port);

    log_info!("PDF Manager (Rust) 已启动");
    log_info!("访问地址: {}", url);
    log_info!("数据库: {}", cfg.database_path.display());
    log_info!("产物目录: {}", cfg.output_md_folder.display());

    // axum 服务在后台线程的 tokio 运行时分发，主线程留给桌面窗口事件循环
    let app = build_router(state.clone(), &cfg);
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                log_error!("创建异步运行时失败：{}", e);
                return;
            }
        };
        rt.block_on(async move {
            // 启动恢复：重置卡死下载、放弃过期上传批次、合并已就绪分块、重排中断文档
            let recovered = ocr::recover_pipeline_on_startup(&state);
            log_info!(
                "[恢复] 启动恢复完成：重置下载 {}，放弃过期上传批次 {}，待合并 {}，已合并 {}，重排 {}",
                recovered.reset_downloading,
                recovered.expired_upload_batches,
                recovered.merge_ready,
                recovered.merged,
                recovered.requeued
            );
            // 调度器依赖 tokio 运行时，必须在进入 runtime 后恢复
            ocr::init_scheduler_from_meta(state.clone());
            if ocr::start_recovery_worker(&state) {
                log_info!("[恢复] 调度器未开启，已启动一次性恢复线程");
            }
            let listener = match tokio::net::TcpListener::from_std(listener) {
                Ok(l) => l,
                Err(e) => {
                    log_error!("初始化监听失败：{}", e);
                    return;
                }
            };
            if let Err(e) = axum::serve(listener, app).await {
                log_error!("服务运行失败：{}", e);
            }
        });
    });

    // 等待内嵌服务就绪，避免窗口首次加载出现空白页
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    if cfg.open_browser {
        let url = url.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(600));
            let _ = open::that(url);
        });
    }

    desktop::run(&url, &webview_data_dir);
}
