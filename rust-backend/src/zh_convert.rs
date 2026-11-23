//! 繁简中文转换：基于 zhconv 预编译词库（MediaWiki/OpenCC 合并表），
//! 支持用户自定义「繁→简」映射，命中映射的词优先于内置词典。

use std::collections::HashMap;

use zhconv::{zhconv, Variant};

/// 解析映射文本：每行 `繁体词=>简体词`，空行与 `#` 开头的注释行忽略。
/// 返回按键长度降序排列的映射对，保证最长匹配优先。
pub fn parse_mapping(raw: &str) -> Result<Vec<(String, String)>, String> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (idx, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once("=>") else {
            return Err(format!("第 {} 行缺少 => 分隔符：{}", idx + 1, line));
        };
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() || value.is_empty() {
            return Err(format!("第 {} 行映射词不能为空：{}", idx + 1, line));
        }
        pairs.push((key.to_string(), value.to_string()));
    }
    // 长键优先：「乾隆」这类长短语先于单字键匹配
    pairs.sort_by(|a, b| {
        b.0.chars()
            .count()
            .cmp(&a.0.chars().count())
            .then_with(|| a.0.cmp(&b.0))
    });
    Ok(pairs)
}

/// direction：t2s = 繁→简（应用自定义映射），s2t = 简→繁（仅内置转换）
pub fn convert(text: &str, direction: &str, mapping_raw: &str) -> Result<String, String> {
    match direction {
        "s2t" => Ok(zhconv(text, Variant::ZhHant)),
        "t2s" => {
            let pairs = parse_mapping(mapping_raw)?;
            if pairs.is_empty() {
                return Ok(zhconv(text, Variant::ZhHans));
            }
            Ok(convert_with_mapping(text, &pairs, Variant::ZhHans))
        }
        other => Err(format!("未知转换方向：{}", other)),
    }
}

/// 逐字符扫描，命中映射键的片段直接输出映射值（不再走内置词典），
/// 未命中的连续片段整段交给 zhconv，保留其词级转换质量（如 头髮→头发）。
fn convert_with_mapping(text: &str, pairs: &[(String, String)], target: Variant) -> String {
    let mut by_first: HashMap<char, Vec<(&str, &str)>> = HashMap::new();
    for (key, value) in pairs {
        if let Some(first) = key.chars().next() {
            by_first.entry(first).or_default().push((key.as_str(), value.as_str()));
        }
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 64);
    let mut pending = String::new();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        let mut matched = false;
        if let Some(candidates) = by_first.get(&c) {
            for (key, value) in candidates {
                let key_chars: Vec<char> = key.chars().collect();
                if i + key_chars.len() <= chars.len()
                    && chars[i..i + key_chars.len()].iter().eq(key_chars.iter())
                {
                    if !pending.is_empty() {
                        out.push_str(&zhconv(&pending, target));
                        pending.clear();
                    }
                    out.push_str(value);
                    i += key_chars.len();
                    matched = true;
                    break;
                }
            }
        }
        if !matched {
            pending.push(c);
            i += 1;
        }
    }
    if !pending.is_empty() {
        out.push_str(&zhconv(&pending, target));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_mapping() {
        assert!(parse_mapping("乾隆=>乾隆\n皇后=>皇后").is_ok());
        assert!(parse_mapping("缺分隔符").is_err());
        assert!(parse_mapping("=>空键").is_err());
        let pairs = parse_mapping("# 注释\n皇后=>皇后\n乾隆=>乾隆").unwrap();
        assert_eq!(pairs.len(), 2);
        // 等长键顺序不敏感，只保证长键在前（这里全部等长，仅校验完整性）
        assert!(pairs.iter().all(|(k, _)| k.chars().count() == 2));
    }

    #[test]
    fn test_convert_t2s_with_mapping() {
        let out = convert("乾隆年間，皇后頭髮很長，發現天乾物燥。", "t2s", "乾隆=>乾隆\n皇后=>皇后").unwrap();
        assert_eq!(out, "乾隆年间，皇后头发很长，发现天干物燥。");
    }

    #[test]
    fn test_convert_s2t() {
        let out = convert("皇后头发很长，发现天干物燥。", "s2t", "").unwrap();
        assert_eq!(out, "皇后頭髮很長，發現天乾物燥。");
    }
}
