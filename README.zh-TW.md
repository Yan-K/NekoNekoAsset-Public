<p align="center">
  <img src="site/assets/readme-banner.png" alt="NekoNekoAsset" width="900">
</p>

<p align="center">
  <a href="https://github.com/Yan-K/NekoNekoAsset-Public/releases"><img alt="Release" src="https://img.shields.io/github/v/release/Yan-K/NekoNekoAsset-Public?include_prereleases&label=release&color=FF5FA2"></a>
  <img alt="Windows x64" src="https://img.shields.io/badge/platform-Windows%20x64-0969DA">
  <img alt="Unity 2022.3+" src="https://img.shields.io/badge/Unity-2022.3%2B-222222">
  <a href="LICENSE"><img alt="MIT License" src="https://img.shields.io/badge/license-MIT-2EA043"></a>
</p>

<p align="center">
  <a href="README.md">English</a> · <b>繁體中文</b> · <a href="README.ja.md">日本語</a>
</p>

<h3 align="center">素材買太多？交給貓貓整理！</h3>

<p align="center">
NekoNekoAsset 是一款 Windows 桌面 App。它會同步你在 BOOTH、Gumroad、Jinxxy 買過的 VRChat 素材，自動下載、解壓縮、分類，再一鍵送進 Unity。所有資料都只存在你的電腦裡。
</p>

<p align="center">
  <a href="https://yan-k.github.io/NekoNekoAsset-Public/zh-tw/"><b>官方網站</b></a> · <a href="https://github.com/Yan-K/NekoNekoAsset-Public/releases"><b>下載 Windows 版</b></a>
</p>

## 功能特色

- **自動同步購買紀錄**。登入一次就好，之後每次打開 App，新買的素材會自動出現在清單裡。
- **下載＋自動解壓縮**。ZIP、RAR、7z 自動解開並保留資料夾結構，日文檔名不亂碼，unitypackage 原檔保留。
- **分類、標籤，一找就有**。自動分類，還能加標籤、標記對應角色、建立收藏集；中文、日文都搜得到。
- **一鍵送進 Unity**。勾選檔案後先在 Unity 裡預覽，確認了才會寫進專案。
- **匯入自己的檔案**。資料夾、ZIP 拖進來一起管理，App 會複製一份到素材庫，原始檔案不會被動到。
- **花費統計、主題與語言**。依幣別統計有付款證據的金額；多款內建主題；支援繁中、简中、日本語、한국어、English。

## 支援的商店

| 來源 | 會同步的內容 |
| --- | --- |
| BOOTH | Library、購買紀錄、禮物、免費商品 |
| Gumroad | Library、Bundle |
| Jinxxy | Inventory |
| 你的電腦 | 資料夾、ZIP、單一檔案（複製進素材庫，不動原檔） |

每家商店都能登入多個帳號，彼此獨立。

## 安裝

