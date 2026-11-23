//! 批量重命名的纯逻辑：文件名替换（与前端编辑器一致的 JS 替换语义）、文件名合法性校验。
//!
//! Rust `regex` crate 的替换语法与 JS 不一致（`$&` 不支持、命名组写作 `${name}`、
//! 不存在的组号会被置空），因此这里按 ECMAScript `GetSubstitution` 规则自行展开，
//! 保证与前端 Markdown 编辑器的替换行为一致。

use regex::{Captures, Regex, RegexBuilder};

/// 单个文件名允许的最大字符数（不含扩展名）
const MAX_STEM_CHARS: usize = 180;
/// 完整文件名允许的最大字符数
const MAX_NAME_CHARS: usize = 200;

#[derive(Debug, Clone, Default)]
pub struct RenameRule {
    /// literal | regex
    pub mode: String,
    pub find: String,
    pub replace: String,
    pub case_sensitive: bool,
    /// 只替换主名，扩展名固定保留 .pdf
    pub main_name_only: bool,
    /// 清理替换结果的首尾空格、重复空格与不可见空白（默认关闭）
    pub clean_whitespace: bool,
}

/// 编译查找正则；查找内容为空时返回 None（预览阶段会被判为“无变化”）
pub fn build_regex(rule: &RenameRule) -> Result<Option<Regex>, String> {
    if rule.find.is_empty() {
        return Ok(None);
    }
    let pattern = if rule.mode == "regex" {
        rule.find.clone()
    } else {
        regex::escape(&rule.find)
    };
    RegexBuilder::new(&pattern)
        .case_insensitive(!rule.case_sensitive)
        .build()
        .map(Some)
        .map_err(|e| e.to_string())
}

/// 按 JS 规则展开替换串：$1~$99、$<name>、$&、$$、$`、$'。
/// 不存在的组号原样保留；正则含命名组但名字不存在时替换为空串。
pub fn expand_template(re: &Regex, caps: &Captures, haystack: &str, template: &str) -> String {
    let chars: Vec<char> = template.chars().collect();
    let whole = caps.get(0);
    let group_count = re.captures_len().saturating_sub(1);
    let has_named = re.capture_names().any(|n| n.is_some());
    let mut out = String::with_capacity(template.len() + 16);
    let mut i = 0usize;

    while i < chars.len() {
        let c = chars[i];
        if c != '$' || i + 1 >= chars.len() {
            out.push(c);
            i += 1;
            continue;
        }
        let next = chars[i + 1];
        match next {
            '$' => {
                out.push('$');
                i += 2;
            }
            '&' => {
                if let Some(m) = whole {
                    out.push_str(m.as_str());
                }
                i += 2;
            }
            '`' => {
                if let Some(m) = whole {
                    out.push_str(&haystack[..m.start()]);
                }
                i += 2;
            }
            '\'' => {
                if let Some(m) = whole {
                    out.push_str(&haystack[m.end()..]);
                }
                i += 2;
            }
            '<' => {
                let rest = &chars[i + 2..];
                match rest.iter().position(|&ch| ch == '>') {
                    Some(end) => {
                        let name: String = rest[..end].iter().collect();
                        let known = re
                            .capture_names()
                            .flatten()
                            .any(|n| n == name);
                        if known {
                            if let Some(m) = caps.name(&name) {
                                out.push_str(m.as_str());
                            }
                        } else if has_named {
                            // 有命名组但该名字不存在：JS 置空
                        } else {
                            out.push_str("$<");
                            out.push_str(&name);
                            out.push('>');
                        }
                        i += 2 + end + 1;
                    }
                    None => {
                        out.push('$');
                        i += 1;
                    }
                }
            }
            '0'..='9' => {
                // JS 优先取两位组号，越界则退一位，再越界则整段原样保留
                let mut j = i + 1;
                let mut num = 0usize;
                while j < chars.len() && chars[j].is_ascii_digit() && j - (i + 1) < 2 {
                    num = num * 10 + chars[j].to_digit(10).unwrap_or(0) as usize;
                    j += 1;
                }
                let two_digit = j - (i + 1) == 2;
                let single = chars[i + 1].to_digit(10).unwrap_or(0) as usize;
                if two_digit && num >= 1 && num <= group_count {
                    if let Some(m) = caps.get(num) {
                        out.push_str(m.as_str());
                    }
                    i = j;
                } else if single >= 1 && single <= group_count {
                    if let Some(m) = caps.get(single) {
                        out.push_str(m.as_str());
                    }
                    i += 2;
                } else {
                    out.push('$');
                    out.push(chars[i + 1]);
                    i += 2;
                }
            }
            _ => {
                out.push('$');
                i += 1;
            }
        }
    }
    out
}

