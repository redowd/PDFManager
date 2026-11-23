# MinerU 异步解耦流水线架构设计与接口说明

本文档详细说明在 `pdf_manager` 项目中实现的 **MinerU 异步解耦双流水线架构（Submitter-Harvester Decoupled Pipeline）**。

---

## 一、架构设计背景与目标

### 1. 传统微批架构的痛点
在传统的串行批处理模型中，处理流程为：
`打包切块 (本地) -> 上传云端 (网络) -> 等待解析完成 (云端排队+计算) -> 下载合成落盘 (本地)`。
这种模式存在三个无法回避的瓶颈：
- **流水线空转**：在等待云端解析的 10~25 分钟内，本地完全空闲，没有向云端输送新任务，云端算力利用率低下。
- **长尾木桶效应**：一个批次只要有任意一本大部头慢书，整批所有已经解析好的短文档都无法落盘，前端状态长期卡在 40%。
- **磁盘膨胀风险**：如果在本地提前生成大量切片，本地磁盘空间会迅速被几十 GB 的临时 PDF 占满。

### 2. 解耦架构设计目标
- **吞吐最大化**：严格遵守官方频控红线，将云端排队队列持续塞满。
- **零磁盘堆积**：实现“即传即焚”，本地磁盘峰值永久锁定在 2GB 以内。
- **高韧性断点续跑**：批次状态完全持久化于 SQLite，随时关机重启均可自动恢复。

---

## 二、双流水线系统架构图

```text
               ┌─────────────────────────────────────┐
               │    SQLite Database (pdf_documents)  │
               │    状态: uploaded & ocr_disabled=0   │
               └──────────────────┬──────────────────┘
                                  │
                                  ▼
┌────────────────────────────────────────────────────────────────────────┐
│ 线程 1: 异步投递生产者 (Submitter Pipeline)                              │
│                                                                        │
│ 1. 贪心聚合待处理文档 (最多 30 本 / 48 个 chunks，紧贴官方单批 50 上限)      │
│ 2. 按页数升序调度 (shortest_first 优先)，提高批次密度                    │
│ 3. 调用 MinerU /file-urls/batch 申请批次预签名链接                     │
│ 4. 逐文件并发推送至阿里云 OSS                                           │
│ 5. ★ 核心保障 [即传即焚]：文件上传 OSS 成功后，立刻删除本地临时切片！     │
│ 6. 将 batch_id 与关联 doc_ids 写入 ocr_batches 表持久化                 │
│ 7. 安全限速：每批投递完成后休眠 65 秒 (严格低于 50 文件/分钟 频控)       │
│ 8. 日限额看门狗：累计上传接近 5,000 文件/天时自动安全休眠                │
└─────────────────────────────────┬──────────────────────────────────────┘
                                  │
                                  ▼
           ┌──────────────────────────────────────────────┐
           │      MinerU Cloud Cluster (分布式 GPU 集群)   │
           │      - 多批次并在云端并行排队与 VLM 大模型推理 │
           └──────────────────────┬───────────────────────┘
                                  │
                                  ▼
┌────────────────────────────────────────────────────────────────────────┐
│ 线程 2: 异步收割消费者 (Harvester Pipeline)                              │
│                                                                        │
│ 1. 独立循环：每 15 秒非阻塞扫描 ocr_batches 表中未完成的批次            │
│    (查询频次 4 次/分钟，远低于官方 1,000 次/分钟 查询配额)              │
│ 2. 调用 /extract-results/batch/{id} 单次探活                            │
│ 3. 发现批次进入终态 (done / failed)：                                  │
│    - 下载对应 full_zip_url 压缩包                                      │
│    - 提取 middle.json 结构化结果                                       │
│    - [单文件隔离]：某文件失败不影响同批次其他文件                       │
│    - 逐本合成最终 Markdown 并落盘至 backend/output/md/                 │
│    - 标记文档为 completed (100%) 并更新 ocr_batches 表为 completed     │
└────────────────────────────────────────────────────────────────────────┘
```

---

## 三、关键机制与安全防线

### 1. 官方频控与限流对齐
| 官方限制指标 | 官方红线 | 本架构实际参数 | 安全状态 |
| :--- | :--- | :--- | :--- |
| **单批申请文件数** | ≤ 50 个 / 批 | `BATCH_CHUNK_LIMIT = 48` | 安全 (留 2 个容错冗余) |
| **任务提交频控** | ≤ 50 个 / 分钟 | 每批 ≤ 48 块，批次间隔安全休眠 **65 秒** | 绝对安全 (永远不超速) |
| **结果查询频控** | ≤ 1,000 次 / 分钟 | 巡检间隔 **15 秒 / 次** (约 4 次/分钟) | 极度安全 (仅占配额 0.4%) |
| **单日上传总量** | ≤ 5,000 文件 / 天 | Submitter 实时累计，达 4,950 自动休眠 | 具备日限额熔断保护 |
| **单文件页数** | ≤ 200 页 | `MAX_PAGES = 200` 自动拆分 | 100% 合规 |

