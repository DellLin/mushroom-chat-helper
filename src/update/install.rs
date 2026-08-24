//! 把下載回來的執行檔換到定位。
//!
//! Windows 不讓你覆寫正在執行的 exe,但**允許改它的名字**。整個流程就架在這件
//! 事情上:先把自己改名讓出檔名 → 新檔搬進來 → 啟動新的、自己結束。舊檔這一刻
//! 還在跑,刪不掉,留到下次啟動再清(見 [`cleanup_stale`])。
//!
//! 中途失敗一定要把舊檔搬回去。這裡動的是使用者唯一那顆執行檔,留下一個
//! 「新的還沒到、舊的已經改名」的狀態等於把程式弄不見。
//!
//! `relaunch` 啟動新行程時會把自己的 PID 一起帶過去,新行程開機第一件事
//! ([`wait_for_predecessor_exit`])就是等這個 PID 真的死透,才繼續往下做
//! 任何需要獨佔資源的初始化(清 `.old`、註冊全域快捷鍵)。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};

/// 換裝過程中會短暫出現在執行檔旁邊的兩個檔名後綴。
/// `.new` 是還沒就位的新版,`.old` 是讓出檔名、等著被刪的舊版。
const STAGED_SUFFIX: &str = ".new";
const BACKUP_SUFFIX: &str = ".old";

/// 命令列參數前綴,見 [`relaunch`] 與 [`wait_for_predecessor_exit`]。
const WAIT_FOR_EXIT_PREFIX: &str = "--wait-for-exit=";

/// 等舊行程結束的逾時上限。正常情況下舊行程在下一次重繪(至多幾百毫秒)後就會
/// 觸發關閉,這裡給到 5 秒是為了應付系統一時忙碌;真的卡住的話,寧可讓新行程
/// continue 下去正常啟動(頂多快捷鍵註冊失敗、`.old` 晚一次啟動才清掉,這兩者
/// 都不影響核心功能),也不要讓使用者對著一個永遠沒反應的黑畫面等待。
const PREDECESSOR_EXIT_TIMEOUT_MS: u32 = 5000;

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

/// 啟動指定的執行檔,並把自己的 PID 帶給它。呼叫端接著要讓自己結束,新舊兩份
/// 才不會同時跑。
///
/// 新行程收到 PID 後,會先等這個行程真的結束才繼續初始化(見
/// [`wait_for_predecessor_exit`])——舊行程結束前,全域快捷鍵還沒真的釋放、
/// `.old` 也還被鎖著,新行程這時候搶著做這些事只會失敗。
pub fn relaunch(exe: &Path) -> Result<()> {
    std::process::Command::new(exe)
        .arg(format!("{WAIT_FOR_EXIT_PREFIX}{}", std::process::id()))
        .spawn()
        .with_context(|| format!("啟動 {} 失敗", exe.display()))?;
    Ok(())
}

/// 如果命令列帶著 `--wait-for-exit=<pid>`(見 [`relaunch`]),就等那個行程真的
/// 結束再回傳;一般啟動(沒有這個參數)立刻回傳,不影響正常啟動路徑。
///
/// **為什麼需要這一步:**自動更新換裝完、使用者按下「立即重新啟動」時,新行程
/// 幾乎是舊行程還沒來得及關掉的那個瞬間就啟動的——`relaunch()` 只是送出
/// spawn,並不等舊行程真的死。舊行程死透之前,兩件事都還卡著:
/// 全域快捷鍵(`RegisterHotKey`)還沒被系統釋放,新行程搶著註冊同一組會直接
/// 失敗;`.old` 也還被舊行程自己的執行檔映像鎖著,`cleanup_stale()` 這時候
/// 清不掉,要等下一次啟動才有機會。「舊行程收到關閉訊號」跟「舊行程真的
/// 結束」中間有個窗口——送出 `HotkeyCmd::Shutdown` 之後,還要等一輪
/// 25ms 的輪詢、視窗真正關閉、行程物件被系統回收,這段期間新行程完全有可能
/// 已經在搶了。在這裡卡住,把「新行程開始動任何獨佔資源」延到「舊行程真的不
/// 在了」之後,兩個問題同時解決,不必個別去等快捷鍵執行緒或猜多久夠安全。
pub fn wait_for_predecessor_exit() {
    let Some(pid) = std::env::args()
        .find_map(|a| a.strip_prefix(WAIT_FOR_EXIT_PREFIX).map(str::to_owned))
        .and_then(|s| s.parse::<u32>().ok())
    else {
        return;
    };
    log::info!("等待上一個行程(pid {pid})結束後再繼續啟動");
    wait_for_pid_exit(pid, PREDECESSOR_EXIT_TIMEOUT_MS);
}