/// 按规则替换，返回“扩展名还没补齐”的原始文本。
/// 只改主名时输入为主名，否则输入为完整文件名；扩展名的补齐与校验交给 normalize_new_name。
pub fn renamed_raw(old_name: &str, rule: &RenameRule) -> Result<String, String> {
    let re = build_regex(rule)?.ok_or_else(|| "查找内容不能为空".to_string())?;
    let target = if rule.main_name_only {
        strip_pdf_ext(old_name).to_string()
    } else {
        old_name.to_string()
    };
    Ok(match re.captures(&target) {
        Some(caps) => {
            let m = caps.get(0).expect("captures 至少包含整体匹配");
            let mut out = String::with_capacity(target.len() + 16);
            out.push_str(&target[..m.start()]);
            out.push_str(&expand_template(&re, &caps, &target, &rule.replace));
            out.push_str(&target[m.end()..]);
            out
        }
        None => target,
    })
}

fn strip_pdf_ext(name: &str) -> &str {
    name.strip_suffix(".pdf")
        .or_else(|| name.strip_suffix(".PDF"))
        .unwrap_or(name)
}

/// 规范化结果：最终文件名 + 非阻断提示（例如“已清理空格”“结尾是不可见字符”）
pub struct Normalized {
    pub name: String,
    pub note: String,
}

/// 结尾不可见字符的中文说明
fn invisible_desc(ch: char) -> Option<&'static str> {
    match ch {
        '\u{00A0}' => Some("非断行空格"),
        '\u{3000}' => Some("全角空格"),
        '\u{200B}' => Some("零宽空格"),
        '\u{200C}' => Some("零宽非连接符"),
        '\u{200D}' => Some("零宽连接符"),
        '\u{FEFF}' => Some("零宽不换行空格(BOM)"),
        _ => None,
    }
}

/// 供错误提示展示的候选名：超过 60 字符截断，避免消息过长
fn quote(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() > 60 {
        let head: String = chars[..57].iter().collect();
        format!("\"{}…\"", head)
    } else {
        format!("\"{}\"", name)
    }
}

