//! 自動更新:開機時跟 GitHub 對一下版號,有新版就問使用者要不要換掉自己。
//!
//! ```text
//! 啟動 ─→ wait_for_predecessor_exit() 若是更新後重啟,先等舊行程真的死透
//!      └→ cleanup_stale()             清掉上次更新留下的 .old
//!      └→ spawn_check()  背景執行緒 ─ GitHub API ─→ UiEvent::Update(Available)
//!                                                        │ 使用者按「立即更新」
//!         spawn_install() 背景執行緒 ─ 下載 ─ 驗 SHA256 ─→ 換檔 ─→ Installed
//!                                                        │ 使用者按「重新啟動」
//!                                                     relaunch + 結束自己
//! ```
//!
//! 網路與檔案 I/O 全部在背景執行緒,UI 只收 [`UpdateEvent`]。所有失敗都是
//! 「照常使用舊版」,更新這件事不該讓一個聊天疊圖工具開不起來。

mod github;
mod install;
mod version;

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use crossbeam_channel::Sender;

pub use github::Release;
pub use install::{cleanup_stale, relaunch, wait_for_predecessor_exit};

use crate::model::UiEvent;

/// 目前這份執行檔的版號。
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// 進度回報的間隔。每個 chunk 都送 UI 也畫不出差別,反而洗爆 channel。
const PROGRESS_STEP: u64 = 256 * 1024;

/// 更新流程回報給 UI 的進度。
#[derive(Debug)]
pub enum UpdateEvent {
    /// 已經是最新版。自動檢查時 UI 會安靜吞掉,手動檢查才顯示。
    UpToDate,
    /// 查到有新版可以更新。
    Available(Box<Release>),
    /// 下載中。`total` 來自 Content-Length,對方沒給就是 `None`。
    Progress { done: u64, total: Option<u64> },
    /// 新版已經就定位(附帶執行檔路徑),接下來等使用者按重新啟動。
    Installed(PathBuf),
    Failed(String),
}

/// 背景查有沒有新版。
///
/// `skipped` 是使用者按過「略過這個版本」的 tag;`manual` 代表這次是使用者
/// 自己按的檢查,此時無視 `skipped`(他都主動問了,就照實回答)。
pub fn spawn_check(tx: Sender<UiEvent>, skipped: Option<String>, manual: bool) {
    std::thread::spawn(move || {
        let event = match github::latest_release() {
            Ok(release) => classify(release, skipped.as_deref(), manual),
            Err(e) => UpdateEvent::Failed(format!("檢查更新失敗:{e:#}")),
        };
        let _ = tx.send(UiEvent::Update(event));
    });
}

/// 查到的 Release 到底算不算「有新版可以更新」。抽出來是為了可以測 —— 真正
/// 容易出錯的判斷都在這裡,而不在上面那段 thread spawn。
fn classify(release: Release, skipped: Option<&str>, manual: bool) -> UpdateEvent {
    if !version::is_newer(&release.tag, CURRENT) {
        return UpdateEvent::UpToDate;
    }
    if !manual && skipped == Some(release.tag.as_str()) {
        return UpdateEvent::UpToDate;
    }
    UpdateEvent::Available(Box::new(release))
}

/// 背景下載新版、驗過雜湊後換到定位。
pub fn spawn_install(tx: Sender<UiEvent>, release: Release) {
    std::thread::spawn(move || {
        let event = match download_and_install(&release, &tx) {
            Ok(exe) => UpdateEvent::Installed(exe),
            Err(e) => UpdateEvent::Failed(format!("{e:#}")),
        };
        let _ = tx.send(UiEvent::Update(event));
    });
}

fn download_and_install(release: &Release, tx: &Sender<UiEvent>) -> Result<PathBuf> {
    let url = release.exe_url.as_deref().ok_or_else(|| {
        anyhow!(
            "這個版本沒有附可直接更新的執行檔,請到 {} 手動下載",
            release.page_url
        )
    })?;

    let mut reported = 0;
    let bytes = github::download(url, |done, total| {
        if done - reported >= PROGRESS_STEP || Some(done) == total {
            reported = done;
            let _ = tx.send(UiEvent::Update(UpdateEvent::Progress { done, total }));
        }
    })?;

    verify(release, &bytes)?;
    install::apply(&bytes)
}

