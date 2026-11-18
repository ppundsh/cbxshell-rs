# 零信任輸入驗證與隔離規則 (Security & Zero-Trust)

CBX Shell 遵循零信任（Zero-Trust）原則，預設所有外部輸入、壓縮檔封裝內容與使用者設定檔均可能存在資料異常或超出預期規格。

---

## 1. 壓縮檔、文件與影像資源上限保護 (Bomb & Parser Protection)

解碼與解壓縮作業針對高壓縮比項目（Zip Bomb、Solid Block Bomb）、偽造檔頭與解析器 OOM 建構了多層上限機制：
- **最大解壓位元組限制**：單一候選封面圖檔解壓縮上限受限於設定值（預設 256 MiB，可配置範圍 8～2047 MiB），採用分塊串流讀取，不直接信任檔頭宣告的配置長度。
- **解壓前後雙重壓縮比例檢查**：當項目大於 8 MiB 時，若壓縮比例超過 200:1 或壓縮大小偽造為 `0`，於解壓前即予拒絕；實際解壓完成後亦會以真實位元組長度再次驗證 200:1 上限，防範偽造檔頭繞過。
- **RAR 讀取上限**：RAR 以 seek 逐一走訪區塊標頭（每個標頭驗證 CRC、單一標頭 ≤ 2 MiB），只讀取封面所屬區塊（固實項目為其固實群組到封面為止），總量上限 16–96 MiB；無法以快速路徑處理時才退回有上限的前綴讀取。
- **固實壓縮（Solid Archive）耗損上限**：對 7z / RAR 固實壓縮區塊，設定累計讀取位元組上限（128 MiB）與掃描逾時預算，限制 CPU 與磁碟 I/O 消耗。
- **影像尺寸上限與降採樣淨化**：透過 `image::Limits` 限制最大寬高 `16384` px、最大像素 `64,000,000` 及解碼記憶體 `256 MiB`；縮圖輸出經 `CatmullRom` 重採樣降採樣以消除 LSB 隱寫與縮放攻擊特徵，並透過 `detect_image_trailer_overhang` 偵測圖種（Polyglot）附加資料並標記 `RISK`。
- **文件與音訊封面解析強化**：
  - **OOXML / PSD / 音訊封面**：OOXML 內嵌縮圖限制 16 MiB 並套用 `image::Limits`；PSD/PSB 複合解碼強制溢位檢查與通道／尺寸／掃描表上限；MP4 `covr/data` atom 拒絕 `size < 8`（防無窮迴圈）；FLAC `METADATA_BLOCK_PICTURE` 改以受限串流跳過變長欄位，不依未驗證 `u32` 長度配置記憶體。
  - **純文字與語法上色渲染**：限制讀取 16 KiB、繪製最多 24 行 × 80 字元，並於 GDI 排版繪製前剝除 C0/C1 控制字元（保留 `\t`、`\r`、`\n`）及 Unicode 雙向覆蓋／零寬字元；繪製前另經憑證遮罩處理（見 §7）。
- **全面套用嚴格解碼上限**：所有不受信任的影像一律經 `image_dec::decode_bytes_strict`（尺寸、像素數與配置上限）解碼，包含 EPUB/MOBI/IBA 封面、PSD 縮圖資源與 PDF 轉譯器輸出的 PNG（先前使用 `image::load_from_memory` 的寬鬆預設值）。
- **容器預先驗證**：以 ZIP 為基礎的格式（CBZ、EPUB、IBA、DOCX、XLSX）在交給 ZIP 解析器前，必須先通過受限的 EOCD 尾端檢查；7z 必須通過簽章檔頭檢查（`NextHeaderSize` ≤ 16 MiB 且 `NextHeaderOffset + size` 位於檔案內），避免偽造的檔頭大小變成使宿主行程直接中止的配置炸彈。
- **迴圈／放大上限**：以長度欄位驅動的走訪皆設有與逾時無關的獨立上限——ID3 影格（4,096）、FLAC 中繼區塊（1,024）、MP4 `covr` 子 atom（64）、bencode 節點（每個 `.torrent` 200,000 個、深度 64、整數最多 20 位數）。這些上限很重要，因為看門狗只能「放棄」卡住的工作執行緒，無法終止它。

