//! 診斷：原始碼位置、穩定錯誤碼、rustc 風格的繪製。
//!
//! 規格是 **G51**（`PLAN.md` §10）：**一次報全 + 穩定錯誤碼**，
//! 每條至少帶 **行列號 + 原始碼片段 + caret**。樣例見
//! [DSL 指令集參考](../../../docs/architecture/dsl-reference.md) §九。
//!
//! ★ ★ **錯誤碼一旦發出就不能改含義**（G51 明寫，與 D9 相鄰）。所以它們集中登記在
//! [`DiagCode`] 的常數表裡，並由 `tests/` 裡的一條測試釘住「編號不重複、且與文件裡
//! 那條樣例用的 `FUL-DSL-0007` 對得上」——文件裡印出去的那個號碼就是契約的一部分。

use std::fmt::Write as _;

/// 原始碼裡的一段位元組區間。半開區間 `[start, end)`。
///
/// ★ 存**位元組**偏移而不是行列：行列是繪製時才算的派生量。詞法器每讀一個 token 都去
/// 維護行列，既慢又容易在多位元組字元上算錯，而錯了不會有任何症狀——診斷只是指偏一格。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// 把兩段合成一段（取外包絡）。
    pub fn join(self, other: Span) -> Span {
        Span::new(self.start.min(other.start), self.end.max(other.end))
    }
}

/// 一份原始碼檔案：路徑 + 全文 + 行首偏移索引。
#[derive(Debug, Clone)]
pub struct Source {
    path: String,
    text: String,
    /// 每一行的起始位元組偏移。`line_starts[0] == 0`。
    line_starts: Vec<usize>,
}

/// 一個位元組偏移對應的人讀位置。行、列都從 **1** 開始。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    pub line: usize,
    /// ★ 列按**字元**數，不是位元組數。使用者看到的是字元。
    pub col: usize,
}

