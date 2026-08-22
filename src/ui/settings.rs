//! 設定視窗:按 ⚙ 開出來的獨立 OS 視窗,內含四個分頁。
//!
//! 這是唯一能完全關閉程式的地方——主視窗沒有關閉鈕。

use std::sync::atomic::Ordering;

use egui::{Color32, RichText};

use crate::config::{self, AllViewMode, ResolutionPreset, RoiMode, ViewDef};
use crate::hotkey;
use crate::model::{CaptureCmd, CaptureState, HotkeyCmd};

use super::picker::{color_picker_rgb, request_pick};
use super::{filtered_windows, ColorTarget, PickMode, RoiTarget, Tab};

/// 設定視窗固定的 ViewportId(同一個字串每次算出來都一樣,不用另外存狀態)。
pub(super) fn settings_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("maple_chat_filter_settings")
}

impl super::App {
    /// 設定視窗沒開就什麼都不做;開著的話每幀重建一次 builder(見下方註解)。
    pub(super) fn ui_settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }

        let settings_id = settings_viewport_id();
        // 主視窗置頂時設定視窗也跟著置頂,不然疊圖時設定視窗容易被遊戲蓋住。
        // 每幀都用目前的 always_on_top 重建 builder,egui 只在值真的改變時
        // 才會送出 WindowLevel 命令(見 ViewportBuilder::patch)。
        let settings_level = if self.always_on_top {
            egui::viewport::WindowLevel::AlwaysOnTop
        } else {
            egui::viewport::WindowLevel::Normal
        };
        ctx.show_viewport_immediate(
            settings_id,
            egui::ViewportBuilder::default()
                .with_title(format!(
                    "設定 — 楓之谷聊天篩選器 v{}",
                    env!("CARGO_PKG_VERSION")
                ))
                .with_inner_size([760.0, 600.0])
                .with_window_level(settings_level),
            |ctx, _class| {
                egui::TopBottomPanel::top("settings_tabs").show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.tab, Tab::Capture, "🎥 擷取設定");
                        ui.selectable_value(&mut self.tab, Tab::Channels, "🎨 頻道");
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
                if ui.button("💾 儲存設定").clicked() {
                    save_now = true;
                }
            });

            ui.horizontal(|ui| {
                ui.label("畫面去重時間窗");
                ui.add(
                    egui::DragValue::new(&mut cfg.dedup_seconds)
                        .range(1..=600)
                        .suffix("s"),
                );
                ui.label(
                    RichText::new(
                        "(內容相似的訊息畫面在這段時間內只顯示一次;太短會看到同一則\
                         訊息因背景抖動而重複洗版,太長則新訊息可能被誤擋)",
                    )
                    .weak()
                    .small(),
                );
            });

            ui.separator();
            ui.label(RichText::new("擷取範圍(ROI)").strong());

            let was_advanced = matches!(cfg.roi_mode, RoiMode::Advanced);
            let mut mode_advanced = was_advanced;
            ui.horizontal(|ui| {
                if ui.selectable_label(!mode_advanced, "一般設定").clicked() {
                    mode_advanced = false;
                }
                if ui.selectable_label(mode_advanced, "進階設定").clicked() {
                    mode_advanced = true;
                }
            });
            if mode_advanced != was_advanced {
                if mode_advanced {
                    cfg.roi_mode = RoiMode::Advanced;
                    // 切回進階設定,還原上次手動輸入的值(而不是沿用剛剛「一般設定」
                    // 換算出來的值),不然切一般設定再切回來,自己輸入的座標就不見了。
                    cfg.roi = cfg.custom_roi;
                    cfg.gate.roi = cfg.custom_gate_roi;
                } else {
                    // 已經有擷取畫面的話,直接依實際解析度選對應的預設值,不用
                    // 等下一次 Preview 事件(最慢 500ms 後)才自動校正。
                    if let Some(p) = &self.preview {
                        if let Some(detected) = ResolutionPreset::from_dims(p.frame_w, p.frame_h) {
                            self.resolution_preset = detected;
                        }
                    }
                    cfg.roi_mode = RoiMode::Preset(self.resolution_preset);
                    cfg.roi = self.resolution_preset.chat_roi();
                    cfg.gate.roi = self.resolution_preset.gate_roi();
                }
            }

            match cfg.roi_mode {
                RoiMode::Preset(_) => {
                    ui.label(
                        RichText::new(
                            "選擇遊戲視窗的解析度,套用該解析度下實測的聊天範圍與主畫面\
                             判斷範圍,不必手動輸入座標。若座標不夠精準,可切換到\
                             「進階設定」微調。",
                        )
                        .weak()
                        .small(),
                    );
                    ui.horizontal(|ui| {
                        ui.label("目標視窗解析度:");
                        let mut changed = false;
                        egui::ComboBox::from_id_salt("resolution_preset")
                            .selected_text(self.resolution_preset.label())
                            .show_ui(ui, |ui| {
                                for p in ResolutionPreset::ALL {
                                    if ui
                                        .selectable_value(&mut self.resolution_preset, p, p.label())
                                        .changed()
                                    {
                                        changed = true;
                                    }
                                }
                            });
                        if changed {
                            cfg.roi_mode = RoiMode::Preset(self.resolution_preset);
                            cfg.roi = self.resolution_preset.chat_roi();
                            cfg.gate.roi = self.resolution_preset.gate_roi();
                        }
                    });
                    let c = cfg.roi;
                    let g = cfg.gate.roi;
                    ui.label(
                        RichText::new(format!(
                            "聊天範圍:x={} y={} w={} h={}　主畫面判斷範圍:x={} y={} w={} h={}",
                            c.x, c.y, c.w, c.h, g.x, g.y, g.w, g.h
                        ))
                        .weak()
                        .small(),
                    );
                    if c.w == 0 || g.w == 0 {
                        ui.label(
                            RichText::new(
                                "此解析度尚未提供實測座標,請切換到「進階設定」手動輸入\
                                 (或用「在畫面上框選」)。",
                            )
                            .color(Color32::YELLOW)
                            .small(),
                        );
                    }
                }
                RoiMode::Advanced => {
                    ui.label(
                        "在下方預覽圖上「拖曳框選」單一則對話的範圍(ROI),或直接手動輸入座標。\
                         只會處理框內畫面,框選範圍請貼齊一行文字的高度:",
                    );
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.roi_target, RoiTarget::Chat, "框選:聊天範圍");
                        ui.radio_value(&mut self.roi_target, RoiTarget::Gate, "框選:主畫面判斷範圍");
                        ui.separator();
                        if ui.button("🎯 在畫面上框選").clicked() {
                            request_pick(
                                &mut self.pending_pick,
                                &self.full_frame_req,
                                &self.capture_state,
                                &mut self.last_error,
                                PickMode::Roi(self.roi_target),
                            );
                        }
                        if self.pending_pick.is_some() {
                            ui.label(RichText::new("擷取畫面中…").weak());
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("聊天 x");
                        ui.add(egui::DragValue::new(&mut cfg.roi.x).range(0..=20000));
                        ui.label("y");
                        ui.add(egui::DragValue::new(&mut cfg.roi.y).range(0..=20000));
                        ui.label("w");
                        ui.add(egui::DragValue::new(&mut cfg.roi.w).range(0..=20000));
                        ui.label("h");
                        ui.add(egui::DragValue::new(&mut cfg.roi.h).range(0..=20000));
                    });
                    ui.horizontal(|ui| {
                        ui.label("判斷 x");
                        ui.add(egui::DragValue::new(&mut cfg.gate.roi.x).range(0..=20000));
                        ui.label("y");
                        ui.add(egui::DragValue::new(&mut cfg.gate.roi.y).range(0..=20000));
                        ui.label("w");
                        ui.add(egui::DragValue::new(&mut cfg.gate.roi.w).range(0..=20000));
                        ui.label("h");
                        ui.add(egui::DragValue::new(&mut cfg.gate.roi.h).range(0..=20000));
                    });

                    // 進階模式下,不論是這裡的欄位、下方預覽圖拖曳框選,還是
                    // 「在畫面上框選」視窗寫回來的值,每幀都同步存一份,
                    // 這樣切去一般設定再切回來才找得回目前這組值。
                    cfg.custom_roi = cfg.roi;
                    cfg.custom_gate_roi = cfg.gate.roi;
                }
            }

            ui.separator();
            ui.checkbox(&mut cfg.gate.enabled, "啟用主畫面判斷");
            ui.label(
                RichText::new(
                    "判斷指定範圍內是否出現指定顏色,藉此偵測目前是否在遊戲主畫面\
                     (有聊天視窗)。使用者進入商城、拍賣等其他畫面時該範圍不會出現此\
                     顏色,就會暫停聊天判斷,避免持續誤判出訊息。",
                )
                .weak()
                .small(),
            );
            ui.horizontal(|ui| {
                ui.label("顏色");
                if color_picker_rgb(ui, "gate_color", &mut cfg.gate.color) {
                    request_pick(
                        &mut self.pending_pick,
                        &self.full_frame_req,
                        &self.capture_state,
                        &mut self.last_error,
                        PickMode::Color(ColorTarget::Gate),
                    );
                }
                ui.label("容差");
                ui.add(egui::DragValue::new(&mut cfg.gate.tolerance).range(0..=120));
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

                if mode_advanced {
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
                                match self.roi_target {
                                    RoiTarget::Chat => {
                                        if x1 > x0 + 24 && y1 > y0 + 10 {
                                            cfg.roi.x = x0;
                                            cfg.roi.y = y0;
                                            cfg.roi.w = x1 - x0;
                                            cfg.roi.h = y1 - y0;
                                        }
                                    }
                                    RoiTarget::Gate => {
                                        if x1 > x0 + 4 && y1 > y0 + 4 {
                                            cfg.gate.roi.x = x0;
                                            cfg.gate.roi.y = y0;
                                            cfg.gate.roi.w = x1 - x0;
                                            cfg.gate.roi.h = y1 - y0;
                                        }
                                    }
                                }
                                self.drag_start = None;
                            }
                        }
                    }
                }

                // 畫出目前的聊天 ROI 與主畫面判斷 ROI。
                let s = 1.0 / to_frame;
                if cfg.roi.w > 0 {
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
                if cfg.gate.roi.w > 0 {
                    let r = egui::Rect::from_min_size(
                        origin
                            + egui::vec2(cfg.gate.roi.x as f32 * s, cfg.gate.roi.y as f32 * s),
                        egui::vec2(cfg.gate.roi.w as f32 * s, cfg.gate.roi.h as f32 * s),
                    );
                    ui.painter().rect_stroke(
                        r,
                        egui::Rounding::ZERO,
                        egui::Stroke::new(2.0, Color32::from_rgb(255, 140, 60)),
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

    fn ui_channels(&mut self, ui: &mut egui::Ui) {
        let mut save_now = false;
        {
            let mut cfg = self.cfg.write().unwrap();

            ui.label(
                "頻道以「文字顏色」辨識。預設值為近似值,點色票旁的「🎯 吸取畫面顏色」\
                 可直接從遊戲畫面取樣校正:",
            );
            ui.add_space(4.0);

            let mut remove: Option<usize> = None;
            for i in 0..cfg.channels.len() {
                let ch = &mut cfg.channels[i];
                ui.horizontal(|ui| {
                    ui.checkbox(&mut ch.enabled, "");
                    if color_picker_rgb(ui, ("ch_color", i), &mut ch.color) {
                        request_pick(
                            &mut self.pending_pick,
                            &self.full_frame_req,
                            &self.capture_state,
                            &mut self.last_error,
                            PickMode::Color(ColorTarget::Channel(i)),
                        );
                    }
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
        }
        if save_now {
            config::save(&self.cfg.read().unwrap());
        }
    }

    fn ui_views(&mut self, ui: &mut egui::Ui) {
        let mut save_now = false;
        {
            let mut cfg = self.cfg.write().unwrap();

            ui.label(RichText::new("訊息顯示").strong());
            if ui.checkbox(&mut cfg.show_message_meta, "顯示訊息時間/頻道資訊").changed() {
                save_now = true;
            }
            ui.label(
                RichText::new(
                    "關閉時直接把擷取到的截圖放進聊天室,不額外顯示時間戳記跟頻道\
                     標籤(預設關閉)。",
                )
                .weak()
                .small(),
            );
            ui.horizontal(|ui| {
                ui.label("文字大小");
                ui.add(egui::Slider::new(&mut cfg.ui_font_px, 8.0..=40.0).suffix("px"));
            });
            ui.label(
                RichText::new("套用到整個應用程式的所有文字(含這個設定視窗、聊天視窗的時間/頻道標籤等)。")
                    .weak()
                    .small(),
            );
            ui.horizontal(|ui| {
                ui.label("截圖放大倍率");
                ui.add(egui::Slider::new(&mut cfg.image_scale, 1.0..=8.0).suffix("x"));
            });
            ui.separator();

            ui.label(RichText::new("主畫面判斷").strong());
            if ui.checkbox(&mut cfg.gate_auto_hide_chat, "不在遊戲主畫面時自動隱藏聊天視窗").changed() {
                save_now = true;
            }
            ui.label(
                RichText::new(
                    "需要先在「擷取設定」開啟主畫面判斷才有作用;不在主畫面(如商城、\
                     拍賣)時把聊天視窗搬到螢幕外,回到主畫面時自動搬回來,\
                     避免置頂的聊天視窗擋住畫面(預設開啟)。",
                )
                .weak()
                .small(),
            );
            ui.separator();

            ui.label(RichText::new("「全部」檢視的行為").strong());
            ui.horizontal(|ui| {
                if ui
                    .radio_value(
                        &mut cfg.all_view_mode,
                        AllViewMode::ShowMessages,
                        "顯示擷取到的全部對話截圖",
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
                 1. 「擷取設定」選擇遊戲視窗 → 開始擷取,ROI 可用「一般設定」選解析度\n\
                    自動套用,或「進階設定」手動輸入/在畫面上框選\n\
                 2. 「頻道」點色票旁的「🎯 吸取畫面顏色」直接從遊戲畫面取樣校正各頻道顏色\n\
                 3. 「檢視管理」建立自己的分頁(如「社交」=密語+好友+公會)、\n\
                    調整全應用程式的文字大小與截圖放大倍率、選擇聊天視窗內要不要顯示\n\
                    時間/頻道資訊、選擇「全部」檢視要顯示全部對話還是隱藏聊天\n\
                 4. 主視窗工具列可開啟視窗置頂、鎖定視窗避免誤觸拖曳/縮放;\n\
                    切換檢視的系統層級快捷鍵(預設 Shift+Tab)在「檢視管理」設定\n\
                 5. 關掉設定視窗,回到聊天畫面看過濾後的訊息截圖",
            );
            ui.add_space(8.0);
            ui.label(RichText::new("建議的遊戲設定").strong());
            ui.label(
                "將遊戲內聊天視窗背景調成不透明(黑底),顏色分類準確率會大幅提升;\n\
                 遊戲使用視窗模式或無邊框(獨占全螢幕無法擷取)。",
            );
            ui.add_space(8.0);
            ui.label(RichText::new("運作方式").strong());
            ui.label(
                "本工具不做文字辨識(OCR),而是依「文字顏色」判斷 ROI 框住的這則訊息\n\
                 屬於哪個頻道,再把實際擷取到的畫面直接顯示出來,所以不支援文字搜尋。",
            );
            ui.add_space(8.0);
            ui.label(RichText::new("已知限制").strong());
            ui.label(
                "・遊戲最小化時擷取會暫停(Windows.Graphics.Capture 限制)\n\
                 ・顏色分類可能誤判(不同頻道顏色太接近時),請調整頻道容差\n\
                 ・視窗大小改變後,ROI 需要重新框選\n\
                 ・本工具只讀取畫面,不寫入遊戲、不注入、不自動操作;\n\
                   使用第三方工具仍請自行留意遊戲服務條款",
            );
        });
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n).collect();
        format!("{t}…")
    }
}
