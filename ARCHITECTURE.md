# 架構說明

## 資料流

```
[遊戲視窗]
   │ Windows.Graphics.Capture(free-threaded FramePool)
   ▼
capture 執行緒 ── 節流至 N fps ── staging texture → CPU BGRA
   │ FramePacket,bounded(2) channel,滿了丟幀
   ▼
vision 執行緒
   1. ROI 裁切(只處理聊天區域)
   2. 像素標記:逐像素比對頻道色票(核心容差 0.55x);
      亮度快篩門檻依目前啟用頻道的顏色/容差動態算出(不是寫死常數),
      避免深色頻道文字被門檻擋在比對邏輯之前
   3. 固定高度網格切行:從 ROI 最上面開始,每 line_height_px 切一則候選訊息
      (不看像素內容,是否有效訊息交給後面的分類/OCR 判斷)
   4. 整幀快篩:全部 band 的遮罩雜湊 == 上一幀 → 跳過
   5. 逐 band:LRU 雜湊去重 → 主色分類 → 位置冷卻(BandCooldown)
   6. OCR 前處理:遮罩(頻道色 1.4x 寬鬆容差,或未分類行的亮度門檻)
      → 裁切到遮罩實際命中的左右邊界(避免整個 ROI 寬度都拿去放大,
        可能超過 Windows OCR 的影像尺寸上限)→ 黑字白底 → 最近鄰放大
      → 放大「後」再膨脹 1px(不是放大前,避免 CJK 筆畫被等比例放大的
        膨脹黏在一起)→ 3x3 均值模糊 → 白邊
   7. Windows.Media.Ocr (zh-Hant) → CJK 空白清理
   8. 文字模糊去重(編輯距離,非精確字串比對,見下)→ 剝頻道標籤 → 解析發話者
   │ ChatMessage {channel, sender, content, time}
   ▼
egui UI:聊天檢視(主視窗)+ 設定(獨立視窗,見下)
   ▼
hotkey 執行緒:Win32 RegisterHotKey,系統層級快捷鍵循環切換檢視
```

## 執行緒與通道

| 通道 | 型別 | 容量 | 背壓策略 |
|---|---|---|---|
| cmd (UI→capture) | CaptureCmd | unbounded | — |
| frame (capture→vision) | FramePacket | bounded(2) | try_send 丟幀 |
| ui (vision/capture/hotkey→UI) | UiEvent | unbounded | UI 每幀最多汲取 256 筆 |
| hotkey cmd (UI→hotkey) | HotkeyCmd | unbounded | — |

共享狀態:`Arc<RwLock<Config>>`(vision 每幀 clone 快照)、
`Arc<AtomicBool>` 校準模式、`Arc<AtomicU64>` 擷取間隔 ms(fps 即時生效)。

## UI 結構

主視窗與設定視窗是**兩個獨立的 egui viewport**(`ctx.show_viewport_immediate`),
不是同一個視窗裡切換的分頁:

- **主視窗**:無邊框(`with_decorations(false)`)、無系統最小尺寸限制(只留一個
  很小的下限防止縮到抓不到邊框),永遠只渲染聊天內容。拖曳靠工具列背景的自訂
  drag sense 區(`ViewportCommand::StartDrag`),縮放靠手動裝的邊框/角落感應區
  (`ViewportCommand::BeginResize`,只留右/下/右下角)。這兩個都可以被「鎖定」
  暫時停用。
- **設定視窗**:按 ⚙ 用 `show_viewport_immediate` 開出的獨立 OS 視窗,保留系統
  裝飾(標題列/關閉鈕),內含四個分頁(擷取設定/頻道校準/檢視管理/說明)跟
  「結束應用程式」按鈕——這是唯一能完全關閉程式的地方,因為主視窗沒有關閉鈕。
  再按一次 ⚙ 若視窗已開著,會送 `ViewportCommand::Focus` 把它叫到前面,而不是
  重新開一個。

「全部」檢視的行為由 `Config::all_view_mode` 決定:
- `ShowMessages`:正常顯示未過濾的訊息列表。
- `HideChat`(預設):把主視窗**實際縮小**到只剩工具列高度(不是用半透明遮罩
  蓋掉),讓底下的遊戲聊天視窗直接露出來;切回其他檢視時還原成收合前的高度。
  用視窗大小而非透明合成實現,行為比依賴 Windows 桌面合成的透明效果穩定。

## 關鍵設計決策

**為什麼切行改成固定高度網格,不再用像素投影找行界?**
早期版本用「有文字像素的列 vs 空白列」找行的邊界,但半透明背景疊在會動的
遊戲畫面上,容易讓相鄰行之間該有的空白間隔消失,導致多行被誤判黏成一個
巨大的 band。固定網格從 ROI 頂端開始按已知的行高切,完全不依賴背景穩不穩定,
副作用是同一則訊息會隨畫面捲動移進不同格子、重新觸發辨識——這由下一點的
模糊去重機制接住。

**為什麼文字去重要用編輯距離而不是精確字串比對?**
固定網格切行下,同一則靜態訊息也可能因為半透明背景的像素抖動導致遮罩雜湊
不夠穩定,被重新 OCR;OCR 本身在雜訊畫面下每次讀出來的結果也不會完全一樣
(例如「謝謝你」「謝謝你胸」「謝謝作」)。精確字串比對完全抓不到這種近似
重複,因此改用編輯距離,差異在字串長度一定比例以內視為同一則訊息。

