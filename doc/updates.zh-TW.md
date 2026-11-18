# 安裝、版本檢測與自動更新

CBX Shell 以單一安裝檔 `CBXShell-Setup.exe` 發行，安裝於目前使用者，不需要系統管理員權限。管理工具會向 GitHub Releases 查詢新版本，並在使用者同意後下載、**驗證數位簽章**、就地升級。整體流程參考 WinNUT-Client-RS 的 Tauri 更新器（同樣使用 minisign／Ed25519 簽章與 `latest.json` 清單），但不依賴 Tauri。

---

## 1. 安裝程式

| 項目 | 說明 |
|---|---|
| 安裝位置 | `%LOCALAPPDATA%\Programs\CBXShell`（可用 `--dir` 指定） |
| 權限 | `asInvoker`，只寫入 HKCU；不需要 UAC |
| 解除安裝 | 註冊於「設定 › 應用程式 › 已安裝的應用程式」（`DisplayVersion`、`UninstallString`、`QuietUninstallString`、`ModifyPath`） |
| 開始功能表 | 建立「CBX Shell」捷徑（指向 `cbx-manager.exe`） |

命令列參數：

| 參數 | 作用 |
|---|---|
| （無） | 圖形化安裝精靈 |
| `--quiet` / `--silent` / `/s` | 以預設選項靜默安裝 |
| `--dir <資料夾>` | 指定安裝資料夾 |
| `--uninstall [--quiet]` | 解除安裝 |
| `--update --dir <資料夾> [--wait-pid <PID>]` | **就地更新**：只替換程式檔、重新註冊 `cbxshell.dll`、更新 `DisplayVersion`；**保留所有設定、檔案關聯與捷徑**；完成後重新開啟管理工具。由管理工具的更新器呼叫 |
| `--lang zh-TW|en` | 指定安裝程式語言 |

被檔案總管鎖定的檔案（例如已載入的 `cbxshell.dll`）會改名為 `*.old` 後再寫入新檔；下次更新時清除。

---

## 2. 版本檢測

- 管理工具「關於」頁的 **軟體更新** 卡片顯示目前版本、上次檢查日期與狀態，並提供「檢查更新」「下載並安裝」「版本資訊」按鈕。
- **自動檢查更新**（預設開啟，登錄值 `AutoCheckUpdates`）：管理工具啟動時，若距離上次成功檢查（`LastUpdateCheck`，Unix 秒）已超過 24 小時，就在背景檢查一次。只會顯示「有新版本」，**不會自動下載或安裝**。
- 版本比較採 `主.次.修訂`；同版號時正式版高於預覽版（`2.0.1` > `2.0.1-beta.1`）；無法解析的版本一律不視為新版。

清單位址（永遠指向最新的正式版）：

```
https://github.com/ppundsh/cbxshell-rs/releases/latest/download/latest.json
```

`latest.json` 格式與 Tauri 相同：

```json
{
  "version": "2.0.2",
  "notes": "版本說明",
  "pub_date": "2026-11-20T00:00:00Z",
  "platforms": {
    "windows-x86_64": {
      "signature": "<CBXShell-Setup.exe.sig 的內容>",
      "url": "https://github.com/ppundsh/cbxshell-rs/releases/download/v2.0.2/CBXShell-Setup.exe"
    }
  }
}
```

---

## 3. 自動更新流程

1. 使用者按「下載並安裝」並在確認對話框按「確定」。
2. 以 WinHTTP 下載安裝檔（使用系統 Proxy 設定，TLS 由 Windows 驗證；跟隨 GitHub 轉址到 CDN），顯示下載進度。
3. **驗證簽章**：以編譯進管理工具的公鑰（`assets/updater.pub`）驗證 minisign（Ed25519）簽章。驗證失敗即中止，不寫入、不執行任何檔案。若此版本列有 YubiKey 核准金鑰，還必須通過維護者核准簽章（見第 4 節）。
4. 寫入 `%TEMP%\CBXShell-Update\CBXShell-Setup-<版本>.exe`，以 `--update --dir <安裝資料夾> --wait-pid <管理工具 PID>` 啟動，接著關閉管理工具。
5. 安裝程式等待管理工具結束、替換檔案、重新註冊外掛，最後重新開啟管理工具。失敗時顯示錯誤並重新開啟舊版管理工具。

### 安全設計

| 威脅 | 對策 |
|---|---|
| 竄改的安裝檔／中間人 | 下載內容必須通過內建公鑰的 Ed25519 簽章驗證；TLS 只是第二層 |
| GitHub 帳號、CI 或簽章私鑰外洩 | 另需維護者 YubiKey（FIDO2，PIN＋觸碰）對版本與 SHA-256 的核准簽章，私鑰無法從 YubiKey 匯出 |
| 惡意清單指向其他網站 | `url` 必須以 `https://github.com/ppundsh/cbxshell-rs/releases/download/` 開頭，且不得含 `..`、`?`、`#`、`@`、反斜線或空白 |
| 降版攻擊 | 只接受版本號嚴格大於目前版本的清單 |
| 資源耗盡 | 清單 ≤ 64 KiB、安裝檔 ≤ 256 MiB；JSON 巢狀深度 ≤ 16 |
| 未設定金鑰的建置 | `assets/updater.pub` 沒有金鑰時，更新功能停用並提示改從 GitHub 下載 |
| 未經同意的安裝 | 自動檢查只會提示；下載與安裝都需要使用者按鈕並確認 |

