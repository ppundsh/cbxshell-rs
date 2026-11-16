# 電子書封面擷取支援 (E-Books)

CBX Shell 可直接從串流中讀取主流電子書格式之內嵌封面影像，無需仰賴外部閱讀器即可於檔案總管顯示封面縮圖。

---

## 1. 支援電子書格式

- **EPUB / IBA（`.epub`, `.iba`）**：
  - 依序嘗試：
    1. iBooks／QuickLook 預覽圖（如 `QuickLook/Thumbnail.png`、`iBooksArtwork`）；
    2. 解析 `META-INF/container.xml` 定位 OPF，依序查找 `properties="cover-image"`（EPUB 3）、`<meta name="cover">`（EPUB 2）以及 id／href 含 `cover` 的影像項目；
    3. 檔名含 `cover`、`front`、`thumbnail` 的圖檔；
    4. 依自然排序與連續編號規則選出的第一張圖檔。
  - 封面影像一律套用嚴格的尺寸與記憶體配置上限解碼。
- **MOBI / AZW（`.mobi`, `.azw`）**：
  - 解析 PalmDB 紀錄表與 MOBI/EXTH 標頭（`EXTH` 類型 `201` 封面位移／`202` 縮圖位移），單筆紀錄上限 `64 MiB`；找不到時最多掃描 200 筆影像紀錄。以 ZIP 封裝的檔案改走 EPUB 流程。
- **PDF（`.pdf`）**：
  - 可選支援項目（預設關閉，可於控制中心「電子書」分頁啟用），由 Windows 內建 `Windows.Data.Pdf` 繪製第一頁，檔案讀取上限 `128 MiB`。

---

## 2. 顯示與樣式選項

- **書籍封面外框**：可套用書脊折線與頁緣立體層次（或於「無外框」、「無外框圓弧」等 6 種外框樣式中自由選擇）。
- **關聯應用程式圖示**：支援於電子書縮圖角落顯示系統預設開啟程式圖示。
