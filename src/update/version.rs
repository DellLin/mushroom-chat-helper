//! 版號比較。
//!
//! 要比的東西只有兩種來源:`Cargo.toml` 的 `X.Y.Z` 與 GitHub tag 的 `vX.Y.Z`。
//! 規則短到寫得完,就不為了這件事再拉一個 semver 相依進來。

/// 解析過的版號。欄位順序就是比較的優先順序 —— derive 出來的 `Ord` 依序比欄位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    core: (u64, u64, u64),
    /// 正式版排在預發佈版之後(semver:`1.0.0-rc.1` < `1.0.0`)。
    /// `false`(帶 `-預發佈` 標籤)< `true`(正式版),bool 的排序剛好就是這個意思。
    is_release: bool,
}

/// `vX.Y.Z`、`X.Y.Z`、`X.Y.Z-rc.1`、`X.Y.Z+build` 都認;其他一律回 `None`。
fn parse(s: &str) -> Option<Version> {
    let s = s.trim();
    let s = s.strip_prefix('v').unwrap_or(s);
    // build metadata(`+…`)依 semver 不參與比較,直接丟掉。
    let s = s.split('+').next()?;
    let (core, pre) = match s.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (s, None),
    };

    // 三段都要是數字,而且剛好三段 —— `1.2.3.4` 不是這個專案發得出來的版號,
    // 與其猜它想表達什麼,不如不認。
    let nums: Option<Vec<u64>> = core.split('.').map(|p| p.parse::<u64>().ok()).collect();
    let [major, minor, patch] = nums?[..] else {
        return None;
    };

    Some(Version {
        core: (major, minor, patch),
        is_release: pre.is_none_or(str::is_empty),
    })
}

/// `latest` 比 `current` 新嗎?
///
/// 任一邊解析不出來就回 `false`:這個判斷的下游是「慫恿使用者覆蓋掉自己的
/// 執行檔」,看不懂的版號寧可漏報也不要誤報。
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse(latest), parse(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_versions_are_detected() {
        assert!(is_newer("0.3.0", "0.2.0"));
        assert!(is_newer("0.2.1", "0.2.0"));
        assert!(is_newer("1.0.0", "0.99.99"));
    }

    /// GitHub 的 tag 帶 `v`,Cargo.toml 的版號不帶,兩邊要能直接對。
    #[test]
    fn the_v_prefix_on_tags_is_ignored() {
        assert!(is_newer("v0.3.0", "0.2.0"));
        assert!(!is_newer("v0.2.0", "0.2.0"));
    }

    /// 版號是數字不是字串:`0.10.0` 比 `0.9.0` 新,字串比較會說反話。
    #[test]
    fn version_parts_compare_as_numbers() {
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(is_newer("0.2.10", "0.2.9"));
        assert!(!is_newer("0.9.0", "0.10.0"));
    }

    /// 同版或更舊都不該跳更新提示 —— 尤其不能把使用者降版回去。
    #[test]
    fn same_or_older_never_prompts() {
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("0.2.0", "1.0.0"));
    }

    #[test]
    fn prereleases_sort_below_the_matching_release() {
        assert!(is_newer("1.0.0", "1.0.0-rc.1"));
        assert!(!is_newer("1.0.0-rc.1", "1.0.0"));
        assert!(is_newer("1.0.0-rc.2", "0.9.0"));
    }

    /// build metadata 不參與比較。
    #[test]
    fn build_metadata_is_ignored() {
        assert!(!is_newer("0.2.0+abc", "0.2.0"));
        assert!(is_newer("0.3.0+abc", "0.2.0"));
    }

    /// 認不得的東西一律當成「沒有新版」,不要拿它去觸發更新流程。
    #[test]
    fn unparseable_versions_never_prompt() {
        assert!(!is_newer("latest", "0.2.0"));
        assert!(!is_newer("", "0.2.0"));
        assert!(!is_newer("1.2", "0.2.0"));
        assert!(!is_newer("1.2.3.4", "0.2.0"));
        assert!(!is_newer("v.x.y", "0.2.0"));
        assert!(!is_newer("0.3.0", "不是版號"));
    }
}
