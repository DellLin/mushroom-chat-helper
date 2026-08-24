//! GitHub Releases API:查最新版、下載附件、算 SHA256。

use std::io::Read;
use std::time::Duration;

use anyhow::{bail, Context, Result};

/// 發佈來源。fork 出去自己維護的話只要改這一行。
const REPO: &str = "DellLin/mushroom-chat-helper";

/// GitHub API 會用 403 擋掉沒有 User-Agent 的請求,一定要帶。
const USER_AGENT: &str = concat!("mushroom-chat-helper/", env!("CARGO_PKG_VERSION"));

/// Release 附件裡那顆可以直接換上去的執行檔(見 `.github/workflows/release.yml`)。
/// 檔名刻意不帶版號,更新流程才不用自己拼字串猜檔名。校驗檔是它加上 `.sha256`。
pub const EXE_ASSET: &str = "mushroom-chat-helper.exe";

/// 查版號用的請求該在幾秒內講完。開機時跑在背景執行緒,拖太久沒有意義。
const API_TIMEOUT: Duration = Duration::from_secs(20);

/// 連線建立階段的上限。下載本身不設總時限(檔案大、網路慢都是合理的),
/// 但「連不上」要很快就放棄。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 單一附件的下載上限。對方回一個無限長的 body 時用來保住記憶體 ——
/// 這個專案的執行檔大約 10 MB,128 MB 已經寬鬆到不可能誤擋。
const MAX_DOWNLOAD_BYTES: u64 = 128 * 1024 * 1024;

/// GitHub 上的一個 Release。
#[derive(Clone, Debug)]
pub struct Release {
    /// tag,例:`v0.3.0`。
    pub tag: String,
    /// Release 說明。GitHub 給的是 Markdown 原文,UI 直接當純文字顯示。
    pub notes: String,
    /// 這個 Release 的網頁。自動更新走不通時請使用者手動下載用。
    pub page_url: String,
    /// 可直接下載的執行檔。舊版 release workflow 沒上傳這顆附件,所以是 `Option`。
    pub exe_url: Option<String>,
    /// 上面那顆執行檔的 SHA256 校驗檔。
    pub sha256_url: Option<String>,
}

/// GitHub API 回應裡我們用得到的欄位(其餘一律忽略)。
#[derive(serde::Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(serde::Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
}

fn agent(global_timeout: Option<Duration>) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(global_timeout)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .build()
        .into()
}

/// 查這個 repo 最新的正式 Release。
///
/// 用的是 `/releases/latest`,GitHub 在這個端點本來就會排除掉 draft 與
/// pre-release,不必自己過濾。
pub fn latest_release() -> Result<Release> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body = agent(Some(API_TIMEOUT))
        .get(&url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .context("連不上 GitHub")?
        .into_body()
        .read_to_string()
        .context("讀取 GitHub 回應失敗")?;

    let api: ApiRelease = serde_json::from_str(&body).context("GitHub 回應的格式不符預期")?;
    let asset = |name: &str| {
        api.assets
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.browser_download_url.clone())
    };
    let exe_url = asset(EXE_ASSET);
    let sha256_url = asset(&format!("{EXE_ASSET}.sha256"));

    Ok(Release {
        tag: api.tag_name,
        notes: api.body.unwrap_or_default(),
        page_url: api.html_url,
        exe_url,
        sha256_url,
    })
}

/// 下載一個附件,邊下載邊回報 `(已下載, 總長度)`。總長度來自 `Content-Length`,
/// 對方沒給就是 `None`(UI 那邊改顯示不定量的進度)。
pub fn download(url: &str, mut on_progress: impl FnMut(u64, Option<u64>)) -> Result<Vec<u8>> {
    let resp = agent(None)
        .get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .with_context(|| format!("下載失敗:{url}"))?;

    let total = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if let Some(total) = total.filter(|t| *t > MAX_DOWNLOAD_BYTES) {
        bail!("附件大得不合理({total} bytes),放棄下載");
    }

    // 先照 Content-Length 配好空間,省下一路 realloc;沒給就從 0 長起。
    let mut buf = Vec::with_capacity(total.unwrap_or(0) as usize);
    let mut chunk = vec![0u8; 64 * 1024];
    let mut reader = resp.into_body().into_reader();
    loop {
        let n = reader.read(&mut chunk).context("下載中斷")?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        // Content-Length 可以騙人,實際收到的量也要自己盯著。
        if buf.len() as u64 > MAX_DOWNLOAD_BYTES {
            bail!("下載內容超過上限,放棄");
        }
        on_progress(buf.len() as u64, total);
    }
    Ok(buf)
}

/// 從校驗檔內容取出雜湊值。格式是 release workflow 產的 `<hex>  <檔名>`。
pub fn expected_hash(sha256_file: &str) -> Option<String> {
    let hash = sha256_file.split_whitespace().next()?;
    // 長度/字元都對才算數,免得把「404 Not Found」這種東西當成雜湊值拿去比。
    let looks_like_sha256 =
        hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit());
    looks_like_sha256.then(|| hash.to_ascii_lowercase())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().fold(String::with_capacity(64), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 對照值來自 `echo -n "" | sha256sum` 與 `echo -n "abc" | sha256sum`。
    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// release workflow 寫出來的格式:`<hex>  <檔名>`,後面還有一個換行。
    #[test]
    fn hash_is_extracted_from_the_workflow_format() {
        let line = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  mushroom-chat-helper.exe\n";
        assert_eq!(
            expected_hash(line).as_deref(),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
    }

    #[test]
    fn uppercase_hashes_are_normalised() {
        let upper = "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855  x.exe";
        assert_eq!(expected_hash(upper).as_deref().map(str::len), Some(64));
        assert!(expected_hash(upper).is_some_and(|h| h.starts_with("e3b0")));
    }

    /// 抓到的不是校驗檔(例如 GitHub 回了一頁錯誤訊息)時要看得出來,
    /// 不能把垃圾當成雜湊值去比對 —— 那會變成「雜湊對不上」的誤導訊息。
    #[test]
    fn non_hash_content_is_rejected() {
        assert_eq!(expected_hash(""), None);
        assert_eq!(expected_hash("404: Not Found"), None);
        assert_eq!(expected_hash("deadbeef  x.exe"), None, "長度不足 64 不算");
        let non_hex = "z".repeat(64);
        assert_eq!(expected_hash(&non_hex), None, "非 16 進位字元不算");
    }
}
