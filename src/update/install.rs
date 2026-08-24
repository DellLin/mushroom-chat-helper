//! 把下載回來的執行檔換到定位。
//!
//! Windows 不讓你覆寫正在執行的 exe,但**允許改它的名字**。整個流程就架在這件
//! 事情上:先把自己改名讓出檔名 → 新檔搬進來 → 啟動新的、自己結束。舊檔這一刻
//! 還在跑,刪不掉,留到下次啟動再清(見 [`cleanup_stale`])。
//!
//! 中途失敗一定要把舊檔搬回去。這裡動的是使用者唯一那顆執行檔,留下一個
//! 「新的還沒到、舊的已經改名」的狀態等於把程式弄不見。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// 換裝過程中會短暫出現在執行檔旁邊的兩個檔名後綴。
/// `.new` 是還沒就位的新版,`.old` 是讓出檔名、等著被刪的舊版。
const STAGED_SUFFIX: &str = ".new";
const BACKUP_SUFFIX: &str = ".old";

/// `foo.exe` + `.old` → `foo.exe.old`(接在整個檔名後面,不是換掉副檔名)。
fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    exe.with_file_name(name)
}

/// 用 `bytes` 取代目前正在執行的執行檔,回傳它的路徑。
pub fn apply(bytes: &[u8]) -> Result<PathBuf> {
    let exe = std::env::current_exe().context("找不到目前的執行檔路徑")?;
    swap_in_place(&exe, bytes)?;
    Ok(exe)
}

/// [`apply`] 的實作本體。拆出來才測得到 —— 測試不可能真的去換掉正在跑測試的
/// 那顆執行檔,但這段(改名的順序、失敗時的回復)正是最需要被測的地方。
fn swap_in_place(exe: &Path, bytes: &[u8]) -> Result<()> {
    let staged = sibling(exe, STAGED_SUFFIX);
    let backup = sibling(exe, BACKUP_SUFFIX);

    std::fs::write(&staged, bytes).with_context(|| {
        format!("寫入 {} 失敗(安裝目錄可能沒有寫入權限)", staged.display())
    })?;

    // 上次更新留下、當時刪不掉的 .old 可能還在,先讓開位子。
    let _ = std::fs::remove_file(&backup);

    if let Err(e) = std::fs::rename(exe, &backup) {
        let _ = std::fs::remove_file(&staged);
        return Err(e).context("無法把舊版執行檔改名(可能被防毒軟體鎖住)");
    }
    if let Err(e) = std::fs::rename(&staged, exe) {
        // 舊檔已經讓出檔名了,務必搬回去:少了這一步,使用者下次啟動會發現
        // 執行檔整個不見。
        let _ = std::fs::rename(&backup, exe);
        let _ = std::fs::remove_file(&staged);
        return Err(e).context("無法把新版執行檔搬到定位");
    }
    Ok(())
}

/// 啟動指定的執行檔。呼叫端接著要讓自己結束,新舊兩份才不會同時跑。
pub fn relaunch(exe: &Path) -> Result<()> {
    std::process::Command::new(exe)
        .spawn()
        .with_context(|| format!("啟動 {} 失敗", exe.display()))?;
    Ok(())
}

/// 清掉上一次更新留下的檔案。啟動時呼叫 —— 那時舊版行程已經結束,`.old` 才刪得掉。
///
/// 刪不掉就算了:那只是一個佔空間的檔案,不值得為它中斷啟動或去煩使用者,
/// 下次啟動會再試一次。
pub fn cleanup_stale() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    for suffix in [BACKUP_SUFFIX, STAGED_SUFFIX] {
        let path = sibling(&exe, suffix);
        if path.exists() {
            match std::fs::remove_file(&path) {
                Ok(()) => log::info!("已清除更新殘留檔 {}", path.display()),
                Err(e) => log::debug!("清除 {} 失敗(下次啟動再試): {e}", path.display()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每個測試自己一間房,避免平行執行時互相踩到檔名。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            // 不為了測試拉一個 tempfile 相依:pid + 標籤就足以避開碰撞。
            let dir = std::env::temp_dir()
                .join(format!("mch-update-test-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("建不出暫存目錄");
            Self(dir)
        }
        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn suffix_is_appended_to_the_whole_file_name() {
        // 不是 with_extension —— 那會把 foo.exe 變成 foo.old,撞掉別的東西。
        let p = sibling(Path::new(r"C:\app\mushroom-chat-helper.exe"), ".old");
        assert_eq!(p.file_name().unwrap(), "mushroom-chat-helper.exe.old");
        assert_eq!(p.parent().unwrap(), Path::new(r"C:\app"));
    }

    #[test]
    fn swap_replaces_the_exe_and_keeps_the_old_one_around() {
        let dir = TempDir::new("swap");
        let exe = dir.join("app.exe");
        std::fs::write(&exe, b"OLD").unwrap();

        swap_in_place(&exe, b"NEW").expect("換裝應該要成功");

        assert_eq!(std::fs::read(&exe).unwrap(), b"NEW", "執行檔要換成新版");
        assert_eq!(
            std::fs::read(dir.join("app.exe.old")).unwrap(),
            b"OLD",
            "舊版要留在 .old 等下次啟動清掉"
        );
        assert!(!dir.join("app.exe.new").exists(), ".new 應該已經改名走了");
    }

    /// 連續更新兩次:第一次留下的 .old 不能擋住第二次。
    #[test]
    fn a_leftover_backup_does_not_block_the_next_update() {
        let dir = TempDir::new("leftover");
        let exe = dir.join("app.exe");
        std::fs::write(&exe, b"V1").unwrap();
        std::fs::write(dir.join("app.exe.old"), b"V0").unwrap();

        swap_in_place(&exe, b"V2").expect("上次的 .old 還在也要能更新");

        assert_eq!(std::fs::read(&exe).unwrap(), b"V2");
        assert_eq!(std::fs::read(dir.join("app.exe.old")).unwrap(), b"V1");
    }

    /// 寫不進去(目錄不存在,等同沒有寫入權限)時,原本的執行檔必須毫髮無傷。
    #[test]
    fn a_failed_swap_leaves_the_original_exe_in_place() {
        let dir = TempDir::new("failed");
        let exe = dir.join("nope").join("app.exe");
        assert!(swap_in_place(&exe, b"NEW").is_err(), "寫不進去就該失敗");
        assert!(!exe.exists());

        // 真的存在的執行檔,則要維持可執行狀態。
        let exe = dir.join("app.exe");
        std::fs::write(&exe, b"OLD").unwrap();
        // 先佔住 .new 這個名字當作目錄,讓 fs::write 一定失敗。
        std::fs::create_dir_all(dir.join("app.exe.new")).unwrap();

        assert!(swap_in_place(&exe, b"NEW").is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"OLD", "舊版必須還在原位");
    }

    #[test]
    fn cleanup_ignores_a_directory_without_leftovers() {
        // 沒有殘留檔時 cleanup_stale 不該 panic(它對現役執行檔跑,不能出事)。
        cleanup_stale();
    }
}
