//! 設定檔:頻道色票、ROI、檢視定義。存於執行檔旁的 maple_chat_filter.toml。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 一個聊天頻道的定義(以文字顏色辨識)。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelDef {
    pub id: String,
    pub name: String,
    /// 文字顏色 RGB。預設值為近似值,請用「吸取畫面顏色」功能校正。
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

/// 目標視窗解析度(一般設定選項):選定後直接套用該解析度下實測的聊天 ROI 與
/// 主畫面判斷 ROI(見下方各 CHAT_ROI_*/GATE_ROI_* 常數),不必手動輸入座標、
/// 也不是用比例換算——不同解析度下遊戲 UI 不一定照比例縮放,實測才準。
/// 尚未實測的解析度(座標全 0)在 UI 上仍可選,但要記得先到「進階設定」補上
/// 實際座標,否則 ROI 太小會被影像管線直接忽略。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum ResolutionPreset {
    R1366x768,
    R1920x1080,
    R2560x1440,
    R3840x2160,
}

impl ResolutionPreset {
    pub const ALL: [ResolutionPreset; 4] = [
        ResolutionPreset::R1366x768,
        ResolutionPreset::R1920x1080,
        ResolutionPreset::R2560x1440,
        ResolutionPreset::R3840x2160,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ResolutionPreset::R1366x768 => "1366 x 768",
            ResolutionPreset::R1920x1080 => "1920 x 1080",
            ResolutionPreset::R2560x1440 => "2560 x 1440",
            ResolutionPreset::R3840x2160 => "3840 x 2160",
        }
    }

    fn dims(self) -> (u32, u32) {
        match self {
            ResolutionPreset::R1366x768 => (1366, 768),
            ResolutionPreset::R1920x1080 => (1920, 1080),
            ResolutionPreset::R2560x1440 => (2560, 1440),
            ResolutionPreset::R3840x2160 => (3840, 2160),
        }
    }

    /// 依實際擷取到的畫面寬高找對應的解析度選項(需完全相符),供「一般設定」
    /// 自動選擇目標解析度使用;找不到就回傳 None,交給使用者手動選。
    pub fn from_dims(w: u32, h: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.dims() == (w, h))
    }

    /// 此解析度下實測的聊天 ROI。
    pub fn chat_roi(self) -> Roi {
        match self {
            ResolutionPreset::R1366x768 => CHAT_ROI_1366X768,
            ResolutionPreset::R1920x1080 => CHAT_ROI_1920X1080,
            ResolutionPreset::R2560x1440 => CHAT_ROI_2560X1440,
            ResolutionPreset::R3840x2160 => CHAT_ROI_3840X2160,
        }
    }

    /// 此解析度下實測的主畫面判斷 ROI。
    pub fn gate_roi(self) -> Roi {
        match self {
            ResolutionPreset::R1366x768 => GATE_ROI_1366X768,
            ResolutionPreset::R1920x1080 => GATE_ROI_1920X1080,
            ResolutionPreset::R2560x1440 => GATE_ROI_2560X1440,
            ResolutionPreset::R3840x2160 => GATE_ROI_3840X2160,
        }
    }
}

const CHAT_ROI_1366X768: Roi = Roi {
    x: 283,
    y: 716,
    w: 552,
    h: 14,
};
const GATE_ROI_1366X768: Roi = Roi {
    x: 857,
    y: 764,
    w: 51,
    h: 33,
};

const CHAT_ROI_1920X1080: Roi = Roi {
    x: 561,
    y: 1029,
    w: 552,
    h: 14,
};
const GATE_ROI_1920X1080: Roi = Roi {
    x: 1134,
    y: 1076,
    w: 51,
    h: 33,
};

const CHAT_ROI_2560X1440: Roi = Roi {
    x: 530,
    y: 1286,
    w: 1030,
    h: 24,
};
const GATE_ROI_2560X1440: Roi = Roi {
    x: 1610,
    y: 1380,
    w: 85,
    h: 55,
};

// 尚未實測,先留空(全 0)。
const CHAT_ROI_3840X2160: Roi = Roi {
    x: 0,
    y: 0,
    w: 0,
    h: 0,
};
const GATE_ROI_3840X2160: Roi = Roi {
    x: 0,
    y: 0,
    w: 0,
    h: 0,
};

/// ROI 設定模式。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum RoiMode {
    /// 一般設定:依選定的目標解析度自動換算 ROI。
    Preset(ResolutionPreset),
    /// 進階設定:手動輸入 ROI 座標。
    Advanced,
}

impl Default for RoiMode {
    fn default() -> Self {
        RoiMode::Preset(ResolutionPreset::R2560x1440)
    }
}