1. 到[官方網站](https://yan-k.github.io/NekoNekoAsset-Public/zh-tw/)或 [Releases](https://github.com/Yan-K/NekoNekoAsset-Public/releases) 下載最新的 `NekoNekoAsset_x.y.z_x64-setup.exe`。
2. 執行安裝程式。如果跳出 SmartScreen 警告，確認檔案是從這兩個地方下載的之後，點「其他資訊」→「仍要執行」。
3. 選擇素材庫位置，登入商店帳號（也可以先離線使用），完成！

目前只支援 Windows 64 位元。之後的新版本會在 App 內提示，驗證簽章後才安裝。

## Unity Bridge

想把素材一鍵送進 Unity，請在 Unity 2022.3 以上的專案安裝 NekoNekoAsset Bridge。三種格式內容相同，擇一即可，都在 [Releases](https://github.com/Yan-K/NekoNekoAsset-Public/releases) 頁面：

| 格式 | 安裝方式 |
| --- | --- |
| `.unitypackage` | 拖進 Unity 的 Project 視窗匯入 |
| UPM（`.tgz`） | Package Manager →「+」→「Add package from tarball…」 |
| VPM（`.zip`） | 解壓縮到專案的 `Packages/com.yank.nekonekoasset.bridge` |

Bridge 只在你的電腦內部（localhost）接收 App 傳來的檔案；每次匯入都會先在 Unity 裡預覽，確認後才寫入。

## 隱私與安全

你的資料只住在你的電腦裡。NekoNekoAsset 沒有自己的伺服器，也不收集任何使用數據。

- **不經手密碼**。在商店官方頁面登入；App 只保存登入 cookie，並用 Windows DPAPI 加密。
- **沒有雲端**。資料庫、縮圖、素材都在你選的資料夾；沒有註冊、沒有雲端同步。
- **零追蹤**。沒有遙測、使用分析、廣告或當機回報。
- **AI 預設關閉**。開啟後，只會把你選取的文字與圖片送給你選的服務。
- **原始碼公開**。MIT 授權，歡迎檢查每一行程式碼。

App 會連線的地方，全部在這裡：

| 連線對象 | 用途 | 時機 |
| --- | --- | --- |
| BOOTH・Gumroad・Jinxxy | 登入、同步清單、下載你買的檔案 | 你登入後 |
| Google・Apple・X 等登入服務 | 用這些帳號登入商店 | 只在登入時 |
| GitHub | 讀取公開的版本資訊、下載更新 | 檢查更新時 |
| 你選的 AI 服務 | 你選取的文字與圖片 | 開啟 AI 並使用時 |
| 你貼上的商品網址 | 抓取公開的商品資訊 | 匯入本機檔案時 |
| Unity Bridge | 在你的電腦內部（localhost）傳檔 | 匯入時 |

## 常見問題

### 隱私與安全

<details>
<summary>NekoNekoAsset 會知道我的商店密碼嗎？</summary>

**不會**。登入是在 App 開啟的商店官方登入頁面完成，密碼是你直接交給 BOOTH、Gumroad 或 Jinxxy。NekoNekoAsset 不會讀取、也不會儲存你的密碼，只保存登入成功後商店發給你的登入 cookie，用來讀取「你自己的」購買清單和下載檔案。

</details>

<details>
<summary>登入資訊存在哪裡？會不會被別人拿走？</summary>

每個帳號的登入資訊分開存在這台電腦的使用者資料夾，並用 Windows 內建的 **DPAPI 加密**。只有同一台電腦上的同一個 Windows 使用者解得開，檔案被複製到別台電腦也無法使用。登入資訊不會寫進素材庫、備份或匯出檔。

提醒：如果電腦已經中了以你的身分執行的惡意程式，任何本機 App 都無法完全防範，請保持系統安全。

</details>

<details>
<summary>我的購買紀錄和素材庫會被上傳到雲端嗎？</summary>

**不會**。資料庫（SQLite）、縮圖和下載的檔案，全都存在你選的素材庫資料夾。NekoNekoAsset 沒有帳號系統、沒有開發者伺服器，也不會自動上傳或雲端備份任何東西。

如果你自己把素材庫放進 OneDrive、Dropbox 這類雲端同步資料夾，上傳的是那些同步軟體，不是 App。我們建議放在本機硬碟或 NAS。

</details>

<details>
<summary>開發者看得到我的資料嗎？有沒有偷偷收集使用數據？</summary>

**看不到，也沒有收集**。App 裡沒有任何遙測（telemetry）、使用分析、廣告或當機回報工具。和開發者相關的連線只有一個：啟動時向 GitHub 讀取公開的版本資訊檔來檢查更新，這個請求不包含你的帳號、購買紀錄或素材資料。

</details>

<details>
<summary>App 到底會連到哪些地方？</summary>

請看上方「隱私與安全」的連線表，那就是全部。Unity Bridge 只在你的電腦內部傳檔，不會連到外部網路。

</details>

<details>
<summary>AI 功能會把我的資料送去哪裡？</summary>

**AI 預設是關閉的**。開啟後要填入你自己的 API 金鑰，App 只會把你選取商品的文字資訊（例如名稱、標籤、檔名、說明文件片段）和你明確選擇的圖片，送給「你選的那一家」AI 服務；也可以改用在自己電腦上執行的本機模型。

縮圖分析另外預設關閉，圖片會先縮小到 1536px 以內。登入 cookie、下載連結和素材壓縮檔永遠不會送出。

</details>

<details>
<summary>AI 的 API 金鑰存在哪裡？</summary>

存在 **Windows 認證管理員**（Credential Manager），每組金鑰分開保存，只留在這台電腦，不會進入素材庫、備份、匯出檔或記錄檔。

</details>

<details>
<summary>備份、匯出或「複製診斷資訊」會包含隱私資料嗎？</summary>

備份與匯出**不包含登入資訊和 API 金鑰**。「複製診斷資訊」只會複製固定格式的狀態和計數（例如卡在哪個步驟、重試了幾次），不含帳號、訂單、商品名稱、網址或檔案路徑。它只會放進你的剪貼簿，要不要貼給別人由你決定。

</details>

<details>
<summary>素材庫放在 NAS、或兩台電腦輪流用安全嗎？</summary>

可以。同一個素材庫**一次只能由一台電腦開啟**，App 會自動上鎖，避免同時寫入造成損壞；關掉 App 後另一台電腦就能接著用。

登入資訊、API 金鑰和 AI 聊天紀錄只存在各自的電腦，所以換電腦時需要重新登入商店。

</details>

<details>
<summary>怎麼清除我的登入資訊？</summary>

在帳號設定裡按「登出」或「刪除」，就會清除那個帳號在這台電腦的登入狀態和加密 cookie；刪除帳號時，可以選擇要不要一起刪除它的資產記錄，已下載的檔案會保留。AI 金鑰可以在 AI 設定中移除。

</details>

<details>
<summary>Unity Bridge 會不會亂改我的專案？</summary>

Bridge 只在你的電腦內部接收 App 傳來的檔案，連線需要驗證，不會連到外部網路。每次匯入都會**先在 Unity 裡顯示預覽，由你確認後才寫入**；匯入失敗時，也只會清掉這次新寫入、而且沒被改過的檔案。

</details>

<details>
<summary>這是官方工具嗎？使用會有風險嗎？</summary>

**不是官方工具**。NekoNekoAsset 是個人開發的非官方開源專案，和 BOOTH（pixiv）、Gumroad、Jinxxy、VRChat、Unity 都沒有關係。App 用你自己的登入狀態讀取你自己的購買清單，會限制請求速度，並遵守伺服器要求的等待時間。各平台的使用條款請以官方為準，並自行評估。

</details>

<details>
<summary>為什麼安裝時跳出「Windows 已保護您的電腦」？</summary>

安裝檔目前沒有購買程式碼簽章憑證，所以 SmartScreen 可能會警告。請**只從官方網站或本專案的 GitHub Releases 下載**，確認來源後點「其他資訊」→「仍要執行」即可。App 內的自動更新會先驗證簽章才安裝。

</details>

### 使用

<details>
<summary>要付費嗎？</summary>

完全免費，以 MIT 授權開源。

</details>

<details>
<summary>支援哪些作業系統？</summary>

目前只支援 Windows（64 位元）。macOS 和 Linux 尚未支援。

</details>

<details>
<summary>登入後會自動下載全部素材嗎？</summary>

不會。自動下載預設關閉，同步時只會先抓商品資訊和縮圖。要下載哪些由你決定，也可以之後再到設定開啟自動下載。

</details>

<details>
<summary>一定要安裝 Unity 嗎？</summary>

不用。只有想「一鍵送進 Unity」時，才需要在 Unity 2022.3 以上的專案安裝 NekoNekoAsset Bridge（提供 unitypackage、UPM、VPM 三種格式）。

</details>

<details>
<summary>為什麼「全部」裡看不到某些商品？</summary>

BOOTH 也賣很多和 VRChat 無關的東西，所以預設的「全部」只顯示 VRChat 相關素材。其他商品仍然完整保存，可以在「未分類」裡找到，或手動修改分類。

</details>

還有其他問題？[到 GitHub Issues 問我們](https://github.com/Yan-K/NekoNekoAsset-Public/issues)。

## 從原始碼建置

```sh
cd app
pnpm install --frozen-lockfile
pnpm native:dev      # 開發模式
pnpm native:build    # 建置安裝檔
```

需要 Node.js 與 pnpm、Rust（MSVC 工具鏈）、Visual Studio C++ 建置工具與 Windows SDK、WebView2 Runtime。想參與開發請先看 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 授權

原始碼以 [MIT 授權](LICENSE)釋出。內附的第三方元件（例如 7-Zip、Noto Sans CJK）保留各自的授權，詳見 [app/src-tauri/resources/licenses](app/src-tauri/resources/licenses)。官網字型（Fredoka、Nunito、Chiron GoRound TC、Zen Maru Gothic）採用 SIL Open Font License，詳見 [site/assets/fonts](site/assets/fonts)。

## 免責聲明

NekoNekoAsset 是非官方的個人開源專案，與 BOOTH（pixiv）、Gumroad、Jinxxy、VRChat、Unity 皆無關聯。文中提到的商標均屬於各自的所有者。
