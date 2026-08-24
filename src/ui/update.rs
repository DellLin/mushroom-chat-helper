//! 設定視窗的「更新」分頁:檢查新版本、下載、換裝。
//!
//! 為什麼是分頁,不是自己開一個對話框?更新提示屬於「一定要讓使用者看到」的
//! 東西,而主視窗靠不住——它會被收合成只剩工具列(見 `chat::sync_chat_collapse`),
//! 也可能正被主畫面判斷搬到螢幕外。設定視窗剛好是這個程式唯一「有系統標題列、
//! 位置正常、一定看得到」的視窗,借它來講話最穩,也不必為了一個對話框再養一個
//! viewport 與它自己的生命週期。查到新版時自動把設定視窗打開並切到這一頁
//! (見 [`UpdateUi::wants_attention`] 與 `ui::App::drain_events`)。
//!
//! 「啟動時自動檢查」開關與手動檢查也放在這一頁,更新相關的東西集中一處。

use std::path::PathBuf;

use egui::{Color32, RichText};

use crate::config;
use crate::update::{self, Release, UpdateEvent};

/// 這一輪更新流程走到哪了。
#[derive(Default, PartialEq)]
pub(super) enum Stage {
    /// 沒有在進行任何更新相關的事。
    #[default]
    Idle,
    Checking,
    Available,
    Downloading {
        done: u64,
        total: Option<u64>,
    },
    /// 新版已經就定位,等使用者按重新啟動。
    Installed(PathBuf),
    UpToDate,
    Failed(String),
}

/// 更新分頁的狀態。
#[derive(Default)]
pub(super) struct UpdateUi {
    stage: Stage,
    /// 使用者正在等這件事的結果嗎?
    ///
    /// 開機時的自動檢查是背景行為,「已是最新版」與「檢查失敗」都安靜略過——
    /// 網路不通、GitHub 掛掉都不是使用者能處理的事,為它跳出視窗只是打擾。
    /// 但只要他自己按過「檢查更新」或「立即更新」,就換成另一回事了:人在等
    /// 回應,這時候不吭聲等於按了按鈕卻什麼都沒發生。
    user_initiated: bool,
    /// 這個結果已經自動把設定視窗叫出來過了,不要每幀重複叫。
    announced: bool,
    /// 最近一次查到的版本。下載/安裝階段還要用它顯示版號與手動下載連結。
    release: Option<Box<Release>>,
}

impl UpdateUi {
    /// 使用者按下「檢查更新」。
    fn begin_manual_check(&mut self) {
        self.stage = Stage::Checking;
        self.user_initiated = true;
        self.announced = true; // 人就在這一頁上看著,不用再叫視窗
    }

    /// 使用者按下「立即更新」。
    fn begin_install(&mut self) {
        self.user_initiated = true;
        self.stage = Stage::Downloading { done: 0, total: None };
    }

    pub(super) fn apply(&mut self, event: UpdateEvent) {
        match event {
            UpdateEvent::UpToDate => self.stage = Stage::UpToDate,
            UpdateEvent::Available(release) => {
                self.release = Some(release);
                self.stage = Stage::Available;
            }
            UpdateEvent::Progress { done, total } => {
                self.stage = Stage::Downloading { done, total }
            }
            UpdateEvent::Installed(exe) => self.stage = Stage::Installed(exe),
            UpdateEvent::Failed(msg) => {
                log::warn!("更新流程失敗: {msg}");
                self.stage = Stage::Failed(msg);
            }
        }
    }

    /// 這個結果值得把設定視窗叫出來嗎?只回報一次(叫過就記著)。
    ///
    /// 只有「沒人在等的背景檢查」得到的沉默結果不吵人:查到新版一定要說,
    /// 使用者自己按出來的結果也一定要回話。
    pub(super) fn wants_attention(&mut self) -> bool {
        let worth_it = match self.stage {
            Stage::Idle | Stage::Checking => false,
            Stage::UpToDate | Stage::Failed(_) => self.user_initiated,
            Stage::Available | Stage::Downloading { .. } | Stage::Installed(_) => true,
        };
        if worth_it && !self.announced {
            self.announced = true;
            return true;
        }
        false
    }

    /// 目前是不是有事情正在跑(用來擋掉重複點擊)。
    fn is_busy(&self) -> bool {
        matches!(self.stage, Stage::Checking | Stage::Downloading { .. })
    }
}

impl super::App {
    /// 設定視窗的「更新」分頁。
    pub(super) fn ui_update_tab(&mut self, ui: &mut egui::Ui) {
        self.ui_update_status(ui);
        self.ui_update_prefs(ui);
    }

