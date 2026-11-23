# PDF文件管理系统 - 前端文档

## 目录

1. [前端概述](#1-前端概述)
2. [应用入口 index.js](#2-应用入口-indexjs)
3. [主页面：文档管理工作台（App.js）](#3-主页面文档管理工作台appjs)
4. [功能模块详解](#4-功能模块详解)
5. [弹窗视图：添加标签](#5-弹窗视图添加标签)
6. [弹窗视图：扫描目录](#6-弹窗视图扫描目录)
7. [文档表格列定义](#7-文档表格列定义)
8. [状态管理（useState）](#8-状态管理usestate)
9. [前端调用的 API 清单](#9-前端调用的-api-清单)
10. [前端数据结构](#10-前端数据结构)
11. [已知问题与代码观察](#11-已知问题与代码观察)

---

## 1. 前端概述

### 1.1 技术栈

| 技术 | 版本 | 用途 |
|------|------|------|
| React | ^18.2.0 | UI 框架（函数组件 + Hooks） |
| react-dom | ^18.2.0 | DOM 渲染 |
| react-scripts | 5.0.1 | 脚手架（CRA，启动/构建） |
| antd | ^5.11.5 | UI 组件库（Layout / Table / Upload / Modal / Tag 等） |
| @ant-design/icons | - | 图标 |
| axios | ^1.6.2 | HTTP 请求 |

### 1.2 目录结构

```
frontend/
├── package.json          # 依赖与脚本；proxy 代理到后端 http://localhost:5000
├── package-lock.json
├── public/
│   └── index.html        # HTML 模板（标题：PDF文件管理系统，挂载点 #root）
└── src/
    ├── index.js          # 应用入口：创建 React 根节点并渲染 <App/>
    └── App.js            # 唯一页面组件：文档管理工作台（含全部业务逻辑与两个弹窗）
```

### 1.3 页面清单

前端为**单页应用（SPA）**，未引入路由（react-router），整个应用只有 1 个页面 + 2 个模态弹窗（作为子视图）：

| 视图 | 文件 | 说明 |
|------|------|------|
| 文档管理工作台（主页面） | `src/App.js` | 顶部工具栏 + 文档列表表格，承载全部业务操作 |
| 添加标签弹窗 | `src/App.js`（Modal） | 为指定文档输入/选择标签 |
| 扫描目录弹窗 | `src/App.js`（Modal） | 输入本地目录路径，批量导入 PDF |

### 1.4 开发代理

`package.json` 中配置了 `"proxy": "http://localhost:5000"`，开发环境下所有 `/api/*` 请求会被代理到 Flask 后端，因此前端代码中直接使用相对路径 `/api/...`。

---

## 2. 应用入口 index.js

文件：`frontend/src/index.js`

负责把 React 应用挂载到 `public/index.html` 的 `#root` 节点：

- 使用 React 18 的 `createRoot` API；
- 包裹 `<React.StrictMode>`（开发期额外检查，渲染两次以暴露副作用问题）；
- 渲染唯一的 `<App />` 组件。

```jsx
const root = ReactDOM.createRoot(document.getElementById('root'));
root.render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
```

---

## 3. 主页面：文档管理工作台（App.js）

文件：`frontend/src/App.js`

`App` 是整个前端唯一的页面组件，采用 antd 的 `Layout` 布局，由两部分组成：

```
┌──────────────────────────────────────────────────────────────┐
│ Header（顶部工具栏）                                          │
│  PDF文件管理系统        [刷新][扫描目录][上传PDF][批量OCR][批量删除] │
├──────────────────────────────────────────────────────────────┤
│ Content（内容区）                                             │
│  [🔍 搜索文件名或标签...（输入即过滤）]                        │
│  ┌────────────────────────────────────────────────────────┐  │
│  │ Table 文档列表（分页，每页10条）                          │  │
│  │ ☑ | 文件名 | 大小 | 页数 | 状态 | 标签 | 操作           │  │
│  │ ☑ | ...                              [OCR][MD路径][打标][删除] │
│  └────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────┘

弹窗1：添加标签（Modal）   弹窗2：扫描目录（Modal，含目录浏览器）
```

> 注：操作列的「MD路径」按钮仅在文档已生成 Markdown（`markdown_path` 非空，即 OCR 完成）时显示。

- **Header**：左侧为系统标题，右侧为 5 个操作按钮（刷新、扫描目录、上传 PDF、批量 OCR、批量删除），各按钮带独立 loading 态。
- **Content**：顶部为搜索框（按文件名/标签即时本地过滤，见 4.12）；下方为 antd `Table`，展示过滤后的 PDF 文档，支持多选（全选跟随搜索结果）、分页（前端分页，每页 10 条）、边框、加载遮罩（仅数据加载时显示）。

页面在挂载时（`useEffect`）自动拉取一次「文档列表」和「标签列表」。

---

## 4. 功能模块详解

主页面包含以下功能模块（括号内为 `App.js` 中的函数/位置）：

### 4.1 数据加载模块

- `fetchDocuments()`：`GET /api/documents`，拉取全部文档并写入 `documents`；失败提示「获取文档列表失败」。
- `fetchTags()`：`GET /api/tags`，拉取全部已有标签并写入 `tags`；失败提示「获取标签列表失败」。
- `useEffect(() => { fetchDocuments(); fetchTags(); }, [])`：组件首次挂载时各调用一次。
- 「刷新」按钮直接调用 `fetchDocuments()` 重新拉取文档列表。

### 4.2 上传 PDF 模块（handleUpload）

- 入口：Header 中的 `<Upload>` 包裹的「上传PDF」按钮，`accept=".pdf"`，`showUploadList={false}`（不显示上传列表），通过 `customRequest` 自定义上传。
- 逻辑：用 `FormData` 携带文件，`POST /api/documents`（`multipart/form-data`）；成功提示「文件上传成功」并刷新列表。
- 加载态：上传期间「上传PDF」按钮显示 `uploading` 独立 loading，不影响其他操作。

### 4.3 扫描目录模块（目录浏览器 openScanModal / fetchFolders + handleScanDirectory）

- 入口：Header「扫描目录」按钮 → 调用 `openScanModal()` 打开扫描弹窗并加载盘符列表。
- **目录浏览**（`fetchFolders(path)`）：`GET /api/folders?path=...`；path 为空时后端返回可用盘符（C:/D:/E:），传目录路径时返回该目录下的子文件夹列表（仅文件夹）及上级路径。单击文件夹进入下级，「上级」按钮返回父目录，「盘符」按钮回到盘符列表；底部 `Input.Search` 支持直接粘贴路径回车跳转。
- **扫描**（`handleScanDirectory(dirPath)`）：以当前浏览到的目录 `browsePath` 为目标，`POST /api/documents/scan`，请求体 `{ directory }`；
- 成功后提示 `{message}，新增 {added} 个文件，跳过 {skipped} 个文件`（目录无 PDF 时后端只返回 message，前端提示「未找到PDF文件」），随后刷新列表、关闭弹窗、重置浏览状态；
- 失败时读取后端错误信息提示（如「目录不存在」「没有权限访问」）。
- 说明：浏览器沙箱禁止网页直接浏览本地目录树并获取绝对路径，故目录列表由**本机运行的后端**提供（Web 端文件选择器的通用做法）。

### 4.4 单个 OCR 模块（handleOCR）

- 入口：表格「操作」列每行的「OCR」按钮。
- 逻辑（**异步提交**）：`POST /api/documents/{docId}/ocr`，后端立即返回 `202`；前端提示「OCR任务已提交，正在后台处理」后随即静默刷新列表。请求返回 `409`（文档处理中）时提示「请勿重复提交」。
- 加载态：OCR 按钮转圈/禁用**由服务端 status 驱动**——文档状态为 `splitting` / `ocr_processing` 时持续转圈（刷新页面后依然正确）；`ocrLoadingIds` 仅覆盖提交请求的在途瞬间。
- 状态流转与结果由**轮询机制**（见下）追踪，不再 `await` 处理完成。

### 4.4.1 轮询与结果通知机制（useEffect + 定时器）

- **轮询驱动源**：`hasProcessing`——列表中存在 `splitting` / `ocr_processing` 状态文档时为 true；
- **轮询**：`hasProcessing` 为 true 时启动 `setInterval`，每 5 秒静默调用 `fetchDocuments(true)`（不触发表格遮罩、不弹错误）；全部到达终态后自动 `clearInterval` 停止；
- **结果通知**：用 `useRef` 维护「上一轮处理中的文档 id 集合」，每次列表更新时比对：某文档从处理中变为 `completed` 则弹出成功提示，变为 `failed` 则弹出含 `error_message` 的失败提示；提交时会立即把 docId 登记进集合，保证任务秒级失败（轮询未及观察到处理中状态）时通知也不丢失。

### 4.5 批量 OCR 模块（handleBatchOCR）

- 入口：Header「批量OCR」按钮；未选中任何文档时点击会警告「请选择文档」。
- 逻辑（**异步批量提交**）：`POST /api/documents/batch/ocr`，请求体 `{ doc_ids: selectedRows }`；后端逐个受理（处理中/不存在的跳过），立即返回 `{accepted, skipped, details}`；前端按汇总提示「已提交 N 个任务，跳过 M 个（处理中或不存在）」，随后清空选择并静默刷新，后续进度同样由轮询追踪。
- 加载态：`batchLoading` 仅覆盖提交请求本身，批量 OCR 与批量删除按钮共用。

### 4.6 单个删除模块（handleDelete）

- 入口：表格「操作」列每行的「删除」按钮（红色 danger）。
- 逻辑：先弹出 `Modal.confirm` 二次确认（「删除后无法恢复，确定要删除吗？」），确认后 `DELETE /api/documents/{docId}`；成功提示「删除成功」并刷新列表。

### 4.7 批量删除模块（handleBatchDelete）

- 入口：Header「批量删除」按钮；未选中时警告「请选择文档」。
- 逻辑：先弹出确认框，提示将删除选中的 N 个文档；确认后**并发**对每个 id 调用 `DELETE /api/documents/{docId}`（`Promise.allSettled`），并统计成功/失败数量：
  - 全部成功：提示「批量删除成功，共删除 N 个文档」；
  - 部分失败：警告「删除完成：成功 X 个，失败 Y 个」，不影响已成功的删除。
- 完成后刷新列表并清空选择；加载态使用 `batchLoading`。
- 按钮禁用条件：未选中文档。

### 4.8 打标（添加标签）模块（showTagModal / handleAddTag）

- `showTagModal(docId)`：点击某行「打标」按钮时，记录 `currentDocId = docId` 并打开添加标签弹窗。
- `handleAddTag()`：校验标签名非空后，`POST /api/documents/{currentDocId}/tags`，请求体 `{ tag_name: newTagName }`；成功后清空输入、同时刷新文档列表与标签列表、关闭弹窗。
- 后端行为：标签名不存在时会自动创建标签（「获取或创建」），因此前端无需单独建标签。

### 4.9 标签移除模块（handleRemoveTag）

- 入口：表格「标签」列中每个 Tag 上的关闭按钮（`closable` 的 ×）。
- 逻辑：`DELETE /api/documents/{record.id}/tags/{tagId}`（文档 id 取自当前行 `record.id`）；成功提示「标签移除成功」并刷新列表。

### 4.10 多选模块（表格选择列）

- 表格首列为 `Checkbox`：
  - 表头复选框：勾选时选中当前全部文档（`documents.map(doc => doc.id)`，跨页全选），取消时清空选择；
  - 行复选框：勾选/取消单个文档 id，维护在 `selectedRows` 数组中。
- `selectedRows` 是批量 OCR、批量删除的数据来源。

### 4.11 Markdown 路径复制模块（handleCopyMarkdownPath）

- 入口：表格「操作」列的「MD路径」按钮，仅当文档 `markdown_path` 非空（OCR 已完成）时显示。
- 逻辑：点击后将 Markdown 文件的服务器端路径写入剪贴板（`navigator.clipboard.writeText`），提示「Markdown 文件路径已复制到剪贴板」。
- 兜底：在非安全上下文（如 HTTP 环境）下剪贴板 API 不可用时，自动降级为隐藏 `textarea` + `document.execCommand('copy')` 方案。

### 4.12 搜索过滤模块（searchText + filteredDocuments）

- 入口：内容区表格上方的搜索框（`Input`，带放大镜前缀图标、`allowClear` 一键清空），占位提示「搜索文件名或标签，支持不区分大小写的模糊匹配」。
- 逻辑：**纯前端本地过滤**——文档列表已全量加载，输入即过滤，无需请求后端、无需防抖等待。关键字同时匹配：
  - 文件名（`filename`，不区分大小写的包含匹配）；
  - 该文档的任一标签名（`tags[].name`）。
- 表格数据源为 `filteredDocuments`；无匹配时表格自然显示空状态。
- **全选跟随过滤结果**：搜索状态下表头复选框的勾选/取消只作用于当前过滤出的行（合并/差集更新 `selectedRows`），不会误选不可见的行；清空搜索框恢复全量列表。
- 轮询不受影响：后台 OCR 轮询仍基于全量 `documents` 判断 `hasProcessing`，搜索过滤不改变轮询行为。

---

## 5. 弹窗视图：添加标签

由 `isTagModalVisible` 控制显隐，`currentDocId` 标识目标文档。

弹窗内容：

1. **标签名输入框**（`Input`）：绑定 `newTagName`，占位提示「输入标签名称」。
2. **已有标签区**：展示 `tags` 中的所有标签（彩色 `Tag`）；**点击任一已有标签会把其名称填入输入框**（便于复用，避免重复建标签）。
3. **底部按钮**：
   - 「取消」：关闭弹窗；
   - 「添加」：调用 `handleAddTag()` 提交。

提交时若标签名为空会警告「标签名称不能为空」。

---

## 6. 弹窗视图：扫描目录（可视化目录浏览器）

由 `isScanModalVisible` 控制显隐，`openScanModal()` 打开时自动加载盘符列表。弹窗宽度 640px。

弹窗内容（自上而下）：

1. **导航工具栏**：
   - 「上级」按钮：进入父目录，到达盘符根/盘符列表时禁用；
   - 「盘符」按钮：回到盘符（C:/D:/E:）列表；
   - 右侧显示当前路径（盘符列表页显示「此电脑（请选择磁盘）」）。
2. **文件夹列表**（antd `List`，最高 300px 可滚动，加载中显示 `foldersLoading`）：
   - 每项为黄色文件夹图标 + 名称，**单击进入该目录**；
   - 盘符列表页显示 C: / D: / E:；目录页显示其子文件夹；
   - 空目录提示「该目录下没有子文件夹」。
3. **路径输入跳转框**（`Input.Search`）：绑定 `scanPath`，可直接粘贴绝对路径（如 `D:\Documents\PDFs`）回车或点「跳转」进入，兼顾手动输入习惯。
4. **提示文字**：「点击文件夹进入下级目录；扫描会递归查找所选目录下的所有 PDF 文件，已存在的文件自动跳过」，并在下方显示当前选中路径。
5. **底部按钮**：
   - 「取消」：关闭弹窗；
   - 「扫描当前目录」：以 `browsePath` 调用 `handleScanDirectory()`，未进入具体目录（盘符列表页）时禁用，提交期间显示 `scanning` loading。

> 设计说明：浏览器安全沙箱禁止网页直接读取本地目录树与绝对路径，因此目录数据来自后端 `GET /api/folders`（后端运行在本机）。

---

## 7. 文档表格列定义

`columns` 数组定义了 7 列：

| 列 | dataIndex | 渲染说明 |
|----|-----------|----------|
| 选择 | id | 表头全选 + 行单选的 `Checkbox`，绑定 `selectedRows` |
| 文件名 | filename | 文本，`ellipsis` 超长省略 |
| 大小 | file_size | 字节数自动格式化为 `B / KB / MB`（保留两位小数） |
| 页数 | page_count | 直接显示整数 |
| 状态 | status | 彩色 `Tag`，按下表映射 |
| 标签 | tags | 多个彩色 `Tag`，每个可点 × 移除 |
| 操作 | - | 「OCR」「MD路径」「打标」「删除」按钮；「MD路径」仅在文档已生成 Markdown 时显示，OCR 按钮按行显示独立 loading |

状态列的颜色与文案映射（`statusMap`）：

| status 值 | 颜色 | 文案 |
|-----------|------|------|
| uploaded | blue（蓝） | 已上传 |
| splitting | orange（橙） | 分割中 |
| ocr_processing | orange（橙） | OCR处理中 |
| completed | green（绿） | 已完成 |
| failed | red（红） | 失败 |
| 其他/未知 | gray（灰） | 原样显示状态值 |

文件大小格式化规则：

```
size < 1024        →  "{size} B"
size < 1024*1024   →  "{(size/1024).toFixed(2)} KB"
否则               →  "{(size/1024/1024).toFixed(2)} MB"
```

表格属性：`rowKey="id"`、`bordered`、`pagination={{ pageSize: 10 }}`（前端分页）、`loading={tableLoading}`（仅拉取列表时显示遮罩）。

---

## 8. 状态管理（useState）

组件未使用 Redux 等状态库，全部通过 `useState` 在 `App` 内管理：

| 状态 | 初始值 | 用途 |
|------|--------|------|
| `documents` | `[]` | 文档列表（表格数据源） |
| `selectedRows` | `[]` | 选中的文档 id 数组（批量操作依据） |
| `isTagModalVisible` | `false` | 添加标签弹窗显隐 |
| `isScanModalVisible` | `false` | 扫描目录弹窗显隐 |
| `scanPath` | `''` | 扫描目录输入框的值 |
| `currentDocId` | `null` | 当前打标的目标文档 id |
| `newTagName` | `''` | 添加标签输入框的值 |
| `tags` | `[]` | 全部已有标签（弹窗中展示） |
| `tableLoading` | `false` | 文档列表加载态（表格遮罩 + 「刷新」按钮 loading） |
| `uploading` | `false` | 上传 PDF 的按钮 loading |
| `scanning` | `false` | 扫描目录弹窗「扫描」按钮 loading |
| `batchLoading` | `false` | 批量 OCR / 批量删除按钮 loading |
| `ocrLoadingIds` | `[]` | 单行 OCR 提交请求在途的文档 id（按钮瞬间 loading；持续转圈由服务端 status 驱动） |
| `browsePath` | `''` | 目录浏览器当前所在路径（空串表示盘符列表页） |
| `folderParent` | `null` | 当前目录的上级路径（盘符根/盘符列表页为 null） |
| `folderList` | `[]` | 目录浏览器当前显示的文件夹/盘符列表 |
| `foldersLoading` | `false` | 目录列表加载态 |
| `searchText` | `''` | 搜索关键字；派生 `filteredDocuments`（按文件名/标签名过滤） |
| prevProcessingRef | `new Set()` | `useRef`，上一轮处于处理中的文档 id 集合（结果通知比对用，非渲染状态） |

全局反馈使用 antd 的 `message`（成功/失败/警告）与 `modal.confirm`（删除二次确认），二者均通过 `App.useApp()` 从应用上下文获取（根组件用 antd `<App>` 包裹，见 1.2）；不使用 antd 静态方法，以确保在 React 18 下正常渲染。

---

## 9. 前端调用的 API 清单

所有请求通过 `axios` 发起，基础路径为相对路径 `/api`（开发期由 proxy 转发到 `http://localhost:5000`）。

| 功能 | 方法 | 路径 | 请求体 | 调用位置 |
|------|------|------|--------|----------|
| 获取文档列表 | GET | `/api/documents` | - | fetchDocuments（轮询同样调用此接口静默刷新） |
| 获取标签列表 | GET | `/api/tags` | - | fetchTags |
| 浏览本地目录 | GET | `/api/folders?path=` | - | fetchFolders（path 空返回盘符，否则返回子文件夹） |
| 上传 PDF | POST | `/api/documents` | `multipart/form-data`（file） | handleUpload |
| 扫描目录 | POST | `/api/documents/scan` | `{ directory }` | handleScanDirectory |
| 单个 OCR（异步） | POST | `/api/documents/{id}/ocr` | - | handleOCR（202 已受理 / 409 处理中） |
| 批量 OCR（异步） | POST | `/api/documents/batch/ocr` | `{ doc_ids: [] }` | handleBatchOCR（返回 accepted/skipped） |
| 删除文档 | DELETE | `/api/documents/{id}` | - | handleDelete / handleBatchDelete |
| 文档添加标签 | POST | `/api/documents/{id}/tags` | `{ tag_name }` | handleAddTag |
| 文档移除标签 | DELETE | `/api/documents/{id}/tags/{tagId}` | - | handleRemoveTag |

> 后端另提供 `GET /api/documents/{id}` 单文档状态查询、`POST/DELETE /api/tags` 标签管理接口；前端轮询目前复用全量列表接口，单文档查询接口预留。

---

## 10. 前端数据结构

### 10.1 文档对象 Document（GET /api/documents 返回元素）

```json
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
    { "id": 1, "name": "AI", "color": "#1890ff" }
  ]
}
```

| 字段 | 类型 | 前端用途 |
|------|------|----------|
| id | number | 行 key、选择、各操作的目标标识 |
| filename | string | 「文件名」列 |
| file_size | number | 「大小」列（格式化为 B/KB/MB） |
| page_count | number | 「页数」列 |
| status | string | 「状态」列（映射为彩色 Tag）、OCR 按钮禁用判断 |
| created_at / updated_at | string(ISO) | 后端返回，前端暂未展示 |
| markdown_path | string | 非空时操作列显示「MD路径」按钮，点击可复制 Markdown 文件路径 |
| tags | Tag[] | 「标签」列渲染与移除 |

### 10.2 标签对象 Tag

```json
{ "id": 1, "name": "AI", "color": "#1890ff" }
```

### 10.3 扫描结果（POST /api/documents/scan 返回）

```json
{ "message": "扫描完成", "added": 5, "skipped": 3, "total_found": 8 }
```

---

## 11. 历史问题修复记录与架构观察

### 11.1 已修复问题

#### 问题 1（已修复 · 高）：上传 PDF 实际不生效
- **原因**：`<Upload>` 同时设置了 `customRequest={handleUpload}` 和 `beforeUpload={() => false}`。antd 规范中 `beforeUpload` 返回 `false` 会停止整个上传流程，导致 `customRequest` 不被触发。
- **修复**：移除了 `beforeUpload={() => false}`，由 `customRequest` 正常接管上传。

#### 问题 2（已修复 · 高）：表格内移除标签用错文档 ID
- **原因**：表格「标签」列的渲染函数最初使用全局状态 `currentDocId`（该变量仅在点击「打标」打开弹窗时才会被赋值）。直接点击某行标签的 × 会使用 `currentDocId`（可能为 `null` 报 404，或者删错之前打标过的其他文档）。
- **修复**：将渲染签名更新为 `render: (tags, record) => ...`，并在 `onClose` 中使用 `record.id`（加空值保护 `tags?.map`）。

#### 问题 3（已优化 · 低）：Modal 使用已废弃的 `visible` 属性
- **原因**：antd v5 推荐使用 `open` 控制弹窗显隐，`visible` 会产生弃用警告。
- **修复**：已将两个 `Modal` 的 `visible` 属性替换为 `open`。

#### 问题 4（已优化 · 中）：全局单一 loading，任意操作导致所有行按钮同时转圈
- **原因**：最初所有操作共用一个 `loading` 状态，上传/扫描/删除/OCR 任一进行时，表格遮罩与每一行 OCR 按钮都会转圈，且互相阻塞。
- **修复**：拆分为 `tableLoading`、`uploading`、`scanning`、`batchLoading`、`ocrLoadingIds` 五个独立加载态；其中单个 OCR 为**按行 loading**（id 进入 `ocrLoadingIds`），处理期间其他行仍可正常操作。

#### 问题 5（已优化 · 中）：批量删除串行循环、失败不可控
- **原因**：最初用 `for...of` 串行逐个调用删除接口，中途任一失败即中断，无成功/失败汇总，失败后选择状态也未清空。
- **修复**：改为 `Promise.allSettled` 并发删除，统计 `fulfilled` / `rejected` 数量：全部成功提示删除总数，部分失败提示「成功 X 个，失败 Y 个」；完成后清空选择并刷新列表。

#### 问题 6（已优化 · 低）：已完成文档的 Markdown 路径无法获取
- **原因**：后端返回的 `markdown_path` 字段前端未利用，用户无法直接定位 OCR 产物。
- **修复**：操作列对 `markdown_path` 非空的文档显示「MD路径」按钮，点击将路径复制到剪贴板；`navigator.clipboard` 在非安全上下文（HTTP）不可用时，自动降级为隐藏 `textarea` + `execCommand('copy')`。

---

### 11.2 待优化/架构观察事项

#### 事项 1（交互/架构 · 中）：OCR 为长耗时同步请求，无进度与自动刷新
`handleOCR` / `handleBatchOCR` 仍会一直 `await` 后端处理，而 OCR 在后端是同步执行（分割 PDF → MinerU 上传/轮询/下载 → 转 Markdown），可能耗时数分钟。加载态细化后，单行 OCR 期间其他行不再被阻塞，但请求本身仍长时间挂起，存在浏览器/代理超时风险，且文档状态不会实时更新（需等请求返回或手动「刷新」）。

建议改为后端异步任务（返回任务 id）+ 前端轮询状态，或至少在 OCR 期间定时刷新列表。

#### 事项 2（功能 · 低）：新建标签无法选择颜色
添加标签时只能输入名称，新标签统一使用后端默认颜色 `#1890ff`；弹窗中「已有标签」点击仅把名称填入输入框，不携带颜色。如需彩色分类，可增加颜色选择或允许复用已有标签的颜色。

#### 事项 3（展示 · 低）：时间字段未展示、Markdown 暂不支持在线预览/下载
`created_at`、`updated_at` 字段后端已返回但前端未展示；「MD路径」目前仅复制服务器端文件路径，尚未提供在线预览或直接下载 Markdown 文件的入口。