### 2. “即传即焚” 零磁盘积压机制
- **原理**：PDF 分块生成在 `backend/temp/` 目录下。分块通过 HTTP PUT 成功传至阿里云 OSS 的瞬间，代码立刻调用 `os.remove(file_path)`。
- **收益**：即使云端当前堆叠了 10 个批次（近 500 个文件在云端排队解析），本地 `temp/` 目录的体积也几乎为 0，彻底根除了本地 43GB 剩余磁盘爆满的风险。

### 3. 断点续跑与宕机恢复
- **批次持久化**：所有向 MinerU 投递成功的任务，其 `batch_id`、关联 `doc_ids` 及映射信息均实时写入数据库 `ocr_batches` 与 `app_meta`。
- **服务重启自愈**：后端启动时会自动拉起 Harvester 线程，继续监听此前已在云端排队的批次；Submitter 则会无缝从数据库拉取剩余的 `uploaded` 文档继续投递。

---

## 四、核心数据模型 (Database Schema)

### `ocr_batches` 表
记录投递给 MinerU 的异步批次生命周期。

```sql
CREATE TABLE ocr_batches (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    batch_id VARCHAR(100) UNIQUE NOT NULL,      -- MinerU 返回的 batch_id
    status VARCHAR(50) DEFAULT 'processing',    -- processing / completed / failed
    doc_ids_json TEXT NOT NULL,                 -- 本批包含的文档ID数组，如 "[40, 42, 49]"
    file_count INTEGER DEFAULT 0,               -- 本批包含的分块文件数量
    error_message VARCHAR(500),                 -- 批次级错误信息
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX ix_ocr_batches_batch_id ON ocr_batches (batch_id);
CREATE INDEX ix_ocr_batches_status ON ocr_batches (status);
```

---

## 五、API 接口说明

### 1. 获取解耦流水线状态
- **URL**: `GET /api/scheduler/status`
- **说明**: 获取当前流水线的投递状态、活跃批次、并发数及今日已上传量。
- **响应示例**:
  ```json
  {
    "active_batches": 2,
    "active_workers": 60,
    "daily_limit": 5000,
    "enabled": true,
    "mode": "producer_consumer_pipeline",
    "pending_count": 17539,
    "running": true,
    "today_uploaded_files": 48
  }
  ```
- **字段释义**:
  - `running`: 流水线后台线程（Submitter / Harvester）是否处于活跃状态。
  - `enabled`: 持久化开关状态（决定重启后是否自动恢复）。
  - `active_batches`: 当前在 MinerU 云端正在解析、等待收割的批次数。
  - `active_workers`: 当前处于 `ocr_processing` 状态的图书文档总数。
  - `today_uploaded_files`: 今天累计向 MinerU 推送的分块文件数（对齐 5000 限制）。
  - `pending_count`: 数据库中剩余待处理的有效文档数。

### 2. 启动流水线
- **URL**: `POST /api/scheduler/start`
- **说明**: 开启异步流水线，后台将自动启动 Submitter 和 Harvester 守护线程，并持久化该状态。
- **响应示例**:
  ```json
  {
    "message": "解耦流水线已启动",
    "enabled": true,
    "running": true,
    "mode": "producer_consumer_pipeline"
  }
  ```

### 3. 暂停流水线
- **URL**: `POST /api/scheduler/stop`
- **说明**: 发送停止信号。Submitter 停止投递新任务；Harvester 会继续运行直至把云端已存在的批次收割落盘完毕。
- **响应示例**:
  ```json
  {
    "message": "解耦流水线已暂停",
    "enabled": false,
    "running": false
  }
  ```

---

## 六、配置参数说明 (`backend/config.py` 与 `.env`)

| 参数名 | 默认值 | 作用与推荐设置 |
| :--- | :--- | :--- |
| `BATCH_DOC_LIMIT` | `30` | 单批次聚合的最大图书数量上限。小文档多时可提高聚合度。 |
| `BATCH_CHUNK_LIMIT` | `48` | 单批次聚合的切片分块数上限。**切勿超过 48**（官方单批硬上限为 50）。 |
| `SCHEDULER_SORT_MODE` | `shortest_first` | 调度排序模式：`shortest_first`（优先短文档，极速落盘高周转）；`id_asc`（按导入顺序）。 |
| `MINERU_POLL_TIMEOUT` | `21600` (6小时) | 单批次最长轮询容忍超时（秒），适应超额后公用闲时算力排队。 |
| `MINERU_POLL_INTERVAL` | `8` | 批次内部轮询基础间隔（秒）。 |