---

## 2. 安全告警角標與細部偵測規則 (Risk Badges & Rules)

系統可針對壓縮檔內部的可疑檔案結構標記安全告警角標（`RISK`），各項規則皆可獨立設定開關（全面套用於 ZIP、7z、RAR 與 TAR）：

| 規則識別 | 偵測標的與行為 |
| :--- | :--- |
| **存在可執行檔與指令碼** | 包含 `.exe`, `.dll`, `.bat`, `.cmd`, `.ps1`, `.vbs`, `.vbe`, `.js`, `.jse`, `.scr`, `.msi`, `.lnk`, `.url` 等可執行程式或腳本。 |
| **目錄穿越與路徑溢出** | 檔名包含 `..`, 絕對磁碟路徑（如 `C:\`）或 NTFS 替代資料流（ADS，如 `file:stream`）。 |
| **隱藏檔與點目錄** | 檔名以 `.` 開頭（如 `.git/`, `.env`）或包含 `__MACOSX` 資源分支。 |
| **文字與說明文件** | 封裝內包含 `.txt`, `.nfo`, `.diz` 等一般說明文件。 |
| **敏感憑證與私密金鑰** | 包含 `.key`, `.pem`, `.pfx`, `.crt` 等認證金鑰。 |
| **偽裝技巧 (RTLO、零寬字元與保留字)** | 使用 Unicode 雙向覆蓋字元（`U+202A..U+202E`、`U+2066..U+2069`）、不可見零寬格式字元（`U+200B..U+200F`、`U+2060..U+2064`、`U+FEFF`）、尾隨空格／句點，或 Windows 保留設備名稱（`CON`, `PRN`, `AUX`, `NUL`, `CONIN$`, `CONOUT$`, `COM0-9`, `LPT0-9`、上標形式 `COM¹-³` / `LPT¹-³`，以及帶尾隨空白的主檔名如 `CON .png`）。 |

---

## 3. 設定檔防篡改與介面零信任防護 (Defensive Sanitization)

- **註冊表設定讀取保護**：
  若設定檔遭意外或外部竄改（如亮度門檻上限小於下限、無效之數值範圍、包含命令列特殊符號等），系統在載入時自動執行 `Settings::sanitize()`：
  - 數值一律鉗位至合法範圍內。
  - 亮度上下限若發生倒置，自動回復至安全推薦值。
  - 自訂路徑檢查：過濾控制字元、跳脫與重導向字元（`"`、`'`、`|`、`&`、`;`、`<`、`>`、`` ` ``、`$`、`^`、`%`）、UNC／裝置路徑（`\\`）、NTFS 替代資料流冒號與相對路徑，長度限制為 260 字元，且必須為本機實際存在的 `.exe` 絕對路徑。
- **使用者介面輸入過濾**：
  GUI 數值輸入框僅接受 ASCII 數字（`0`..=`9`，最多 4 位）；「自訂純文字副檔名」欄位僅接受英數字、`,`、`.`、`_`、`-` 與空白（最多 64 字元），儲存時再經 `Settings::sanitize()` 過濾為每項最多 16 字元的英數副檔名。

---

## 4. 多媒體解碼安全與解碼器漏洞 (如 CVE-2026-8461) 之架構層防禦

### 解碼器隔離背景
傳統 C/C++ 多媒體解碼函式庫（如 FFmpeg 的 `libavcodec`）因處理複雜的二進位位元流，歷史上曾出現記憶體存取錯誤。例如 **CVE-2026-8461** 即為 FFmpeg 8.1.2 之前版本中，`libavcodec/magicyuv.c` 的 MagicYUV 解碼器存在越界寫入（Out-of-Bounds Write）漏洞，異常的影片影格可能引發服務中斷（DoS）或潛在的遠端程式碼執行（RCE）。

### CBX Shell 的分層隔離設計
為降低解碼器內部異常對主機系統的影響，CBX Shell 採取以下架構邊界隔離措施：

