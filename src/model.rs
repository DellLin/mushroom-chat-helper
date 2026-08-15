//! 共用資料模型與執行緒間訊息定義。

use chrono::{DateTime, Local};

/// 頻道識別字串(對應 Config.channels[].id;"unknown" 為未分類)。
pub type ChannelId = String;

/// 一則已解析的聊天訊息。
#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub time: DateTime<Local>,
    pub channel: ChannelId,
    pub sender: Option<String>,
    pub content: String,
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
    Message(ChatMessage),
    /// 已縮小的全視窗預覽(RGBA),附原始 frame 尺寸供 ROI 座標換算。
    Preview {
        frame_w: u32,
        frame_h: u32,
        w: usize,
        h: usize,
        rgba: Vec<u8>,
    },
    /// 校準模式下,一行文字的主色、OCR 結果,以及送進 OCR 引擎的實際畫面
    /// (黑字白底、已放大)——方便判斷辨識錯誤是不是遮罩/裁切造成的。
    CalibSample {
        color: [u8; 3],
        text: String,
        img_w: u32,
        img_h: u32,
        img_rgba: Vec<u8>,
    },
    CaptureState(CaptureState),
    Error(String),
    /// 系統層級快捷鍵被按下(切換聊天檢視用)。
    HotkeyTriggered,
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
