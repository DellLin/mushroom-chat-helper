//! 桌面(所有螢幕合起來的虛擬桌面)範圍,以及「這個視窗位置還叫得回來嗎」的判斷。
//!
//! 主畫面判斷會把主視窗搬到 (-32000, -32000) 當作隱藏(見 [`crate::ui`] 的
//! `sync_gate_visibility`)。那個座標一旦被寫進設定檔,下次啟動視窗就直接生在
//! 看不見的地方、再也叫不回來——所以視窗位置在「寫回設定檔」「啟動還原」
//! 「從隱藏搬回來」三個路口都要先過這裡的檢查。

use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

/// 視窗左上角至少要離桌面右/下邊界這麼遠,才算「看得到、也抓得到來拖曳」。
/// 左/上則允許超出邊界同樣的距離——使用者可能刻意把視窗貼齊邊緣切掉一點,
/// 那是正常用法,不該被當成壞掉的座標丟掉。
const REACHABLE_MARGIN: f32 = 64.0;

/// 虛擬桌面範圍 `(x, y, w, h)`,單位是實體像素。查不到就回 None。
pub fn virtual_screen_px() -> Option<(f32, f32, f32, f32)> {
    // SAFETY: GetSystemMetrics 只讀系統設定值,沒有指標參數也不會失敗。
    let m = |i| unsafe { GetSystemMetrics(i) } as f32;
    let (w, h) = (m(SM_CXVIRTUALSCREEN), m(SM_CYVIRTUALSCREEN));
    (w > 0.0 && h > 0.0).then(|| (m(SM_XVIRTUALSCREEN), m(SM_YVIRTUALSCREEN), w, h))
}

/// `pos` 放得出一個叫得回來的視窗嗎?`screen` 是同一套單位的桌面範圍。
fn pos_is_reachable_in(pos: [f32; 2], screen: (f32, f32, f32, f32)) -> bool {
    let (x, y, w, h) = screen;
    pos[0] >= x - REACHABLE_MARGIN
        && pos[1] >= y - REACHABLE_MARGIN
        && pos[0] <= x + w - REACHABLE_MARGIN
        && pos[1] <= y + h - REACHABLE_MARGIN
}

/// 同上,但自己去查目前的桌面範圍,並把實體像素換算成 egui points
/// (視窗位置在 egui 這邊一律是 points)。
///
/// 查不到桌面範圍時回 `true`:寧可留著使用者原本的位置,也不要因為查不到
/// 就把校正好的座標丟掉。
pub fn pos_is_reachable(pos: [f32; 2], pixels_per_point: f32) -> bool {
    let Some((x, y, w, h)) = virtual_screen_px() else {
        return true;
    };
    let s = pixels_per_point.max(0.1);
    pos_is_reachable_in(pos, (x / s, y / s, w / s, h / s))
}

/// 桌面左上角往內縮一點的位置(egui points)。原本的位置不能用時的退路,
/// 保證視窗一定落在看得到的地方。
pub fn fallback_pos(pixels_per_point: f32) -> [f32; 2] {
    let Some((x, y, ..)) = virtual_screen_px() else {
        return [100.0, 100.0];
    };
    let s = pixels_per_point.max(0.1);
    [x / s + 100.0, y / s + 100.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 單螢幕 1920x1080,原點在 (0, 0)。
    const SINGLE: (f32, f32, f32, f32) = (0.0, 0.0, 1920.0, 1080.0);

    #[test]
    fn gate_hidden_position_is_never_reachable() {
        // 主畫面判斷用來「隱藏」視窗的座標,絕對不能被當成正常位置存回設定檔。
        assert!(!pos_is_reachable_in([-32000.0, -32000.0], SINGLE));
        // 副螢幕接在左邊(虛擬桌面原點是負的)也一樣擋得掉。
        assert!(!pos_is_reachable_in([-32000.0, -32000.0], (-1920.0, 0.0, 3840.0, 1080.0)));
    }

    #[test]
    fn ordinary_positions_survive() {
        assert!(pos_is_reachable_in([0.0, 0.0], SINGLE));
        assert!(pos_is_reachable_in([700.0, 900.0], SINGLE));
        // 貼齊左上角切掉一點點是正常用法,不該被丟掉。
        assert!(pos_is_reachable_in([-20.0, -10.0], SINGLE));
        // 接在左邊的副螢幕上也是正常位置。
        assert!(pos_is_reachable_in([-1500.0, 200.0], (-1920.0, 0.0, 3840.0, 1080.0)));
    }

    #[test]
    fn positions_past_the_far_edges_are_rejected() {
        // 整個視窗掉到桌面右邊/下面外面(例如那台螢幕已經拔掉了)。
        assert!(!pos_is_reachable_in([1900.0, 500.0], SINGLE));
        assert!(!pos_is_reachable_in([500.0, 1060.0], SINGLE));
        // 左/上超出太多也一樣。
        assert!(!pos_is_reachable_in([-200.0, 500.0], SINGLE));
    }
}
