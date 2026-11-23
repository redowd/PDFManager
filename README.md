# PDF Manager

本地 PDF 管理与 OCR 工作台：把扫描版 / 文字版 PDF 批量交给 MinerU 做 OCR，产出 Markdown、图片与 JSON，并在同一个界面里完成 Markdown 预览、编辑、全文检索与导出。

- 后端：Rust（axum + rusqlite + Tantivy），前端构建产物通过 rust-embed 内嵌进 exe
- 前端：React 18 + Ant Design 5（CodeMirror 6 负责 Markdown 编辑）
- 形态：Windows 单文件 exe，双击即用，桌面窗口基于 WebView2

它解决的问题：

1. 成百上千个 PDF 需要统一登记、去重、跟踪 OCR 进度；
2. OCR 结果需要能直接读、能改、能查；
3. 改完的 Markdown 需要能导出成常见格式（EPUB / Word）交付。

---

## 运行方式

直接运行根目录的 `PDFManager.exe`（或 `rust-backend/target/release/pdf-manager.exe`）。

- 默认监听 `http://127.0.0.1:5000`，只绑定本机回环地址；端口被占用时会自动换一个空闲端口（日志中会打印实际地址）。
- 启动后打开桌面窗口；设置 `OPEN_BROWSER=1` 时额外用系统浏览器打开同一地址。
- 日志写在数据目录的 `pdf_manager.log`。

首次启动会做这些事（全部幂等，可重复启动）：

1. 创建缺失的数据目录（上传、产物、临时、Markdown / JSON / 图片 / 检索索引 / 导出目录）；
2. 初始化 / 迁移 SQLite 数据库；
3. 读取配置面板保存的覆盖项（`app_meta`）；
4. 回填历史文档的 JSON 路径、迁移旧 OCR 产物目录、后台补算文件 md5；
5. 校验全文索引：缺失视为「未构建」；版本不兼容或损坏则隔离旧目录并自动重建；正常则加载并记录文档数；
6. 恢复上次未完成的 OCR 任务（调度器未开启时只做一次性恢复）。

---

## 功能清单

**文档管理**
- 单个 / 批量上传 PDF，按 MD5 与原始路径去重；扫描目录批量导入。
- 列表支持关键字搜索（文件名 / 标签名）、原始路径独立搜索、状态与标签筛选、多列排序（含更新时间、点击次数、评分）、列宽拖拽（双击恢复默认）、跨页勾选、每页 10/20/50/100/1000 条。

**OCR（MinerU）**
- 上传 → 分割 → 提交 → 轮询 → 下载 → 解压 → 生成 Markdown / 图片 / JSON 的完整链路，带断点续跑。
- 失败自动重试（次数、退避、抖动、下载轮次可配），也可对单篇或勾选的文档重新提交 OCR。
- 批量启用 / 停用 OCR：`ocr_disabled` 是显式开关，写入即生效，不因当前值相同而拒绝。
- 批量打分（0–10）、单篇评分、点击计数。
- 每日上传配额、上传间隔、并发数、最大页数 / 文件大小均可在配置面板调整。

**Markdown 预览与编辑**
- 三种模式：预览、编辑、实时预览（CodeMirror 6，图片按真实尺寸占位）。
- 搜索 / 替换：字面与正则两种模式，支持大小写、整词、`^` / `$` 逐行锚点（多行标志）、`$1` / `$<name>` / `$&` / `$$` 替换语义；上一个 / 下一个跳转，单次替换与全部替换；零长度匹配会被过滤。
- 搜索栏与替换栏默认隐藏，`Ctrl+F` / `Ctrl+H` 呼出；快捷键与「打开预览时是否显示」可在预览设置中修改并持久化。
- 保存快捷键默认 `Ctrl+S`，可改；保存不切换模式、不重置滚动位置。
- 弹窗内可直接评分、选择或新建标签；目录可折叠；目录宽度、正文宽度、弹窗宽高均可拖拽调节（本地持久化）。
- 导出 EPUB 与 Word（`.docx`）：标题层级、段落、列表、表格、代码块、引用与正文图片均转为目标格式的原生结构，图片真正内嵌；缺失图片在正文占位并在文末列出清单。有未保存修改时导出当前屏幕内容，否则读取磁盘 Markdown。
- 由本地 JSON（MinerU 原始结果）重建 Markdown，可单篇或批量执行。
- 批量重命名：字面或正则替换文件名，先预览冲突 / 非法 / 只读 / 不存在，再确认执行，逐条返回结果。
- 繁→简自定义映射（`繁体词=>简体词`，每行一条，空串表示关闭）。

**全文检索（Tantivy）**
- 两种模式：`smart`（分词 + AND，无结果时退化为 OR 并标记）与 `exact`（归一化后做子串精确校验）。
- 结果带片段高亮、文档信息与命中位置；索引可增量更新、全量重建，状态接口返回文档数、构建状态与目录。
- 索引带 schema 版本与元数据文件，启动校验；更换索引目录后旧索引不可沿用，会提示重建。

