//! egui 主視窗:聊天檢視、擷取設定、頻道校準、檢視管理。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use crossbeam_channel::{Receiver, Sender};
use egui::{Color32, RichText};

use crate::capture::win_enum::{self, WindowInfo};
use crate::config::{self, AllViewMode, Config, ViewDef};
use crate::hotkey;
use crate::model::{CaptureCmd, CaptureState, ChatMessage, HotkeyCmd, UiEvent};

const MAX_MESSAGES: usize = 3000;
const MAX_CALIB_SAMPLES: usize = 40;

/// 設定視窗內的分頁(聊天本身不再是分頁,永遠是主視窗內容)。
#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Capture,
    Channels,
    Views,
    Help,
}

pub struct AppInit {
    pub cfg: Arc<RwLock<Config>>,
    pub calibrating: Arc<AtomicBool>,
    pub interval_ms: Arc<AtomicU64>,
    pub cmd_tx: Sender<CaptureCmd>,
    pub ui_rx: Receiver<UiEvent>,
    pub hotkey_tx: Sender<HotkeyCmd>,
}

struct PreviewState {
    tex: egui::TextureHandle,
    frame_w: u32,
    frame_h: u32,
}

/// 校準模式的一筆樣本:主色、OCR 文字,以及送進 OCR 引擎的實際畫面縮圖。
struct CalibEntry {
    color: [u8; 3],
    text: String,
    tex: egui::TextureHandle,
}

pub struct App {
    cfg: Arc<RwLock<Config>>,
    calibrating: Arc<AtomicBool>,
    interval_ms: Arc<AtomicU64>,
    cmd_tx: Sender<CaptureCmd>,
    ui_rx: Receiver<UiEvent>,
    hotkey_tx: Sender<HotkeyCmd>,

    tab: Tab,
    messages: VecDeque<ChatMessage>,
    /// None = 「全部」
    active_view: Option<usize>,
    filter: String,

    windows: Vec<WindowInfo>,
    selected_win: Option<(isize, String)>,
    preview: Option<PreviewState>,
    drag_start: Option<egui::Pos2>,