    /// 上半部:這一輪檢查/下載的狀態。Idle 時整段不畫。
    fn ui_update_status(&mut self, ui: &mut egui::Ui) {
        match &self.update.stage {
            Stage::Idle => return,
            Stage::Checking => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("正在跟 GitHub 確認有沒有新版本…");
                });
            }
            Stage::UpToDate => {
                ui.label(
                    RichText::new(format!("✔ 目前已是最新版本(v{})。", update::CURRENT))
                        .color(Color32::LIGHT_GREEN),
                );
            }
            Stage::Available => self.ui_update_available(ui),
            Stage::Downloading { done, total } => {
                let (done, total) = (*done, *total);
                ui.label(RichText::new("正在下載新版本…").strong());
                ui.add_space(6.0);
                // 對方沒給 Content-Length 就沒有百分比可以算,改顯示已下載的量。
                match total {
                    Some(total) if total > 0 => {
                        ui.add(
                            egui::ProgressBar::new(done as f32 / total as f32)
                                .show_percentage()
                                .animate(true),
                        );
                        ui.label(
                            RichText::new(format!("{} / {}", mib(done), mib(total)))
                                .weak()
                                .small(),
                        );
                    }
                    _ => {
                        ui.add(egui::ProgressBar::new(0.0).animate(true));
                        ui.label(RichText::new(format!("已下載 {}", mib(done))).weak().small());
                    }
                }
                ui.label(
                    RichText::new("下載完成後會核對 SHA256,確認無誤才會取代執行檔。")
                        .weak()
                        .small(),
                );
            }
            Stage::Installed(exe) => {
                let exe = exe.clone();
                ui.label(
                    RichText::new("✔ 新版本已經準備好了。")
                        .color(Color32::LIGHT_GREEN)
                        .strong(),
                );
                ui.label("重新啟動後就會換成新版本。現在不重啟也沒關係,下次開啟時一樣是新版。");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("🔄 立即重新啟動").strong()).clicked() {
                        self.restart_into(&exe);
                    }
                    if ui.button("稍後再說").clicked() {
                        self.update.stage = Stage::Idle;
                    }
                });
            }
            Stage::Failed(msg) => {
                let msg = msg.clone();
                let page = self.update.release.as_ref().map(|r| r.page_url.clone());
                ui.label(RichText::new("更新沒有完成").color(Color32::LIGHT_RED).strong());
                ui.label(&msg);
                ui.label(
                    RichText::new("目前這個版本仍然可以正常使用,可以稍後再試或手動下載。")
                        .weak()
                        .small(),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if let Some(page) = page {
                        ui.hyperlink_to("前往下載頁面", page);
                    }
                    if ui.button("知道了").clicked() {
                        self.update.stage = Stage::Idle;
                    }
                });
            }
        }
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
    }

    fn ui_update_available(&mut self, ui: &mut egui::Ui) {
        // 借用要在動到 self.update 之前結束,先把要顯示的東西複製出來。
        let Some((tag, notes, page_url)) = self
            .update
            .release
            .as_ref()
            .map(|r| (r.tag.clone(), r.notes.clone(), r.page_url.clone()))
        else {
            self.update.stage = Stage::Idle;
            return;
        };

        ui.label(RichText::new("發現新版本").heading());
        ui.label(format!("目前版本:v{}　→　最新版本:{tag}", update::CURRENT));
        ui.add_space(8.0);

        if !notes.trim().is_empty() {
            ui.label(RichText::new("更新內容").strong());
            // GitHub 給的是 Markdown 原文,這裡不解析、直接當純文字顯示 ——
            // 為了一段更新說明拉一個 Markdown 算繪器並不划算。
            egui::ScrollArea::vertical()
                .id_salt("update_notes")
                .max_height(200.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.label(RichText::new(notes.trim()).small());
                });
            ui.add_space(8.0);
        }

        ui.horizontal(|ui| {
            if ui.button(RichText::new("⬇ 立即更新").strong()).clicked() {
                if let Some(release) = self.update.release.clone() {
                    self.update.begin_install();
                    update::spawn_install(self.ui_tx.clone(), *release);
                }
            }
            if ui.button("稍後再說").clicked() {
                self.update.stage = Stage::Idle;
            }
            if ui
                .button("略過這個版本")
                .on_hover_text("之後開機不再為這一版跳提示;仍可在這一頁手動檢查更新")
                .clicked()
            {
                self.cfg.write().unwrap().skipped_update_version = Some(tag);
                config::save(&self.cfg.read().unwrap());
                self.update.stage = Stage::Idle;
            }
            ui.separator();
            ui.hyperlink_to("在 GitHub 上查看", page_url);
        });
    }

    /// 下半部:自動檢查開關、手動檢查、略過清單。
    fn ui_update_prefs(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("軟體更新").strong());

        let mut save_now = false;
        let skipped = {
            let mut cfg = self.cfg.write().unwrap();
            if ui
                .checkbox(&mut cfg.auto_check_update, "啟動時自動檢查新版本")
                .changed()
            {
                save_now = true;
            }
            cfg.skipped_update_version.clone()
        };
        ui.label(
            RichText::new(
                "會連到 GitHub 比對版號。更新前一定會核對 SHA256,\
                 且要使用者按下確認才會下載並取代執行檔。",
            )
            .weak()
            .small(),
        );

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let busy = self.update.is_busy();
            if ui
                .add_enabled(!busy, egui::Button::new("🔄 立即檢查更新"))
                .clicked()
            {
                self.update.begin_manual_check();
                // 手動檢查無視「略過這個版本」——使用者都自己問了,就照實回答。
                update::spawn_check(self.ui_tx.clone(), None, true);
            }
            if busy {
                ui.spinner();
            }
            ui.label(
                RichText::new(format!("目前版本 v{}", update::CURRENT))
                    .weak()
                    .small(),
            );
        });

        // 略過過的版本要看得到、也要收得回來,不然使用者按錯就再也等不到提示。
        if let Some(tag) = skipped {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("已略過的版本:{tag}")).weak().small());
                if ui.small_button("取消略過").clicked() {
                    self.cfg.write().unwrap().skipped_update_version = None;
                    save_now = true;
                }
            });
        }

        if save_now {
            config::save(&self.cfg.read().unwrap());
        }
    }

    /// 啟動剛換上去的新版,然後結束自己。
    fn restart_into(&mut self, exe: &std::path::Path) {
        // 先把設定寫下來再開新行程:新版一啟動就會讀設定檔,晚一步寫就會被
        // 讀到舊的內容。on_exit 之後還會再存一次,寫的是同一份東西,無妨。
        config::save(&self.cfg.read().unwrap());
        match update::relaunch(exe) {
            Ok(()) => self.quit_requested = true,
            Err(e) => self.update.stage = Stage::Failed(format!("{e:#}")),
        }
    }
}