impl Source {
    pub fn new(path: impl Into<String>, text: impl Into<String>) -> Self {
        let text: String = text.into();
        let mut line_starts = vec![0usize];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self {
            path: path.into(),
            text,
            line_starts,
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// 位元組偏移 → 行列。偏移越界時鉗到檔案末尾（診斷寧可指得偏，也不該 panic）。
    pub fn location(&self, byte: usize) -> Location {
        let byte = byte.min(self.text.len());
        // 最後一個 <= byte 的行首。行數不多，線性/二分都行，這裡用二分。
        let line_idx = match self.line_starts.binary_search(&byte) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let line_start = self.line_starts[line_idx];
        let col = self.text[line_start..byte].chars().count() + 1;
        Location {
            line: line_idx + 1,
            col,
        }
    }

    /// 取某一行的正文（不含行尾換行；`\r` 也一併去掉）。
    ///
    /// ⚠ `\r` 必須去：設定檔案在 Windows 上被編輯過就會帶 CRLF，留著它繪製出來的
    /// 那一行會把後面的 caret 行覆蓋掉——終端機上看是「錯誤提示少了一行」。
    pub fn line_text(&self, line: usize) -> &str {
        if line == 0 || line > self.line_starts.len() {
            return "";
        }
        let start = self.line_starts[line - 1];
        let end = self
            .line_starts
            .get(line)
            .map(|e| e - 1)
            .unwrap_or(self.text.len());
        self.text[start..end].trim_end_matches('\r')
    }
}

/// 穩定的診斷編號，繪製成 `FUL-DSL-0007`。
///
/// ★ ★ **只增不改**：一個編號一旦出現在某個版本的輸出裡，它的含義就固定了。
/// 要換含義就發新號，舊號留著（哪怕不再產生）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DiagCode(pub u16);

impl DiagCode {
    // ── 詞法 ────────────────────────────────────────────────────────────────
    /// 檔案在塊還開著的時候結束了。
    pub const UNCLOSED_BLOCK: DiagCode = DiagCode(1);
    /// 多出來的 `}`。
    pub const UNEXPECTED_CLOSE: DiagCode = DiagCode(2);
    /// 引號沒閉合。
    pub const UNCLOSED_QUOTE: DiagCode = DiagCode(3);

    // ── 結構 ────────────────────────────────────────────────────────────────
    /// 全域選項塊必須在最前，且只能有一個。
    pub const GLOBAL_BLOCK_POSITION: DiagCode = DiagCode(4);
    /// 站台位址寫法不合法。
    pub const BAD_SITE_ADDRESS: DiagCode = DiagCode(5);
    /// 站台塊缺少 `{`。
    pub const SITE_BLOCK_EXPECTED: DiagCode = DiagCode(6);

    // ── 指令 ────────────────────────────────────────────────────────────────
    /// 未知指令。★ 這個號碼印在 DSL 參考文件 §九的樣例裡，**不能改**。
    pub const UNKNOWN_DIRECTIVE: DiagCode = DiagCode(7);
    /// 指令參數個數不對。
    pub const BAD_ARITY: DiagCode = DiagCode(8);
    /// 未知的子指令。
    pub const UNKNOWN_SUBDIRECTIVE: DiagCode = DiagCode(9);
    /// 指令用在了不允許它出現的位置。
    pub const BAD_PLACEMENT: DiagCode = DiagCode(10);
    /// 需要子塊卻沒給，或給了不該給的子塊。
    pub const BLOCK_MISMATCH: DiagCode = DiagCode(11);

    // ── 匹配器（G50）────────────────────────────────────────────────────────
    /// 引用了沒定義過的 `@name`。
    pub const UNKNOWN_MATCHER: DiagCode = DiagCode(12);
    /// 行內匹配器只能是路徑。
    pub const INLINE_MATCHER_NOT_PATH: DiagCode = DiagCode(13);
    /// `@name` 重複定義。
    pub const DUPLICATE_MATCHER: DiagCode = DiagCode(14);
    /// 未知的匹配條件。
    pub const UNKNOWN_MATCHER_CONDITION: DiagCode = DiagCode(15);

    // ── 佔位符（G61）────────────────────────────────────────────────────────
    /// 未知佔位符。
    pub const UNKNOWN_PLACEHOLDER: DiagCode = DiagCode(16);
    /// 佔位符用在了它不可用的位置。★ 這裡必須是錯誤，不能解析成空串。
    pub const PLACEHOLDER_NOT_AVAILABLE: DiagCode = DiagCode(17);

    // ── 字面值類型（DSL 參考 §七）───────────────────────────────────────────
    /// 時長寫法不對（★ 裸數字是錯誤，不做預設單位）。
    pub const BAD_DURATION: DiagCode = DiagCode(18);
    /// 大小寫法不對。
    pub const BAD_SIZE: DiagCode = DiagCode(19);
    /// 布爾只認 `true` / `false`。
    pub const BAD_BOOL: DiagCode = DiagCode(20);
    /// 枚舉取值不在允許集合裡。
    pub const BAD_ENUM: DiagCode = DiagCode(21);
    /// 狀態碼不合法。
    pub const BAD_STATUS: DiagCode = DiagCode(22);

    // ── 站台級 ──────────────────────────────────────────────────────────────
    /// 同一站台裡重複出現了只能有一次的指令。
    pub const DUPLICATE_DIRECTIVE: DiagCode = DiagCode(23);
    /// 未知的全域選項。
    pub const UNKNOWN_GLOBAL_OPTION: DiagCode = DiagCode(24);
    /// 站台位址在多個塊裡重複。
    pub const DUPLICATE_SITE_ADDRESS: DiagCode = DiagCode(25);
    /// M1 不做 `import`（G62）。
    pub const IMPORT_NOT_SUPPORTED: DiagCode = DiagCode(26);
    /// 一個站台塊裡一條指令都沒有。
    pub const EMPTY_SITE: DiagCode = DiagCode(27);
    /// ★ ★ 這一條**專屬於 G49**：內建順序表把書寫順序與執行順序拆開之後，
    /// 「我寫在後面的備援怎麼先跑了」是必然會出現的意外。G49 配套第 4 條要求
    /// **診斷必須說出它實際跑在第幾步**，否則使用者只能去背那張表。
    pub const UNREACHABLE_STEP: DiagCode = DiagCode(28);
    /// `resolvers` 裡的一條位址寫得不對（形狀層面：連接埠、主機名字元、IPv6 字面值……）。
    ///
    /// ★ ★ 新增。在此之前這一類錯誤長在**裝載期**，處置是
    /// 「打一行 error，本站台的 DNS-01 不啟用」—— 而 `validate` 結束代碼仍是 0、
    /// 站台照常起來，於是一份寫錯的設定**在每一處都顯得正常**，
    /// 直到那張證書永遠籤不下來。⇒ 形狀判據搬到編譯期，它不需要網路。
    /// ⚠ 「主機名解析得出來嗎」**不在**這條裡：那要網路，留到簽發那一刻。
    pub const BAD_RESOLVER: DiagCode = DiagCode(29);
    /// 自動 HTTP 重定向合成的站台，在 :80 上蓋過了使用者自己寫的連接埠備援站台。
    ///
    /// ★ warning 而不是 error：這是**有意的行為**（G12 承諾的自動跳轉），
    /// 但它改變了那幾個主機名在 :80 上的去向 —— 而沉默地改掉別人設定的行為，
    /// 正是本儲存庫反覆點名的「現場看不出問題」。
    pub const AUTO_REDIRECT_SHADOWS: DiagCode = DiagCode(30);
    /// 憑證的來源前綴寫錯了（`fil:` / `ENV:` 這類）。
    ///
    /// ★ ★ 此後「不寫前綴就是值本身」，於是**一個打錯的前綴會被當成憑證**，
    /// 帶著一個根本不是憑證的字串去打對端 —— 現場是「憑證不對」，
    /// 而真正的原因是打錯了幾個字母。這條錯誤就是為它準備的。
    pub const BAD_CREDENTIAL_SOURCE: DiagCode = DiagCode(31);
    /// 一條**必填**的子指令沒寫（M2 批 F 起：`file_server { root … }`）。
    ///
    /// ★ 它與 `BAD_ARITY` 的差別在於「誰缺席」：BAD_ARITY 是子指令**寫了**但參數個數不對，
    /// 這一條是子指令**整個沒出現**——回報錯誤要指著那條鏈上指令本身，而不是指著一個不存在的 span。
    pub const MISSING_REQUIRED_SUB: DiagCode = DiagCode(32);
    /// 路徑必須是絕對路徑（G91）。
    ///
    /// ★ ★ 相對路徑依賴處理程序 cwd，而 systemd 下 cwd 是 `/`、開發機上是專案目錄
    /// ⇒ **同一份設定在兩處指向兩個地方**。那正是「能裝載、行為不同」那一類事故，
    /// 現場看到的只是 404，沒有任何東西說「你的 root 解析到別處去了」。
    pub const PATH_NOT_ABSOLUTE: DiagCode = DiagCode(33);
    /// 這條全域選項**曾經存在，現在刪掉了**（`fallback_*`，G98）。
    ///
    /// ★ ★ 它與 `UNKNOWN_GLOBAL_OPTION` 有意分開：後者說的是「沒這個東西」，
    /// 而這一條說的是「有過，沒了，去哪了」。⚠ 合成一條的話，
    /// 一個照著舊文件寫的設定會得到「你是不是想寫 XXX」這種毫無幫助的建議。
    pub const REMOVED_GLOBAL_OPTION: DiagCode = DiagCode(34);
    /// 多個 `cache` 塊對一件**處理程序級**的事給出了互相矛盾的值（M2 批 H 起：`disk`）。
    ///
    /// ★ ★ 它必須是 error 而不是「取第一個」：快取後端整個處理程序只有一個，
    /// 而兩個不同的 `disk` 目錄裡，**必有一個是使用者以為生效、其實沒有的**。
    /// ⚠ 與它相鄰的 `capacity` 走的是另一條路（取最大值 + 裝載紀錄說出生效值），
    /// 那條能成立是因為「容量取大一點」不會讓任何一個站台拿不到它要的東西；
    /// 而**目錄取一個**會讓另一個站台的快取整個落在別處，現場是
    /// 「我的快取怎麼沒落到我寫的那個盤上」，設定裡一個字都看不出問題。
    pub const CACHE_BACKEND_CONFLICT: DiagCode = DiagCode(35);
    /// `log { headers … }` / `resp_headers` 的白名單裡寫了一個**敏感頭**
    /// （**M2 批 L 第 ③ 步**）。
    ///
    /// ★ ★ ★ **取「編譯期拒絕」而不是「執行環境脫敏」，理由是兩者的失效形態不同**：
    /// 脫敏表要跟得上每一個新的敏感頭名，而**漏一個就是一次靜默洩漏**
    /// —— 紀錄裡多出一行 `req_hdr_authorization`，沒有任何東西會說；
    /// 而編譯期拒絕的失效形態是「這份設定裝不上」，當場可見。
    ///
    /// ⚠ 理由要連著 **G114** 一起讀：owner 推翻的是「記不記頭」這條推薦項，
    /// **沒有**推翻它背後那半條 ——「私鑰、ACME 憑證、上游認證資訊不得進普通紀錄」
    /// （安全基線，也是觀測那一頁「最容易在哪做錯」第 2 條）。
    /// ★ **一個被推翻的推薦項，它的理由通常只有一半被推翻**，那半條要換一個落點繼續成立。
    pub const SENSITIVE_HEADER_LOGGED: DiagCode = DiagCode(36);
    /// `metrics` 這一步**沒有任何限制得了來源的匹配器**（**M2 批 M**，G116）。
    ///
    /// ★ ★ G116 把指標端點做成了普通站台塊裡的終結指令，代價寫在它自己那一條裡：
    /// **指標與業務共用監聽器，matcher 寫錯就會把指標暴露出去**，而
    /// 「這一條只能靠**文件與診斷**兜，架構兜不住」。這個編號就是那個診斷。
    ///
    /// # ★ ★ ★ 判據是「限制得了來源」，不是「有沒有匹配器」
    ///
    /// 一條匹配器算不算數，問的是：**它能不能把兩個發同樣請求的用戶端分開？**
    ///
    /// | 條件 | 算不算 | 為什麼 |
    /// |---|---|---|
    /// | `remote_ip` | ✅ | socket 對端，在這一層偽造不了 |
    /// | `header` | ✅ | 一個共享金鑰可以放在這裡 |
    /// | `path` · `path_regexp` · `host` · `method` · `query` | ❌ | 全都是請求行與請求頭裡的東西，**任何用戶端都能照著發一份** |
    ///
    /// ⚠ ⚠ **「路徑匹配器不算」是這條診斷現在的全部價值所在。**
    /// 從 nginx / Caddy 遷過來的人第一反應就是寫一條 `handle /metrics { metrics }`，
    /// 於是「最可能出現的那個裸奔設定」恰恰是最先撞上來的那一個 ——
    /// 若把它算成保護，這條診斷就在它唯一要抓的東西上沉默，
    /// 而**一個只見過綠的門與一個不存在的門無法區分**。
    ///
    /// ⚠ 仍然判不動的那一半：**匹配器寫得對不對**（網段圈沒圈對、這臺機器擺在誰後面、
    /// `header User-Agent Prometheus` 其實誰都擋不住）。編譯期沒有這些資訊
    /// ⇒ **寧可漏報也不誤報**：一條報「已保護」的假話會讓人不再去看那一行，
    /// 而它恰恰是唯一還會有人看的機會。
    ///
    /// ★ warning 而不是 error：把指標開在內網可信段上、或者開在只有自己連得到的
    /// 環回位址上，都是正當設定 —— 一個拒絕裝載的門會把它們一起擋掉。
    pub const METRICS_UNGUARDED: DiagCode = DiagCode(37);
    /// `reverse_proxy { weight <地址> … }` 裡那個位址**不在這條 `reverse_proxy` 的上游清單裡**
    /// （**M2 批 N**，裁決 R1）。
    ///
    /// ★ ★ 比對是**逐字相同**，不做歸一化：`normalize_upstream`（`backend` → `backend:80`）
    /// 住在 `fulcrum-runtime`，而這一步在 `fulcrum-config`。在這邊再寫一份「差不多的」
    /// 就是分家，而分家的現場是「寫了 `weight backend 3`，設定照過，權重沒生效」。
    /// ⇒ 對不上就在裝載期拒絕。
    ///
    /// ⚠ ⚠ **這條診斷必須把那條 `reverse_proxy` 的上游清單原樣列出來**：
    /// 只說「找不到」等於讓人去猜自己上一行寫的到底是什麼 —— 而這條錯誤最常見的成因
    /// 恰恰是兩處寫法差了一個連接埠。
    pub const UNKNOWN_WEIGHT_UPSTREAM: DiagCode = DiagCode(38);
    /// 同一個上游寫了兩條 `weight`（**M2 批 N**）。
    ///
    /// ★ ★ 必須是 error，⛔ **不許「後寫的贏」**：那種規則下刪掉或挪動其中一行
    /// 會**靜默**改掉權重，而它與「兩行本來就是一樣的」在設定里長得一模一樣。
    pub const DUPLICATE_WEIGHT: DiagCode = DiagCode(39);
    /// 權重不在 `[1, 65535]` 裡 —— 含 `0`、負數、帶單位、不是數字（**M2 批 N**，裁決 R3）。
    ///
    /// ★ ★ **`0` 不合法是有意的**：「這臺不參與排程」**只有一種表達方式**，
    /// 就是管理面臨時覆蓋層的 `disable`（G18）。讓 `weight 0` 也表示摘掉，
    /// 就是兩條路做同一件事 —— 而兩條路遲早分家，且分家那天沒有任何東西會說。
    pub const BAD_WEIGHT: DiagCode = DiagCode(40);
    /// 同一個 `reverse_proxy` 塊裡寫了兩行 `id`（**M2 批 N 任務 2.8**，裁決 R6 ⇒ G125）。
    ///
    /// ★ ★ 必須是 error，⛔ **不許「後寫的贏」** —— 與 [`DiagCode::DUPLICATE_WEIGHT`]
    /// 逐字同一條理由：那種規則下刪掉或挪動其中一行會**靜默**改掉這條 `reverse_proxy`
    /// 的 id，而 id 正是管理面覆蓋層的尋址依據 ⇒ 現場是「`disable` 指著的那臺機器還在跑」。
    pub const DUPLICATE_PROXY_ID: DiagCode = DiagCode(41);
    /// `id` 寫了個空串（**M2 批 N 任務 2.8**）。
    ///
    /// ★ ★ 它不是潔癖：**沒寫 `id` 的那條在鍵裡 id 那一格就是空串**（裁決 R6 ②）
    /// ⇒ `id ""` 與「根本沒寫」是**同一個鍵**。
    /// ⚠ 最壞的現場恰恰是最像修好了的那一種：兩條 `reverse_proxy` 的鍵相同，
    /// 有人給其中一條補了 `id ""` 想把它們分開 —— 而那一行什麼都沒改變。
    /// ⇒ ★ ★ **同鍵不是錯誤**（裁決 R6 ③ 第二輪 ⇒ G125），裝載照樣成功、
    /// 兩條照樣共享同一個覆蓋格子 ⇒ **除了這一條診斷，沒有任何東西會說話**。
    pub const EMPTY_PROXY_ID: DiagCode = DiagCode(42);
    /// `id` 不在取值域裡：`[A-Za-z0-9_.-]`、長度 1–64（**M2 批 P**，裁決 R6 三之三）。
    ///
    /// # ★ ★ ★ 收緊的理由只有一條半，⛔ 別把它記成「基數風險」
    ///
    /// 1. **硬的那條 —— 與備援記號撞車**：本儲存庫用 `<other>` / `<none>` / `<unknown>` /
    ///    `<undeclared>` 表示「取不到 / 備援」，靠的是**尖括號在真值裡不可能出現**。
    ///    而在這一條之前，`id <none>` 是**合法**的。
    /// 2. **軟的那半條 —— 可讀性**：`id` 裡的換行會讓 `/stats` 與紀錄上那一行
    ///    在人眼裡斷成兩行。
    ///
    /// ⛔ **不是**指標基數：G126 明寫 `fulcrum_overrides_active` **無標籤**，`id`
    /// 一個字都不進指標。★ 一個假理由會讓下一個人理直氣壯地把限制放寬回去。
    ///
    /// ⚠ **空串不走這一條**，它有 [`DiagCode::EMPTY_PROXY_ID`] 與自己那段解釋
    /// （「`id ""` 與根本沒寫是同一個鍵」）—— 那句話是那條診斷的全部價值，
    /// ⛔ 不許被這條泛泛的「不合法」頂掉。
    ///
    /// ⚠ 這是一次**收緊**：在此之前能裝上的某些設定以後裝不上。M4 還沒到
    /// （沒有版本號、沒有相容性承諾，`PLAN.md` §7）⇒ 要收就現在收最便宜。
    ///
    /// ⚠ ⚠ **它只管 DSL 那條路。** 結構化設定（`POST /load`，G11 的公開入口）
    /// 不經過 `fulcrum compile` ⇒ 這條診斷在那條路上**一次都不會跑**，
    /// 由 `fulcrum_runtime` 建圖時用**同一個** `is_valid_proxy_id` 再攔一遍。
    pub const BAD_PROXY_ID: DiagCode = DiagCode(43);
    /// `passive_fail` 的值不是 **`[1, 65535]`** 裡的整數 —— 含 `0`、負數、帶單位、不是數字
    /// （**M2**，G136）。
    ///
    /// ★ ★ **`0` 不合法是有意的，理由與 [`BAD_WEIGHT`](Self::BAD_WEIGHT) 逐字同源**：
    /// 「這條 `reverse_proxy` 不做被動熔斷」**只有一種表達方式** —— **不寫 `passive_fail`**。
    /// 讓 `passive_fail 0` 也表示關閉，就是兩條路做同一件事，而兩條路遲早分家。
    ///
    /// ⚠ ⚠ **這條診斷的真正價值在「不是數字」那一半，而不是 `0`。**
    /// 在它之前，`compile.rs` 那一行是 `first.parse().ok()` —— `passive_fail 五` 會**靜默**
    /// 變成 `None`，而 `None` 的語義恰好是**整個特性關掉** ⇒ 一個手滑的值讓熔斷悄悄消失，
    /// 設定照過、裝載照過、沒有任何一行字提到它。★ 這正是本儲存庫反覆抓到的那一族。
    pub const BAD_PASSIVE_FAIL: DiagCode = DiagCode(44);
    /// 寫了 `passive_window` / `passive_cooldown`，卻**沒寫 `passive_fail`**（**M2**，G136）。
    ///
    /// 那兩個旋鈕此刻**一個都不生效**：不寫 `passive_fail` 就完全不熔斷（G136），
    /// 於是這兩行是一段**讀起來完全成立、而執行環境一步都不做**的設定。
    ///
    /// ★ warning 而不是 error，兩條理由：① 它不危險，只是沒用；
    /// ② 把它判紅會讓「先把旋鈕調好、待會兒再開」這種正當的分步操作裝不上。
    /// ⚠ 但**必須說出來** —— 與 [`METRICS_UNGUARDED`](Self::METRICS_UNGUARDED) 同一條紀律：
    /// 一條設定若與「正在生效」長得一模一樣，那麼沉默就是讓人以為它生效了。
    pub const PASSIVE_WITHOUT_THRESHOLD: DiagCode = DiagCode(45);
    /// `threads_l7` / `threads_l4` / `threads_admin` 的值不在 `[1, 1024]` 裡 ——
    /// 含 `0`、負數、帶單位、不是數字（**M3**，G140）。
    ///
    /// ★ ★ **`0` 不合法這一條比 `weight 0` 更硬**：pingora 拿這個數去建 tokio runtime，
    /// `0` 會在**啟動時 panic**。⇒ 不在這裡攔住，症狀是「改了一個數字，服務起不來」，
    /// 而回報錯誤出自 tokio 深處、**一個字都不提設定**。
    ///
    /// ⚠ ⚠ **和 [`BAD_PASSIVE_FAIL`](Self::BAD_PASSIVE_FAIL) 一樣，真正的價值在
    /// 「不是數字」那一半**：若這裡寫成 `first.parse().ok()`，`threads_l7 八` 會**靜默**
    /// 變成 `None`，而 `None` 的語義恰好是**回落到角色預設** ⇒ 一個手滑的值讓整條旋鈕
    /// 悄悄失效，設定照過、裝載照過、啟動紀錄裡那個數看起來也很正常。
    /// ★ 這是本儲存庫反覆抓到的那一族，⛔ 別在這裡退回 `.ok()`。
    pub const BAD_THREADS: DiagCode = DiagCode(46);

    pub fn as_str(&self) -> String {
        format!("FUL-DSL-{:04}", self.0)
    }
}

/// 嚴重級別。M1 只有 `Error` 與 `Warning` 兩檔。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// 一條診斷。
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: DiagCode,
    /// 標題行那句話。
    pub message: String,
    pub span: Span,
    /// caret 下面那幾個字（指著這一段到底哪裡不對）。
    pub label: String,
    /// `= help:` 行，通常是「你是不是想寫 X」。
    pub help: Option<String>,
    /// `= note:` 行，通常指向文件。
    pub note: Option<String>,
}

impl Diagnostic {
    pub fn error(code: DiagCode, span: Span, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code,
            message: message.into(),
            span,
            label: String::new(),
            help: None,
            note: None,
        }
    }