**為什麼校準取樣要分兩種函式(labeled_avg_color / dominant_bright_color)?**
已經被分類成功的行,取樣直接用分類階段核心容差比對成功的像素平均色
(`labeled_avg_color`),準確反映真正的文字顏色。未分類的行沒有「正確頻道」
可以鎖定像素,只能退回舊的「找整個 band 裡面積最大的亮色」啟發式
(`dominant_bright_color`)——但這個方法對高亮度背景色塊(例如全彩橫幅廣播
訊息)容易誤判成背景色而非文字色,只在沒有更好選擇時使用。

**為什麼 OCR 前處理要先找文字邊界再裁切,不直接處理整個 ROI 寬度?**
一個 band 常常大部分是空白背景,把整個 ROI 寬度都拿去放大會讓圖片不必要地
巨大,寬度較大的 ROI 配合較高的 OCR 放大倍率很容易超過 Windows OCR 的
`MaxImageDimension`(預設約 2600px),導致該行整個辨識失敗(不是認錯字,
是完全沒送出去)。改成只裁切、放大遮罩命中的實際文字範圍後大幅緩解。

**為什麼膨脹(dilate)要放大「後」才做,不是放大前?**
1px 膨脹如果在放大前的原始小尺寸上做,會被最近鄰放大等比例放大成
`scale` 像素,CJK 筆畫間距本來就窄,很容易直接把相鄰筆畫或相鄰字黏在一起。
改到放大後的最終尺寸上做,1px 就是真的 1px,只用來補回反鋸齒缺角造成的
細筆畫斷裂,不會反過來造成沾黏。

**為什麼亮度快篩門檻要動態算,不能寫死?**
早期版本寫死 `r.max(g).max(b) < 90` 就丟棄,理由是快速濾掉背景雜訊像素。
但如果使用者設定了較暗的頻道顏色(例如深酒紅色),這個門檻會在容差比對
邏輯跑之前就把合法的文字像素擋掉,不管容差調多寬都救不回來,而且錯誤現象
很難跟「顏色/容差沒調準」區分。改成依目前啟用頻道的顏色與容差動態算出最
寬鬆的合理門檻,對深色頻道更友善,同時仍保留快篩背景雜訊的效果。

**為什麼先分類再 OCR?**
顏色分類每行只需一次整數比較掃描,OCR 貴數百倍。未分類且未開啟顯示的行
直接跳過,省下大量算力。

**為什麼顏色可校準而非寫死?**
遊戲改版、UI 透明度、不同語系都會偏移色值。校準模式即時顯示每行
(偵測主色、OCR 文字、送進 OCR 引擎的實際畫面縮圖),一鍵套用,比任何
硬編碼耐用。

**為什麼系統層級快捷鍵用 RegisterHotKey 而不是 egui 內部按鍵監聽?**
egui 的鍵盤事件只在應用程式視窗有焦點時才收得到,但這個工具的典型使用情境
是遊戲視窗在前景、應用程式視窗在背景疊圖或縮到最小。Win32 `RegisterHotKey`
註冊的是系統層級的全域快捷鍵,不論哪個視窗有焦點都能觸發,獨立執行緒輪詢
`WM_HOTKEY` 訊息,搭配一個小命令 channel 讓使用者可以即時更換組合鍵
(先 `UnregisterHotKey` 舊的、再 `RegisterHotKey` 新的)。

**為什麼純 Rust 影像處理?**
需要的操作(容差比對、切行、裁切、放大、膨脹、模糊)都是幾十行迴圈,
換 OpenCV 的代價是 Windows 上出名難搞的建置與 DLL 發佈。`ocr::OcrBackend`
與 vision 模組邊界即為日後替換點。

**擷取細節**
- FramePool 用 `CreateFreeThreaded`,回呼在 MTA 執行緒,不需訊息迴圈;
  `HandlerCtx` 的 COM 欄位被 `unsafe impl Send/Sync`,因為執行緒已用
  `RO_INIT_MULTITHREADED` 初始化,free-threaded pool 本來就允許回呼在
  任意執行緒觸發
- 回呼內先 `TryGetNextFrame` 取走幀再節流,避免 pool 積壓
- `ContentSize` 變動時 `Recreate` pool(視窗縮放)
- staging texture 快取重用,只在尺寸變動時重建

## 模組對應

| 路徑 | 職責 |
|---|---|
| `capture/wgc.rs` | WGC 擷取執行緒、D3D11 讀回 |
| `capture/win_enum.rs` | 可見視窗列舉 |
| `vision/classify.rs` | 色票、像素標記(含動態亮度門檻)、主色統計、校準取樣 |
| `vision/lines.rs` | 固定高度網格切行 |
| `vision/preprocess.rs` | OCR 前處理(裁切、遮罩、放大、放大後膨脹、模糊) |
| `vision/dedup.rs` | LRU 雜湊 / 文字模糊去重(編輯距離) / 位置冷卻 |
| `vision/mod.rs` | 管線編排 |
| `ocr/` | OcrBackend trait + Windows.Media.Ocr 實作 |
| `textutil.rs` | CJK 空白清理、標籤剝除、發話者解析(含單元測試) |
| `hotkey.rs` | 系統層級全域快捷鍵(RegisterHotKey)執行緒 |
| `ui/mod.rs` | 主視窗(聊天)+ 設定視窗(多 viewport)、無邊框視窗的拖曳/縮放/鎖定 |
| `config.rs` | TOML 設定持久化 |
