//! 共用資料模型與執行緒間訊息定義。

use chrono::{DateTime, Local};

/// 頻道識別字串(對應 Config.channels[].id)。
pub type ChannelId = String;

/// 一則聊天訊息:直接擷取的畫面(不做 OCR/文字辨識),只依文字顏色分類頻道。
#[derive(Clone, Debug)]
pub struct ChatImage {
    pub time: DateTime<Local>,
    pub channel: ChannelId,
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// 擷取執行緒的目前狀態(顯示於 UI)。
#[derive(Clone, Debug, PartialEq)]
pub enum CaptureState {
    Idle,
    Running(String),
    Failed(String),
}

/// UI 執行緒接收的事件。
pub enum UiEvent {
    Message(ChatImage),
    /// 已縮小的全視窗預覽(RGBA),附原始 frame 尺寸供 ROI 座標換算。
    Preview {
        frame_w: u32,
        frame_h: u32,
        w: usize,
        h: usize,
        rgba: Vec<u8>,
    },
    /// 應 UI 要求送出的單張全解析度畫面(未縮放),供「在畫面上框選 ROI」使用。
    FullFrame {
        w: u32,
        h: u32,
        rgba: Vec<u8>,
    },
    CaptureState(CaptureState),
    /// 主畫面判斷狀態改變時送出:true = 目前在遊戲主畫面(有聊天視窗),
    /// false = 不在(例如商城、拍賣等畫面)。啟用主畫面判斷時停用主視窗一律
    /// 視為 true。UI 收到 false 時隱藏整個聊天視窗,收到 true 時還原顯示。
    GateState(bool),
    Error(String),
    /// 系統層級快捷鍵被按下(切換聊天檢視用)。
    HotkeyTriggered,
    /// 自動更新流程的進度(檢查版號 / 下載 / 安裝完成 / 失敗)。
    Update(crate::update::UpdateEvent),
}

/// UI → 擷取執行緒的指令。
pub enum CaptureCmd {
    Start { hwnd: isize, title: String },
    Stop,
    Shutdown,
}

/// UI → 快捷鍵執行緒的指令。
pub enum HotkeyCmd {
    SetCombo { ctrl: bool, alt: bool, shift: bool, win: bool, vk: u32 },
    Shutdown,
}

/// 擷取執行緒 → 影像管線的一張畫面(BGRA、緊密排列)。
pub struct FramePacket {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}