1. **跨處理序隔離 (Out-of-Process Isolation)**：
   - 於預設的處理序隔離模式下，不將第三方解碼動態程式庫直接載入 `explorer.exe` 的記憶體空間。
   - 原生 Media Foundation 管線預設於 Windows 的 COM 代理處理序（`dllhost.exe`，COM Surrogate）中執行。
   - FFmpeg 支援管線採用獨立衍生之暫時性子處理序（`ffmpeg.exe`）以 `CREATE_NO_WINDOW` 執行。即使解碼器內部遭遇未捕獲的記憶體存取違規（Access Violation / SIGSEGV）或中止，僅會結束該短暫子處理序，檔案總管主程式與桌面環境維持隔離。

2. **通訊協定、解多工器與解碼器白名單**（`FFMPEG_INPUT_HARDENING_ARGS`）：
   - `-protocol_whitelist file`：僅允許本機檔案協定；拒絕網路協定（HTTP、HTTPS、TCP、UDP、RTP）、`pipe:`／`fd:` 以及 `concat:`／`subfile:` 等 URL 技巧。
   - 輸入一律以明確的 `file:<絕對路徑>` URL 傳入，惡意檔名永遠不會被解讀為命令列選項（`-…`）或其他協定。
   - `-format_whitelist` 將格式探測限制於一般影片容器（MOV/MP4、Matroska/WebM、AVI、ASF、FLV、MPEG-TS/PS、Ogg、RM、裸 H.264/HEVC 等），阻斷**內容型別混淆**：副檔名為 `.mp4`／`.avi`、實際內容卻是 HLS/DASH 播放清單、`ffconcat` 腳本或 `image2` 序列樣式的檔案，再也無法讓 FFmpeg 開啟「其他」本機檔案並把其位元組繪進縮圖（已知的本機檔案外洩攻擊鏈）。
   - `-codec_whitelist` 僅允許主流影片解碼器；檔案無法強迫 FFmpeg 載入 MagicYUV（CVE-2026-8461）、Indeo、Cinepak 等罕見／陳舊解碼器——記憶體安全漏洞最集中的類別。若需支援其他合法編碼，可於 `video_mf.rs` 擴充清單。
   - 擷取影格時加上 `-an -sn -dn`，永不開啟音訊、字幕與資料串流解碼器。

3. **解碼探測與記憶體額度限制**：
   - 串流探測設定上限：`-probesize 1048576`（1 MiB）與 `-analyzeduration 1000000`（1 秒）。
   - 限制大型檔頭或異常封包在探測階段造成的記憶體消耗。

4. **看門狗逾時中斷與管道上限 (Watchdog & Bounded Pipes)**：
   - 獨立看門狗定時器（可配置 3～60 秒，預設 12 秒）同時監控 COM 工作執行緒與每次 FFmpeg 子行程呼叫；若子行程無回應或逾時，父行程立即強制終止並回收資源。stdout/stderr 讀取亦設有嚴格上限（16 MiB stdout / 512 KiB stderr），並以 `image::Limits` 解碼影格。

5. **編解碼器支援縮減**：
   - 縮圖預覽僅需標準常見格式（H.264, HEVC, AV1, VP9, MPEG-4）。於自訂建置環境配置 FFmpeg 時，可透過編譯旗標剔除罕見或陳舊解碼器（如 MagicYUV、Indeo、Cinepak）以縮小曝險範圍。

---

## 5. 外部工具最小化權限與防偽裝機制 (Least Privilege & Anti-Spoofing)