    calib_samples: VecDeque<CalibEntry>,
    calib_target: usize,
    calib_tex_seq: u64,

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
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, init: AppInit) -> Self {
        let font_ok = install_cjk_fonts(&cc.egui_ctx);
        let hint = init.cfg.read().unwrap().window_title_hint.clone();
        let windows = filtered_windows(&hint);
        Self {
            cfg: init.cfg,
            calibrating: init.calibrating,
            interval_ms: init.interval_ms,
            cmd_tx: init.cmd_tx,
            ui_rx: init.ui_rx,
            hotkey_tx: init.hotkey_tx,
            tab: Tab::Capture,
            messages: VecDeque::new(),
            active_view: None,
            filter: String::new(),
            windows,
            selected_win: None,
            preview: None,
            drag_start: None,
            calib_samples: VecDeque::new(),
            calib_target: 0,
            calib_tex_seq: 0,
            capture_state: CaptureState::Idle,
            last_error: None,
            always_on_top: false,
            font_ok,
            settings_open: false,
            quit_requested: false,
            chat_collapsed: false,
            saved_height: 320.0,
            window_locked: false,
        }
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        for _ in 0..256 {
            match self.ui_rx.try_recv() {
                Ok(UiEvent::Message(m)) => self.push_message(m),
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
                }
                Ok(UiEvent::CalibSample { color, text, img_w, img_h, img_rgba }) => {
                    let img = egui::ColorImage::from_rgba_unmultiplied(
                        [img_w as usize, img_h as usize],
                        &img_rgba,
                    );
                    self.calib_tex_seq += 1;
                    let tex = ctx.load_texture(
                        format!("calib{}", self.calib_tex_seq),
                        img,
                        egui::TextureOptions::NEAREST,
                    );
                    self.calib_samples.push_front(CalibEntry { color, text, tex });
                    while self.calib_samples.len() > MAX_CALIB_SAMPLES {
                        self.calib_samples.pop_back();
                    }
                }
                Ok(UiEvent::CaptureState(s)) => self.capture_state = s,
                Ok(UiEvent::Error(e)) => self.last_error = Some(e),
                Ok(UiEvent::HotkeyTriggered) => self.cycle_view(),
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

    /// 折行訊息(無發話者且緊接同頻道)併入上一則。
    fn push_message(&mut self, m: ChatMessage) {
        if m.sender.is_none() {
            if let Some(last) = self.messages.back_mut() {
                if last.channel == m.channel
                    && (m.time - last.time).num_seconds() <= 2
                    && !last.content.is_empty()
                {
                    last.content.push_str(&m.content);
                    return;
                }
            }
        }
        self.messages.push_back(m);
        while self.messages.len() > MAX_MESSAGES {
            self.messages.pop_front();
        }
    }

    // ---------------- 聊天檢視 ----------------

    /// 頂端工具列:檢視切換、搜尋、字體大小、置頂、設定。永遠不透明,拖曳感應也在這裡。
    fn ui_chat_toolbar(&mut self, ui: &mut egui::Ui) {
        let (cfg_views, all_view_mode) = {
            let cfg = self.cfg.read().unwrap();
            (cfg.views.iter().map(|v| v.name.clone()).collect::<Vec<_>>(), cfg.all_view_mode)
        };

        ui.horizontal_wrapped(|ui| {
            // 沒有工具列可以拖曳視窗了,改成這一列背景可以拖曳。先佔滿整列的感應區,
            // 之後加入的按鈕/輸入框仍會在自己的範圍內優先接收點擊,不影響操作。
            let row_height = ui.spacing().interact_size.y.max(22.0);
            let drag_rect = egui::Rect::from_min_size(
                ui.cursor().min,
                egui::vec2(ui.available_width(), row_height),
            );
            let drag_resp =
                ui.interact(drag_rect, ui.id().with("chat_toolbar_drag"), egui::Sense::drag());
            if drag_resp.drag_started() && !self.window_locked {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }

            let all_hover = match all_view_mode {
                AllViewMode::HideChat => "會直接隱藏下方內容,透出遊戲原本的聊天視窗(可到「檢視管理」改成顯示全部對話)",
                AllViewMode::ShowMessages => "顯示 OCR 擷取到的全部對話,不套用頻道過濾",
            };
            if ui
                .selectable_label(self.active_view.is_none(), "全部")
                .on_hover_text(all_hover)
                .clicked()
            {
                self.active_view = None;
            }
            for (i, name) in cfg_views.iter().enumerate() {
                if ui.selectable_label(self.active_view == Some(i), name).clicked() {
                    self.active_view = Some(i);
                }
            }
            ui.separator();
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("搜尋文字或發話者…")
                    .desired_width(160.0),
            );
            if ui.button("清空訊息").clicked() {
                self.messages.clear();
            }
            ui.separator();
            ui.label("字");
            {
                let mut cfg = self.cfg.write().unwrap();
                ui.add(
                    egui::DragValue::new(&mut cfg.chat_font_px)
                        .range(8.0..=40.0)
                        .speed(0.2)
                        .suffix("px"),
                );
            }
            if ui.checkbox(&mut self.always_on_top, "視窗置頂").changed() {
                let level = if self.always_on_top {
                    egui::viewport::WindowLevel::AlwaysOnTop
                } else {
                    egui::viewport::WindowLevel::Normal
                };
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
            }
            if ui.button("⚙").on_hover_text("設定").clicked() {
                if self.settings_open {
                    // 已經開著了,只是可能被遊戲擋住/沒有焦點,叫它上前來。
                    ui.ctx().send_viewport_cmd_to(
                        settings_viewport_id(),
                        egui::ViewportCommand::Focus,
                    );
                } else {
                    self.settings_open = true;
                }
            }
            let lock_icon = if self.window_locked { "🔒" } else { "🔓" };
            let lock_hover =
                if self.window_locked { "解除鎖定(目前無法拖曳/縮放視窗)" } else { "鎖定視窗(禁止拖曳/縮放)" };
            if ui.button(lock_icon).on_hover_text(lock_hover).clicked() {
                self.window_locked = !self.window_locked;
            }
        });
    }

    /// 訊息列表。選「全部」且設定為「隱藏聊天」時,這裡不會被呼叫
    /// (見 update() 的視窗收合邏輯);設定為「顯示全部對話」時則正常顯示、不套用頻道過濾。
    fn ui_chat_messages(&mut self, ui: &mut egui::Ui) {
        let cfg = self.cfg.read().unwrap().clone();
        let allowed: Option<&ViewDef> = self.active_view.and_then(|i| cfg.views.get(i));
        let filter = self.filter.trim().to_lowercase();

        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink(false)
            .show(ui, |ui| {
                for m in &self.messages {
                    if let Some(v) = allowed {
                        if !v.channels.iter().any(|c| *c == m.channel) {
                            continue;
                        }
                    }
                    if !filter.is_empty() {
                        let hay = format!(
                            "{} {}",
                            m.sender.as_deref().unwrap_or(""),
                            m.content
                        )
                        .to_lowercase();
                        if !hay.contains(&filter) {
                            continue;
                        }
                    }
                    let (name, color) = match cfg.channel_by_id(&m.channel) {
                        Some(c) => {
                            (c.name.clone(), Color32::from_rgb(c.color[0], c.color[1], c.color[2]))
                        }
                        None => ("未分類".to_string(), Color32::GRAY),
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(m.time.format("%H:%M:%S").to_string())
                                .weak()
                                .size(cfg.chat_font_px * 0.7),
                        );
                        ui.label(
                            RichText::new(format!("[{name}]"))
                                .color(color)
                                .strong()
                                .size(cfg.chat_font_px),
                        );
                        if let Some(s) = &m.sender {
                            ui.label(
                                RichText::new(format!("{s}:")).strong().size(cfg.chat_font_px),
                            );
                        }
                        ui.label(RichText::new(&m.content).size(cfg.chat_font_px));
                    });
                }
            });
    }

    // ---------------- 擷取設定 ----------------

    fn ui_capture(&mut self, ui: &mut egui::Ui) {
        let mut save_now = false;
        {
            let mut cfg = self.cfg.write().unwrap();

            ui.horizontal(|ui| {
                ui.label("視窗篩選:");
                ui.add(
                    egui::TextEdit::singleline(&mut cfg.window_title_hint).desired_width(140.0),
                );
                if ui.button("重新整理視窗清單").clicked() {
                    self.windows = filtered_windows(&cfg.window_title_hint);
                }
                let selected_text = self
                    .selected_win
                    .as_ref()
                    .map(|(_, t)| truncate(t, 40))
                    .unwrap_or_else(|| "選擇遊戲視窗…".to_string());
                egui::ComboBox::from_id_salt("win_select")
                    .width(320.0)
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        for w in &self.windows {
                            let label = truncate(&w.title, 60);
                            let sel = self.selected_win.as_ref().map(|(h, _)| *h) == Some(w.hwnd);
                            if ui.selectable_label(sel, label).clicked() {
                                self.selected_win = Some((w.hwnd, w.title.clone()));
                            }
                        }
                    });
            });

            ui.horizontal(|ui| {
                let running = matches!(self.capture_state, CaptureState::Running(_));
                if !running {
                    if ui.button("▶ 開始擷取").clicked() {
                        if let Some((hwnd, title)) = self.selected_win.clone() {
                            let _ = self.cmd_tx.send(CaptureCmd::Start { hwnd, title });
                        } else {
                            self.last_error = Some("請先選擇遊戲視窗".into());
                        }
                    }
                } else if ui.button("⏹ 停止擷取").clicked() {
                    let _ = self.cmd_tx.send(CaptureCmd::Stop);
                }
                match &self.capture_state {
                    CaptureState::Idle => ui.label(RichText::new("狀態:待機").weak()),
                    CaptureState::Running(t) => ui.label(
                        RichText::new(format!("狀態:擷取中 — {}", truncate(t, 30)))
                            .color(Color32::LIGHT_GREEN),
                    ),
                    CaptureState::Failed(e) => {
                        ui.label(RichText::new(format!("狀態:失敗 — {e}")).color(Color32::LIGHT_RED))
                    }
                };
            });

            ui.horizontal(|ui| {
                ui.label("處理頻率");
                if ui.add(egui::Slider::new(&mut cfg.fps, 2..=15).suffix(" fps")).changed() {
                    self.interval_ms.store(1000 / cfg.fps.max(1) as u64, Ordering::Relaxed);
                }
                ui.separator();
                ui.label("OCR 放大");
                ui.add(egui::Slider::new(&mut cfg.ocr_scale, 1..=6).suffix("x"));
                ui.separator();
                ui.checkbox(&mut cfg.include_unknown, "顯示未分類訊息");
                if ui.button("💾 儲存設定").clicked() {
                    save_now = true;
                }
            });

            ui.horizontal(|ui| {
                ui.label("切行高度");
                ui.add(
                    egui::DragValue::new(&mut cfg.line_height_px)
                        .range(4..=200)
                        .suffix("px"),
                );
                ui.label(
                    RichText::new("(從 ROI 最上面開始,每隔此高度切一則候選訊息)")
                        .weak()
                        .small(),
                );
            });

            ui.horizontal(|ui| {
                ui.label("文字去重時間窗");
                ui.add(
                    egui::DragValue::new(&mut cfg.dedup_seconds)
                        .range(1..=600)
                        .suffix("s"),
                );
                ui.label(
                    RichText::new(
                        "(固定切行下,同則訊息捲動到新格子會重新觸發辨識;\
                         這段時間內出現相同文字會被視為重複而略過,太短會看到同訊息一直重複)",
                    )
                    .weak()
                    .small(),
                );
            });

            ui.separator();
            ui.label(
                "在下方預覽圖上「拖曳框選」聊天區域(ROI),或直接手動輸入座標。只會處理框內畫面:",
            );
            ui.horizontal(|ui| {
                ui.label("x");
                ui.add(egui::DragValue::new(&mut cfg.roi.x).range(0..=20000));
                ui.label("y");
                ui.add(egui::DragValue::new(&mut cfg.roi.y).range(0..=20000));
                ui.label("w");
                ui.add(egui::DragValue::new(&mut cfg.roi.w).range(0..=20000));
                ui.label("h");
                ui.add(egui::DragValue::new(&mut cfg.roi.h).range(0..=20000));
            });

            if let Some(p) = &self.preview {
                let avail_w = ui.available_width().min(760.0);
                let tex_size = p.tex.size_vec2();
                let shown_scale = (avail_w / tex_size.x).min(2.0);
                let shown = tex_size * shown_scale;
                let resp = ui.add(
                    egui::Image::new(&p.tex)
                        .fit_to_exact_size(shown)
                        .sense(egui::Sense::click_and_drag()),
                );
                let origin = resp.rect.min;
                // frame 座標 ↔ 顯示座標的比例
                let to_frame = p.frame_w as f32 / shown.x;

                if resp.drag_started() {
                    self.drag_start = resp.interact_pointer_pos();
                }
                if let (Some(s), Some(cur)) = (self.drag_start, resp.interact_pointer_pos()) {
                    if resp.dragged() || resp.drag_stopped() {
                        let rect = egui::Rect::from_two_pos(s, cur);
                        ui.painter().rect_stroke(
                            rect,
                            egui::Rounding::ZERO,
                            egui::Stroke::new(2.0, Color32::YELLOW),
                        );
                        if resp.drag_stopped() {
                            let x0 = ((rect.min.x - origin.x).max(0.0) * to_frame) as u32;
                            let y0 = ((rect.min.y - origin.y).max(0.0) * to_frame) as u32;
                            let x1 = ((rect.max.x - origin.x).max(0.0) * to_frame) as u32;
                            let y1 = ((rect.max.y - origin.y).max(0.0) * to_frame) as u32;
                            let (x1, y1) = (x1.min(p.frame_w), y1.min(p.frame_h));
                            if x1 > x0 + 24 && y1 > y0 + 10 {
                                cfg.roi.x = x0;
                                cfg.roi.y = y0;
                                cfg.roi.w = x1 - x0;
                                cfg.roi.h = y1 - y0;
                            }
                            self.drag_start = None;
                        }
                    }
                }

                // 畫出目前 ROI
                if cfg.roi.w > 0 {
                    let s = 1.0 / to_frame;
                    let r = egui::Rect::from_min_size(
                        origin
                            + egui::vec2(cfg.roi.x as f32 * s, cfg.roi.y as f32 * s),
                        egui::vec2(cfg.roi.w as f32 * s, cfg.roi.h as f32 * s),
                    );
                    ui.painter().rect_stroke(
                        r,
                        egui::Rounding::ZERO,
                        egui::Stroke::new(2.0, Color32::LIGHT_GREEN),
                    );
                }
            } else {
                ui.label(RichText::new("(開始擷取後這裡會顯示遊戲畫面預覽)").weak());
            }
        }
        if save_now {
            config::save(&self.cfg.read().unwrap());
        }
    }

    // ---------------- 頻道與校準 ----------------

    fn ui_channels(&mut self, ui: &mut egui::Ui) {
        let mut save_now = false;
        {
            let mut cfg = self.cfg.write().unwrap();

            ui.label("頻道以「文字顏色」辨識。預設值為近似值,請用下方校準功能校正:");
            ui.add_space(4.0);

            let mut remove: Option<usize> = None;
            for i in 0..cfg.channels.len() {
                let ch = &mut cfg.channels[i];
                ui.horizontal(|ui| {
                    ui.checkbox(&mut ch.enabled, "");
                    ui.color_edit_button_srgb(&mut ch.color);
                    ui.add(egui::TextEdit::singleline(&mut ch.name).desired_width(90.0));
                    ui.label("容差");
                    ui.add(egui::DragValue::new(&mut ch.tolerance).range(4..=120));
                    ui.label(
                        RichText::new(format!(
                            "RGB({},{},{})",
                            ch.color[0], ch.color[1], ch.color[2]
                        ))
                        .weak()
                        .small(),
                    );
                    if ui.button("刪除").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                cfg.channels.remove(i);
                self.calib_target = 0;
            }
            ui.horizontal(|ui| {
                if ui.button("＋ 新增頻道").clicked() {
                    let id = format!("custom{}", cfg.channels.len());
                    cfg.channels.push(crate::config::ChannelDef {
                        id,
                        name: "新頻道".into(),
                        color: [255, 255, 255],
                        tolerance: 40,
                        enabled: true,
                    });
                }
                if ui.button("💾 儲存設定").clicked() {
                    save_now = true;
                }
            });

            ui.separator();

            let mut calib = self.calibrating.load(Ordering::Relaxed);
            if ui
                .checkbox(&mut calib, "校準模式(即時列出每行偵測到的文字顏色與內容)")
                .changed()
            {
                self.calibrating.store(calib, Ordering::Relaxed);
                if calib {
                    self.calib_samples.clear();
                }
            }

            if calib {
                ui.horizontal(|ui| {
                    ui.label("套用目標頻道:");
                    let names: Vec<String> =
                        cfg.channels.iter().map(|c| c.name.clone()).collect();
                    self.calib_target = self.calib_target.min(names.len().saturating_sub(1));
                    egui::ComboBox::from_id_salt("calib_target")
                        .selected_text(
                            names.get(self.calib_target).cloned().unwrap_or_default(),
                        )
                        .show_ui(ui, |ui| {
                            for (i, n) in names.iter().enumerate() {
                                ui.selectable_value(&mut self.calib_target, i, n);
                            }
                        });
                });
                ui.add_space(4.0);
                // 不用自己的固定高度捲動區,直接交給設定視窗外層的捲動區處理——
                // 這樣可見高度會跟著設定視窗大小走,而不是卡在一個寫死的高度。
                for (idx, entry) in self.calib_samples.iter().enumerate() {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            let c = Color32::from_rgb(
                                entry.color[0],
                                entry.color[1],
                                entry.color[2],
                            );
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(26.0, 15.0),
                                egui::Sense::hover(),
                            );
                            ui.painter().rect_filled(rect, 2.0, c);
                            ui.label(
                                RichText::new(format!(
                                    "({},{},{})",
                                    entry.color[0], entry.color[1], entry.color[2]
                                ))
                                .weak()
                                .small(),
                            );
                            if ui
                                .button(RichText::new("套用→").small())
                                .on_hover_text("將此顏色設為目標頻道的顏色")
                                .clicked()
                            {
                                if let Some(ch) = cfg.channels.get_mut(self.calib_target) {
                                    ch.color = entry.color;
                                }
                            }
                            ui.label(truncate(&entry.text, 40));
                        });
                        // 實際送進 OCR 引擎的畫面(黑字白底、已放大),原始像素大小顯示,
                        // 用來判斷辨識錯誤是不是遮罩/裁切造成的。太寬時可左右捲動。
                        egui::ScrollArea::horizontal()
                            .id_salt(("calib_img", idx))
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                ui.add(egui::Image::new(&entry.tex));
                            });
                    });
                    if idx > 30 {
                        break;
                    }
                }
            }
        }
        if save_now {
            config::save(&self.cfg.read().unwrap());
        }
    }

    // ---------------- 檢視管理 ----------------

    fn ui_views(&mut self, ui: &mut egui::Ui) {
        let mut save_now = false;
        {
            let mut cfg = self.cfg.write().unwrap();

            ui.label(RichText::new("聊天文字大小").strong());
            ui.add(egui::Slider::new(&mut cfg.chat_font_px, 8.0..=40.0).suffix("px"));
            ui.separator();

            ui.label(RichText::new("「全部」檢視的行為").strong());
            ui.horizontal(|ui| {
                if ui
                    .radio_value(
                        &mut cfg.all_view_mode,
                        AllViewMode::ShowMessages,
                        "顯示 OCR 擷取到的全部對話",
                    )
                    .changed()
                {
                    save_now = true;
                }
                if ui
                    .radio_value(
                        &mut cfg.all_view_mode,
                        AllViewMode::HideChat,
                        "隱藏聊天,直接看遊戲原始畫面",
                    )
                    .changed()
                {
                    save_now = true;
                }
            });
            ui.separator();

            ui.label("檢視 = 一個分頁,勾選要包含的頻道(例:「社交」= 密語+好友+公會):");
            ui.add_space(4.0);

            let channel_list: Vec<(String, String)> =
                cfg.channels.iter().map(|c| (c.id.clone(), c.name.clone())).collect();

            let mut remove: Option<usize> = None;
            for vi in 0..cfg.views.len() {
                let v = &mut cfg.views[vi];
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut v.name).desired_width(120.0));
                        if ui.button("刪除檢視").clicked() {
                            remove = Some(vi);
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        for (id, name) in &channel_list {
                            let mut on = v.channels.contains(id);
                            if ui.checkbox(&mut on, name).changed() {
                                if on {
                                    v.channels.push(id.clone());
                                } else {
                                    v.channels.retain(|c| c != id);
                                }
                            }
                        }
                        let mut unknown_on = v.channels.iter().any(|c| c == "unknown");
                        if ui.checkbox(&mut unknown_on, "未分類").changed() {
                            if unknown_on {
                                v.channels.push("unknown".into());
                            } else {
                                v.channels.retain(|c| c != "unknown");
                            }
                        }
                    });
                });
            }
            if let Some(i) = remove {
                cfg.views.remove(i);
                self.active_view = None;
            }
            ui.horizontal(|ui| {
                if ui.button("＋ 新增檢視").clicked() {
                    cfg.views.push(ViewDef { name: "新檢視".into(), channels: vec![] });
                }
                if ui.button("💾 儲存設定").clicked() {
                    save_now = true;
                }
            });

            ui.separator();
            ui.label(
                RichText::new("切換檢視快捷鍵(系統層級,遊戲視窗有焦點時也能觸發)").strong(),
            );
            ui.label(
                RichText::new("按下時會依序循環:全部 → 第一個檢視 → 第二個檢視 → … → 全部")
                    .weak()
                    .small(),
            );
            let mut hk_changed = false;
            ui.horizontal(|ui| {
                hk_changed |= ui.checkbox(&mut cfg.hotkey.ctrl, "Ctrl").changed();
                hk_changed |= ui.checkbox(&mut cfg.hotkey.alt, "Alt").changed();
                hk_changed |= ui.checkbox(&mut cfg.hotkey.shift, "Shift").changed();
                hk_changed |= ui.checkbox(&mut cfg.hotkey.win, "Win").changed();
                ui.label("+");
                egui::ComboBox::from_id_salt("hotkey_key")
                    .selected_text(cfg.hotkey.key.clone())
                    .show_ui(ui, |ui| {
                        for name in hotkey::all_key_names() {
                            if ui
                                .selectable_value(&mut cfg.hotkey.key, name.to_string(), name)
                                .changed()
                            {
                                hk_changed = true;
                            }
                        }
                    });
            });
            if hk_changed {
                if let Some(vk) = hotkey::vk_for_name(&cfg.hotkey.key) {
                    let _ = self.hotkey_tx.send(HotkeyCmd::SetCombo {
                        ctrl: cfg.hotkey.ctrl,
                        alt: cfg.hotkey.alt,
                        shift: cfg.hotkey.shift,
                        win: cfg.hotkey.win,
                        vk,
                    });
                }
                save_now = true;
            }
        }
        if save_now {
            config::save(&self.cfg.read().unwrap());
        }
    }

    // ---------------- 說明 ----------------

    fn ui_help(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.label(
                RichText::new(format!("楓之谷聊天篩選器 v{}", env!("CARGO_PKG_VERSION")))
                    .weak()
                    .small(),
            );
            ui.add_space(4.0);
            ui.label(RichText::new("使用步驟").strong());
            ui.label(
                "主視窗永遠只顯示聊天;所有設定都在右上角 ⚙ 按鈕打開的設定視窗裡:\n\
                 1. 「擷取設定」選擇遊戲視窗 → 開始擷取\n\
                 2. 在預覽圖上拖曳框選聊天區域(ROI)\n\
                 3. 「頻道/校準」開啟校準模式,請遊戲內朋友在各頻道發話,\n\
                    將偵測到的顏色一鍵套用到對應頻道\n\
                 4. 「檢視管理」建立自己的分頁(如「社交」=密語+好友+公會)、\n\
                    調整聊天文字大小、選擇「全部」檢視要顯示全部對話還是隱藏聊天\n\
                 5. 主視窗工具列可設定系統層級快捷鍵切換檢視(預設 Shift+Tab)、\n\
                    開啟視窗置頂、鎖定視窗避免誤觸拖曳/縮放\n\
                 6. 關掉設定視窗,回到聊天畫面看過濾後的訊息",
            );
            ui.add_space(8.0);
            ui.label(RichText::new("建議的遊戲設定").strong());
            ui.label(
                "將遊戲內聊天視窗背景調成不透明(黑底),辨識準確率會大幅提升;\n\
                 聊天字型維持預設;遊戲使用視窗模式或無邊框(獨占全螢幕無法擷取)。",
            );
            ui.add_space(8.0);
            ui.label(RichText::new("OCR 語言需求").strong());
            ui.label(
                "需要 Windows 的「中文(台灣)」OCR 功能:\n\
                 設定 → 時間與語言 → 語言與地區 → 中文(台灣) → 語言選項 → 光學字元辨識\n\
                 或系統管理員 PowerShell:\n\
                 Add-WindowsCapability -Online -Name \"Language.OCR~~~zh-TW~0.0.1.0\"",
            );
            ui.add_space(8.0);
            ui.label(RichText::new("已知限制").strong());
            ui.label(
                "・遊戲最小化時擷取會暫停(Windows.Graphics.Capture 限制)\n\
                 ・OCR 可能誤認少數字元;相同文字在去重時間窗內只會顯示一次\n\
                 ・視窗大小改變後,ROI 需要重新框選\n\
                 ・本工具只讀取畫面,不寫入遊戲、不注入、不自動操作;\n\
                   使用第三方工具仍請自行留意遊戲服務條款",
            );
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events(ctx);
        ctx.request_repaint_after(std::time::Duration::from_millis(250));

        if self.quit_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        if !self.font_ok {
            egui::TopBottomPanel::top("font_warn").show(ctx, |ui| {
                ui.label(
                    RichText::new("找不到系統中文字型(msjh.ttc),中文可能顯示為方框")
                        .color(Color32::YELLOW),
                );
            });
        }
        if let Some(err) = self.last_error.clone() {
            egui::TopBottomPanel::top("err").show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&err).color(Color32::LIGHT_RED));
                    if ui.button("✕").clicked() {
                        self.last_error = None;
                    }
                });
            });
        }

        // 主視窗永遠只顯示聊天,大小/字級才能貼著遊戲聊天視窗調整、疊上去用。
        // 所有設定都收在另一個獨立的作業系統視窗裡,按 ⚙ 才會打開。
        let toolbar_resp =
            egui::TopBottomPanel::top("chat_toolbar").show(ctx, |ui| self.ui_chat_toolbar(ui));
        let toolbar_h = toolbar_resp.response.rect.height();

        // 選「全部」且設定為「隱藏聊天」時,直接把視窗縮到只剩工具列高度,內容區塊實際消失
        // (不是用半透明蓋掉),底下的遊戲聊天視窗自然就露出來;不依賴 Windows 桌面合成的透明效果。
        // 設定為「顯示全部對話」時則不收合,正常顯示未過濾的訊息列表。
        let hide_chat_on_all =
            matches!(self.cfg.read().unwrap().all_view_mode, AllViewMode::HideChat);
        let want_collapsed = self.active_view.is_none() && hide_chat_on_all;
        if want_collapsed != self.chat_collapsed {
            let screen = ctx.input(|i| i.screen_rect());
            if want_collapsed {
                self.saved_height = screen.height();
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                    screen.width(),
                    toolbar_h + 4.0,
                )));
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                    screen.width(),
                    self.saved_height.max(120.0),
                )));
            }
            self.chat_collapsed = want_collapsed;
        }

        if !want_collapsed {
            egui::CentralPanel::default().show(ctx, |ui| self.ui_chat_messages(ui));
        }

        // 主視窗無邊框,少了系統內建的邊緣拖曳縮放,補上手動的邊框/角落感應區。
        // 鎖定時不裝這些感應區,滑鼠移過去也不會顯示縮放游標,明確表示目前不能調整。
        if !self.window_locked {
            install_resize_border(ctx);
        }

        if self.settings_open {
            let settings_id = settings_viewport_id();
            ctx.show_viewport_immediate(
                settings_id,
                egui::ViewportBuilder::default()
                    .with_title(format!(
                        "設定 — 楓之谷聊天篩選器 v{}",
                        env!("CARGO_PKG_VERSION")
                    ))
                    .with_inner_size([760.0, 600.0]),
                |ctx, _class| {
                    egui::TopBottomPanel::top("settings_tabs").show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut self.tab, Tab::Capture, "🎥 擷取設定");
                            ui.selectable_value(&mut self.tab, Tab::Channels, "🎨 頻道/校準");
                            ui.selectable_value(&mut self.tab, Tab::Views, "📑 檢視管理");
                            ui.selectable_value(&mut self.tab, Tab::Help, "❓ 說明");
                            ui.separator();
                            if ui
                                .button(RichText::new("⏻ 結束應用程式").color(Color32::LIGHT_RED))
                                .clicked()
                            {
                                self.quit_requested = true;
                            }
                        });
                    });
                    egui::CentralPanel::default().show(ctx, |ui| {
                        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                            match self.tab {
                                Tab::Capture => self.ui_capture(ui),
                                Tab::Channels => self.ui_channels(ui),
                                Tab::Views => self.ui_views(ui),
                                Tab::Help => self.ui_help(ui),
                            }
                        });
                    });
                    if ctx.input(|i| i.viewport().close_requested()) {
                        self.settings_open = false;
                    }
                },
            );
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        config::save(&self.cfg.read().unwrap());
        let _ = self.cmd_tx.send(CaptureCmd::Shutdown);
        let _ = self.hotkey_tx.send(HotkeyCmd::Shutdown);
    }
}

