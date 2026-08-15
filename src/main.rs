//! 楓之谷聊天篩選器 — 進入點與執行緒編排。
//!
//! 執行緒:
//!   capture (WGC)  → frame channel(bounded 2,滿了丟幀)→ vision(切行/分類/OCR)
//!   vision → ui channel → egui App
//! 完全唯讀:只擷取畫面,不觸碰遊戲行程。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod capture;
mod config;
mod hotkey;
mod model;
mod ocr;
mod textutil;
mod ui;
mod vision;

use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, RwLock};

fn main() -> eframe::Result<()> {
    env_logger::init();

    let cfg = Arc::new(RwLock::new(config::load()));
    let calibrating = Arc::new(AtomicBool::new(false));
    let fps = cfg.read().unwrap().fps.max(1) as u64;
    let interval_ms = Arc::new(AtomicU64::new(1000 / fps));

    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let (frame_tx, frame_rx) = crossbeam_channel::bounded(2);
    let (ui_tx, ui_rx) = crossbeam_channel::unbounded();
    let (hotkey_tx, hotkey_cmd_rx) = crossbeam_channel::unbounded();

    capture::wgc::spawn(cmd_rx, frame_tx, ui_tx.clone(), interval_ms.clone());
    vision::spawn(frame_rx, ui_tx.clone(), cfg.clone(), calibrating.clone());

    let hk = cfg.read().unwrap().hotkey.clone();
    let initial_vk = hotkey::vk_for_name(&hk.key).unwrap_or(0x09);
    hotkey::spawn(hotkey_cmd_rx, ui_tx, (hk.ctrl, hk.alt, hk.shift, hk.win, initial_vk));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([480.0, 320.0])
            // 留一個很小的下限,純粹避免視窗被縮到連邊框拖曳感應區都抓不到的程度;
            // 工具列本身用 horizontal_wrapped,窄的時候會自動換行,不會被裁切/遮住。
            .with_min_inner_size([260.0, 90.0])
            .with_title(format!("楓之谷聊天篩選器 v{}", env!("CARGO_PKG_VERSION")))
            // 無工具列(無最小化/最大化/關閉鈕),疊在遊戲聊天視窗上時才不會露出裝飾邊框;
            // 結束應用程式改放進設定視窗裡。
            .with_decorations(false),
        ..Default::default()
    };

    let init = ui::AppInit { cfg, calibrating, interval_ms, cmd_tx, ui_rx, hotkey_tx };
    eframe::run_native(
        "楓之谷聊天篩選器",
        options,
        Box::new(move |cc| Ok(Box::new(ui::App::new(cc, init)))),
    )
}