    pub fn warning(code: DiagCode, span: Span, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            ..Self::error(code, span, message)
        }
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// 繪製成 rustc 風格的一段文字。格式與 DSL 參考 §九的樣例逐字元對齊。
    pub fn render(&self, src: &Source) -> String {
        let loc = src.location(self.span.start);
        let end = src.location(self.span.end.max(self.span.start));
        let line_no = loc.line.to_string();
        let gutter = " ".repeat(line_no.len());
        let line_text = src.line_text(loc.line);

        // caret 的起點與長度按**顯示寬度**算，不是字元數——中文一個字佔兩列。
        // 用字元數會讓含中文的那一行 caret 整體左偏，而偏移量恰好等於中文字數。
        let before: String = line_text.chars().take(loc.col - 1).collect();
        let pad = display_width(&before);
        let span_chars = if end.line == loc.line {
            end.col.saturating_sub(loc.col)
        } else {
            line_text.chars().count().saturating_sub(loc.col - 1)
        };
        let highlighted: String = line_text
            .chars()
            .skip(loc.col - 1)
            .take(span_chars.max(1))
            .collect();
        let caret_len = display_width(&highlighted).max(1);

        let mut out = String::new();
        let _ = writeln!(
            out,
            "{}[{}]: {}",
            self.severity.label(),
            self.code.as_str(),
            self.message
        );
        let _ = writeln!(out, "{gutter}--> {}:{}:{}", src.path(), loc.line, loc.col);
        let _ = writeln!(out, "{gutter} |");
        let _ = writeln!(out, "{line_no} | {line_text}");
        let label_sep = if self.label.is_empty() { "" } else { " " };
        let _ = writeln!(
            out,
            "{gutter} | {}{}{label_sep}{}",
            " ".repeat(pad),
            "^".repeat(caret_len),
            self.label
        );
        if self.help.is_some() || self.note.is_some() {
            let _ = writeln!(out, "{gutter} |");
        }
        if let Some(h) = &self.help {
            let _ = writeln!(out, "{gutter} = help: {h}");
        }
        if let Some(n) = &self.note {
            let _ = writeln!(out, "{gutter} = note: {n}");
        }
        out
    }
}