/// [`wait_for_predecessor_exit`] 的實作本體,拆出 pid 好讓測試能控制等待對象,
/// 不必真的透過命令列參數。
fn wait_for_pid_exit(pid: u32, timeout_ms: u32) {
    // SAFETY:純粹的控制代碼操作,沒有指標參數。pid 開不到代表那個行程已經不
    // 存在了(通常就是我們要等的舊行程早就死了),當作「不用等」處理。
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else {
            return;
        };
        WaitForSingleObject(handle, timeout_ms);
        let _ = CloseHandle(handle);
    }
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

    /// 開一個立刻結束的 `cmd`,等它真的結束再拿 PID——確保測試對的是「這個
    /// PID 現在已經不存在」,不是「這個 PID 還沒真正跑起來」。`exit` 是 cmd
    /// 的內建指令,不經過 PATH 搜尋,不受下面 ping 那個問題影響。
    fn spawn_and_wait_for_exit() -> u32 {
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "exit 0"])
            .spawn()
            .expect("啟動不了 cmd.exe,這台機器的環境有問題");
        let pid = child.id();
        child.wait().expect("等不到 cmd.exe 結束");
        pid
    }

    /// 開一個會活一陣子的行程,用來測「還在跑的時候真的會等」。
    ///
    /// 特意不用 Windows 的 `timeout.exe`:它會用 `GetConsoleMode` 檢查 stdin
    /// 是不是真正的主控台,`cargo test` 這種 stdin 被重導向的環境下會直接印
    /// 「Input redirection is not supported」然後秒退,讓測試看起來過了、其實
    /// 什麼都沒測到。也不用 PATH 搜尋到的 `timeout`——這台機器上 Git for
    /// Windows 的 `usr/bin` 排在 `System32` 前面,`cmd /C timeout ...` 會叫到
    /// GNU coreutils 版本,參數語法完全不通,一樣是秒退。`ping` 不吃 stdin、
    /// 也沒有版本衝突,是這裡唯一乾淨的選擇。
    fn spawn_long_lived_process(ping_count: u32) -> std::process::Child {
        std::process::Command::new("ping")
            .args(["-n", &ping_count.to_string(), "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("啟動不了 ping.exe,這台機器的環境有問題")
    }

    /// 迴歸測試核心案例之一:pid 已經死了,不該傻等到逾時。
    ///
    /// 這條線在真實情境裡對應「呼叫 wait_for_predecessor_exit 時,舊行程剛好
    /// 已經結束了」——不常見,但不該因此白白卡住 5 秒。
    #[test]
    fn a_pid_that_no_longer_exists_returns_immediately() {
        let dead_pid = spawn_and_wait_for_exit();
        let start = std::time::Instant::now();
        wait_for_pid_exit(dead_pid, PREDECESSOR_EXIT_TIMEOUT_MS);
        assert!(
            start.elapsed() < std::time::Duration::from_millis(500),
            "PID 已經不存在,OpenProcess 應該直接失敗回傳,不該還在等"
        );
    }

    /// 核心案例:行程還活著時真的會等,行程一死就立刻回來——不是傻等到逾時,
    /// 也不是完全不等就衝過去。這正是修掉「快捷鍵註冊失敗」那個 bug 的機制。
    #[test]
    fn waiting_stops_as_soon_as_the_process_actually_exits() {
        let mut child = spawn_long_lived_process(2); // 約 1~2 秒
        let pid = child.id();

        let start = std::time::Instant::now();
        wait_for_pid_exit(pid, 10_000); // 逾時給很寬,確認是「等到它死」而不是撞到逾時
        let elapsed = start.elapsed();

        // 下界確認真的等了(不是被改壞成整個不等就直接回來,那樣上界也會過);
        // 上界確認等到的是行程結束、不是傻等到 10 秒逾時。兩邊都要顧到。
        assert!(
            elapsed >= std::time::Duration::from_millis(300),
            "花費時間太短,像是沒有真的等行程結束就回來了:{elapsed:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "行程約 1~2 秒後就會結束,不該拖到 10 秒逾時才回來:{elapsed:?}"
        );
        let _ = child.wait();
    }

    /// 逾時要真的生效:行程一直不結束時,不能讓呼叫端被無限期卡住——這是
    /// 「舊行程真的卡死」時的最後一道保險,新行程還是要能繼續啟動。
    #[test]
    fn a_short_timeout_gives_up_on_a_still_running_process() {
        let mut child = spawn_long_lived_process(60); // 約一分鐘,測試期間必定還活著
        let pid = child.id();

        let start = std::time::Instant::now();
        wait_for_pid_exit(pid, 200); // 故意給很短的逾時
        let elapsed = start.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "逾時應該要生效,不能被一直活著的行程卡住:{elapsed:?}"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    /// `relaunch` 傳給新行程的參數格式,要跟 `wait_for_predecessor_exit` 解析
    /// 的格式對得上——兩邊分開寫容易漂移,用同一個常數才不會各講各的。
    #[test]
    fn the_relaunch_arg_format_matches_what_wait_for_predecessor_exit_parses() {
        let arg = format!("{WAIT_FOR_EXIT_PREFIX}{}", 12345);
        let parsed = arg.strip_prefix(WAIT_FOR_EXIT_PREFIX).and_then(|s| s.parse::<u32>().ok());
        assert_eq!(parsed, Some(12345));
    }
}