/// 位元組數顯示成 MiB。下載進度只需要看個大概,一位小數就夠。
fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_release() -> Box<Release> {
        Box::new(Release {
            tag: "v999.0.0".into(),
            notes: String::new(),
            page_url: "https://example.invalid".into(),
            exe_url: None,
            sha256_url: None,
        })
    }

    /// 開機自動檢查沒查到東西時,不要把設定視窗叫出來打擾人。
    #[test]
    fn a_quiet_background_check_stays_quiet() {
        let mut ui = UpdateUi::default();
        ui.apply(UpdateEvent::UpToDate);
        assert!(!ui.wants_attention(), "自動檢查「已是最新版」不該跳視窗");

        let mut ui = UpdateUi::default();
        ui.apply(UpdateEvent::Failed("連不上".into()));
        assert!(!ui.wants_attention(), "自動檢查失敗(通常是沒網路)不該跳視窗");
    }

    /// 但查到新版一定要說。
    #[test]
    fn a_background_check_still_reports_a_new_version() {
        let mut ui = UpdateUi::default();
        ui.apply(UpdateEvent::Available(fake_release()));
        assert!(ui.wants_attention());
    }

    /// 叫過一次就夠了,不要每幀都把視窗搶到前面。
    #[test]
    fn attention_is_only_requested_once_per_result() {
        let mut ui = UpdateUi::default();
        ui.apply(UpdateEvent::Available(fake_release()));
        assert!(ui.wants_attention());
        assert!(!ui.wants_attention(), "同一個結果不該重複把視窗叫出來");
    }

    /// 迴歸測試:開機自動檢查查到新版 → 使用者按「立即更新」→ 下載失敗。
    ///
    /// 這一條真的壞過。當初「要不要顯示失敗」是看「這輪檢查是不是使用者按的」,
    /// 於是自動檢查起頭的這條路上,使用者按了按鈕卻什麼回應都沒有。判斷依據
    /// 要是「有沒有人在等結果」,不是「這輪是誰起頭的」。
    #[test]
    fn a_failure_after_the_user_clicked_install_is_always_surfaced() {
        let mut ui = UpdateUi::default();
        ui.apply(UpdateEvent::Available(fake_release())); // 自動檢查查到的
        assert!(!ui.user_initiated, "前提:這一輪不是使用者按出來的");
        assert!(ui.wants_attention());

        ui.begin_install(); // 使用者按下「立即更新」
        ui.apply(UpdateEvent::Failed("這個版本沒有附可直接更新的執行檔".into()));

        // announced 已經是 true(視窗早就開著了),重點是這個結果不會被歸類成
        // 「安靜略過」——使用者停在更新分頁上,ui_update_status 會把失敗畫出來。
        assert!(ui.user_initiated, "按過立即更新之後,失敗必須算成使用者在等的結果");
        let mut fresh = UpdateUi { announced: false, ..UpdateUi::default() };
        fresh.user_initiated = true;
        fresh.apply(UpdateEvent::Failed("同上".into()));
        assert!(fresh.wants_attention(), "使用者按了按鈕就必須看到結果");
    }

    /// 安裝完成後不管這輪是誰起頭的,都要讓使用者看到「可以重新啟動了」。
    #[test]
    fn a_finished_install_is_always_surfaced() {
        let mut ui = UpdateUi::default();
        ui.apply(UpdateEvent::Installed(PathBuf::from("x.exe")));
        assert!(ui.wants_attention());
    }

    /// 手動檢查時人就盯著這一頁,不需要再把視窗搶到前面。
    #[test]
    fn a_manual_check_does_not_re_summon_the_window() {
        let mut ui = UpdateUi::default();
        ui.begin_manual_check();
        ui.apply(UpdateEvent::UpToDate);
        assert!(!ui.wants_attention(), "使用者已經在這一頁上了");
    }
}
