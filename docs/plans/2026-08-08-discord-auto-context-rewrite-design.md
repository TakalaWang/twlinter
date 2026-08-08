# Discord 自動情境改寫設計

## 目標

TWLinter 在管理者註冊的 Discord 頻道中，對一般訊息自動執行繁體中文（台灣語境）檢查；只要規則引擎判定需要修改，就優先交給 Gemini 依照規則候選、原文與上下文進行安全改寫。使用者不需要輸入任何改寫指令。

Discord 指令只負責管理觸發範圍：啟用目前頻道、停用目前頻道、查看目前註冊頻道。頻道清單以本機 JSON 檔保存，Bot 重啟後仍然有效。

## 方案與取捨

採用「頻道 allowlist + Gemini 優先 + 規則式 fallback」：

- allowlist 避免 Bot 在整個伺服器的所有頻道回覆，未註冊頻道完全不處理一般訊息。
- Gemini 只在核心引擎產生變更時呼叫；輸入仍由既有 `RewriteRequest` 傳遞，輸出必須保留受保護內容，並重新通過核心規則檢查。
- Gemini 失敗、未設定或輸出不安全時，退回既有確定性建議，不阻塞訊息處理。
- 不保留 `/tw-rewrite` 這種使用者觸發路徑，避免兩種產品行為並存。

相較於「所有頻道自動處理」，allowlist 多一個管理步驟，但能控制噪音與 Gemini 成本；相較於「只用規則」，Gemini 能處理 `程序`、`進程` 等需要上下文的情境，而規則引擎仍是候選與安全邊界。

## 元件與資料流

1. `message` event 先忽略 Bot、自訂頻道未啟用或空訊息。
2. `CoreEngine::analyze` 產生規則問題，`CoreEngine::apply` 先產生確定性草稿與候選決策。
3. 有 Gemini 且草稿有變更時，送出原文、草稿、問題與 protected spans。
4. 回應通過 protected-span 檢查，重新分析後沒有殘留問題，才回覆改寫結果；否則回覆確定性草稿。
5. `interaction_create` 只處理管理指令，並要求 `MANAGE_GUILD`；每次啟動在已加入的 Guild 建立同一個本地 slash command。

頻道資料只保存 Discord channel ID，不保存訊息內容或 Token。寫入失敗時指令回覆錯誤，記憶體中的清單不宣稱已持久化成功。

## 測試與限制

- Registry 單元測試覆蓋新增、移除、重載、啟用判斷與空檔案。
- Policy 測試保留既有 deterministic fallback、protected spans 與無變更不回覆行為。
- 編譯、完整 Rust 測試、ruleset 檢查與 diff whitespace 檢查必須通過。
- 每次有變更的訊息都可能產生一次 Gemini rewrite 請求；本次不加入未被要求的 rate limiter，後續若有成本或延遲證據再處理。