1. **FFmpeg 子行程最小權限沙盒**：
   - **先暫停、再隔離、後恢復**：子行程以 `CREATE_SUSPENDED` 建立，先綁定 Job Object 並剝除權限，之後才恢復執行。先前的做法是在行程「已開始執行後」才套用 Job（存在可讓解碼器漏洞衍生行程的競態視窗）。若 Job 無法建立或綁定，直接終止子行程——FFmpeg 絕不在沙盒外執行（失效即關閉）。
   - **OS 核心 Job Object 隔離**：每次啟動 `ffmpeg.exe` 均綁定匿名 Win32 Job Object，強制限制 `JOB_OBJECT_LIMIT_ACTIVE_PROCESS = 1`（禁止衍生任何子行程或 Shell）、`JOB_OBJECT_LIMIT_PROCESS_MEMORY = 512 MiB`（核心層級記憶體上限）、`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 與 `JOBOBJECT_BASIC_UI_RESTRICTIONS`（阻斷剪貼簿讀取、桌面與跨視窗句柄存取）。
   - **Token 權限最小化**：透過 `AdjustTokenPrivileges(DisableAllPrivileges = true)` 剝除子行程所有可選權限。
   - **環境變數與工作目錄隔離**：透過 `cmd.env_clear()` 清除繼承環境，僅傳入 `SystemRoot`、`WINDIR` 與受限 `PATH`（`<ffmpeg_dir>;System32`），並將 `current_dir` 鎖定於 `<ffmpeg_dir>`，防止從未受信任的影片資料夾載入惡意 DLL（DLL Planting）。
   - **執行檔探索與參數防護**：自動掃描 `PATH` 時略過相對路徑（`.`）、UNC 路徑、`Temp` 與 `Downloads` 目錄；拒絕執行與目標影片位於同一資料夾的 `ffmpeg.exe`；輸入路徑先轉為絕對路徑並加上 `file:` 前綴，防止選項注入與協定注入。

2. **Everything IPC 防偽裝與路徑驗證**：
   - **IPC 視窗行程驗證**：查詢 `EVERYTHING_TASKBAR_NOTIFICATION` 前，先透過 `GetWindowThreadProcessId` 與 `QueryFullProcessImageNameW` 檢查視窗所屬執行檔是否為位於本機非暫存目錄的 `Everything.exe` / `Everything64.exe`，防止同 Session 惡意程式偽造 IPC 視窗回傳。
   - **回傳結果嚴格校驗**：預設僅接受本機磁碟（`DRIVE_FIXED` / `DRIVE_REMOVABLE`）路徑（阻擋 UNC/SMB NTLM 憑證外洩；使用者明確允許網路共用資料夾時，改依 §6 的網路政策判斷），拒絕 `..` 遍歷與 ADS 冒號，並強制比對回傳檔名與檔案位元組大小（`expected_size`）完全相符；查詢字串亦過濾引號與控制字元。

3. **系統工具絕對路徑呼叫**：
   - 清除縮圖快取重啟檔案總管時，以 `GetSystemWindowsDirectoryW` 回報的 Windows 目錄（絕不寫死磁碟代號）組出 `taskkill.exe` 與 `explorer.exe` 絕對路徑，搭配系統 `current_dir` 執行，不經由 `cmd.exe` 或 `PATH` 搜尋路徑。

---

## 6. Shell 提供的名稱一律不信任（混淆代理人防禦）

在處理序隔離模式下，Shell 交給擴充元件的是 `IStream`；隨附的 `STATSTG::pwcsName` 經常只是「單純檔名」（或由第三方命名空間擴充，例如 ZIP 資料夾、雲端供應者所決定的名稱）。舊版直接對該名稱呼叫 `Path::is_file()`，而這會以宿主行程的**目前工作目錄**解析，導致某項目的縮圖可能由另一個同名但不相干的本機檔案產生。

- 只有通過 `media::is_openable_media_path` 的名稱才會直接開啟：預設必須是**非網路磁碟**上、磁碟代號開頭的絕對路徑（`is_trusted_local_abs_path`），且不含 UNC／裝置前綴（`\\server`、`\\?\`、`\\.\`）、ADS 冒號、控制字元與 `..` 元件；否則僅作為副檔名提示，實際位元組一律來自串流。
- **網路共用資料夾為選擇性開啟**（控制中心 → 影片 →「允許讀取網路共用資料夾」，預設關閉，開啟時會跳出警告）：開啟後 UNC 路徑、對應的網路磁碟機與 Everything 對網路位置的結果才會被直接開啟。子選項「只能讀取區域網路」預設開啟，只接受私有／鏈結本機／迴路 IP、單一名稱（NetBIOS／LLMNR）與 `.local`、`.lan`、`.home.arpa` 等區域網路名稱的伺服器；判斷**完全不做 DNS 查詢**，WebDAV（`\\host@SSL\…`）與裝置命名空間永遠拒絕。對應的網路磁碟機以 `QueryDosDeviceW` 取得實際伺服器後套用同一規則。
- 串流無法由 Media Foundation 解碼時（FLV、許多 MKV、Hi10P 等），會把串流的開頭與目標位置附近的資料經**標準輸入**交給 FFmpeg（`-protocol_whitelist pipe`，其餘白名單與沙盒完全相同），宿主程序本身不開檔、也不連線網路。
- 影片的名稱式後備解析（快取 → Everything → CWD → TypedPaths → 使用者資料夾 → 最近使用 → 磁碟根目錄）在**串流大小未知（0）時完全停用**，且所有候選檔案的大小都必須與串流完全相符。
- `TypedPaths` 項目、`.lnk` 目標與最後目錄快取只會在本機固定／卸除式磁碟上追蹤，網址列歷史或捷徑永遠無法觸發對外 SMB/WebDAV 連線（NTLM 雜湊外洩）或網路卡頓。
- 第三方串流的 `IStream::Read` 結果會被驗證（`read_bytes ≤ 要求長度`）、`BufferedIStream::Seek` 改用溢位檢查運算，縮圖邊長要求鉗位於 16–2560 px。

---

## 7. 隱私保護

縮圖是**持久化**的：檔案總管會將其存入 `thumbcache_*.db`，即使原始檔案刪除後仍會留存，且任何以該使用者身分執行的行程都能讀取。

- **預覽內容憑證遮罩**（`cbx_core::redact`）：純文字、Word 與 Excel 預覽會遮罩 PEM/OpenSSH/PGP 私鑰區塊、鍵名代表憑證的 `key = value`（`password`、`secret`、`token`、`api_key`、`connection_string` 等，以「完整單字」比對，`author`、`tokenizer` 不受影響）、內嵌於 URL 的密碼（`scheme://user:•••@host`），以及常見權杖格式（雲端存取金鑰、版本控制／通訊軟體權杖、JWT、長且大小寫混合的高熵字串）。
- **移除顯示偽裝字元**：torrent 檔案清單、文件行與試算表儲存格皆剝除雙向覆蓋、零寬／不可見字元與控制字元，`photo<U+202E>gpj.exe` 無法再顯示成 `photoexe.jpg`。
- **即時監控歷程僅存於記憶體**：共享區段改由分頁檔支援，不再寫入 `%TEMP%` 中的檔案；縮圖過的路徑清單永不落地，並在最後一個持有它的行程結束後消失。監控功能維持預設關閉。
- **二進位檔不含建置機資訊**：`build.ps1` 以 `--remap-path-prefix` 重新對映 `USERPROFILE`、`CARGO_HOME`、`RUSTUP_HOME` 與專案根目錄；MSVC 連結器只記錄 PDB 檔名（`/PDBALTPATH:%_PDB%`）；若任何發行二進位檔仍含建置者的設定檔路徑、使用者名稱或專案路徑（ASCII 或 UTF-16），**隱私閘門**會使建置失敗。
- **儲存庫不含個人資料**：不寫死磁碟代號或使用者資料夾（系統位置取自 `GetSystemWindowsDirectoryW`、`%ProgramFiles%`、`%LOCALAPPDATA%`），本地時區改由作業系統提供而非固定偏移；`repository_contains_no_build_machine_paths_or_personal_data` 測試會掃描原始碼、設定檔、測試與文件中的使用者設定檔路徑與電子郵件地址。

