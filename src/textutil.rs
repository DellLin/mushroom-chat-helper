//! OCR 文字後處理:CJK 空白清理、頻道標籤剝除、發話者解析。

/// 是否為 CJK / 全形字元(含日韓,涵蓋遊戲內常見字集)。
pub fn is_cjk(c: char) -> bool {
    matches!(
        c as u32,
        0x3000..=0x303F   // CJK 標點
        | 0x3040..=0x30FF // 日文假名
        | 0x3400..=0x4DBF // CJK 擴充 A
        | 0x4E00..=0x9FFF // CJK 統一表意
        | 0xAC00..=0xD7AF // 韓文
        | 0xF900..=0xFAFF // 相容表意
        | 0xFF00..=0xFFEF // 全形符號
    )
}

/// Windows OCR 會在中文字間插入空白,將「你 好 世 界」還原為「你好世界」。
pub fn clean_ocr_text(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == ' ' {
            let prev = chars[..i].iter().rev().find(|c| **c != ' ');
            let next = chars[i + 1..].iter().find(|c| **c != ' ');
            if let (Some(&p), Some(&n)) = (prev, next) {
                if is_cjk(p) && is_cjk(n) {
                    continue;
                }
            }
        }
        out.push(c);
    }
    out.trim().to_string()
}

/// 剝除行首的頻道標籤,如「[公會]」「【系統】」。
pub fn strip_leading_tag(s: &str) -> &str {
    let t = s.trim_start();
    for (open, close) in [('[', ']'), ('【', '】'), ('〈', '〉')] {
        if t.starts_with(open) {
            let mut iter = t.char_indices();
            iter.next(); // 跳過開括號
            for (idx, c) in iter.take(10) {
                if c == close {
                    return t[idx + c.len_utf8()..].trim_start();
                }
            }
        }
    }
    t
}

/// 以第一個冒號(半形/全形)切出發話者名稱。
/// 楓之谷格式通常為「角色名 : 內容」。
pub fn parse_sender(text: &str) -> (Option<String>, String) {
    let mut count = 0usize;
    for (idx, c) in text.char_indices() {
        count += 1;
        if count > 20 {
            break;
        }
        if c == ':' || c == '：' {
            let name = text[..idx].trim();
            let rest = text[idx + c.len_utf8()..].trim();
            if !name.is_empty() && name.chars().count() <= 14 && !rest.is_empty() {
                return (Some(name.to_string()), rest.to_string());
            }
            break;
        }
    }
    (None, text.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_space_cleanup() {
        assert_eq!(clean_ocr_text("你 好 abc 世 界"), "你好 abc 世界");
    }

    #[test]
    fn tag_strip() {
        assert_eq!(strip_leading_tag("[公會] 大家好"), "大家好");
        assert_eq!(strip_leading_tag("沒有標籤"), "沒有標籤");
    }

    #[test]
    fn sender_parse() {
        let (s, c) = parse_sender("王小明 : 收購楓幣");
        assert_eq!(s.as_deref(), Some("王小明"));
        assert_eq!(c, "收購楓幣");
        let (s2, c2) = parse_sender("系統公告訊息沒有冒號");
        assert!(s2.is_none());
        assert_eq!(c2, "系統公告訊息沒有冒號");
    }
}