/// 清理首尾空格与重复空格：ASCII 空格/Tab、NBSP、全角空格归一为单个空格，去掉零宽字符
fn clean_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if matches!(ch, '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}') {
            continue;
        }
        if matches!(ch, ' ' | '\t' | '\u{00A0}' | '\u{3000}') {
            if out.is_empty() || prev_space {
                continue;
            }
            out.push(' ');
            prev_space = true;
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// 把替换结果规范成最终 `.pdf` 文件名并校验。
///
/// 尾随点/空格的规则按 Windows 的真实行为区分：
/// - 输入不含扩展名时，它就是“主名”，Windows 会静默丢掉尾随空格/点，直接拒绝；
/// - 输入本身以 `.pdf` 结尾时，点之前有空格是合法的（如 `书名 .pdf`），不判非法。
/// - 结尾是 NBSP/全角空格/零宽字符时 Windows 会保留，不判非法，但给出可诊断提示；
///   勾选清理选项后这些字符会被去掉。
pub fn normalize_new_name(raw: &str, clean: bool) -> Result<Normalized, String> {
    let mut note = String::new();
    let mut candidate = raw.to_string();
    if clean {
        let cleaned = clean_whitespace(&candidate);
        if cleaned != candidate {
            note = "已清理首尾空格与重复空格".to_string();
        }
        candidate = cleaned;
    }
    if candidate.trim().is_empty() {
        return Err("文件名为空".to_string());
    }
    let has_ext = candidate.to_lowercase().ends_with(".pdf");
    let stem = if has_ext {
        &candidate[..candidate.len() - 4]
    } else {
        candidate.as_str()
    };
    if !has_ext {
        if stem.ends_with('.') {
            return Err(format!("新名 {}（结尾：点）不能以点或空格结尾", quote(stem)));
        }
        if stem.ends_with(' ') || stem.ends_with('\t') {
            return Err(format!("新名 {}（结尾：空格）不能以点或空格结尾", quote(stem)));
        }
    }
    if let Some(last) = stem.chars().last() {
        if let Some(desc) = invisible_desc(last) {
            let msg = format!(
                "结尾是不可见字符 {}（{}），Windows 会保留它；可勾选「清理首尾空格」去掉",
                desc,
                format!("U+{:04X}", last as u32)
            );
            note = if note.is_empty() { msg } else { format!("{}；{}", note, msg) };
        }
    }
    if stem.trim().is_empty() {
        return Err("文件名为空".to_string());
    }
    if let Some(ch) = stem
        .chars()
        .find(|c| "\\/:*?\"<>|".contains(*c) || (*c as u32) < 0x20)
    {
        return Err(format!("新名 {} 包含非法字符：{}", quote(stem), ch.escape_debug()));
    }
    let upper = stem.split('.').next().unwrap_or("").trim().to_uppercase();
    let reserved = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit()
            && upper.as_bytes()[3] != b'0');
    if reserved {
        return Err(format!("{} 是 Windows 保留名", upper));
    }
    if stem.chars().count() > MAX_STEM_CHARS {
        return Err(format!("文件名过长（超过 {} 个字符）", MAX_STEM_CHARS));
    }
    let full = format!("{}.pdf", stem);
    if full.chars().count() > MAX_NAME_CHARS {
        return Err(format!("完整文件名过长（超过 {} 个字符）", MAX_NAME_CHARS));
    }
    Ok(Normalized { name: full, note })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(mode: &str, find: &str, replace: &str) -> RenameRule {
        RenameRule {
            mode: mode.into(),
            find: find.into(),
            replace: replace.into(),
            case_sensitive: true,
            main_name_only: true,
            clean_whitespace: false,
        }
    }

    fn renamed(old: &str, r: &RenameRule) -> Result<String, String> {
        Ok(normalize_new_name(&renamed_raw(old, r)?, r.clean_whitespace)?.name)
    }

    #[test]
    fn numbered_group_and_literal_dollar() {
        let r = rule("regex", r"^(\d+)", "# $1");
        assert_eq!(renamed("1937 报告.pdf", &r).unwrap(), "# 1937 报告.pdf");
        let r2 = rule("regex", r"^(\d+)", "$$$1");
        assert_eq!(renamed("1937 报告.pdf", &r2).unwrap(), "$1937 报告.pdf");
    }

    #[test]
    fn named_group_and_missing_group() {
        let r = rule("regex", r"^(?<year>\d{4})", "$<year>年");
        assert_eq!(renamed("1983 年.pdf", &r).unwrap(), "1983年 年.pdf");
        let r2 = rule("regex", r"^(\d{4})", "$1-$9");
        assert_eq!(renamed("1983 年.pdf", &r2).unwrap(), "1983-$9 年.pdf");
    }

    #[test]
    fn whole_match_and_literal_mode() {
        let r = rule("regex", r"\d{4}", "[$&]");
        assert_eq!(renamed("1983 年.pdf", &r).unwrap(), "[1983] 年.pdf");
        let r2 = rule("literal", "$1", "X");
        assert_eq!(renamed("a$1b.pdf", &r2).unwrap(), "aXb.pdf");
    }

    #[test]
    fn zlibrary_suffix_delete_and_add() {
        let name = "中国财政史 (《中国财政史》编写组编著) (z-library.sk, 1lib.sk, z-lib.sk).pdf";
        // 带前导空格删除 → 干净结尾
        let r = rule("literal", " (z-library.sk, 1lib.sk, z-lib.sk)", "");
        assert_eq!(
            renamed(name, &r).unwrap(),
            "中国财政史 (《中国财政史》编写组编著).pdf"
        );
        // 不带前导空格删除 → 真的留下尾随空格，拒绝
        let r2 = rule("literal", "(z-library.sk, 1lib.sk, z-lib.sk)", "");
        let err = renamed(name, &r2).unwrap_err();
        assert!(err.contains("结尾：空格"), "{}", err);
        // 勾选清理后可通过
        let mut r3 = rule("literal", "(z-library.sk, 1lib.sk, z-lib.sk)", "");
        r3.clean_whitespace = true;
        let ok = normalize_new_name(&renamed_raw(name, &r3).unwrap(), true).unwrap();
        assert_eq!(
            ok.name,
            "中国财政史 (《中国财政史》编写组编著).pdf"
        );
        assert!(ok.note.contains("已清理"));
        // 追加该串不产生尾随空格
        let r4 = rule("literal", "编著)", "编著) (z-library.sk, 1lib.sk, z-lib.sk)");
        assert!(renamed(name, &r4).unwrap().ends_with("z-lib.sk).pdf"));
    }

    #[test]
    fn trailing_dot_or_space_rule() {
        // 主名（无扩展名）尾随空格/点 → 非法
        assert!(normalize_new_name("书名 ", false).is_err());
        assert!(normalize_new_name("1983.", false).is_err());
        // 带扩展名时，点之前的空格合法
        assert_eq!(normalize_new_name("书名 .pdf", false).unwrap().name, "书名 .pdf");
        assert_eq!(normalize_new_name("书名 .PDF", false).unwrap().name, "书名 .pdf");
        assert_eq!(normalize_new_name("正常名字", false).unwrap().name, "正常名字.pdf");
        // 清理选项：尾随空格、重复空格、全角空格、NBSP、零宽字符
        assert_eq!(normalize_new_name("书名 ", true).unwrap().name, "书名.pdf");
        assert_eq!(normalize_new_name("书名  (副本) ", true).unwrap().name, "书名 (副本).pdf");
        assert_eq!(
            normalize_new_name("书名\u{3000}\u{00A0}甲", true).unwrap().name,
            "书名 甲.pdf"
        );
        assert_eq!(
            normalize_new_name("书名\u{200B}", true).unwrap().name,
            "书名.pdf"
        );
        // 未勾选清理时，结尾不可见字符不判非法，但会提示
        let n = normalize_new_name("书名\u{00A0}", false).unwrap();
        assert!(n.note.contains("非断行空格"), "{}", n.note);
    }

    #[test]
    fn invalid_chars_detected() {
        assert!(normalize_new_name("a*b", false).is_err());
        assert!(normalize_new_name("a?", false).is_err());
        assert!(normalize_new_name("CON", false).is_err());
        assert!(normalize_new_name("正常 文件", false).is_ok());
    }
}