---

## 8. 執行檔與供應鏈強化

- **DLL 植入**：`CBXShell-Setup.exe`、`cbx-manager.exe` 與 `cbx-cli.exe` 以 `/DEPENDENTLOADFLAG:0x800` 連結（靜態匯入僅從 System32 解析），並於啟動時呼叫 `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)`；放在「下載」資料夾中安裝程式旁的 `version.dll`／`dwmapi.dll` 永遠不會被載入。（GNU 工具鏈沒有對應的連結器旗標，但執行期呼叫仍然有效。）
- **自動更新**：下載的安裝檔必須通過內建公鑰的 minisign（Ed25519）簽章驗證才會執行；清單網址限定本儲存庫的 Release 資產、只接受較新版本；另需維護者以 YubiKey（FIDO2，PIN＋觸碰）簽署的版本＋SHA-256 核准，因此 GitHub 帳號、CI 或簽章私鑰單獨外洩都無法推送更新。詳見 [安裝、版本檢測與自動更新](./updates.zh-TW.md)。
- **解除安裝程式重新啟動**：為解除安裝資料夾鎖定而建立的自我複本，改放在 `%TEMP%` 下新建、名稱唯一的子資料夾，而非固定且可被預先植入的路徑。
- **CI**：發行工作流程預設 `contents: read`，僅發行工作授予 `contents: write`，且 checkout 不保存憑證。建議將第三方 Actions 釘選至完整 commit SHA。

