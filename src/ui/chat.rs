//! 主視窗:無邊框的聊天疊圖層。
//!
//! 這個 viewport 永遠只有兩塊東西——頂端工具列與訊息截圖列表。沒有系統標題列
//! 也就沒有內建的拖曳與縮放,兩者都在這裡自己補上,而且都能被「鎖定」停用。

use std::time::Duration;

use egui::{Color32, RichText};

use crate::config::{AllViewMode, ViewDef};
use crate::desktop;

use super::settings::settings_viewport_id;

/// 啟動後這段時間內不套用主畫面判斷的自動隱藏。
///
/// 剛開起來一定要看得到視窗:自動隱藏一旦因為任何原因卡住(遊戲切到別的畫面、
/// 判斷顏色被改壞),使用者唯一想得到的救援動作就是「關掉重開」,那條路必須
/// 永遠通。時間到了才交還給主畫面判斷,該藏的還是會藏。
const GATE_STARTUP_GRACE: Duration = Duration::from_secs(5);

impl super::App {
    /// 畫出整個主視窗。這裡的順序等同面板的堆疊順序,不要隨意調換:視窗命令要
    /// 在任何面板之前送出,提示條疊在工具列上面,訊息列表才吃掉剩下的空間。
    pub(super) fn ui_chat_window(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.ensure_window_reachable(&ctx);
        self.sync_gate_visibility(&ctx);

        // 背景拖曳感應區要最先註冊。egui 在同一層(這裡都是 root ui 所在的
        // background 層)裡用「後註冊的蓋在上層」判斷點擊優先權——如果晚於
        // 工具列/面板才呼叫,這片蓋滿全視窗的拖曳區反而會蓋掉底下所有按鈕、
        // 捲軸的點擊(見 issue:整個視窗能拖但按鈕全部點不到)。用 ui.interact
        // 直接掛在 root ui 上(而不是另開一個 egui::Area)是關鍵:另開 Area
        // 即使同樣設 Order::Background,也會是一個獨立的 layer,一旦在工具列
        // 之後才第一次畫出來,就會永遠疊在工具列所在的 background layer 上面。
        if !self.window_locked {
            install_background_drag(ui);
        }

        self.ui_banners(ui);
        self.ui_quit_confirm(&ctx);

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

        // 主視窗無邊框,沒有系統內建的邊緣縮放,這裡補上。鎖定時不裝:滑鼠移到
        // 邊緣也不會顯示縮放游標,明確表示目前不能調整視窗。
        if !self.window_locked {
            install_resize_border(&ctx);
        }
    }

    /// 開機檢查:第一次拿到實際視窗位置時確認它真的落在桌面範圍內,不是就直接
    /// 搬回來。設定檔可能留著螢幕外的座標(舊版本會把自動隱藏用的位置存進去),
    /// 上次用的那台螢幕也可能已經拔掉——「重新開啟應用程式一定看得到視窗」的
    /// 最後一道保險,只做一次。
    fn ensure_window_reachable(&mut self, ctx: &egui::Context) {
        if self.window_pos_checked {
            return;
        }
        // 第一幀還讀不到實際位置,下一幀再檢查。
        let Some(rect) = ctx.input(|i| i.viewport().inner_rect) else {
            return;
        };
        self.window_pos_checked = true;
        let ppp = ctx.pixels_per_point();
        if !desktop::pos_is_reachable([rect.min.x, rect.min.y], ppp) {
            let [x, y] = desktop::fallback_pos(ppp);
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
        }
    }