/// 設定視窗固定的 ViewportId(同一個字串每次算出來都一樣,不用另外存狀態)。
fn settings_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("maple_chat_filter_settings")
}

/// 無邊框視窗補上邊緣/角落拖曳縮放感應區(系統少了內建的視窗邊框可以拖)。
fn install_resize_border(ctx: &egui::Context) {
    use egui::viewport::ResizeDirection as R;
    use egui::CursorIcon as C;

    // 只留右邊/下面/右下角可以拖曳縮放——上面跟左邊拿掉,避免點選頻道工具列
    // (在視窗最上方)時不小心誤觸縮放感應區。
    const T: f32 = 8.0;
    let s = ctx.screen_rect();

    let regions: [(&str, egui::Rect, R, C); 3] = [
        (
            "rz_e",
            egui::Rect::from_min_max(
                egui::pos2(s.right() - T, s.top()),
                egui::pos2(s.right(), s.bottom() - T),
            ),
            R::East,
            C::ResizeEast,
        ),
        (
            "rz_se",
            egui::Rect::from_min_max(egui::pos2(s.right() - T, s.bottom() - T), s.right_bottom()),
            R::SouthEast,
            C::ResizeSouthEast,
        ),
        (
            "rz_s",
            egui::Rect::from_min_max(
                egui::pos2(s.left(), s.bottom() - T),
                egui::pos2(s.right() - T, s.bottom()),
            ),
            R::South,
            C::ResizeSouth,
        ),
    ];

    for (id, rect, dir, cursor) in regions {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            continue;
        }
        egui::Area::new(egui::Id::new(("resize_border", id)))
            .fixed_pos(rect.min)
            .order(egui::Order::Foreground)
            .interactable(true)
            .show(ctx, |ui| {
                let resp = ui.allocate_response(rect.size(), egui::Sense::drag());
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(cursor);
                }
                if resp.drag_started() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
                }
            });
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

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n).collect();
        format!("{t}…")
    }
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
            fonts.font_data.insert("cjk".into(), egui::FontData::from_owned(bytes));
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push("cjk".into());
            }
            ctx.set_fonts(fonts);
            return true;
        }
    }
    false
}
