//! 全螢幕畫面擷取工具:框選 ROI 與吸取顏色共用的第三個 viewport。
//!
//! 兩種操作共用同一套疊圖 + 十字準心 + 放大鏡機制(見 [`super::PickMode`]),
//! 只有點擊語意跟套用結果的地方不同。順帶放了觸發它的色票按鈕。

use std::sync::atomic::{AtomicBool, Ordering};

use egui::{Color32, RichText};

use crate::config::Roi;
use crate::model::CaptureState;

use super::{ColorTarget, PickMode, RoiTarget};

/// 全螢幕畫面擷取工具視窗固定的 ViewportId。
fn screen_picker_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("mushroom_chat_helper_screen_picker")
}

impl super::App {
    /// 全螢幕置頂顯示剛擷取到的全解析度畫面,滑鼠變成十字準心,旁邊跟著一個
    /// 放大鏡(逐像素取樣)顯示目前指到的座標與顏色。共用同一套機制服務兩種
    /// 操作(見 PickMode):框選 ROI 用「點兩下」決定範圍(點一下設定起點,
    /// 再點一下設定終點)而不是拖曳——egui 的拖曳判定要先移動超過一小段門檻
    /// 距離才會觸發,拖曳起點記到的其實是「觸發當下」的位置而不是使用者實際
    /// 按下滑鼠的位置,框選框看起來才會跟滑鼠起始位置有落差;吸取顏色則單純
    /// 點一下就直接取樣目前像素套用。有效座標/顏色一律用同一個 to_frame()
    /// 換算、同一個 effective 座標,確保框選/吸色跟放大鏡顯示的完全一致。
    pub(super) fn ui_screen_picker(&mut self, ctx: &egui::Context) {
        if self.screen_pick.is_none() {
            return;
        }

        ctx.show_viewport_immediate(
            screen_picker_viewport_id(),
            egui::ViewportBuilder::default()
                .with_title("畫面擷取")
                .with_decorations(false)
                .with_fullscreen(true)
                .with_window_level(egui::viewport::WindowLevel::AlwaysOnTop),
            |ctx, _class| {
                // 全螢幕視窗底下 Windows 常常不畫系統游標(尤其是 with_fullscreen),
                // 光靠 set_cursor_icon 使用者會完全看不到滑鼠在哪。改成隱藏系統游標,
                // 自己畫一條貫穿全螢幕的十字準心,精準度更高、也保證看得到。
                ctx.set_cursor_icon(egui::CursorIcon::None);
                let cancel = ctx
                    .input(|i| i.key_pressed(egui::Key::Escape) || i.viewport().close_requested());

                egui::CentralPanel::default()
                    .frame(egui::Frame::none().fill(Color32::from_gray(15)))
                    .show(ctx, |ui| {
                        let (mode, frame_w, frame_h, tex_id) = {
                            let p = self.screen_pick.as_ref().unwrap();
                            (p.mode, p.frame_w, p.frame_h, p.tex.id())
                        };
                        let click_start = self.screen_pick.as_ref().unwrap().click_start;

                        let hint = match mode {
                            PickMode::Roi(target) => {
                                let target_label = match target {
                                    RoiTarget::Chat => "聊天範圍",
                                    RoiTarget::Gate => "主畫面判斷範圍",
                                };
                                let step_hint = if click_start.is_some() {
                                    "點一下設定終點"
                                } else {
                                    "點一下設定起點"
                                };
                                format!("框選「{target_label}」—{step_hint},Esc 取消")
                            }
                            PickMode::Color(_) => "點一下擷取顏色,Esc 取消".to_string(),
                        };
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(hint).color(Color32::WHITE).strong());
                        });

                        let avail = ui.available_size();
                        let (fw, fh) = (frame_w as f32, frame_h as f32);
                        let scale = (avail.x / fw).min(avail.y / fh).max(0.01);
                        let shown = egui::vec2(fw * scale, fh * scale);
                        let area_min = ui.min_rect().min;
                        let area_rect = egui::Rect::from_min_size(area_min, avail);
                        let img_min = area_min + (avail - shown) / 2.0;
                        let img_rect = egui::Rect::from_min_size(img_min, shown);

                        let resp = ui.interact(
                            area_rect,
                            ui.id().with("screen_pick_area"),
                            egui::Sense::click(),
                        );

                        ui.painter().image(
                            tex_id,
                            img_rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );

                        // 螢幕座標 → 擷取畫面座標,以及反方向。框選/吸色的座標跟放大鏡
                        // 顯示的座標一律用這兩個函式換算,確保兩邊看到的數字一致。
                        let to_frame = |p: egui::Pos2| -> (u32, u32) {
                            let rel = p - img_rect.min;
                            let fx = (rel.x / img_rect.width() * fw).clamp(0.0, fw - 1.0);
                            let fy = (rel.y / img_rect.height() * fh).clamp(0.0, fh - 1.0);
                            (fx as u32, fy as u32)
                        };
                        let to_screen = |fc: (u32, u32)| -> egui::Pos2 {
                            egui::pos2(
                                img_rect.min.x + (fc.0 as f32 + 0.5) / fw * img_rect.width(),
                                img_rect.min.y + (fc.1 as f32 + 0.5) / fh * img_rect.height(),
                            )
                        };

                        // 目前有效座標:滑鼠移動時用滑鼠位置校正,滑鼠沒動的話維持
                        // 上次的值——這樣 W/A/S/D 微調出來的位置才不會被滑鼠「拉回去」。
                        // 十字準心、放大鏡、框選/吸色座標全部只認這一個座標,確保一致。
                        let hover = ctx.input(|i| i.pointer.hover_pos());
                        {
                            let p = self.screen_pick.as_mut().unwrap();
                            let moved = match (p.last_hover, hover) {
                                (Some(a), Some(b)) => a.distance(b) > 0.5,
                                (None, Some(_)) => true,
                                _ => false,
                            };
                            if moved {
                                if let Some(h) = hover {
                                    p.effective = to_frame(h);
                                }
                            }
                            p.last_hover = hover;

                            let mut dx: i32 = 0;
                            let mut dy: i32 = 0;
                            ctx.input(|i| {
                                if i.key_pressed(egui::Key::A) {
                                    dx -= 1;
                                }
                                if i.key_pressed(egui::Key::D) {
                                    dx += 1;
                                }
                                if i.key_pressed(egui::Key::W) {
                                    dy -= 1;
                                }
                                if i.key_pressed(egui::Key::S) {
                                    dy += 1;
                                }
                            });
                            if dx != 0 || dy != 0 {
                                let nx = (p.effective.0 as i32 + dx).clamp(0, frame_w as i32 - 1);
                                let ny = (p.effective.1 as i32 + dy).clamp(0, frame_h as i32 - 1);
                                p.effective = (nx as u32, ny as u32);
                            }
                        }
                        let effective = self.screen_pick.as_ref().unwrap().effective;
                        let effective_screen = to_screen(effective);

                        // 框選模式下,已經點了起點的話,畫出起點到目前有效座標的即時預覽矩形。
                        if let PickMode::Roi(_) = mode {
                            if let Some(start_fc) = click_start {
                                let rect =
                                    egui::Rect::from_two_pos(to_screen(start_fc), effective_screen);
                                ui.painter().rect_stroke(
                                    rect,
                                    egui::Rounding::ZERO,
                                    egui::Stroke::new(2.0_f32, Color32::YELLOW),
                                );
                                ui.painter().circle_filled(
                                    to_screen(start_fc),
                                    3.0,
                                    Color32::YELLOW,
                                );
                            }
                        }

                        let mut apply_roi: Option<Roi> = None;
                        let mut apply_color: Option<[u8; 3]> = None;
                        if resp.clicked() {
                            let fc = effective;
                            match mode {
                                PickMode::Roi(target) => match click_start {
                                    None => {
                                        self.screen_pick.as_mut().unwrap().click_start = Some(fc);
                                    }
                                    Some(start) => {
                                        self.screen_pick.as_mut().unwrap().click_start = None;
                                        let x0 = start.0.min(fc.0);
                                        let y0 = start.1.min(fc.1);
                                        let x1 = start.0.max(fc.0);
                                        let y1 = start.1.max(fc.1);
                                        let (min_w, min_h) = match target {
                                            RoiTarget::Chat => (24, 10),
                                            RoiTarget::Gate => (4, 4),
                                        };
                                        if x1 > x0 + min_w && y1 > y0 + min_h {
                                            apply_roi = Some(Roi {
                                                x: x0,
                                                y: y0,
                                                w: x1 - x0,
                                                h: y1 - y0,
                                            });
                                        } else {
                                            self.last_error =
                                                Some("框選範圍太小,請重新點選起點".into());
                                        }
                                    }
                                },
                                PickMode::Color(_) => {
                                    let p = self.screen_pick.as_ref().unwrap();
                                    let o = ((fc.1 * p.frame_w + fc.0) * 4) as usize;
                                    apply_color = Some([p.rgba[o], p.rgba[o + 1], p.rgba[o + 2]]);
                                }
                            }
                        }

                        {
                            let screen = ctx.input(|i| i.screen_rect());
                            draw_crosshair(ui.painter(), screen, effective_screen);
                            let rgba = &self.screen_pick.as_ref().unwrap().rgba;
                            draw_magnifier(
                                ui.painter(),
                                screen,
                                effective_screen,
                                rgba,
                                frame_w,
                                frame_h,
                                effective.0,
                                effective.1,
                            );
                        }

                        if let Some(roi) = apply_roi {
                            if let PickMode::Roi(target) = mode {
                                let mut cfg = self.cfg.write().unwrap();
                                match target {
                                    RoiTarget::Chat => cfg.roi = roi,
                                    RoiTarget::Gate => cfg.gate.roi = roi,
                                }
                            }
                            self.screen_pick = None;
                        }
                        if let Some(rgb) = apply_color {
                            if let PickMode::Color(color_target) = mode {
                                let mut cfg = self.cfg.write().unwrap();
                                match color_target {
                                    ColorTarget::Channel(i) => {
                                        if let Some(ch) = cfg.channels.get_mut(i) {
                                            ch.color = rgb;
                                        }
                                    }
                                    ColorTarget::Gate => cfg.gate.color = rgb,
                                }
                            }
                            self.screen_pick = None;
                        }
                    });