/// 終端機上一段文字佔幾列。
///
/// ★ 只是夠用的近似：把常見的東亞全角區間算作 2 列，其餘算 1 列。
/// 它服務的是「caret 對不對得齊」這一件事，不追求 Unicode 寬度表的完備性。
/// ⚠ 組合字元（如聲調符號）會被多算——本專案的設定裡不會出現，出現了也只是偏一格。
pub fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

fn char_width(c: char) -> usize {
    let cp = c as u32;
    let wide = matches!(cp,
        0x1100..=0x115F        // 韓文字母
        | 0x2E80..=0x303E      // CJK 部首、假名標點
        | 0x3041..=0x33FF      // 平假名、片假名、注音、CJK 相容
        | 0x3400..=0x4DBF      // CJK 擴充 A
        | 0x4E00..=0x9FFF      // CJK 統一表意
        | 0xA000..=0xA4CF      // 彝文
        | 0xAC00..=0xD7A3      // 韓文音節
        | 0xF900..=0xFAFF      // CJK 相容表意
        | 0xFE30..=0xFE6F      // CJK 相容形式
        | 0xFF00..=0xFF60      // 全角 ASCII
        | 0xFFE0..=0xFFE6      // 全角符號
        | 0x1F300..=0x1F9FF    // emoji
        | 0x20000..=0x3FFFD    // CJK 擴充 B 及以後
    );
    if wide { 2 } else { 1 }
}

