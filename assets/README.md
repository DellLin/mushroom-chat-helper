# 圖示素材

原創的蘑菇怪造型(走《楓之谷》那種可愛小怪的路線,但**不是**複製任何既有角色),
頭上一個漫畫對話框對應「聊天小幫手」。

| 檔案 | 用途 |
|---|---|
| `icon.svg` | 主要來源檔。細節較多,供 32px 以上使用 |
| `icon-small.svg` | 16/24px 專用。線條加粗、斑點減為 2 顆、留白壓到最小 —— 細節版縮到 16px 會糊成一團 |
| `icon.ico` | 建置產物,`build.rs` 拿去嵌進 exe。內含 16/24/32/48/64/128/256 七個尺寸 |
| `icon-256.png` | 建置產物,`build.rs` 解碼成 RGBA 後給視窗圖示(工作列/Alt-Tab)用 |

`.ico` 與 `.png` 是產物但**有進版控**,這樣一般建置不需要裝 ImageMagick。

## 改圖之後怎麼重新產生

需要 [ImageMagick 7](https://imagemagick.org)(要有 RSVG delegate,`magick -list delegate` 看得到 `svg => rsvg-convert` 就對了):

```bash
cd assets
for s in 16 24;            do magick -background none icon-small.svg -resize ${s}x${s} ico-$s.png; done
for s in 32 48 64 128 256; do magick -background none icon.svg       -resize ${s}x${s} ico-$s.png; done
magick ico-16.png ico-24.png ico-32.png ico-48.png ico-64.png ico-128.png ico-256.png icon.ico
magick -background none icon.svg -resize 256x256 -depth 8 PNG32:icon-256.png
rm ico-*.png
```

`icon-256.png` 那行的 **`-depth 8 PNG32:` 不能省**。ImageMagick 的 Q16 版本預設吐
16-bit PNG,`egui::IconData` 要的是 8-bit RGBA —— 位元深度不對的話解出來的緩衝區
會剛好大一倍、圖示變成雜訊,而且過程中不會有任何錯誤。`build.rs` 已經會主動正規化
並斷言長度,所以真的弄錯會在建置時直接失敗,不會偷偷出貨壞圖示。

改完務必**實際看一下 16px 的樣子**(放大檢查:`magick icon.ico[0] -filter point -resize 800% check.png`)。
圖示在小尺寸糊掉是最常見的問題,而縮圖前在 256px 上看是看不出來的。
