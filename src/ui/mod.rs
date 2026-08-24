//! egui UI 層:共用狀態與三個 viewport 的編排。
//!
//! `App` 這個結構本身(狀態 + 事件汲取 + `eframe::App` 實作)留在這裡,
//! 實際的繪製依 viewport 分到三個子模組:
//!
//! - [`chat`] — 主視窗:無邊框的聊天疊圖層
//! - [`settings`] — 按 ⚙ 開出來的獨立設定視窗
//! - [`picker`] — 框選 ROI / 吸取顏色共用的全螢幕工具
//!
//! 子模組各自用 `impl super::App` 補上自己的方法,所以三邊共用同一份狀態,
//! 不必再多一層 context 物件轉手。
//!
//! [`update`] 沒有自己的 viewport,它是設定視窗裡的一個分頁(原因見該模組)。

mod chat;
mod picker;
mod settings;
mod update;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};

use crate::capture::win_enum::{self, WindowInfo};
use crate::config::{self, Config, ResolutionPreset, RoiMode};
use crate::model::{CaptureCmd, CaptureState, HotkeyCmd, UiEvent};

/// 聊天室保留的訊息上限。每則訊息都是一張獨立的 GPU 材質,必須設上限
/// 避免材質數量隨執行時間無限成長。
const MAX_MESSAGES: usize = 500;

/// 設定視窗內的分頁(聊天本身不再是分頁,永遠是主視窗內容)。
#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Capture,
    Channels,
    Views,
    Update,
    Help,
}

/// 預覽圖上「拖曳框選」目前套用到哪個範圍。
#[derive(PartialEq, Clone, Copy)]
enum RoiTarget {
    /// 聊天訊息 ROI。
    Chat,
    /// 主畫面判斷 ROI。
    Gate,
}

/// 吸取畫面顏色時,吸到的顏色要寫回哪個欄位。
#[derive(PartialEq, Clone, Copy)]
enum ColorTarget {
    /// `cfg.channels[索引].color`。
    Channel(usize),
    /// `cfg.gate.color`。
    Gate,
}

/// 全螢幕畫面擷取工具目前要做什麼:框選一個矩形範圍,還是吸取單一像素顏色。
/// 兩者共用同一套全螢幕疊圖 + 十字準心 + 放大鏡機制(見 ScreenPick/ui_screen_picker),
/// 只有點擊語意跟套用結果的地方不同。
#[derive(PartialEq, Clone, Copy)]
enum PickMode {
    Roi(RoiTarget),
    Color(ColorTarget),
}

pub struct AppInit {
    pub cfg: Arc<RwLock<Config>>,
    pub interval_ms: Arc<AtomicU64>,
    pub cmd_tx: Sender<CaptureCmd>,
    /// UI 自己也要能送 UiEvent:手動檢查更新/下載更新都是從 UI 起頭,
    /// 結果再經由同一條 channel 回到 drain_events。
    pub ui_tx: Sender<UiEvent>,
    pub ui_rx: Receiver<UiEvent>,
    pub hotkey_tx: Sender<HotkeyCmd>,
    /// 向影像管線要求下一張全解析度畫面(供「在畫面上框選 ROI」/「吸取畫面顏色」使用)。
    pub full_frame_req: Arc<AtomicBool>,
}

struct PreviewState {
    tex: egui::TextureHandle,
    frame_w: u32,
    frame_h: u32,
}

/// 全螢幕畫面擷取工具(框選 ROI / 吸取顏色共用)的進行中狀態。
struct ScreenPick {
    mode: PickMode,
    frame_w: u32,
    frame_h: u32,
    /// 全解析度原始像素(RGBA),供放大鏡逐像素取樣。
    rgba: Vec<u8>,
    tex: egui::TextureHandle,
    /// 點選起點(擷取畫面內座標,與放大鏡顯示的座標同一套換算)。
    /// None 表示還在等待第一次點擊。改成「點兩下」而不是拖曳,避免 egui
    /// 拖曳判定需要先移動超過門檻距離才觸發,導致框選起點跟實際按下的
    /// 位置有落差、看起來像「偏移」。
    click_start: Option<(u32, u32)>,
    /// 目前有效的擷取畫面座標:滑鼠移動時跟著滑鼠算,W/A/S/D 每按一下
    /// 精準微調 1 個像素(顯示畫面可能是縮小顯示,滑鼠一個螢幕像素常常
    /// 已經跨過好幾個擷取畫面像素,單靠滑鼠選不到「中間」的那個像素)。
    effective: (u32, u32),
    /// 上一幀的滑鼠螢幕座標,用來判斷滑鼠是否移動過(移動了就以滑鼠位置
    /// 重新校正 effective,否則保留 WASD 微調過的位置不被滑鼠覆蓋)。
    last_hover: Option<egui::Pos2>,
}