    /// 主畫面判斷:不在遊戲主畫面時把整個聊天視窗搬到螢幕座標範圍外,主畫面
    /// 再次出現時搬回原位。只在狀態真的改變時才送一次視窗命令。
    fn sync_gate_visibility(&mut self, ctx: &egui::Context) {
        // 「檢視管理」分頁可以關掉這個自動隱藏行為(關掉時視窗不受 gate 狀態影響)。
        let auto_hide_enabled = self.cfg.read().unwrap().gate_auto_hide_chat;
        // 剛啟動的前幾秒一律不隱藏,見 GATE_STARTUP_GRACE。
        let in_startup_grace = self.started_at.elapsed() < GATE_STARTUP_GRACE;
        let want_hidden = auto_hide_enabled && !self.gate_open && !in_startup_grace;
        if want_hidden != self.chat_hidden_by_gate {
            self.chat_hidden_by_gate = want_hidden;
            if want_hidden {
                if let Some(rect) = ctx.input(|i| i.viewport().inner_rect) {
                    self.pos_before_gate_hide = Some([rect.min.x, rect.min.y]);
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                    -32000.0, -32000.0,
                )));
            } else {
                // 還原一定要送出位置命令:沒記到隱藏前的位置(或記到的位置已經
                // 不在桌面上了,例如那台螢幕拔掉了)就退到桌面左上角——只要少送
                // 這一次,視窗就留在螢幕外再也回不來了。
                let ppp = ctx.pixels_per_point();
                let [x, y] = self
                    .pos_before_gate_hide
                    .take()
                    .filter(|p| desktop::pos_is_reachable(*p, ppp))
                    .unwrap_or_else(|| desktop::fallback_pos(ppp));
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
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
                // 剛送出「從隱藏搬回原位」的那一幀,這裡讀到的還是搬走前的螢幕外
                // 座標(視窗命令下一幀才生效),照寫的話設定檔就會存到 -32000,
                // 下次啟動視窗直接生在看不見的地方。存不出「叫得回來的視窗」的
                // 位置一律跳過,下一幀位置正常了自然會補寫。
                if !desktop::pos_is_reachable(pos, ctx.pixels_per_point()) {
                    return;
                }
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
            ui.separator();
            if ui
                .button(RichText::new("X").color(Color32::LIGHT_RED))
                .on_hover_text("結束應用程式")
                .clicked()
            {
                self.quit_confirm = true;
            }
        });
    }

    /// 結束應用程式前的確認提示。按鈕就在工具列上,跟「清空訊息」相鄰,
    /// 直接送出關閉指令太容易被滑鼠移動路徑上的誤觸點到——多這一步確認。
    /// 用 egui::Modal(有背景遮罩、擋掉底下工具列的點擊)而不是普通 Window,
    /// 避免使用者在確認提示還開著時繼續操作主視窗。
    fn ui_quit_confirm(&mut self, ctx: &egui::Context) {
        if !self.quit_confirm {
            return;
        }
        let resp = egui::Modal::new(egui::Id::new("quit_confirm")).show(ctx, |ui| {
            ui.set_max_width(240.0);
            ui.label("確定要結束應用程式嗎?");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("取消").clicked() {
                    self.quit_confirm = false;
                }
                if ui.button(RichText::new("結束").color(Color32::LIGHT_RED)).clicked() {
                    self.quit_requested = true;
                }
            });
        });
        if resp.backdrop_response.clicked()
            || ctx.input(|i| i.key_pressed(egui::Key::Escape))
        {
            self.quit_confirm = false;
        }
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

/// 無邊框視窗補上「按住背景拖曳整個視窗」——蓋滿整個視窗的拖曳感應區,直接
/// 掛在呼叫端傳入的 root ui 上(同一個 layer),而不是另開 egui::Area(理由
/// 見呼叫端註解)。之後才畫的按鈕、捲軸在 egui 的 hit-test 裡都算「疊在這一
/// 層上面」,會蓋過這片背景搶到點擊,只有點在真正的空白背景上才會觸發拖曳。
fn install_background_drag(ui: &mut egui::Ui) {
    let rect = ui.max_rect();
    let resp = ui.interact(rect, egui::Id::new("chat_bg_drag"), egui::Sense::drag());
    if resp.drag_started() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
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
