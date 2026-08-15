//! 系統層級全域快捷鍵——用 Win32 RegisterHotKey,遊戲視窗有焦點時也能觸發。
//!
//! 執行緒模型:獨立執行緒註冊熱鍵,輪詢 Win32 訊息佇列偵測 WM_HOTKEY,
//! 同時輪詢命令 channel 讓 UI 可以即時更換組合鍵(先解除註冊舊的再註冊新的)。

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN,
};
use windows::Win32::UI::WindowsAndMessaging::{PeekMessageW, MSG, PM_REMOVE, WM_HOTKEY};

use crate::model::{HotkeyCmd, UiEvent};

const HOTKEY_ID: i32 = 1;

/// 按鍵名稱 → Win32 虛擬鍵碼(VK_*)。只涵蓋常用於快捷鍵的鍵。
const KEY_NAMES: &[(&str, u32)] = &[
    ("Tab", 0x09),
    ("Space", 0x20),
    ("Enter", 0x0D),
    ("Escape", 0x1B),
    ("Backspace", 0x08),
    ("Left", 0x25),
    ("Up", 0x26),
    ("Right", 0x27),
    ("Down", 0x28),
    ("A", 0x41),
    ("B", 0x42),
    ("C", 0x43),
    ("D", 0x44),
    ("E", 0x45),
    ("F", 0x46),
    ("G", 0x47),
    ("H", 0x48),
    ("I", 0x49),
    ("J", 0x4A),
    ("K", 0x4B),
    ("L", 0x4C),
    ("M", 0x4D),
    ("N", 0x4E),
    ("O", 0x4F),
    ("P", 0x50),
    ("Q", 0x51),
    ("R", 0x52),
    ("S", 0x53),
    ("T", 0x54),
    ("U", 0x55),
    ("V", 0x56),
    ("W", 0x57),
    ("X", 0x58),
    ("Y", 0x59),
    ("Z", 0x5A),
    ("0", 0x30),
    ("1", 0x31),
    ("2", 0x32),
    ("3", 0x33),
    ("4", 0x34),
    ("5", 0x35),
    ("6", 0x36),
    ("7", 0x37),
    ("8", 0x38),
    ("9", 0x39),
    ("F1", 0x70),
    ("F2", 0x71),
    ("F3", 0x72),
    ("F4", 0x73),
    ("F5", 0x74),
    ("F6", 0x75),
    ("F7", 0x76),
    ("F8", 0x77),
    ("F9", 0x78),
    ("F10", 0x79),
    ("F11", 0x7A),
    ("F12", 0x7B),
];

pub fn vk_for_name(name: &str) -> Option<u32> {
    KEY_NAMES.iter().find(|(n, _)| *n == name).map(|(_, vk)| *vk)
}

pub fn all_key_names() -> impl Iterator<Item = &'static str> {
    KEY_NAMES.iter().map(|(n, _)| *n)
}

fn modifiers(ctrl: bool, alt: bool, shift: bool, win: bool) -> HOT_KEY_MODIFIERS {
    let mut m = MOD_NOREPEAT;
    if ctrl {
        m |= MOD_CONTROL;
    }
    if alt {
        m |= MOD_ALT;
    }
    if shift {
        m |= MOD_SHIFT;
    }
    if win {
        m |= MOD_WIN;
    }
    m
}

fn register(ctrl: bool, alt: bool, shift: bool, win: bool, vk: u32) -> bool {
    unsafe { RegisterHotKey(None, HOTKEY_ID, modifiers(ctrl, alt, shift, win), vk).is_ok() }
}

fn unregister() {
    unsafe {
        let _ = UnregisterHotKey(None, HOTKEY_ID);
    }
}

pub fn spawn(
    cmd_rx: Receiver<HotkeyCmd>,
    ui_tx: Sender<UiEvent>,
    initial: (bool, bool, bool, bool, u32),
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("hotkey".into())
        .spawn(move || {
            let (ctrl, alt, shift, win, vk) = initial;
            if !register(ctrl, alt, shift, win, vk) {
                let _ = ui_tx.send(UiEvent::Error(
                    "快捷鍵註冊失敗,可能已被其他程式佔用,請到「檢視管理」換一組。".into(),
                ));
            }

            loop {
                unsafe {
                    let mut msg = MSG::default();
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        if msg.message == WM_HOTKEY {
                            let _ = ui_tx.send(UiEvent::HotkeyTriggered);
                        }
                    }
                }

                match cmd_rx.try_recv() {
                    Ok(HotkeyCmd::SetCombo { ctrl, alt, shift, win, vk }) => {
                        unregister();
                        if !register(ctrl, alt, shift, win, vk) {
                            let _ = ui_tx.send(UiEvent::Error(
                                "快捷鍵註冊失敗,可能已被其他程式佔用,請換一組。".into(),
                            ));
                        }
                    }
                    Ok(HotkeyCmd::Shutdown) => {
                        unregister();
                        return;
                    }
                    Err(TryRecvError::Disconnected) => {
                        unregister();
                        return;
                    }
                    Err(TryRecvError::Empty) => {}
                }

                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        })
        .expect("無法建立快捷鍵執行緒")
}