---

## 4. YubiKey 發行核准（第二道簽章）

只靠 CI 的 minisign 私鑰時，GitHub 帳號、Actions 工作流程或 Secrets 任何一處被入侵，攻擊者都能推送「簽章正確」的惡意更新。因此更新器另外要求**維護者用 YubiKey 簽的核准**：

| 層級 | 誰持有 | 證明什麼 |
|---|---|---|
| minisign 簽章（`.sig`） | GitHub Actions Secret | 安裝檔是 CI 建置出來的 |
| YubiKey 核准（`latest.json` 的 `approval`） | 維護者的 YubiKey（FIDO2，私鑰無法匯出） | 維護者親自確認了**這個版本號＋這個安裝檔 SHA-256**，簽章時需要 PIN 與觸碰 |

- 核准內容是固定格式的文字：`cbxshell-release\nversion=<版本>\nsha256=<安裝檔 SHA-256>\n`，以 `ssh-keygen -Y sign -n cbxshell-release` 產生 SSHSIG 簽章。
- 更新器自行計算下載內容的 SHA-256 重組這段文字，用 `assets/release-approvers.pub` 裡的 `sk-ssh-ed25519` 公鑰驗證，並要求 FIDO 旗標同時有 **UP（觸碰）與 UV（PIN）**。只接受硬體金鑰（`sk-ssh-ed25519@openssh.com`），一般軟體 SSH 金鑰一律拒絕。
- 只要 `release-approvers.pub` 有任何一把金鑰，從該原始碼建置的版本就**一定要**核准；沒有核准或核准無效時中止更新。
- 標籤也必須用同一把 YubiKey 簽章（`git tag -s`）；`pre-push` hook 會擋下未簽章的提交與標籤，CI 也會在建置前驗證標籤簽章。

### 兩種 YubiKey 金鑰都可以

| | GnuPG（OpenPGP 卡片） | FIDO2 SSH 金鑰 |
|---|---|---|
| 適用 | 已用 `gpg --edit-key` › `keytocard` 把 Ed25519 金鑰放進 YubiKey | 用 `ssh-keygen -t ed25519-sk` 建立 |
| 設定腳本 | `scripts\setup-gpg-signing.ps1` | `scripts\setup-yubikey.ps1` |
| 核准簽章 | `gpg --detach-sign --armor`（SHA-256／SHA-512） | `ssh-keygen -Y sign -n cbxshell-release` |
| `release-approvers.pub` | `openpgp ssh-ed25519 AAAA… openpgp:0x…`（`gpg --export-ssh-key <KEYID>!` 的輸出，只有公開金鑰值，不含 email） | `sk-ssh-ed25519@openssh.com AAAA…` |
| 觸碰證明 | 簽章本身無法證明觸碰；請用 `ykman openpgp keys set-touch sig on` 讓 YubiKey 每次簽章都要觸碰 | 簽章內含 UP／UV 旗標，更新器會檢查 |

更新器依 `approval` 的格式（`BEGIN PGP SIGNATURE` 或 `BEGIN SSH SIGNATURE`）選擇驗證方式；兩者都只接受 Ed25519，且都必須是 `release-approvers.pub` 列出的金鑰。

---

## 5. 發行者設定（一次性）

1. **YubiKey**：
   - 已經把 GnuPG 金鑰放在 YubiKey 上（OpenPGP）：執行 `scripts\setup-gpg-signing.ps1`。它不會建立或修改金鑰，只會找出 YubiKey 上可簽章的 Ed25519 金鑰（或用 `-KeyId` 指定），設定 `gpg.format=openpgp`、`gpg.program`、`user.signingkey`、commit／tag 一律簽章與 `pre-push` hook，並把公開金鑰值加入 `assets/release-approvers.pub`，最後實際簽一次測試。
   - 使用 FIDO2 SSH 金鑰：執行 `scripts\setup-yubikey.ps1`（若 YubiKey 上 `ssh:github` 槽位已有金鑰、這台電腦卻沒有金鑰檔，加 `-Application ssh:cbxshell -KeyName id_ed25519_sk_cbxshell` 另開槽位）。

   提交 `assets/release-approvers.pub`。git 的 `user.email` 必須是金鑰使用者 ID 中、且已在 GitHub 驗證的 email，GitHub 才會顯示 Verified。
2. **minisign 金鑰**（CI 用）：

   ```powershell
   npx @tauri-apps/cli@2 signer generate -w "$HOME\.tauri\cbxshell.key"
   ```

   把 `cbxshell.key.pub` 的內容貼到 `assets/updater.pub` 並提交。**私鑰不可提交。**
3. **GitHub Secrets**（建議放在名為 `release` 的 Environment，見下）：
   - `CBX_UPDATER_KEY`：私鑰檔案的**內容**
   - `CBX_UPDATER_KEY_PASSWORD`：私鑰密碼（沒有就留空字串）
