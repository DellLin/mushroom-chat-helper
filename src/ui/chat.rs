//! 主視窗:無邊框的聊天疊圖層。
//!
//! 這個 viewport 永遠只有兩塊東西——頂端工具列與訊息截圖列表。沒有系統標題列
//! 也就沒有內建的拖曳與縮放,兩者都在這裡自己補上,而且都能被「鎖定」停用。

use egui::{Color32, RichText};

use crate::config::{AllViewMode, ViewDef};

use super::settings::settings_viewport_id;

impl super::App {
    /// 畫出整個主視窗。這裡的順序等同面板的堆疊順序,不要隨意調換:視窗命令要
    /// 在任何面板之前送出,提示條疊在工具列上面,訊息列表才吃掉剩下的空間。
    pub(super) fn ui_chat_window(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.sync_gate_visibility(&ctx);
        self.ui_banners(ui);

        // 主視窗永遠只顯示聊天,大小/字級才能貼著遊戲聊天視窗調整、疊上去用。
        // 所有設定都收在另一個獨立的作業系統視窗裡,按 ⚙ 才會打開。
        let toolbar_resp =
            egui::Panel::top("chat_toolbar").show(ui, |ui| self.ui_chat_toolbar(ui));
        let toolbar_h = toolbar_resp.response.rect.height();

        let want_collapsed = self.sync_chat_collapse(&ctx, toolbar_h);
        self.remember_window_geometry(&ctx);

        if !want_collapsed {
            egui::CentralPanel::default().show(ui, |ui| self.ui_chat_messages(ui));
        }

        // 主視窗無邊框,少了系統內建的邊緣拖曳縮放,補上手動的邊框/角落感應區。
        // 鎖定時不裝這些感應區,滑鼠移過去也不會顯示縮放游標,明確表示目前不能調整。
        if !self.window_locked {
            install_resize_border(&ctx);
        }
    }