/// 主畫面判斷範圍(ROI 判斷視窗):偵測目前畫面是否在遊戲主畫面(有聊天視窗)。
/// 使用者進入商城、拍賣等其他畫面時該範圍內不會出現指定顏色,藉此避免
/// 持續套用聊天判斷邏輯而在這些畫面上誤判出訊息。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct GateRoi {
    /// 是否啟用主畫面判斷。
    pub enabled: bool,
    /// 判斷範圍(視窗內像素座標)。
    pub roi: Roi,
    /// 判斷用的目標顏色 RGB。
    pub color: [u8; 3],
    /// 每個色版允許的最大差值。
    pub tolerance: u8,
}

impl Default for GateRoi {
    fn default() -> Self {
        Self {
            enabled: true,
            roi: GATE_ROI_2560X1440,
            color: [204, 0, 34],
            tolerance: 30,
        }
    }
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
        Self {
            ctrl: false,
            alt: false,
            shift: true,
            win: false,
            key: "Tab".into(),
        }
    }
}

/// 選「全部」檢視時的行為。
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum AllViewMode {
    /// 顯示擷取到的全部對話截圖(不套用任何頻道過濾)。
    ShowMessages,
    /// 直接隱藏聊天內容區(視窗收合到只剩工具列),讓使用者看到遊戲原始畫面。
    #[default]
    HideChat,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 視窗清單篩選關鍵字。
    pub window_title_hint: String,
    /// 每秒處理幀數(2~15)。
    pub fps: u32,
    pub roi: Roi,
    /// 主畫面判斷。
    pub gate: GateRoi,
    /// ROI 設定模式:一般(依解析度自動換算)或進階(手動輸入)。
    pub roi_mode: RoiMode,
    /// 進階設定裡手動輸入的聊天 ROI,獨立存放不受「一般設定」換算覆蓋——
    /// 這樣切去一般設定再切回進階設定時,手動輸入的值還在。
    pub custom_roi: Roi,
    /// 進階設定裡手動輸入的主畫面判斷 ROI,理由同 custom_roi。
    pub custom_gate_roi: Roi,
    /// 整個應用程式的文字大小基準值(px)。套用到所有 egui 文字樣式
    /// (標題/內文/按鈕/等寬字等等都依此等比縮放),不只是聊天時間戳/頻道標籤。
    pub ui_font_px: f32,
    /// 擷取到的訊息畫面顯示放大倍率(原始文字通常很小,需要放大才看得清楚)。
    pub image_scale: f32,
    /// 是否在聊天視窗內顯示每則訊息的時間戳記/頻道標籤。關閉的話直接把
    /// 擷取到的截圖放進聊天室,不額外顯示這些資訊。
    pub show_message_meta: bool,
    /// 啟用主畫面判斷時,不在遊戲主畫面(如商城、拍賣)時是否自動把聊天
    /// 視窗搬到螢幕外隱藏,主畫面再次出現時自動搬回來。關閉的話聊天視窗
    /// 不會因為主畫面判斷而自動隱藏/還原。
    pub gate_auto_hide_chat: bool,
    /// 畫面去重時間窗(秒):內容相似的訊息畫面(比對頻道文字遮罩的重疊比例)
    /// 在這段時間內只會顯示一次,避免同一則訊息因背景抖動重新觸發而洗版。
    pub dedup_seconds: u64,
    pub channels: Vec<ChannelDef>,
    pub views: Vec<ViewDef>,
    /// 系統層級快捷鍵:循環切換聊天檢視(全部→自訂檢視→…)。
    pub hotkey: HotkeyDef,
    /// 選「全部」檢視時的行為。
    pub all_view_mode: AllViewMode,
    /// 是否置頂(啟動時還原,設定視窗也會跟著置頂)。
    pub always_on_top: bool,
    /// 主視窗上次的位置(監視器座標,egui points)。None 表示交給作業系統決定。
    pub window_pos: Option<[f32; 2]>,
    /// 主視窗上次的大小(egui points)。收合成只剩工具列高度時不會覆寫這個值。
    pub window_size: Option<[f32; 2]>,
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
            // 這個字串同時是視窗清單的篩選關鍵字,以及自動開始擷取的依據:啟動時
            // 若剛好有視窗標題與它完全相符,就直接開始擷取(見 ui::App::new)。
            window_title_hint: "新楓之谷：經典版".into(),
            fps: 8,
            roi: CHAT_ROI_2560X1440,
            gate: GateRoi::default(),
            roi_mode: RoiMode::default(),
            custom_roi: CHAT_ROI_2560X1440,
            custom_gate_roi: GATE_ROI_2560X1440,
            ui_font_px: 16.0,
            image_scale: 3.0,
            show_message_meta: false,
            gate_auto_hide_chat: true,
            dedup_seconds: 20,
            // 顏色為近似預設值,務必用「頻道」分頁的色票 + 吸取畫面顏色校正。
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
                ViewDef {
                    name: "社交".into(),
                    channels: vec!["whisper".into(), "buddy".into(), "guild".into()],
                },
                ViewDef {
                    name: "隊伍".into(),
                    channels: vec!["party".into()],
                },
            ],
            hotkey: HotkeyDef::default(),
            all_view_mode: AllViewMode::default(),
            always_on_top: false,
            window_pos: None,
            window_size: None,
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