                if cancel {
                    self.screen_pick = None;
                }
            },
        );
    }
}

/// 請求全螢幕畫面擷取工具(框選 ROI 或吸取顏色)。要有畫面在跑才有東西可以
/// 擷取,所以只有擷取中才會真的送出要求;沒在擷取就直接顯示錯誤訊息。
/// 寫成自由函式、個別借用 App 的欄位而不是拿 &mut self——呼叫處大多在
/// 已經持有 `self.cfg.write()` guard 的範圍內,`self.xxx(...)` 這種要整個
/// &mut self 的方法呼叫會跟 guard 借用衝突,拆開個別欄位借用才不會撞。
pub(super) fn request_pick(
    pending_pick: &mut Option<PickMode>,
    full_frame_req: &AtomicBool,
    capture_state: &CaptureState,
    last_error: &mut Option<String>,
    mode: PickMode,
) {
    if matches!(capture_state, CaptureState::Running(_)) {
        *pending_pick = Some(mode);
        full_frame_req.store(true, Ordering::Relaxed);
    } else {
        *last_error = Some("請先開始擷取,才能在畫面上操作".into());
    }
}

/// 自訂色票按鈕:點一下開一個只有 R/G/B(0-255 整數)欄位跟「🎯 吸取畫面顏色」
/// 按鈕的小視窗。刻意不用 egui 內建的 color_edit_button(它還有 HSV/Hex 格式
/// 切換)——這裡只在乎「文字顏色的 RGB 數值」,格式轉換只是多餘的換算。
/// 回傳 true 表示這一幀使用者按下了「吸取畫面顏色」。
pub(super) fn color_picker_rgb(ui: &mut egui::Ui, id_salt: impl std::hash::Hash, color: &mut [u8; 3]) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(28.0, 18.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let c = Color32::from_rgb(color[0], color[1], color[2]);
        ui.painter().rect_filled(rect, 2.0, c);
        ui.painter().rect_stroke(rect, 2.0, egui::Stroke::new(1.0_f32, Color32::from_gray(90)));
    }

    let popup_id = ui.make_persistent_id(id_salt);
    if resp.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup_id));
    }

    let mut eyedrop_clicked = false;
    egui::popup_below_widget(
        ui,
        popup_id,
        &resp,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            ui.set_min_width(150.0);
            for (label, ch) in [("R", 0usize), ("G", 1), ("B", 2)] {
                ui.horizontal(|ui| {
                    ui.label(label);
                    ui.add(egui::DragValue::new(&mut color[ch]).range(0..=255));
                });
            }
            ui.separator();
            if ui.button("🎯 吸取畫面顏色").clicked() {
                eyedrop_clicked = true;
                ui.memory_mut(|m| m.close_popup());
            }
        },
    );
    eyedrop_clicked
}