4. **GitHub 儲存庫設定**（建議）：
   - **Settings › Environments › New environment `release`**：把上面兩個 Secret 移到這裡，「Deployment tags」限定 `v*`，並勾選 **Required reviewers**（自己），每次發行需在網頁上核准才會拿到私鑰。
   - **Settings › Rules › Rulesets**：對 `refs/tags/v*` 建立規則，限制只有管理員能建立／刪除／更新標籤；對預設分支開啟 **Require signed commits** 與禁止 force push。
   - 帳號 **Settings › Password and authentication**：把 YubiKey 註冊為 Security key／Passkey，並移除簡訊等較弱的 2FA 方式。

> 公鑰（minisign 或 YubiKey）一旦隨版本發佈就不要直接更換；若必須更換，先發行一個「同時列出新舊金鑰、仍以舊金鑰簽署與核准」的過渡版本。建議另外準備一支備用 YubiKey，用同樣方式產生金鑰並一起列入 `release-approvers.pub`，避免遺失唯一的金鑰後無法再發行更新。

---

## 6. 發行步驟

1. `bump-version.ps1` 更新版本 → 簽章提交 → `git tag -s vX.Y.Z -m "vX.Y.Z"` → 推送（各需 YubiKey PIN＋觸碰）。
2. GitHub Actions：驗證標籤簽章 → 測試 → `build.ps1` → `release-manifest.ps1`（minisign 簽署＋`latest.json`）→ 建立**草稿** Release。草稿不會出現在 `releases/latest`，使用者收不到。
3. 本機執行 `scripts\approve-release.ps1 -Tag vX.Y.Z`：確認標籤簽章、下載草稿、顯示安裝檔 SHA-256 供確認，用 YubiKey 簽核准、寫入 `latest.json`、上傳並發布。

`release.yml` 需要的修改（檔案受保護，請手動套用）：

```yaml
jobs:
  release:
    runs-on: windows-latest
    environment: release          # Secrets 只在這個 Environment，需要你按核准
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - name: Verify tag is signed by a release approver (YubiKey)
        shell: pwsh
        run: |
          git fetch --force origin "refs/tags/${{ github.ref_name }}:refs/tags/${{ github.ref_name }}"
          $keys = Get-Content assets/release-approvers.pub |
            Where-Object { $_ -match '^sk-ssh-ed25519@openssh\.com ' } |
            ForEach-Object { 'cbxshell-approver namespaces="git" ' + (($_ -split ' ')[0..1] -join ' ') }
          if (-not $keys) { throw 'assets/release-approvers.pub has no approver key' }
          $keys | Set-Content -Encoding ascii "$env:RUNNER_TEMP/allowed_signers"
          git -c gpg.format=ssh -c gpg.ssh.allowedSignersFile="$env:RUNNER_TEMP/allowed_signers" verify-tag "${{ github.ref_name }}"
          if ($LASTEXITCODE -ne 0) { throw 'tag is not signed by a release approver' }

      # 使用 GnuPG（OpenPGP）金鑰時，改用這個步驟取代上面那個：
      - name: Verify tag is signed by a release approver (YubiKey OpenPGP)
        shell: pwsh
        run: |
          $tag = "${{ github.ref_name }}"
          $gpg = 'C:\Program Files\Git\usr\bin\gpg.exe'
          git fetch --force origin "refs/tags/${tag}:refs/tags/$tag"
          Invoke-WebRequest "https://github.com/${{ github.repository_owner }}.gpg" -OutFile "$env:RUNNER_TEMP/owner.gpg"
          & $gpg --batch --import "$env:RUNNER_TEMP/owner.gpg"
          $raw = git -c gpg.program="$gpg" verify-tag --raw $tag 2>&1 | Out-String
          if ($LASTEXITCODE -ne 0) { throw "tag signature invalid`n$raw" }
          $fpr = ([regex]'VALIDSIG ([0-9A-F]{40})').Match($raw).Groups[1].Value
          $ssh = (& $gpg --export-ssh-key "$fpr!") -split '\s+'
          $line = "openpgp $($ssh[0]) $($ssh[1])"
          if (-not (Select-String -LiteralPath assets/release-approvers.pub -SimpleMatch $line -Quiet)) {
            throw "tag signed by $fpr, which is not a release approver"
          }

      # …（原有的測試與 Build & package installer 步驟）…

      - name: Sign installer & write latest.json
        shell: pwsh
        env:
          TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.CBX_UPDATER_KEY }}
          TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.CBX_UPDATER_KEY_PASSWORD }}
        run: .\release-manifest.ps1 -Tag "${{ github.ref_name }}"

      - name: Create draft release (published after YubiKey approval)
        uses: softprops/action-gh-release@v2
        with:
          draft: true
          files: |
            dist/CBXShell-Setup.exe
            dist/CBXShell-Setup.exe.sig
            dist/latest.json
            dist/CBXShell-Source.zip
          generate_release_notes: true
```

> `releases/latest/download/latest.json` 只會指向**最新的已發布正式版**；草稿與預覽版（prerelease）不會被推送給使用者。
