# PDF 管理系统重复文档排查与 SQL 查询手册

本手册汇总了在 SQLite 数据库中排查重复 PDF 文档（同 MD5、同路径、同名同大小）的常用 SQL 语句与维护方法。

---

## 一、排查范围与排除原则

在排查或处理重复文档时，应当严格遵循：
- **排除正在处理中的任务**：`status NOT IN ('splitting', 'ocr_processing')`
- **排除已经完成识别的任务**：`status != 'completed'`（已完成的具有落盘 Markdown 产物，不应随意标记或覆盖）
- 待处理集合通常界定为：`status IN ('uploaded', 'failed')`

---

## 二、核心查询 SQL 汇总

### 1. 查找所有存在重复 MD5 的哈希值及重复次数
用于快速获知当前库里有多少份物理内容相同的文件：

```sql
SELECT md5, COUNT(*) AS dup_count
FROM pdf_documents
WHERE status IN ('uploaded', 'failed')
  AND md5 IS NOT NULL AND md5 != ''
GROUP BY md5
HAVING COUNT(*) > 1
ORDER BY dup_count DESC;
```

---

### 2. 展开查看重复 MD5 文档的具体明细
列出所有重复文档的 ID、文件名、当前是否禁止 OCR、状态及物理路径：

```sql
SELECT d.id, d.filename, d.md5, d.ocr_disabled, d.status, d.original_path
FROM pdf_documents d
JOIN (
    SELECT md5
    FROM pdf_documents
    WHERE status IN ('uploaded', 'failed')
      AND md5 IS NOT NULL AND md5 != ''
    GROUP BY md5
    HAVING COUNT(*) > 1
) dup ON d.md5 = dup.md5
WHERE d.status IN ('uploaded', 'failed')
ORDER BY d.md5, d.id ASC;
```

---

### 3. 筛选出“需要被禁用的多余重复记录”的 ID
每组重复文档中，通常只保留 1 条作为主记录（例如保留 ID 最小的那一条），其余所有多余记录选出：

```sql
SELECT d.id, d.filename, d.md5
FROM pdf_documents d
JOIN (
    SELECT md5, MIN(id) AS keep_id
    FROM pdf_documents
    WHERE status IN ('uploaded', 'failed')
      AND md5 IS NOT NULL AND md5 != ''
    GROUP BY md5
    HAVING COUNT(*) > 1
) keep ON d.md5 = keep.md5
WHERE d.status IN ('uploaded', 'failed')
  AND d.id != keep.keep_id
  AND (d.ocr_disabled = 0 OR d.ocr_disabled IS NULL);
```

---

### 4. 检查是否有未识别文档的 MD5 其实早已被识别过（交叉查重）
如果某个 `uploaded` 的文档，其 MD5 早已存在于 `completed` 状态中，则该文档无需再浪费 MinerU 算力重新识别：

```sql
SELECT t.id, t.filename, t.md5, c.id AS completed_id, c.markdown_path
FROM pdf_documents t
JOIN pdf_documents c ON t.md5 = c.md5
WHERE t.status IN ('uploaded', 'failed')
  AND c.status = 'completed'
  AND t.md5 IS NOT NULL AND t.md5 != '';
```

---

### 5. 按绝对物理路径查重（同一文件路径被多次录入）
```sql
SELECT original_path, COUNT(*) AS dup_count
FROM pdf_documents
GROUP BY original_path
HAVING COUNT(*) > 1;
```

---

### 6. 按文件名 + 页数 + 大小查重（无 MD5 时辅助比对）
```sql
SELECT filename, file_size, page_count, COUNT(*) AS dup_count
FROM pdf_documents
WHERE status IN ('uploaded', 'failed')
GROUP BY filename, file_size, page_count
HAVING COUNT(*) > 1
ORDER BY dup_count DESC;
```

---

## 三、批量将重复项设为禁止 OCR（`ocr_disabled = 1`）的标准更新语句

在执行去重软禁用时，采用“保留每组 ID 最小的一条，将其余记录置为禁止”的 SQL：

```sql
UPDATE pdf_documents
SET ocr_disabled = 1,
    updated_at = datetime('now')
WHERE id IN (
    SELECT d.id
    FROM pdf_documents d
    JOIN (
        -- 找出每组重复 MD5 中拟保留的一条（最小 ID）
        SELECT md5, MIN(id) AS keep_id
        FROM pdf_documents
        WHERE status IN ('uploaded', 'failed')
          AND md5 IS NOT NULL AND md5 != ''
        GROUP BY md5
        HAVING COUNT(*) > 1
    ) keep ON d.md5 = keep.md5
    WHERE d.status IN ('uploaded', 'failed')
      AND d.id != keep.keep_id
      AND (d.ocr_disabled = 0 OR d.ocr_disabled IS NULL)
);
```

另外，针对**与已完成文件交叉重复**的项，同样置为禁止 OCR：
```sql
UPDATE pdf_documents
SET ocr_disabled = 1,
    updated_at = datetime('now')
WHERE id IN (
    SELECT t.id
    FROM pdf_documents t
    JOIN pdf_documents c ON t.md5 = c.md5
    WHERE t.status IN ('uploaded', 'failed')
      AND c.status = 'completed'
      AND t.md5 IS NOT NULL AND t.md5 != ''
      AND (t.ocr_disabled = 0 OR t.ocr_disabled IS NULL)
);
```