/// 一則聊天訊息:實際擷取到的畫面(不做 OCR),依文字顏色分類頻道。
struct ChatEntry {
    time: chrono::DateTime<chrono::Local>,
    channel: crate::model::ChannelId,
    tex: egui::TextureHandle,
    w: u32,
    h: u32,
}

pub struct App {
    cfg: Arc<RwLock<Config>>,
    interval_ms: Arc<AtomicU64>,
    cmd_tx: Sender<CaptureCmd>,
    ui_tx: Sender<UiEvent>,
    ui_rx: Receiver<UiEvent>,
    hotkey_tx: Sender<HotkeyCmd>,
    full_frame_req: Arc<AtomicBool>,

    tab: Tab,
    messages: VecDeque<ChatEntry>,
    /// None = 「全部」
    active_view: Option<usize>,

    windows: Vec<WindowInfo>,
    selected_win: Option<(isize, String)>,
    preview: Option<PreviewState>,
    drag_start: Option<egui::Pos2>,
    roi_target: RoiTarget,
    /// 一般設定分頁選定的目標解析度(進階模式下仍會記住上次選的值)。
    resolution_preset: ResolutionPreset,
    /// 已按下「在畫面上框選」或「吸取畫面顏色」,正在等待影像管線回傳全解析度
    /// 畫面;記著是哪一種操作,畫面回來後才知道要開哪種全螢幕工具。
    pending_pick: Option<PickMode>,
    /// 全螢幕畫面擷取工具進行中(Some 時每幀都會開一個全螢幕疊圖視窗)。
    screen_pick: Option<ScreenPick>,

    tex_seq: u64,

    capture_state: CaptureState,
    last_error: Option<String>,
    always_on_top: bool,
    font_ok: bool,
    settings_open: bool,
    quit_requested: bool,

    /// 選「全部」時是否已把視窗收合到只剩工具列高度。
    chat_collapsed: bool,
    /// 收合前的視窗高度,切回一般檢視時用來還原。
    saved_height: f32,

    /// 鎖定後主視窗不能拖曳/縮放,避免誤觸移動已經對好位置的疊圖視窗。
    window_locked: bool,

    /// 目前是否在遊戲主畫面(來自 vision 執行緒的 UiEvent::GateState)。
    /// 停用主畫面判斷、或還沒收到任何回報時視為 true(不隱藏)。擷取停止/
    /// 失敗時也強制設回 true,避免視窗卡在隱藏狀態沒有辦法再叫出來。
    gate_open: bool,
    /// 是否已經因為 gate_open == false 把視窗搬到螢幕外。不用
    /// ViewportCommand::Visible 隱藏整個視窗——實測隱藏後 winit 不會再
    /// 收到重繪機會,視窗永遠叫不回來。改成把視窗整個搬到螢幕座標範圍外
    /// (夠遠、不會被任何實際螢幕排列涵蓋到),視窗仍然「可見」但畫面上
    /// 完全看不到、也不會擋到遊戲,主畫面判斷回報 true 時再搬回原本位置。
    chat_hidden_by_gate: bool,
    /// 搬到螢幕外之前記住的視窗位置,還原時用。
    pos_before_gate_hide: Option<[f32; 2]>,
    /// 程式啟動的時間點。啟動後一小段時間內不套用主畫面判斷的自動隱藏,
    /// 「重開一次就看得到視窗」永遠是可行的退路(見 chat::sync_gate_visibility)。
    started_at: Instant,
    /// 是否已經做過「視窗真的生在看得到的地方嗎」的開機檢查(只做一次)。
    window_pos_checked: bool,

    /// 設定視窗「更新」分頁的狀態(見 [`update`])。
    update: update::UpdateUi,

