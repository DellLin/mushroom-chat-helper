//! 設定檔:頻道色票、ROI、檢視定義。存於執行檔旁的 maple_chat_filter.toml。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 一個聊天頻道的定義(以文字顏色辨識)。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelDef {
    pub id: String,
    pub name: String,
    /// 文字顏色 RGB。預設值為近似值,請用校準模式校正。
    pub color: [u8; 3],
    /// 每個色版允許的最大差值。
    pub tolerance: u8,
    pub enabled: bool,
}

/// 使用者自訂檢視(分頁),由多個頻道組成。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ViewDef {
    pub name: String,
    pub channels: Vec<String>,
}

/// 聊天區域(視窗內像素座標)。
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Roi {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// 切換聊天檢視的系統層級快捷鍵(遊戲有焦點時也能觸發)。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HotkeyDef {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    /// 按鍵名稱,見 hotkey::vk_for_name。
    pub key: String,
}

impl Default for HotkeyDef {
    fn default() -> Self {
        Self { ctrl: false, alt: false, shift: true, win: false, key: "Tab".into() }
    }
}

/// 選「全部」檢視時的行為。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum AllViewMode {
    /// 顯示 OCR 擷取到的全部對話(不套用任何頻道過濾)。
    ShowMessages,
    /// 直接隱藏聊天內容區(視窗收合到只剩工具列),讓使用者看到遊戲原始畫面。
    HideChat,
}

impl Default for AllViewMode {
    fn default() -> Self {
        AllViewMode::HideChat
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 視窗清單篩選關鍵字。
    pub window_title_hint: String,
    /// 每秒處理幀數(2~15)。
    pub fps: u32,
    pub roi: Roi,
    /// OCR 語言標籤,新楓之谷(台服)為 zh-Hant。
    pub ocr_language: String,
    /// OCR 前放大倍率。
    pub ocr_scale: u32,
    /// 切行方式:從 ROI 最上面開始,固定以此高度(px)切成一則則候選訊息。
    pub line_height_px: u32,
    /// 聊天檢視的文字大小(px),用來讓視窗大小/字級貼近遊戲內聊天視窗,方便疊在上面。
    pub chat_font_px: f32,
    /// 是否顯示無法分類頻道的訊息。
    pub include_unknown: bool,
    /// OCR 後相同文字的去重時間窗(秒)。
    pub dedup_seconds: u64,
    pub channels: Vec<ChannelDef>,
    pub views: Vec<ViewDef>,
    /// 系統層級快捷鍵:循環切換聊天檢視(全部→自訂檢視→…)。
    pub hotkey: HotkeyDef,
    /// 選「全部」檢視時的行為。
    pub all_view_mode: AllViewMode,
}

impl Default for Config {
    fn default() -> Self {
        let ch = |id: &str, name: &str, color: [u8; 3], tol: u8, enabled: bool| ChannelDef {
            id: id.into(),
            name: name.into(),
            color,
            tolerance: tol,
            enabled,
        };
        Self {
            // 只用「新楓之谷」四字篩選(不含冒號/版本後綴),避免全形/半形冒號打錯字導致篩選不到。
            window_title_hint: "新楓之谷".into(),
            fps: 8,
            roi: Roi { x: 8, y: 400, w: 500, h: 260 },
            ocr_language: "zh-Hant".into(),
            ocr_scale: 3,
            line_height_px: 25,
            chat_font_px: 16.0,
            include_unknown: false,
            // 固定網格切行下,同一則訊息會隨畫面捲動移進不同格子,重新觸發分類/OCR;
            // 真正擋住重複顯示的只有這個文字內容時間窗,留寬一點避免聊天不夠熱絡時
            // 舊訊息捲到新格子又被當成新訊息送出。
            dedup_seconds: 120,
            // 顏色為近似預設值,務必用「頻道/校準」分頁校正。
            channels: vec![
                ch("general", "一般", [255, 255, 255], 20, true),
                ch("whisper", "密語", [118, 240, 118], 48, true),
                ch("buddy", "好友", [255, 238, 120], 48, true),
                ch("party", "隊伍", [160, 205, 255], 48, true),
                ch("guild", "公會", [200, 255, 160], 40, true),
                ch("system", "系統", [255, 175, 80], 55, true),
                ch("mega", "廣播", [255, 150, 210], 55, false),
            ],
            views: vec![
                ViewDef { name: "社交".into(), channels: vec!["whisper".into(), "buddy".into(), "guild".into()] },
                ViewDef { name: "隊伍".into(), channels: vec!["party".into()] },
            ],
            hotkey: HotkeyDef::default(),
            all_view_mode: AllViewMode::default(),
        }
    }
}

impl Config {
    pub fn channel_by_id(&self, id: &str) -> Option<&ChannelDef> {
        self.channels.iter().find(|c| c.id == id)
    }
}

pub fn config_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("maple_chat_filter.toml")
}

pub fn load() -> Config {
    match std::fs::read_to_string(config_path()) {
        Ok(s) => toml::from_str(&s).unwrap_or_else(|e| {
            log::warn!("設定檔解析失敗,使用預設值: {e}");
            Config::default()
        }),
        Err(_) => Config::default(),
    }
}

pub fn save(cfg: &Config) {
    match toml::to_string_pretty(cfg) {
        Ok(s) => {
            if let Err(e) = std::fs::write(config_path(), s) {
                log::warn!("設定檔寫入失敗: {e}");
            }
        }
        Err(e) => log::warn!("設定檔序列化失敗: {e}"),
    }
}