    /// 主畫面判斷:不在遊戲主畫面時把整個聊天視窗搬到螢幕座標範圍外,主畫面
    /// 再次出現時搬回原位。只在狀態真的改變時才送一次視窗命令。
    fn sync_gate_visibility(&mut self, ctx: &egui::Context) {
        // 「檢視管理」分頁可以關掉這個自動隱藏行為(關掉時視窗不受 gate 狀態影響)。
        let auto_hide_enabled = self.cfg.read().unwrap().gate_auto_hide_chat;
        let want_hidden = auto_hide_enabled && !self.gate_open;
        if want_hidden != self.chat_hidden_by_gate {
            self.chat_hidden_by_gate = want_hidden;
            if want_hidden {
                if let Some(rect) = ctx.input(|i| i.viewport().inner_rect) {
                    self.pos_before_gate_hide = Some([rect.min.x, rect.min.y]);
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                    -32000.0, -32000.0,
                )));
            } else if let Some(pos) = self.pos_before_gate_hide.take() {
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                    pos[0], pos[1],
                )));
            }
        }
    }

    /// 疊在工具列上方的提示條:找不到中文字型的警告,以及最近一則錯誤訊息。
    fn ui_banners(&mut self, ui: &mut egui::Ui) {
        if !self.font_ok {
            egui::Panel::top("font_warn").show(ui, |ui| {
                ui.label(
                    RichText::new("找不到系統中文字型(msjh.ttc),中文可能顯示為方框")
                        .color(Color32::YELLOW),
                );
            });
        }
        if let Some(err) = self.last_error.clone() {
            egui::Panel::top("err").show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&err).color(Color32::LIGHT_RED));
                    if ui.button("✕").clicked() {
                        self.last_error = None;
                    }
                });
            });
        }
    }

    /// 依目前檢視決定要不要把視窗收合成只剩工具列,回傳「這一幀是否收合」。
    fn sync_chat_collapse(&mut self, ctx: &egui::Context, toolbar_h: f32) -> bool {
        // 選「全部」且設定為「隱藏聊天」時,直接把視窗縮到只剩工具列高度,內容區塊實際消失
        // (不是用半透明蓋掉),底下的遊戲聊天視窗自然就露出來;不依賴 Windows 桌面合成的透明效果。
        // 設定為「顯示全部對話」時則不收合,正常顯示未過濾的訊息列表。
        let hide_chat_on_all =
            matches!(self.cfg.read().unwrap().all_view_mode, AllViewMode::HideChat);
        let want_collapsed = self.active_view.is_none() && hide_chat_on_all;
        if want_collapsed != self.chat_collapsed {
            let screen = ctx.input(|i| i.viewport_rect());
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
        want_collapsed
    }

    /// 記住主視窗位置/大小,下次啟動時還原(見 main.rs 讀取 cfg.window_pos /
    /// window_size 建立初始 ViewportBuilder)。
    fn remember_window_geometry(&mut self, ctx: &egui::Context) {
        // 收合成只剩工具列高度時的尺寸不算「使用者調整過的大小」,故意不寫回,
        // 避免下次啟動直接變成收合的高度;gate 搬到螢幕外時也不算「使用者調整過
        // 的位置」,一樣故意不寫回,不然下次啟動就會直接生在螢幕外面。
        if !self.chat_hidden_by_gate {
            if let Some(rect) = ctx.input(|i| i.viewport().inner_rect) {
                let pos = [rect.min.x, rect.min.y];
                let size = [rect.width(), rect.height()];
                let needs_update = {
                    let cfg = self.cfg.read().unwrap();
                    cfg.window_pos != Some(pos)
                        || (!self.chat_collapsed && cfg.window_size != Some(size))
                };
                if needs_update {
                    let mut cfg = self.cfg.write().unwrap();
                    cfg.window_pos = Some(pos);
                    if !self.chat_collapsed {
                        cfg.window_size = Some(size);
                    }
                }
            }
        }
    }

    /// 頂端工具列:檢視切換、清空訊息、截圖放大倍率、視窗置頂、設定、鎖定。
    /// 主視窗沒有系統標題列,拖曳視窗的感應區也做在這一列的背景上。
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
                AllViewMode::ShowMessages => "顯示擷取到的全部對話截圖,不套用頻道過濾",
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
            if ui.button("清空訊息").clicked() {
                self.messages.clear();
            }
            ui.separator();
            ui.label("放大");
            {
                let mut cfg = self.cfg.write().unwrap();
                ui.add(
                    egui::DragValue::new(&mut cfg.image_scale)
                        .range(1.0..=8.0)
                        .speed(0.1)
                        .suffix("x"),
                );
            }
            if ui.checkbox(&mut self.always_on_top, "視窗置頂").changed() {
                let level = if self.always_on_top {
                    egui::viewport::WindowLevel::AlwaysOnTop
                } else {
                    egui::viewport::WindowLevel::Normal
                };
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
                // 設定視窗(如果開著)跟著同步置頂狀態,見下方 show_viewport_immediate。
                self.cfg.write().unwrap().always_on_top = self.always_on_top;
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

    /// 訊息列表:直接顯示每則訊息擷取到的畫面截圖(不做 OCR)。選「全部」且設定為
    /// 「隱藏聊天」時,這裡不會被呼叫(見 update() 的視窗收合邏輯);設定為「顯示全部
    /// 對話」時則正常顯示、不套用頻道過濾。
    fn ui_chat_messages(&mut self, ui: &mut egui::Ui) {
        let cfg = self.cfg.read().unwrap().clone();
        let allowed: Option<&ViewDef> = self.active_view.and_then(|i| cfg.views.get(i));
        let scale = cfg.image_scale.clamp(1.0, 8.0);

        // 用同一個雙向捲動區包住整份訊息列表(而不是每張截圖各自一個水平捲動區),
        // 這樣只會有一條橫向卷軸出現在整個視窗底部,拖曳它時所有截圖(跟時間/
        // 頻道標籤)一起橫向移動,而不是每張圖各滾各的。
        egui::ScrollArea::both()
            .stick_to_bottom(true)
            .auto_shrink(false)
            .show(ui, |ui| {
                // 每則訊息之間不留任何垂直間距:截圖本身已經帶著遊戲聊天視窗
                // 的行距,再加上元件間距會讓疊在遊戲上的對話被拉開、對不上原本
                // 的行位置。水平間距(時間與頻道標籤之間)維持預設。
                ui.spacing_mut().item_spacing.y = 0.0;
                for m in &self.messages {
                    if let Some(v) = allowed {
                        if !v.channels.contains(&m.channel) {
                            continue;
                        }
                    }
                    if cfg.show_message_meta {
                        let (name, color) = match cfg.channel_by_id(&m.channel) {
                            Some(c) => (
                                c.name.clone(),
                                Color32::from_rgb(c.color[0], c.color[1], c.color[2]),
                            ),
                            None => ("未分類".to_string(), Color32::GRAY),
                        };
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(m.time.format("%H:%M:%S").to_string())
                                    .weak()
                                    .size(cfg.ui_font_px * 0.7),
                            );
                            ui.label(
                                RichText::new(format!("[{name}]"))
                                    .color(color)
                                    .strong()
                                    .size(cfg.ui_font_px),
                            );
                        });
                    }
                    let size = egui::vec2(m.w as f32 * scale, m.h as f32 * scale);
                    ui.add(egui::Image::new(&m.tex).fit_to_exact_size(size));
                }
            });
    }
}

/// 無邊框視窗補上邊緣/角落拖曳縮放感應區(系統少了內建的視窗邊框可以拖)。
fn install_resize_border(ctx: &egui::Context) {
    use egui::viewport::ResizeDirection as R;
    use egui::CursorIcon as C;

    // 只留右邊/下面/右下角可以拖曳縮放——上面跟左邊拿掉,避免點選聊天工具列
    // (在視窗最上方)時不小心誤觸縮放感應區。
    const T: f32 = 8.0;
    let s = ctx.viewport_rect();

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