    /// 上一次套用到 egui 全域文字樣式的 ui_font_px,值沒變就不用重算/重設 style。
    applied_font_px: f32,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, init: AppInit) -> Self {
        let font_ok = install_cjk_fonts(&cc.egui_ctx);
        let (hint, always_on_top, resolution_preset) = {
            let cfg = init.cfg.read().unwrap();
            let preset = match cfg.roi_mode {
                RoiMode::Preset(p) => p,
                RoiMode::Advanced => ResolutionPreset::R2560x1440,
            };
            (cfg.window_title_hint.clone(), cfg.always_on_top, preset)
        };
        let windows = filtered_windows(&hint);
        // 視窗標題跟篩選字串完全相符的話,不用等使用者手動選視窗、按開始擷取。
        let auto_start = windows.iter().find(|w| w.title.trim() == hint.trim()).cloned();
        let mut app = Self {
            cfg: init.cfg,
            interval_ms: init.interval_ms,
            cmd_tx: init.cmd_tx,
            ui_tx: init.ui_tx,
            ui_rx: init.ui_rx,
            hotkey_tx: init.hotkey_tx,
            full_frame_req: init.full_frame_req,
            tab: Tab::Capture,
            messages: VecDeque::new(),
            active_view: None,
            windows,
            selected_win: None,
            preview: None,
            drag_start: None,
            roi_target: RoiTarget::Chat,
            resolution_preset,
            pending_pick: None,
            screen_pick: None,
            tex_seq: 0,
            capture_state: CaptureState::Idle,
            last_error: None,
            always_on_top,
            font_ok,
            settings_open: false,
            quit_requested: false,
            chat_collapsed: false,
            saved_height: 320.0,
            window_locked: false,
            gate_open: true,
            chat_hidden_by_gate: false,
            pos_before_gate_hide: None,
            started_at: Instant::now(),
            window_pos_checked: false,
            update: update::UpdateUi::default(),
            applied_font_px: -1.0,
        };

        if let Some(w) = auto_start {
            app.selected_win = Some((w.hwnd, w.title.clone()));
            let _ = app.cmd_tx.send(CaptureCmd::Start { hwnd: w.hwnd, title: w.title });
        }

        app
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        for _ in 0..256 {
            match self.ui_rx.try_recv() {
                Ok(UiEvent::Message(m)) => {
                    let img = egui::ColorImage::from_rgba_unmultiplied(
                        [m.w as usize, m.h as usize],
                        &m.rgba,
                    );
                    self.tex_seq += 1;
                    // 訊息截圖原始文字通常只有幾個像素高,聊天視窗又會放大顯示
                    // (image_scale)——用 NEAREST 會讓每個原始像素變成一大塊
                    // 實心方塊(馬賽克感);改用 LINEAR 讓 GPU 放大時做雙線性
                    // 內插,邊緣才會是平滑的漸層而不是硬塊。
                    let tex = ctx.load_texture(
                        format!("msg{}", self.tex_seq),
                        img,
                        egui::TextureOptions::LINEAR,
                    );
                    self.push_message(ChatEntry {
                        time: m.time,
                        channel: m.channel,
                        tex,
                        w: m.w,
                        h: m.h,
                    });
                }
                Ok(UiEvent::Preview { frame_w, frame_h, w, h, rgba }) => {
                    let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
                    match &mut self.preview {
                        Some(p) => {
                            p.tex.set(img, egui::TextureOptions::NEAREST);
                            p.frame_w = frame_w;
                            p.frame_h = frame_h;
                        }
                        None => {
                            let tex =
                                ctx.load_texture("preview", img, egui::TextureOptions::NEAREST);
                            self.preview = Some(PreviewState { tex, frame_w, frame_h });
                        }
                    }
                    // 一般設定模式下,自動依實際擷取到的畫面解析度切換對應的預設 ROI,
                    // 不用使用者手動選——僅在目前就是一般設定時生效,不會動到進階設定。
                    if let Some(preset) = ResolutionPreset::from_dims(frame_w, frame_h) {
                        let mut cfg = self.cfg.write().unwrap();
                        if let RoiMode::Preset(current) = cfg.roi_mode {
                            self.resolution_preset = preset;
                            if current != preset {
                                cfg.roi_mode = RoiMode::Preset(preset);
                                cfg.roi = preset.chat_roi();
                                cfg.gate.roi = preset.gate_roi();
                            }
                        }
                    }
                }
                Ok(UiEvent::FullFrame { w, h, rgba }) => {
                    if let Some(mode) = self.pending_pick.take() {
                        let img = egui::ColorImage::from_rgba_unmultiplied(
                            [w as usize, h as usize],
                            &rgba,
                        );
                        let tex =
                            ctx.load_texture("screen_pick", img, egui::TextureOptions::NEAREST);
                        self.screen_pick = Some(ScreenPick {
                            mode,
                            frame_w: w,
                            frame_h: h,
                            rgba,
                            tex,
                            click_start: None,
                            effective: (w / 2, h / 2),
                            last_hover: None,
                        });
                    }
                }
                Ok(UiEvent::GateState(open)) => self.gate_open = open,
                Ok(UiEvent::CaptureState(s)) => {
                    // 擷取停止/失敗時不會再有新的 GateState 回報,強制視為
                    // 「在主畫面」,避免視窗卡在隱藏狀態、使用者找不到視窗可以叫回來。
                    if !matches!(s, CaptureState::Running(_)) {
                        self.gate_open = true;
                    }
                    self.capture_state = s;
                }
                Ok(UiEvent::Error(e)) => self.last_error = Some(e),
                Ok(UiEvent::HotkeyTriggered) => self.cycle_view(),
                Ok(UiEvent::Update(e)) => {
                    self.update.apply(e);
                    // 更新的 UI 在設定視窗的「更新」分頁裡。有結果要給使用者看
                    // 的時候自己把那個視窗叫出來並切過去——主視窗可能正收合成
                    // 只剩工具列、或被主畫面判斷搬到螢幕外,靠它是傳不到話的。
                    if self.update.wants_attention() {
                        self.tab = Tab::Update;
                        if self.settings_open {
                            ctx.send_viewport_cmd_to(
                                settings::settings_viewport_id(),
                                egui::ViewportCommand::Focus,
                            );
                        } else {
                            self.settings_open = true;
                        }
                    }
                }
                Err(_) => break,
            }
        }
    }

    /// 快捷鍵觸發:循環切換聊天檢視(全部 → 自訂檢視1 → 自訂檢視2 → … → 全部)。
    fn cycle_view(&mut self) {
        let n = self.cfg.read().unwrap().views.len();
        if n == 0 {
            self.active_view = None;
            return;
        }
        self.active_view = match self.active_view {
            None => Some(0),
            Some(i) if i + 1 < n => Some(i + 1),
            Some(_) => None,
        };
    }

    fn push_message(&mut self, m: ChatEntry) {
        self.messages.push_back(m);
        while self.messages.len() > MAX_MESSAGES {
            self.messages.pop_front();
        }
    }
}