/// 貫穿全螢幕的十字準心:系統游標在全螢幕無邊框視窗上常常看不到,自己畫
/// 一條水平線+一條垂直線通過滑鼠位置取代,順便比系統游標更容易對齊像素邊界。
fn draw_crosshair(painter: &egui::Painter, screen: egui::Rect, cursor: egui::Pos2) {
    let stroke = egui::Stroke::new(1.0_f32, Color32::from_rgb(0, 255, 140));
    painter.line_segment(
        [egui::pos2(screen.min.x, cursor.y), egui::pos2(screen.max.x, cursor.y)],
        stroke,
    );
    painter.line_segment(
        [egui::pos2(cursor.x, screen.min.y), egui::pos2(cursor.x, screen.max.y)],
        stroke,
    );
    painter.circle_stroke(cursor, 3.0, egui::Stroke::new(1.5_f32, Color32::WHITE));
}

/// 放大鏡每格(取樣自畫面的一個像素)在螢幕上畫出的邊長,單位:egui point。
const MAG_CELL: f32 = 9.0;
/// 放大鏡以滑鼠指到的像素為中心,左右/上下各取樣幾個像素。
const MAG_RADIUS: i32 = 8;

/// 在滑鼠旁畫出一個逐像素放大鏡:中央十字標記滑鼠實際指到的那個像素,
/// 下方文字顯示該像素在擷取畫面內的 x,y 座標,方便使用者精準對齊 ROI 邊界。
#[allow(clippy::too_many_arguments)]
fn draw_magnifier(
    painter: &egui::Painter,
    screen: egui::Rect,
    cursor: egui::Pos2,
    rgba: &[u8],
    frame_w: u32,
    frame_h: u32,
    fx: u32,
    fy: u32,
) {
    let n = (MAG_RADIUS * 2 + 1) as f32;
    let grid_size = egui::vec2(n * MAG_CELL, n * MAG_CELL);
    let text_h = 20.0;
    let hint_h = 16.0;
    let total_size = grid_size + egui::vec2(8.0, text_h * 2.0 + hint_h + 14.0);

    let mut origin = cursor + egui::vec2(24.0, 24.0);
    if origin.x + total_size.x > screen.max.x {
        origin.x = cursor.x - 24.0 - total_size.x;
    }
    if origin.y + total_size.y > screen.max.y {
        origin.y = cursor.y - 24.0 - total_size.y;
    }
    origin = origin.max(screen.min);

    let bg_rect = egui::Rect::from_min_size(origin, total_size);
    painter.rect_filled(bg_rect, 4.0, Color32::from_black_alpha(235));

    let grid_min = origin + egui::vec2(4.0, 4.0);
    let mut center_color = Color32::from_gray(30);
    for dy in -MAG_RADIUS..=MAG_RADIUS {
        for dx in -MAG_RADIUS..=MAG_RADIUS {
            let sx = fx as i32 + dx;
            let sy = fy as i32 + dy;
            let color = if sx >= 0 && sy >= 0 && (sx as u32) < frame_w && (sy as u32) < frame_h {
                let o = ((sy as u32 * frame_w + sx as u32) * 4) as usize;
                Color32::from_rgb(rgba[o], rgba[o + 1], rgba[o + 2])
            } else {
                Color32::from_gray(30)
            };
            if dx == 0 && dy == 0 {
                center_color = color;
            }
            let cx = grid_min.x + (dx + MAG_RADIUS) as f32 * MAG_CELL;
            let cy = grid_min.y + (dy + MAG_RADIUS) as f32 * MAG_CELL;
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(cx, cy), egui::vec2(MAG_CELL, MAG_CELL)),
                0.0,
                color,
            );
        }
    }
    // 中央十字:標出滑鼠實際指到的那一個像素。
    let center_rect = egui::Rect::from_min_size(
        egui::pos2(
            grid_min.x + MAG_RADIUS as f32 * MAG_CELL,
            grid_min.y + MAG_RADIUS as f32 * MAG_CELL,
        ),
        egui::vec2(MAG_CELL, MAG_CELL),
    );
    painter.rect_stroke(center_rect, 0.0, egui::Stroke::new(1.5_f32, Color32::RED));

    painter.text(
        egui::pos2(grid_min.x, grid_min.y + grid_size.y + 6.0),
        egui::Align2::LEFT_TOP,
        format!("x={fx}  y={fy}"),
        egui::FontId::monospace(14.0),
        Color32::WHITE,
    );
    let rgb_row_y = grid_min.y + grid_size.y + text_h + 8.0;
    let swatch = egui::Rect::from_min_size(egui::pos2(grid_min.x, rgb_row_y + 2.0), egui::vec2(14.0, 14.0));
    painter.rect_filled(swatch, 2.0, center_color);
    painter.rect_stroke(swatch, 2.0, egui::Stroke::new(1.0_f32, Color32::from_gray(120)));
    painter.text(
        egui::pos2(swatch.max.x + 6.0, rgb_row_y),
        egui::Align2::LEFT_TOP,
        format!("RGB({},{},{})", center_color.r(), center_color.g(), center_color.b()),
        egui::FontId::monospace(14.0),
        Color32::WHITE,
    );
    painter.text(
        egui::pos2(grid_min.x, rgb_row_y + text_h + 2.0),
        egui::Align2::LEFT_TOP,
        "W/A/S/D 微調 1px",
        egui::FontId::proportional(12.0),
        Color32::from_gray(190),
    );
}