/// 一次編譯收集到的全部診斷。
///
/// ★ **一次報全**（G51）：呼叫方拿到的是一整份，不是遇到第一條就拋。
#[derive(Debug, Clone, Default)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, d: Diagnostic) {
        self.items.push(d);
    }

    pub fn items(&self) -> &[Diagnostic] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn has_errors(&self) -> bool {
        self.items.iter().any(|d| d.severity == Severity::Error)
    }

    pub fn error_count(&self) -> usize {
        self.items
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count()
    }

    /// 按原始碼位置排序後逐條繪製。
    ///
    /// ★ 排序是有意的：解析器與語義檢查是分兩遍跑的，不排序的話同一個檔案的診斷
    /// 會按「先全部語法錯、再全部語義錯」出現，讀的人要在檔案裡來回跳。
    pub fn render(&self, src: &Source) -> String {
        let mut sorted: Vec<&Diagnostic> = self.items.iter().collect();
        sorted.sort_by_key(|d| (d.span.start, d.code));
        let mut out = String::new();
        for d in sorted {
            out.push_str(&d.render(src));
            out.push('\n');
        }
        out
    }
}

/// 編輯距離，給「你是不是想寫 X」用。
///
/// ★ 固定一組指令名 + 編輯距離，正是 G61 選「小而固定的佔位符集合」時點名想要的
/// 那個好處：候選集是有限的，才提得出建議。
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// 在候選裡找一個「像」的。找不到回傳 `None`。
///
/// 閾值按長度放寬：短詞允許 1 處不同，長詞允許 2 處。全都不像時**不給建議**——
/// 一個亂指的 help 比沒有 help 更耽誤人。
pub fn suggest<'a>(input: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let limit = if input.chars().count() <= 4 { 1 } else { 2 };
    candidates
        .into_iter()
        .map(|c| (levenshtein(input, c), c))
        .filter(|(d, _)| *d <= limit)
        .min_by_key(|(d, c)| (*d, c.len()))
        .map(|(_, c)| c)
}
