# PDF文件管理系统 - 技术文档

## 目录

1. [系统架构](#1-系统架构)
2. [后端核心模块](#2-后端核心模块)
3. [PDF处理流程](#3-pdf处理流程)
4. [OCR处理流程](#4-ocr处理流程)
5. [数据库设计](#5-数据库设计)
6. [API接口详解](#6-api接口详解)

---

## 1. 系统架构

### 1.1 整体架构

```
┌─────────────────────────────────────────────────────────┐
│                    前端 (React + Ant Design)             │
│  ┌───────────────────────────────────────────────────┐  │
│  │  UI组件: 文件列表、扫描目录、标签管理、OCR控制   │  │
│  └───────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────┘
                           ↕ HTTP REST API
┌─────────────────────────────────────────────────────────┐
│                   后端 (Flask + SQLite)                   │
│  ┌──────────────┐  ┌──────────────┐  ┌────────────────┐ │
│  │ API路由层    │→ │ 业务逻辑层   │→ │ 数据访问层(DAO)│ │
│  │ (api.py)     │  │              │  │ (dao.py)       │ │
│  └──────────────┘  └──────────────┘  └────────────────┘ │
│  ┌───────────────────────────────────────────────────┐  │
│  │ 工具模块: pdf_utils, mineru_api, json_to_md       │  │
│  └───────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────┘
                           ↕ HTTP
┌─────────────────────────────────────────────────────────┐
│                    MinerU API (OCR服务)                  │
└─────────────────────────────────────────────────────────┘
```

### 1.2 目录结构说明

```
backend/
├── api.py           # Flask API路由 - 处理所有HTTP请求
├── config.py        # 配置管理 - 加载.env文件和环境变量
├── dao.py           # 数据访问层 - 封装所有数据库操作
├── json_to_md.py    # JSON转Markdown工具 - 保留脚注信息
├── main.py          # 启动入口 - 初始化Flask应用
├── mineru_api.py    # MinerU API封装 - OCR服务调用
├── models.py        # 数据库模型 - 定义表结构
├── pdf_utils.py     # PDF处理工具 - 分割、信息提取
├── requirements.txt # Python依赖
└── .env            # 环境变量配置

data/               # SQLite数据库文件目录
uploads/           # 上传的PDF文件目录
temp/              # 分割后的临时PDF文件
output/            # OCR结果(JSON + Markdown)
```

---

## 2. 后端核心模块

### 2.1 config.py - 配置管理

**作用**：统一管理系统配置，从.env文件加载环境变量

**核心配置项**：

```python
MINERU_TOKEN          # MinerU API认证令牌
MINERU_API_URL       # MinerU API地址
MAX_FILE_SIZE_MB     # 单个文件最大大小(MB)，默认200
MAX_PAGES            # 单个文件最大页数，默认200
UPLOAD_FOLDER        # 上传文件存储目录
OUTPUT_FOLDER        # OCR输出目录
TEMP_FOLDER          # 临时文件目录
```

**MAX_SIZE_BYTES计算**：
```
MAX_SIZE_BYTES = MAX_FILE_SIZE_MB × 1024 × 1024
```

---

### 2.2 pdf_utils.py - PDF处理工具

#### 2.2.1 get_pdf_info(pdf_path)

**功能**：获取PDF文件的基本信息

**处理流程**：
1. 使用 `os.path.getsize()` 获取文件大小(字节)
2. 转换为MB单位便于显示
3. 使用 PyMuPDF (fitz) 打开PDF文件
4. 读取 `doc.page_count` 获取总页数
5. 返回 `(page_count, file_size_mb, file_size_bytes)`

**异常处理**：捕获所有异常，返回 `(0, 0.0, 0)`

---

#### 2.2.2 calculate_split_pages(page_count, file_size_bytes)

**功能**：智能计算PDF分割页数

**算法逻辑**：

```
1. 检查是否需要分割:
   - 如果 页数 ≤ MAX_PAGES 且 文件大小 ≤ MAX_SIZE_BYTES
   → 不需要分割，返回 None

2. 如果需要分割:
   a. 计算 平均每页大小 = file_size_bytes / page_count
   
   b. 计算 200页的预估大小 = avg_size_per_page × 200
   
   c. 判断:
      → 如果 200页预估大小 > 200MB
        • 按 180MB (MAX_SIZE_BYTES × 0.9) 计算可容纳页数
        • split_pages = 180MB / avg_size_per_page
      
      → 否则
        • 按 200页 分割
        • split_pages = 200
   
   d. 确保 split_pages ≥ 1
```

**示例**：

| 情况 | 页数 | 文件大小 | 平均每页 | 200页预估 | 分割策略 |
|------|------|----------|----------|-----------|----------|
| 小文件 | 150页 | 100MB | 0.67MB | 134MB | 不分割 |
| 页数多 | 300页 | 150MB | 0.5MB | 100MB | 按200页分割 → 2个文件 |
| 页很大 | 250页 | 300MB | 1.2MB | 240MB | 按180MB计算 → 150页/个 |

---

#### 2.2.3 split_pdf(pdf_path, document_id)

**功能**：将PDF文件分割成多个小文件

**处理流程**：

1. **准备阶段**：
   - 调用 `get_pdf_info()` 获取PDF信息
   - 调用 `calculate_split_pages()` 确定分割策略

2. **如果不需要分割**：
   - 返回原文件信息，不进行分割

3. **如果需要分割**：
   - 使用 PyMuPDF 打开原PDF
   - 计算需要分割的块数 = `ceil(总页数 / 每页块数)`
   - 循环创建每个分割块：
     a. 计算当前块的起始和结束页码
     b. 创建新的空PDF文档
     c. 从原PDF中插入指定范围的页面
     d. 保存到 `temp/` 目录，文件名格式：`原文件名_文档ID_起始页_结束页.pdf`
     e. 记录该块的信息（文件名、路径、页码范围）

4. **返回所有分割块信息列表**

**文件名格式**：
```
MyDocument_5_1_200.pdf
├─ 原文件名: MyDocument
├─ 文档ID: 5
├─ 起始页: 1
└─ 结束页: 200
```

---

### 2.3 mineru_api.py - MinerU API封装

**类**：`MineruAPI`

#### 2.3.1 request_upload_urls(files_info)

**功能**：向MinerU请求文件上传的预签名URL

**请求参数**：
```python
{
    "files": [
        {"name": "file1.pdf", "data_id": "uuid1"},
        {"name": "file2.pdf", "data_id": "uuid2"}
    ],
    "model_version": "vlm"
}
```

**返回数据**：
```python
{
    "batch_id": "batch-uuid",
    "file_urls": [
        "https://mineru.net/upload-url1",
        "https://mineru.net/upload-url2"
    ]
}
```

**重试机制**：使用 `@retry` 装饰器，失败后自动重试3次

---

#### 2.3.2 upload_file(file_path, upload_url)

**功能**：将PDF文件上传到预签名URL

**处理流程**：
1. 以二进制模式打开本地PDF文件
2. 使用 PUT 请求上传到MinerU的预签名URL
3. 上传成功返回，失败抛出异常

---

#### 2.3.3 submit_task(batch_id)

**功能**：提交OCR处理任务

**请求参数**：
```python
{
    "batch_id": "batch-uuid",
    "response_type": "json"  # 关键！请求JSON格式保留脚注
}
```

**返回数据**：
```python
{
    "task_id": "task-uuid"
}
```

---

#### 2.3.4 check_task_status(task_id)

**功能**：轮询检查OCR任务状态

**处理逻辑**：
- 每隔10秒查询一次任务状态
- 使用 `@retry` 重试10次（约100秒超时）
- 当状态为 "completed" 或 "failed" 时返回

**任务状态**：
| 状态 | 说明 |
|------|------|
| pending | 任务排队中 |
| processing | OCR处理中 |
| completed | 处理完成 |
| failed | 处理失败 |

---

#### 2.3.5 download_result(download_url, save_path)

**功能**：下载OCR结果（JSON格式）

**处理流程**：
1. 发送GET请求到下载URL
2. 将响应内容写入本地JSON文件
3. 保存到 `output/` 目录

---

#### 2.3.6 process_files(file_paths) - 主流程

**完整OCR处理流程**：

```
1. 为每个文件生成唯一 data_id (uuid4)

2. 请求上传URLs (request_upload_urls)
   ↓
3. 逐个上传文件 (upload_file)
   - 每次上传后等待 0.5秒 (避免频率限制)
   ↓
4. 提交OCR任务 (submit_task)
   ↓
5. 轮询任务状态 (check_task_status)
   - 等待直到 completed 或 failed
   ↓
6. 下载所有JSON结果 (download_result)
   ↓
7. 返回结果列表:
   [
     {data_id, json_path, filename},
     ...
   ]
```

---

### 2.4 json_to_md.py - JSON转Markdown

#### 2.4.1 核心设计

**为什么用JSON而不是直接获取Markdown？**

MinerU的直接Markdown输出会丢失脚注信息。而JSON格式包含完整的文档结构，包括：
- 标题块 (title)
- 正文块 (text)
- 列表块 (list)
- 页脚块 (page_footnote) ← 关键！

#### 2.4.2 convert_json_to_markdown(input_json, output_md)

**功能**：将单个JSON文件转换为Markdown

**处理流程**：

1. **加载JSON数据**
2. **提取所有页面并按页码排序**
3. **遍历每个页面**：
   a. 提取该页的正文块，按顺序处理
   b. 提取该页的废弃块（可能包含脚注），按顺序处理
   c. 根据块类型转换为Markdown格式：
      - title: `# 标题内容`
      - text: 直接追加文本
      - list: `- 列表项`
      - page_footnote: 直接追加脚注文本
   d. 如果当前页包含脚注，添加分割线 `---`

4. **保存Markdown到文件**

#### 2.4.3 merge_json_to_markdown(json_paths, output_md)

**功能**：合并多个JSON文件（来自分割的PDF）并转换为单个Markdown

**处理流程**：

1. 遍历所有JSON文件，提取所有页面数据
2. 按 `page_idx` 对所有页面进行排序（确保页码正确）
3. 后续步骤同 `convert_json_to_markdown()`

---

### 2.5 models.py - 数据库模型

#### 2.5.1 PDFDocument

**表名**：`pdf_documents`

**字段**：

| 字段 | 类型 | 说明 |
|------|------|------|
| id | Integer | 主键，自增 |
| filename | String | 原始文件名 |
| original_path | String | 文件完整路径 |
| file_size | Integer | 文件大小(字节) |
| page_count | Integer | PDF总页数 |
| status | String | 处理状态 |
| created_at | DateTime | 创建时间 |
| updated_at | DateTime | 更新时间 |
| markdown_path | String | 生成的Markdown路径 |

**状态值**：

| 状态 | 说明 |
|------|------|
| uploaded | 文件已上传/扫描 |
| splitting | PDF分割中 |
| ocr_processing | OCR处理中 |
| completed | 处理完成 |
| failed | 处理失败 |

**关系**：
- 多个标签 (多对多)
- 多个PDFChunk (一对多)

---

#### 2.5.2 PDFChunk

**表名**：`pdf_chunks`

**字段**：

| 字段 | 类型 | 说明 |
|------|------|------|
| id | Integer | 主键，自增 |
| document_id | Integer | 关联的文档ID |
| filename | String | 分割块文件名 |
| chunk_path | String | 分割块文件路径 |
| start_page | Integer | 起始页码 |
| end_page | Integer | 结束页码 |
| ocr_status | String | OCR状态 |
| json_path | String | OCR结果JSON路径 |

**关系**：
- 一个PDFDocument (多对一)

---

#### 2.5.3 Tag

**表名**：`tags`

**字段**：

| 字段 | 类型 | 说明 |
|------|------|------|
| id | Integer | 主键，自增 |
| name | String | 标签名称，唯一 |
| color | String | 标签颜色，默认 #1890ff |

**关系**：
- 多个文档 (多对多)

---

#### 2.5.4 DocumentTag

**表名**：`document_tags`

**作用**：PDFDocument和Tag的多对多关联表

**字段**：

| 字段 | 类型 | 说明 |
|------|------|------|
| document_id | Integer | 文档ID，主键 |
| tag_id | Integer | 标签ID，主键 |

---

### 2.6 dao.py - 数据访问层

**设计原则**：所有数据库操作封装在DAO类中，业务逻辑不直接操作SQL

#### 2.6.1 PDFDAO

| 方法 | 说明 |
|------|------|
| add_document(filename, path, size, pages) | 新增文档记录 |
| get_document(id) | 根据ID获取文档 |
| get_all_documents() | 获取所有文档（按创建时间倒序） |
| update_document_status(id, status) | 更新文档状态 |
| update_document_markdown_path(id, path) | 更新Markdown路径 |
| delete_document(id) | 删除文档记录 |
| add_chunk(doc_id, filename, path, start, end) | 新增分割块记录 |
| get_chunks_by_document(doc_id) | 获取文档的所有分割块 |
| update_chunk_ocr_status(chunk_id, status, json_path) | 更新分割块OCR状态 |

---

#### 2.6.2 TagDAO

| 方法 | 说明 |
|------|------|
| add_tag(name, color) | 新增标签（如果不存在） |
| get_tag(id) | 根据ID获取标签 |
| get_tag_by_name(name) | 根据名称获取标签 |
| get_all_tags() | 获取所有标签 |
| delete_tag(id) | 删除标签 |
| add_tag_to_document(doc_id, tag_id) | 为文档添加标签 |
| remove_tag_from_document(doc_id, tag_id) | 移除文档标签 |

---

## 3. PDF处理流程

### 3.1 扫描目录流程

```
用户点击"扫描目录"
    ↓
前端发送 POST /api/documents/scan {directory: "D:\\PDFs"}
    ↓
后端处理 (api.py):
1. 验证目录是否存在
2. 递归遍历目录，查找所有 .pdf 文件
3. 查询数据库，获取已存在的文档路径
4. 遍历找到的PDF文件:
   a. 如果路径已存在 → 跳过
   b. 如果PDF无法读取 → 跳过
   c. 否则 → 调用 PDFDAO.add_document() 新增记录
5. 返回统计结果: {added, skipped, total_found}
    ↓
前端更新文档列表
```

---

### 3.2 单个文档OCR处理流程

```
用户点击"OCR"按钮
    ↓
前端发送 POST /api/documents/{id}/ocr
    ↓
【步骤1: 分割PDF】
- PDFDAO.update_document_status(id, "splitting")
- split_pdf() 分割PDF
- 保存每个分割块到 PDFChunk 表
    ↓
【步骤2: OCR处理】
- PDFDAO.update_document_status(id, "ocr_processing")
- MineruAPI.process_files() 处理所有分割块
  ├─ 请求上传URL
  ├─ 上传文件
  ├─ 提交任务
  ├─ 轮询状态
  └─ 下载JSON结果
- 更新 PDFChunk 的 ocr_status 和 json_path
    ↓
【步骤3: 合并结果】
- 收集所有JSON文件路径
- 如果只有1个 → convert_json_to_markdown()
- 如果多个 → merge_json_to_markdown()
- 保存Markdown到 output/ 目录
    ↓
【步骤4: 完成】
- PDFDAO.update_document_markdown_path()
- PDFDAO.update_document_status(id, "completed")
- 返回成功响应
```

---

### 3.3 批量OCR处理流程

与单个文档OCR类似，但是循环处理多个文档ID

---

## 4. OCR处理流程（详细）

### 4.1 MinerU API完整交互序列图

```
[前端]               [后端Flask]        [MinerU API]
   │                     │                   │
   │  POST /ocr        │                   │
   │────────────────────>│                   │
   │                     │                   │
   │                     │  POST /file-urls/batch
   │                     │──────────────────>│
   │                     │                   │
   │                     │<─ batch_id, urls──│
   │                     │                   │
   │                     │  PUT (upload)     │
   │                     │──────────────────>│
   │                     │                   │
   │                     │<── OK ───────────│
   │                     │  (重复上传)       │
   │                     │──────────────────>│
   │                     │                   │
   │                     │<── OK ───────────│
   │                     │                   │
   │                     │  POST /tasks      │
   │                     │──────────────────>│
   │                     │                   │
   │                     │<── task_id ──────│
   │                     │                   │
   │                     │  GET /tasks/{id}  │
   │                     │──────────────────>│
   │                     │                   │
   │                     │<── pending ──────│
   │                     │  (轮询)           │
   │                     │──────────────────>│
   │                     │                   │
   │                     │<── pending ──────│
   │                     │  (轮询)           │
   │                     │──────────────────>│
   │                     │                   │
   │                     │<── completed + urls
   │                     │                   │
   │                     │  GET (download)   │
   │                     │──────────────────>│
   │                     │                   │
   │                     │<── JSON ─────────│
   │                     │  (重复下载)       │
   │                     │──────────────────>│
   │                     │                   │
   │                     │<── JSON ─────────│
   │                     │                   │
   │<── success ────────│                   │
```

---

### 4.2 分割PDF的示例

假设有一个PDF文件：
- 文件名：`DeepLearning.pdf`
- 页数：350页
- 文件大小：250MB

**计算过程**：
1. 平均每页大小 = 250MB / 350 ≈ 0.714MB
2. 200页预估大小 = 0.714 × 200 = 142.8MB < 200MB
3. 按200页分割

**分割结果**：
```
DeepLearning_1_1_200.pdf  (200页)
DeepLearning_1_201_350.pdf (150页)
```

---

## 5. 数据库设计

### 5.1 ER图

```
┌─────────────────┐         ┌─────────────────┐
│  PDFDocument    │         │      Tag        │
├─────────────────┤         ├─────────────────┤
│ id (PK)         │         │ id (PK)         │
│ filename        │         │ name (UK)       │
│ original_path   │         │ color           │
│ file_size       │         └─────────────────┘
│ page_count      │              ↑
│ status          │              │
│ created_at      │              │
│ updated_at      │              │
│ markdown_path   │              │
└─────────────────┘              │
        ↑                        │
        │                        │
        │                        │
        │              ┌─────────────────┐
        │              │  DocumentTag    │
        │              ├─────────────────┤
        │              │ document_id(PK) │
        │              │ tag_id (PK)     │
        │              └─────────────────┘
        │
        │
┌─────────────────┐
│    PDFChunk     │
├─────────────────┤
│ id (PK)         │
│ document_id(FK) │
│ filename        │
│ chunk_path      │
│ start_page      │
│ end_page        │
│ ocr_status      │
│ json_path       │
└─────────────────┘
```

---

## 6. API接口详解

### 6.1 文档管理

#### 6.1.1 GET /api/documents

**功能**：获取所有文档列表

**响应**：
```json
[
  {
    "id": 1,
    "filename": "DeepLearning.pdf",
    "file_size": 262144000,
    "page_count": 350,
    "status": "completed",
    "created_at": "2026-06-06T10:30:00",
    "updated_at": "2026-06-06T10:35:00",
    "markdown_path": "output/DeepLearning.md",
    "tags": [
      {"id": 1, "name": "AI", "color": "#1890ff"}
    ]
  }
]
```

---

#### 6.1.2 POST /api/documents/scan

**功能**：扫描目录发现PDF文件

**请求**：
```json
{
  "directory": "D:\\Documents\\PDFs"
}
```

**响应**：
```json
{
  "message": "扫描完成",
  "added": 5,
  "skipped": 3,
  "total_found": 8
}
```

---

#### 6.1.3 POST /api/documents

**功能**：上传PDF文件

**请求**：`multipart/form-data`，包含 `file` 字段

**响应**：
```json
{
  "id": 1,
  "message": "文件上传成功"
}
```

---

#### 6.1.4 POST /api/documents/{id}/ocr

**功能**：对单个文档进行OCR处理

**响应**：
```json
{
  "message": "OCR处理完成",
  "markdown_path": "output/DeepLearning.md"
}
```

---

#### 6.1.5 POST /api/documents/batch/ocr

**功能**：批量OCR处理

**请求**：
```json
{
  "doc_ids": [1, 2, 3]
}
```

**响应**：
```json
[
  {"doc_id": 1, "status": "success", "message": "OCR处理完成"},
  {"doc_id": 2, "status": "failed", "message": "API调用失败"},
  {"doc_id": 3, "status": "success", "message": "OCR处理完成"}
]
```

---

#### 6.1.6 DELETE /api/documents/{id}

**功能**：删除文档及其相关文件

**响应**：
```json
{
  "message": "删除成功"
}
```

**删除的文件**：
- 原始PDF文件
- Markdown文件
- 所有分割的PDF文件
- 所有JSON结果文件

---

### 6.2 标签管理

#### 6.2.1 GET /api/tags

**功能**：获取所有标签

**响应**：
```json
[
  {"id": 1, "name": "AI", "color": "#1890ff"},
  {"id": 2, "name": "编程", "color": "#52c41a"}
]
```

---

#### 6.2.2 POST /api/tags

**功能**：创建新标签

**请求**：
```json
{
  "name": "技术",
  "color": "#1890ff"
}
```

---

#### 6.2.3 POST /api/documents/{doc_id}/tags

**功能**：为文档添加标签（如果标签不存在则自动创建）

**请求**：
```json
{
  "tag_name": "AI"
}
```

---

#### 6.2.4 DELETE /api/documents/{doc_id}/tags/{tag_id}

**功能**：移除文档的标签

---

## 7. 配置调优

### 7.1 修改文件大小限制

编辑 `backend/.env`：
```env
MAX_FILE_SIZE_MB=300  # 修改为300MB
MAX_PAGES=300         # 修改为300页
```

### 7.2 调整重试策略

编辑 `backend/mineru_api.py` 中的 `@retry` 参数：
```python
@retry(tries=5, delay=3, backoff=2)  # 5次重试，初始3秒，指数退避
```

---

## 8. 错误处理

| 错误场景 | 处理方式 |
|----------|----------|
| PDF文件损坏 | 跳过该文件，记录日志 |
| MinerU API超时 | 自动重试3次，失败标记为failed |
| 文件已存在 | 跳过不重复添加 |
| 目录不存在 | 返回400错误 |