---

## 目录与数据布局

仓库根目录下：

| 路径 | 用途 | 是否可配置 |
| --- | --- | --- |
| `backend/data/pdf_manager.db` | SQLite 数据库（文档、标签、评分、点击、配置覆盖项） | `DATABASE_PATH` / 配置面板（面板不暴露该路径） |
| `backend/data/pdf_manager.log` | 运行日志 | 跟随数据库所在目录 |
| `backend/data/webview2/` | WebView2 缓存与用户数据 | 跟随数据库所在目录 |
| `backend/uploads/` | 上传的 PDF 原件 | `UPLOAD_FOLDER` |
| `backend/output/` | 默认产物根目录（JSON / Markdown / 图片 / 导出 / 索引） | `OUTPUT_FOLDER` |
| `backend/temp/` | PDF 分割与解压临时文件 | `TEMP_FOLDER` |
| `E:\pdf_manager_output\md` | Markdown 产物 | `OUTPUT_MD_FOLDER` / 配置面板 |
| `E:\pdf_manager_output\json` | MinerU 原始 JSON | `OUTPUT_JSON_FOLDER` / 配置面板 |
| `E:\pdf_manager_output\images` | OCR 抽出的图片 | `IMAGE_FOLDER` / 配置面板 |
| `E:\pdf_manager_output\search_index` | Tantivy 检索索引 | `SEARCH_INDEX_FOLDER` / 配置面板 |
| `E:\pdf_manager_output\epub` | EPUB 与 Word 导出文件 | `EPUB_FOLDER` / 配置面板 |
| `E:\temp` | 临时分块与解压目录 | `TEMP_FOLDER` |

说明：

- 存在 `E:` 盘时，Markdown / JSON / 图片 / 索引 / 导出默认放在 `E:\pdf_manager_output\*`，临时目录用 `E:\temp`；没有 `E:` 盘则回退到 `backend/output` 下对应子目录。
- 仓库内存在 `backend/data` 时，数据库与上传、产物、临时目录默认沿用 `backend/` 下；数据库里保存的历史相对路径也按 `backend/` 解析。把 exe 放在项目根目录即可直接看到已有数据。
- 以上数据目录都在 `.gitignore` 中，不会进入版本库。

---

## 从源码构建

前置：

- Windows 10/11 x64
- Node.js 18+ 与 npm
- Rust 稳定版工具链（MSVC target）
- 可选：Windows SDK 的 `rc.exe`（用于把 `rust-backend/assets/app.ico` 编译进 exe；找不到时构建仍成功，只是没有 exe 图标，日志里会有 warning）

步骤：

```powershell
# 1. 前端构建（产物 frontend/build 会在下一步被内嵌进 exe）
cd frontend
npm install
npm run build

# 2. 后端 release 构建（必须在前端构建之后，否则内嵌的是旧前端）
cd ..\rust-backend
cargo build --release
```

产物：`rust-backend/target/release/pdf-manager.exe`。需要单文件分发时，把它复制到任意目录即可运行（数据目录按上方规则创建）。

### 一键构建（推荐）

```powershell
powershell -ExecutionPolicy Bypass -File .\build.ps1
```

脚本会按顺序完成：

1. 检查 `node` / `npm` / `cargo` 是否可用（缺失立即报错退出）；
2. 校验 `frontend/node_modules`，缺失或不完整时执行 `npm ci`（无 lock 文件则 `npm install`）；
3. 前端 production build，并确认 `frontend/build/index.html` 存在；
4. 触碰 `rust-backend/src/routes.rs` 后执行 `cargo build --release`，确保 rust-embed 内嵌的是最新前端；
5. 若 `PDFManager.exe` 正在运行：先请求关闭窗口，10 秒内未退出再强制结束；
6. 复制到根目录覆盖 `PDFManager.exe`（带重试，应对文件句柄释放延迟）。

任一步失败都会立即停止、以退出码 1 结束，并明确提示「根目录 PDFManager.exe 未被替换」；成功时打印产物路径、字节数、耗时与 SHA256。脚本可重复执行，日常增量构建同样适用。

### GitHub Actions 构建

工作流：[.github/workflows/build-exe.yml](.github/workflows/build-exe.yml)

- 触发：推送到 `main` / `master`、任意 Pull Request、手动 `workflow_dispatch`、以及 `v*` 标签推送；
- Runner：`windows-latest`；
- 缓存：npm（`frontend/package-lock.json`）与 Rust `rust-backend/target`；
- 图标：显式检查 Windows SDK 的 `rc.exe`，找不到直接失败（runner 自带 SDK，`build.rs` 会自动选最高版本）；
- 前端：`npm ci` + `npm run build`（设置 `CI=false`，避免 CRA 把 warning 当错误）；
- 后端：`cargo build --release --locked`；
- 构件：`PDFManager-windows-x64`，内含 `PDFManager.exe` 与 `PDFManager.exe.sha256`；推送 `v*` 标签时自动作为 Release 附件上传。

