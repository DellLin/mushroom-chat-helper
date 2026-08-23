//! 建置腳本:處理應用程式圖示。
//!
//! 兩件事,對應 Windows 上兩個不同的圖示來源:
//!   1. exe 本身的圖示資源(檔案總管、捷徑看到的)—— winresource 嵌 .ico
//!   2. 視窗圖示(工作列、Alt-Tab)—— 需要 RGBA 像素,所以先把 PNG 解碼好
//!      放進 OUT_DIR,執行期直接 include_bytes! 拿現成的,不必在 runtime
//!      背一個 PNG 解碼器。

use std::io::Write;

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/icon-256.png");
    println!("cargo:rerun-if-changed=build.rs");

    decode_window_icon();

    // 只有 Windows 目標有資源檔的概念(本專案也只能在 Windows 上建置)。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        if let Err(e) = res.compile() {
            // 嵌圖示失敗不該讓整個建置掛掉 —— 沒有圖示的 exe 仍然完全可用。
            println!("cargo:warning=嵌入 exe 圖示失敗(程式仍可正常建置):{e}");
        }
    }
}

/// 把 assets/icon-256.png 解碼成純 RGBA,寫到 OUT_DIR/window-icon.rgba。
fn decode_window_icon() {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR 未設定");
    let out_path = std::path::Path::new(&out_dir).join("window-icon.rgba");

    let file = std::fs::File::open("assets/icon-256.png").expect("讀不到 assets/icon-256.png");
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    // egui::IconData 要的是每通道 8 bit 的 RGBA。ImageMagick 的 Q16 版本預設會吐
    // 16-bit PNG,直接解出來的緩衝區會剛好大一倍、圖示變成一團雜訊,而且不會有
    // 任何錯誤 —— 所以這裡明確要求正規化成 8-bit 並補上 alpha,不倚賴素材本身的格式。
    decoder.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().expect("PNG 標頭解析失敗");
    let mut buf = vec![0; reader.output_buffer_size().expect("PNG 尺寸異常")];
    let info = reader.next_frame(&mut buf).expect("PNG 影像解碼失敗");

    assert_eq!(
        info.color_type,
        png::ColorType::Rgba,
        "assets/icon-256.png 應該要正規化成 RGBA,目前是 {:?}",
        info.color_type
    );
    buf.truncate(info.buffer_size());
    // 最後把關:長度必須剛好是 w*h*4,否則 egui 會拿到對不上的像素。
    let expected = info.width as usize * info.height as usize * 4;
    assert_eq!(
        buf.len(),
        expected,
        "RGBA 長度不對:{}x{} 應該是 {} bytes,實際 {}",
        info.width,
        info.height,
        expected,
        buf.len()
    );

    let mut out = std::io::BufWriter::new(std::fs::File::create(&out_path).expect("建立輸出檔失敗"));
    out.write_all(&buf).expect("寫入 RGBA 失敗");

    // 尺寸讓 main.rs 用得到,不必在兩個地方各寫死一次。
    println!("cargo:rustc-env=WINDOW_ICON_W={}", info.width);
    println!("cargo:rustc-env=WINDOW_ICON_H={}", info.height);
}