/// 用「檢視管理」裡的文字大小設定,等比縮放 egui 所有內建文字樣式
/// (標題/內文/按鈕/次要文字/等寬字),讓整個應用程式(含設定視窗)的文字
/// 大小一起變動,而不是只影響聊天視窗裡的時間/頻道標籤。
fn apply_ui_font_scale(ctx: &egui::Context, base_px: f32) {
    use egui::{FontId, TextStyle};
    let base = base_px.clamp(8.0, 40.0);
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(base * 0.75)),
            (TextStyle::Body, FontId::proportional(base)),
            (TextStyle::Button, FontId::proportional(base)),
            (TextStyle::Heading, FontId::proportional(base * 1.3)),
            (TextStyle::Monospace, FontId::monospace(base * 0.95)),
        ]
        .into();
    });
}

impl eframe::App for App {
    // eframe 0.36 把 App::update(ctx) 換成 App::ui(ui):面板不再掛在 Context
    // 上,而是疊在這個 root Ui 裡。需要 Context 的地方(視窗命令、輸入、樣式)
    // 從 ui.ctx() 取,先 clone 一份避免跟後面對 ui 的可變借用打架。
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        self.drain_events(ctx);
        ctx.request_repaint_after(std::time::Duration::from_millis(250));

        // 三個 viewport 都掛在同一個 egui::Context 底下,樣式是 Context 全域
        // 共用的,這裡套用一次就會同時影響主視窗、設定視窗與全螢幕工具。
        let ui_font_px = self.cfg.read().unwrap().ui_font_px;
        if (ui_font_px - self.applied_font_px).abs() > f32::EPSILON {
            apply_ui_font_scale(ctx, ui_font_px);
            self.applied_font_px = ui_font_px;
        }

        if self.quit_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        // 一個 viewport 一個方法。順序有意義:主視窗的面板要先堆好,
        // 兩個 show_viewport_immediate 才接在後面。
        // 主視窗的面板疊在 root Ui 裡;另外兩個是各自獨立的作業系統視窗,
        // 用 Context 開自己的 viewport,不需要(也不該)碰到 root Ui。
        // 更新的 UI 不在這裡——它是設定視窗裡的一個分頁(原因見 update.rs)。
        self.ui_chat_window(ui);
        self.ui_settings_window(ctx);
        self.ui_screen_picker(ctx);
    }

    // 用 wgpu 後端,on_exit 不收 glow context(glow 那版簽名會多一個參數)。
    fn on_exit(&mut self) {
        config::save(&self.cfg.read().unwrap());
        let _ = self.cmd_tx.send(CaptureCmd::Shutdown);
        let _ = self.hotkey_tx.send(HotkeyCmd::Shutdown);
    }
}

fn filtered_windows(hint: &str) -> Vec<WindowInfo> {
    let hint = hint.to_lowercase();
    let mut all = win_enum::list_windows();
    if !hint.is_empty() {
        let (matched, rest): (Vec<_>, Vec<_>) = all
            .into_iter()
            .partition(|w| w.title.to_lowercase().contains(&hint));
        all = matched;
        all.extend(rest);
    }
    all
}

/// 載入系統 CJK 字型(egui 內建字型無中文)。
fn install_cjk_fonts(ctx: &egui::Context) -> bool {
    let candidates = [
        "C:\\Windows\\Fonts\\msjh.ttc",   // 微軟正黑體
        "C:\\Windows\\Fonts\\msyh.ttc",   // 微軟雅黑
        "C:\\Windows\\Fonts\\mingliu.ttc",
        "C:\\Windows\\Fonts\\simsun.ttc",
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert("cjk".into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push("cjk".into());
            }
            ctx.set_fonts(fonts);
            return true;
        }
    }
    false
}