构建不读取 `.env`、数据库或 MinerU Token：`frontend/build` 与 `rust-backend/target` 都在仓库外缓存，CI 全部从源码生成；工作流不会向仓库推送任何内容。

开发态：

```powershell
# 终端 1：后端（同时提供 API 与内嵌前端静态文件）
cd rust-backend
cargo run --release

# 终端 2：前端热更新（http://localhost:3000，自动代理 /api 到 5000）
cd frontend
npm start
```

---

## 配置方式与优先级

优先级从高到低：

1. 配置面板保存的值（写入数据库 `app_meta`，键前缀 `settings.*`）
2. 进程环境变量
3. `.env` 文件（依次尝试 `./.env`、`./backend/.env`、exe 同目录及 exe 同目录下的 `backend/.env`；相对路径按该 `.env` 所在目录解析）
4. 内置默认值

常用环境变量：

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `PORT` / `FLASK_RUN_PORT` | `5000` | 监听端口（`FLASK_RUN_PORT` 优先） |
| `DATABASE_PATH` | `backend/data/pdf_manager.db`（无 `backend/data` 时为 `./data/pdf_manager.db`） | SQLite 路径 |
| `UPLOAD_FOLDER` | `backend/uploads` / `./uploads` | 上传目录 |
| `OUTPUT_FOLDER` | `backend/output` / `./output` | 产物根目录 |
| `OUTPUT_MD_FOLDER` / `OUTPUT_JSON_FOLDER` / `IMAGE_FOLDER` | 见目录表 | 产物子目录 |
| `SEARCH_INDEX_FOLDER` | `E:\pdf_manager_output\search_index` 或 `output/search_index` | 检索索引目录 |
| `EPUB_FOLDER` | `E:\pdf_manager_output\epub` 或 `output/epub` | EPUB / Word 导出目录 |
| `TEMP_FOLDER` | `E:\temp` 或 `backend/temp` / `./temp` | 临时目录 |
| `MINERU_TOKEN` | 空 | MinerU API Token |
| `MINERU_API_URL` | `https://mineru.net/api/v4` | MinerU 接口地址 |
| `MAX_FILE_SIZE_MB` | `200` | 单个 PDF 大小上限 |
| `MAX_PAGES` | `200` | 单个 PDF 页数上限 |
| `OCR_MAX_WORKERS` | `2` | OCR 并发文档数 |
| `OCR_DAILY_UPLOAD_LIMIT` | `5000` | 每日上传配额 |
| `OCR_UPLOAD_INTERVAL` | `65` | 上传间隔（秒） |
| `MINERU_POLL_TIMEOUT` / `MINERU_POLL_INTERVAL` | `21600` / `8` | 轮询超时（秒）/ 间隔（秒） |
| `BATCH_DOC_LIMIT` / `BATCH_CHUNK_LIMIT` | `30` / `48` | 批处理文档数 / 分块数上限 |
| `SCHEDULER_SORT_MODE` | `shortest_first` | 调度顺序（`shortest_first` / `id_asc`） |
| `OPEN_BROWSER` | `0` | 设为 `1` 时额外打开系统浏览器 |
| `CORS_ORIGINS` | `http://localhost:3000,http://127.0.0.1:3000` | 允许的跨域来源 |

配置面板可调整：MinerU Token / 接口地址、图片 / JSON / Markdown / 检索索引 / 导出目录、OCR 配额与间隔、并发与批量上限、轮询与重试参数、调度排序、繁→简映射。

生效时机：

- 目录类与业务参数保存后对之后开始的任务生效，运行中的任务沿用自身快照；
- 检索索引目录变更后旧索引不可直接沿用，需要在界面中重建索引；
- 端口、数据库路径、上传 / 产物根目录等启动期参数修改后需要重启程序。

---

## 已知限制

- 预览模式的正则替换匹配的是 marked 渲染后的文本节点：`#` 等 Markdown 标记不存在于其中，`^` / `$` 只在单个文本节点内生效；需要按源码逐行匹配时请切到「编辑」或「实时预览」。
- `exact` 检索最多校验 500 个候选，超出时返回 `truncated=true`，此时 `total` 只代表已校验范围；`smart` 模式无此限制但依赖分词质量。
- 检索索引更换目录后必须重建；索引损坏或 schema 版本不兼容时启动会自动隔离旧目录并全量重建，期间检索不可用。
- 每页 1000 条会渲染上千行，首次切换约需 2–3 秒（已有加载提示），批量勾选也会整页重绘，日常建议 10–100 条/页。
- 导出 EPUB / Word 时，外链图片与 `data:` 图片不会内嵌；本地图片缺失会在正文占位并在文末列出。
- 单文件 exe 内嵌的是构建时的前端产物，改前端后必须重新 `npm run build` 再 `cargo build --release`。