/// 對過 SHA256 才敢覆蓋執行檔。
///
/// 校驗檔缺席就整個放棄,不「那就不驗了吧」——這是自動下載一顆待會就要執行的
/// 二進位檔,沒有東西可以核對的話,請使用者自己去 Release 頁面下載比較誠實。
fn verify(release: &Release, bytes: &[u8]) -> Result<()> {
    let url = release.sha256_url.as_deref().ok_or_else(|| {
        anyhow!(
            "這個版本沒有附 SHA256 校驗檔,為安全起見不自動更新,請到 {} 手動下載",
            release.page_url
        )
    })?;

    let raw = github::download(url, |_, _| {}).context("下載 SHA256 校驗檔失敗")?;
    let text = String::from_utf8(raw).context("SHA256 校驗檔不是文字")?;
    let expected = github::expected_hash(&text)
        .ok_or_else(|| anyhow!("SHA256 校驗檔的格式不符預期,無法核對下載內容"))?;

    let actual = github::sha256_hex(bytes);
    if actual != expected {
        bail!("下載內容的 SHA256 對不上(預期 {expected},實際 {actual}),已放棄更新");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str) -> Release {
        Release {
            tag: tag.into(),
            notes: String::new(),
            page_url: "https://example.invalid".into(),
            exe_url: Some("https://example.invalid/x.exe".into()),
            sha256_url: Some("https://example.invalid/x.exe.sha256".into()),
        }
    }

    #[test]
    fn the_current_version_is_not_an_update() {
        assert!(matches!(
            classify(release(&format!("v{CURRENT}")), None, false),
            UpdateEvent::UpToDate
        ));
    }

    #[test]
    fn a_newer_tag_is_offered() {
        assert!(matches!(
            classify(release("v999.0.0"), None, false),
            UpdateEvent::Available(_)
        ));
    }

    /// 按過「略過這個版本」之後,開機自動檢查不該再拿同一版來煩人。
    #[test]
    fn a_skipped_version_stays_quiet_on_the_automatic_check() {
        assert!(matches!(
            classify(release("v999.0.0"), Some("v999.0.0"), false),
            UpdateEvent::UpToDate
        ));
    }

    /// 但使用者自己按「檢查更新」時要照實回答,不然他會以為功能壞了。
    #[test]
    fn a_manual_check_ignores_the_skip_list() {
        assert!(matches!(
            classify(release("v999.0.0"), Some("v999.0.0"), true),
            UpdateEvent::Available(_)
        ));
    }

    /// 略過的是別的版本,新版照樣要提示。
    #[test]
    fn skipping_one_version_does_not_skip_the_next() {
        assert!(matches!(
            classify(release("v999.0.1"), Some("v999.0.0"), false),
            UpdateEvent::Available(_)
        ));
    }

    /// 沒有校驗檔就不更新 —— 這條規則被放寬的話,更新流程等於在下載任意執行檔。
    #[test]
    fn a_release_without_a_checksum_is_refused() {
        let mut r = release("v999.0.0");
        r.sha256_url = None;
        let err = verify(&r, b"whatever").unwrap_err().to_string();
        assert!(err.contains("SHA256"), "錯誤訊息要講清楚為什麼不更新:{err}");
    }

    /// 真的去打一次 GitHub。需要網路,所以標了 `#[ignore]`,CI 的 `cargo test`
    /// 不會跑到;動過 `github.rs`(HTTP 設定、JSON 欄位、附件命名)之後手動跑:
    ///
    /// ```text
    /// cargo test -- --ignored --nocapture
    /// ```
    ///
    /// 這一段是唯一能證明「TLS、User-Agent、回應格式、附件名稱」全都對得上的
    /// 檢查 —— 其餘測試都只在本機的假資料上打轉。
    #[test]
    #[ignore = "需要網路"]
    fn the_real_repo_still_answers_in_the_shape_we_expect() {
        let r = github::latest_release().expect("查不到最新 Release");
        println!("tag={} exe={:?}", r.tag, r.exe_url);
        assert!(version::is_newer(&r.tag, "0.0.0"), "tag 應該解析得出來:{}", r.tag);
        assert!(r.page_url.contains("mushroom-chat-helper"));
        // exe_url 只在「發過帶自動更新附件的版本」之後才會有,這裡不強制要求。
    }
}
