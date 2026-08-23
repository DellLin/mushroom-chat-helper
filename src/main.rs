//! 蘑菇聊天小幫手 — 進入點與執行緒編排。
//!
//! 執行緒:
//!   capture (WGC)  → frame channel(bounded 2,滿了丟幀)→ vision(ROI 裁切/依顏色分類)
//!   vision → ui channel → egui App(直接顯示擷取到的畫面,不做 OCR)
//! 完全唯讀:只擷取畫面,不觸碰遊戲行程。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod capture;
mod config;
mod hotkey;
mod model;
mod ui;
mod vision;

use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, RwLock};

/// 視窗圖示(工作列 / Alt-Tab)。
///
/// RGBA 像素由 build.rs 在建置期從 `assets/icon-256.png` 解好放進 OUT_DIR,
/// 這裡直接嵌現成的位元組 —— 執行期不必為了開一張圖就背一個 PNG 解碼器。
fn window_icon() -> egui::IconData {
    egui::IconData {
        rgba: include_bytes!(concat!(env!("OUT_DIR"), "/window-icon.rgba")).to_vec(),
        width: env!("WINDOW_ICON_W").parse().expect("build.rs 應該要給出寬度"),
        height: env!("WINDOW_ICON_H").parse().expect("build.rs 應該要給出高度"),
    }
}

fn main() -> eframe::Result<()> {
    env_logger::init();

    let cfg = Arc::new(RwLock::new(config::load()));
    let full_frame_req = Arc::new(AtomicBool::new(false));
    let fps = cfg.read().unwrap().fps.max(1) as u64;
    let interval_ms = Arc::new(AtomicU64::new(1000 / fps));

    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let (frame_tx, frame_rx) = crossbeam_channel::bounded(2);
    let (ui_tx, ui_rx) = crossbeam_channel::unbounded();
    let (hotkey_tx, hotkey_cmd_rx) = crossbeam_channel::unbounded();

    capture::wgc::spawn(cmd_rx, frame_tx, ui_tx.clone(), interval_ms.clone());
    vision::spawn(frame_rx, ui_tx.clone(), cfg.clone(), full_frame_req.clone());

    let hk = cfg.read().unwrap().hotkey.clone();
    let initial_vk = hotkey::vk_for_name(&hk.key).unwrap_or(0x09);
    hotkey::spawn(hotkey_cmd_rx, ui_tx, (hk.ctrl, hk.alt, hk.shift, hk.win, initial_vk));

    let (saved_pos, saved_size, saved_on_top) = {
        let c = cfg.read().unwrap();
        (c.window_pos, c.window_size, c.always_on_top)
    };

    let mut viewport = egui::ViewportBuilder::default()
        // 沒有存過大小時(首次啟動)開成長條:這個工具是疊在遊戲聊天列上看的,
        // 方形視窗一開就得手動拉。932 寬在 1366x768 上也塞得下,不會開到畫面外。
        .with_inner_size(saved_size.unwrap_or([932.0, 152.0]))
        // 留一個很小的下限,純粹避免視窗被縮到連邊框拖曳感應區都抓不到的程度;
        // 工具列本身用 horizontal_wrapped,窄的時候會自動換行,不會被裁切/遮住。
        .with_min_inner_size([260.0, 90.0])
        .with_title(format!("蘑菇聊天小幫手 v{}", env!("CARGO_PKG_VERSION")))
        // exe 本身的圖示由 build.rs 嵌成資源(檔案總管/捷徑看到的那個);這裡設的
        // 是視窗圖示,決定工作列與 Alt-Tab 顯示什麼。兩者是不同來源,都要設 ——
        // 實測過:只嵌 exe 資源、不呼叫 with_icon 的話,視窗會拿到 winit 的預設
        // 圖示,而不是退回用 exe 的 .ico。
        .with_icon(window_icon())
        // 無工具列(無最小化/最大化/關閉鈕),疊在遊戲聊天視窗上時才不會露出裝飾邊框;
        // 結束應用程式改放進設定視窗裡。
        .with_decorations(false);
    if let Some(pos) = saved_pos {
        viewport = viewport.with_position(pos);
    }
    if saved_on_top {
        viewport = viewport.with_always_on_top();
    }

    let options = eframe::NativeOptions { viewport, ..Default::default() };

    let init = ui::AppInit { cfg, interval_ms, cmd_tx, ui_rx, hotkey_tx, full_frame_req };
    eframe::run_native(
        "蘑菇聊天小幫手",
        options,
        Box::new(move |cc| Ok(Box::new(ui::App::new(cc, init)))),
    )
}