---

## 9. 對抗性測試策略（未知攻擊偵測）

`crates/cbx-core/src/adversarial_tests.rs` 以「預言機（oracle）」補足固定的回歸測試，可偵測整個「類別」的新漏洞：

| 預言機 | 偵測目標 |
| :--- | :--- |
| 不得 panic（`catch_unwind`） | 越界切片、溢位、對惡意資料 `unwrap` |
| 單一案例時間預算 | 無窮迴圈、演算法爆炸、在看門狗之後仍持續執行的工作 |
| I/O 放大預算（計數用 `CountingReader`） | 依長度欄位重複定位／讀取數百萬次的迴圈 |
| 輸出尺寸上限 | 信任檔頭尺寸的解碼器 |
| 信任邊界輔助函式的性質測試 | `sanitize_archive_path` 的路徑穿越／ADS／裝置名稱／雙向字元繞過；`is_trusted_local_abs_path` 接受非本機路徑 |
| 設定斷言 | FFmpeg 協定／解多工器／解碼器白名單與 `file:` 輸入前綴 |
| 儲存庫隱私掃描 | 建置機路徑、使用者名稱、電子郵件地址 |

輸入來自對涵蓋所有解析器的種子語料（ZIP/CBZ、EPUB、DOCX、XLSX、TAR、7z、RAR、PSD、ID3、FLAC、MP4、MOBI、torrent、純文字、PDF）進行**結構感知變異**：「說謊的長度」（以 16/32/64 位元、大小端序的邊界整數覆寫長度欄位）、區塊放大、跨格式拼接（內容型別混淆）、截斷與位元翻轉；每個變異樣本也會以另一個副檔名重新分派。數百 MiB 的惡意檔案以虛擬串流模擬，不實際配置記憶體。

執行：`cargo test -p cbx-core`（Windows）。長時間測試：`$env:CBX_FUZZ_ITERS=20000; cargo test -p cbx-core --release structure_aware`。亂數產生器為確定性，失敗訊息會列出重現所需的迭代編號與副檔名。

---

## 10. 剩餘風險與前提假設

- **作業系統解碼器**：Media Foundation、`Windows.Data.Pdf` 與 WIC/GDI 屬於 Windows 元件；CBX Shell 會限制其輸入與輸出，但其記憶體安全仰賴 Windows Update。請保持處理序隔離模式啟用。
- **被放棄的工作執行緒**：逾時後看門狗會先回應檔案總管，但無法終止工作執行緒；§1 的各解析器上限限制了此類執行緒可執行的時間。
- **7z 編碼檔頭**：簽章檔頭檢查限制了檔頭「區塊」大小，但壓縮檔頭宣告的解壓後大小仍由 `sevenz-rust2` 信任。
- **使用者可寫入的安裝資料夾**：預設的每使用者安裝位於 `%LOCALAPPDATA%`，同一使用者的任何行程皆可修改（每使用者安裝的固有特性）；因此從該資料夾提權執行 `cbx-manager.exe` 等同信任該使用者環境。
- **隨附 FFmpeg**：勾選「安裝 FFmpeg」時，安裝程式會複製與其同資料夾的 `ffmpeg.exe`。請只放置可信任的 FFmpeg 建置，並建議使用 FFmpeg ≥ 8.1.2（CVE-2026-8461）。
- **同 Session 行程**：即時監控共享區段（`Local\…`）與 Everything IPC 視窗可被同一登入工作階段中的其他行程存取；其資料一律視為不受信任並設有大小上限。

