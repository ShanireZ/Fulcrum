//! 指令表與**執行順序表**。
//!
//! ★ ★ ★ **這一頁是 G49 的承重牆。** 站台塊內的指令按內建順序表執行、不按書寫順序
//! （照 Caddy），換來「使用者隨便寫也能跑對」與「Caddyfile 遷移零意外」。代價是
//! **書寫順序 ≠ 執行順序**，而 `PLAN.md` §3 恰恰點名批評過 nginx 的 `location`
//! 匹配順序反直覺。四條配套約束裡最要緊的一條是：
//!
//! > **新增指令必須在表裡有位置，否則編譯不過。**
//!
//! 因為「不在表裡就排最後」會讓新指令**悄悄跑在錯誤的位置上**——設定能裝載、
//! 請求能通、紀錄一行不報。這與本儲存庫反覆抓到的「宣告了卻沒人接」完全同形。
//!
//! # 這條約束在這裡是怎麼完成實作的
//!
//! 分成兩半，**兩半都要有，缺一半就只擋住了一種走樣**：
//!
//! 1. **進不了表就不存在。** [`ChainDirective`] 的每個變體都由下面 `chain_directives!`
//!    的一行生成，而那一行的**第一個欄位就是序號**。想加一條鏈上指令，就必須同時給它
//!    一個位置——不是「忘了會怎樣」，是**沒有「忘了」這個選項**。
//! 2. **有位置不等於有人接。** [`crate::compile`] 裡對 `ChainDirective` 的 `match`
//!    **沒有 `_` 備援臂**。加了變體卻沒教編譯器拿它怎麼辦，那個 `match` 當場編不過。
//!    ★ 這一半是**可以被證紅**的：加一個變體、`cargo build`，就能看見它紅。
//!
//! ⚠ 順序表同時是**公開契約**，與 [DSL 指令集參考](../../../docs/architecture/dsl-reference.md)
//! §三同級維護。`tests/doc_contract.rs` 把兩邊逐行比對——程式碼改了文件沒改，或者反過來，
//! 都會紅。

use crate::diag::{Span, suggest};

/// 指令在執行鏈上的類別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 中介軟體：按序依次執行，可疊加。
    Middleware,
    /// 終結類：**第一個匹配即停**，其後的終結指令不再考慮。
    Terminal,
    /// 容器：本身不產生行為，決定塊內那些指令怎麼跑。
    Container,
}

impl Kind {
    /// 文件 §三「類別」列印的字。★ 契約的一部分，`tests/doc_contract.rs` 逐字比對。
    pub const fn doc_label(self) -> &'static str {
        match self {
            Kind::Middleware => "中间件",
            Kind::Terminal => "终结",
            Kind::Container => "容器",
        }
    }
}

/// 這條指令由誰承擔。
///
/// ⚠ ⚠ **（M2 批 G）起這個枚舉只剩三檔**：G47 那個「隱式回落」
/// 已經沒有物件了 —— 回落層整塊刪除（G98），詳見 `Reserved` 上那條註解。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    /// M1 自研。
    SelfBuilt,
    /// M2 自研（G92）。
    ///
    /// ★ 單獨一檔而不是重複使用 `SelfBuilt`，是因為 `doc_label()` 是**逐字契約**：
    /// 拿「M1 自研」去印一件 M2 才做出來的事，文件表裡就多了一句假話，
    /// 而那張表恰恰是本儲存庫用來讀進度的地方。`cache` 做 D8 時重複使用這一檔。
    SelfBuiltM2,
    /// 語法上認得、M1 不產生任何行為（`tracing` 就是這一檔）。
    ///
    /// ⚠ ⚠ **`FallbackNginx` / `FallbackCaddy` 兩檔（M2 批 G）刪除**（G98）：
    /// §6.3 那個過渡回落層的使用者逐塊歸零 —— `l4` 在批 B 拆掉、`file_server` 在批 F
    /// 改自研、`cache` 在批 G 改自研 ⇒ **一個都不剩**，整層隨之刪除。
    /// ★ 於是 `fallback_engine()` 這個函式也一併沒了：一個所有輸入都回傳 `None`
    /// 的函式，與一段死程式碼的區別只在於它讀起來像還有用。
    Reserved,
}

impl Owner {
    /// 文件 §三「歸屬」列印的字。★ 契約的一部分，`tests/doc_contract.rs` 逐字比對。
    ///
    /// ⚠ 列頭由「M1 歸屬」改成「歸屬」（G92）：一列既然要裝
    /// 「M2 自研」，它的名字就不能再自稱 M1。
    pub const fn doc_label(self) -> &'static str {
        match self {
            Owner::SelfBuilt => "M1 自研",
            Owner::SelfBuiltM2 => "M2 自研",
            Owner::Reserved => "预留",
        }
    }
}

/// 生成**鏈上指令**的枚舉與順序表。
///
/// 每一行的形狀是：`<序号> <变体> <DSL 名> <类别> <M1 归属>`。
/// ★ 序號寫在最前面不是排版偏好——它是這條約束的執行方式：
///   **一行少了序號就編不過，於是「加了指令卻沒給位置」這件事無從發生。**
macro_rules! chain_directives {
    (
        $( $order:literal $variant:ident $name:literal $kind:ident $owner:ident ),* $(,)?
    ) => {
        /// 站台塊的執行鏈上可以出現的指令。
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        pub enum ChainDirective {
            $( #[doc = $name] $variant ),*
        }

        impl ChainDirective {
            /// 全表，**按宣告順序**（也就是按序號遞增）。
            pub const ALL: &'static [ChainDirective] = &[ $( ChainDirective::$variant ),* ];

            /// 執行順序表裡的序號。★ 這個 `match` 由宏生成、沒有 `_` 臂。
            pub const fn order(self) -> u16 {
                match self { $( ChainDirective::$variant => $order ),* }
            }

            /// DSL 裡寫出來的名字。
            pub const fn name(self) -> &'static str {
                match self { $( ChainDirective::$variant => $name ),* }
            }

            pub const fn kind(self) -> Kind {
                match self { $( ChainDirective::$variant => Kind::$kind ),* }
            }

            pub const fn owner(self) -> Owner {
                match self { $( ChainDirective::$variant => Owner::$owner ),* }
            }

            pub fn from_name(name: &str) -> Option<ChainDirective> {
                match name { $( $name => Some(ChainDirective::$variant), )* _ => None }
            }
        }
    };
}

chain_directives! {
    10 Tracing      "tracing"       Middleware Reserved,
    20 Header       "header"        Middleware SelfBuilt,
    30 Rewrite      "rewrite"       Middleware SelfBuilt,
    40 Encode       "encode"        Middleware SelfBuiltM2,
    50 Cache        "cache"         Middleware SelfBuiltM2,
    55 Handle       "handle"        Container  SelfBuilt,
    56 Route        "route"         Container  SelfBuilt,
    60 Redir        "redir"         Terminal   SelfBuilt,
    70 Respond      "respond"       Terminal   SelfBuilt,
    75 Metrics      "metrics"       Terminal   SelfBuiltM2,
    80 ReverseProxy "reverse_proxy" Terminal   SelfBuilt,
    90 FileServer   "file_server"   Terminal   SelfBuiltM2,
}

/// 生成**站台級指令**的枚舉。
///
/// ★ 它們**不在執行鏈上**，所以沒有序號——`tls` 與 `log` 是站台的屬性，
/// `handle_errors` 是錯誤處理器。給它們編一個假的序號，只會讓順序表這份契約變髒。
macro_rules! site_directives {
    ( $( $variant:ident $name:literal $owner:ident ),* $(,)? ) => {
        /// 只能出現在站台塊頂層的指令。
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        pub enum SiteDirective {
            $( #[doc = $name] $variant ),*
        }

        impl SiteDirective {
            pub const ALL: &'static [SiteDirective] = &[ $( SiteDirective::$variant ),* ];

            pub const fn name(self) -> &'static str {
                match self { $( SiteDirective::$variant => $name ),* }
            }

            pub const fn owner(self) -> Owner {
                match self { $( SiteDirective::$variant => Owner::$owner ),* }
            }

            pub fn from_name(name: &str) -> Option<SiteDirective> {
                match name { $( $name => Some(SiteDirective::$variant), )* _ => None }
            }
        }
    };
}

site_directives! {
    Tls          "tls"           SelfBuilt,
    Log          "log"           SelfBuilt,
    HandleErrors "handle_errors" SelfBuilt,
}

/// 一條指令的身分：要麼在鏈上，要麼是站台級。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Directive {
    Chain(ChainDirective),
    Site(SiteDirective),
}

impl Directive {
    pub fn from_name(name: &str) -> Option<Directive> {
        ChainDirective::from_name(name)
            .map(Directive::Chain)
            .or_else(|| SiteDirective::from_name(name).map(Directive::Site))
    }

    pub const fn name(self) -> &'static str {
        match self {
            Directive::Chain(d) => d.name(),
            Directive::Site(d) => d.name(),
        }
    }

    pub const fn owner(self) -> Owner {
        match self {
            Directive::Chain(d) => d.owner(),
            Directive::Site(d) => d.owner(),
        }
    }
}

/// 全部指令名（鏈上 + 站台級），給「你是不是想寫 X」用。
pub fn all_names() -> Vec<&'static str> {
    ChainDirective::ALL
        .iter()
        .map(|d| d.name())
        .chain(SiteDirective::ALL.iter().map(|d| d.name()))
        .collect()
}

/// 未知指令時給一個建議。找不到像的就不給——亂指的 help 比沒有 help 更耽誤人。
pub fn suggest_directive(input: &str) -> Option<&'static str> {
    suggest(input, all_names())
}

// ── 子指令 ──────────────────────────────────────────────────────────────────

/// 參數的字面值類型（DSL 參考 §七）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgType {
    /// 任意裸詞或引號串。
    Word,
    /// `5s` `1m` `500ms`。★ **裸數字是錯誤**，不做預設單位。
    Duration,
    /// `10MB` `1GiB`。
    Size,
    /// ★ **只認 `true` / `false`**，不接受 `yes/no/on/off`。
    Bool,
    /// HTTP 狀態碼，或 `2xx` 這類族。
    StatusPattern,
    /// 固定取值集合。
    Enum(&'static [&'static str]),
}

/// 一條子指令的規格。
#[derive(Debug, Clone, Copy)]
pub struct SubSpec {
    pub name: &'static str,
    pub min_args: usize,
    pub max_args: usize,
    pub arg_type: ArgType,
}

const fn sub(name: &'static str, min_args: usize, max_args: usize, arg_type: ArgType) -> SubSpec {
    SubSpec {
        name,
        min_args,
        max_args,
        arg_type,
    }
}

/// `reverse_proxy` 的子塊（DSL 參考 §4.1）。
pub const REVERSE_PROXY_SUBS: &[SubSpec] = &[
    sub(
        "lb_policy",
        1,
        1,
        ArgType::Enum(&["round_robin", "least_conn", "ip_hash", "random"]),
    ),
    sub("health_uri", 1, 1, ArgType::Word),
    sub("health_interval", 1, 1, ArgType::Duration),
    sub("health_timeout", 1, 1, ArgType::Duration),
    sub("health_status", 1, 1, ArgType::StatusPattern),
    // ★ 這一條直接消滅 nginx OSS 那個經典事故源：上游網域只在啟動時解析一次。
    sub("dns_refresh", 1, 1, ArgType::Duration),
    // ⚠ `passive_fail` 的 `arg_type` 是 `Word` 而不是某種「整數」：`check_sub_args` 只做
    //   粗粒度的形狀檢查，取值域（`[1, 65535]`，含「不是數字」）在 `compile.rs` 裡判，
    //   報 `FUL-DSL-0044`。★ 與 `weight` 那一條同一處置。
    sub("passive_fail", 1, 1, ArgType::Word),
    sub("passive_window", 1, 1, ArgType::Duration),
    // ★ ★ G136 新增：熔斷之後歇多久再放**一個**半開探針。
    //   ⚠ 它與 `passive_window` 有意是兩個旋鈕，⛔ 不是 nginx `fail_timeout` 那樣一格兩用 ——
    //   視窗問「失敗要多密才算一個模式」，冷卻問「給上游多久緩過來」。
    sub("passive_cooldown", 1, 1, ArgType::Duration),
    // ★ ★ **M2 批 N**：`weight <上游地址> <正整数>`，可寫多行，沒寫的上游權重是 1。
    //   ⚠ 取「位址 + 值」而不是 Caddy 那種位置式（`lb_policy weighted_round_robin 3 1`）：
    //   位置式一改位址的書寫順序就**靜默換了權重**，而設定裡一個字都看不出問題。
    //   ⚠ `arg_type` 是 `Word` 而不是某種「整數」：`check_sub_args` 只給**第一個**參數
    //   套類型，而第一個參數是位址。權重那一格的值域檢查在 `compile.rs` 裡做 ——
    //   它要說的話（值域、`0` 為什麼不合法、摘節點該寫什麼）遠超一句類型錯誤。
    sub("weight", 2, 2, ArgType::Word),
    // ★ ★ **M2 批 N**（裁決 R6 ⇒ G125）：給這一條 `reverse_proxy` 一個**穩定 id**，
    //   管理面的臨時覆蓋層用 `(站点名, id, 归一化后的上游地址)` 尋址。**選填**。
    //   ⚠ `arg_type` 是 `Word`：值域檢查（不許是空串）在 `compile.rs` 裡做 ——
    //   它要說的話（空串與「沒寫」在鍵空間裡同形）遠超一句類型錯誤，
    //   與上面 `weight` 把值域留給 `compile.rs` 是同一條理由。
    //   ⛔ **不自動派生**：內容雜湊會讓「加一臺機器」把剛摘掉的壞節點的覆蓋頂懸空，
    //   站台內序號會讓「換一下書寫順序」靜默改掉尋址 —— 兩條 owner 都排除過。
    sub("id", 1, 1, ArgType::Word),
    sub("header_up", 1, 2, ArgType::Word),
    sub("header_down", 1, 2, ArgType::Word),
    sub("transport", 1, 1, ArgType::Enum(&["http", "https"])),
    sub("tls_insecure_skip_verify", 0, 1, ArgType::Bool),
    // ★ ★ **M2 批 D**：給上游發一個 PROXY 頭。參數可省，省了就是 **v2**（owner 拍板）。
    //   ⚠ 用 `Enum` 而不是 `Word`：版本寫錯了要在**編譯期**red，
    //   而不是在執行環境發出一個上游讀不懂的頭 —— 後者的現場表現是「上游握手失敗」。
    sub(
        "proxy_protocol",
        0,
        1,
        ArgType::Enum(crate::model::PROXY_PROTOCOL_VERSIONS),
    ),
];

/// `tls` 的子塊（DSL 參考 §4.4）。
pub const TLS_SUBS: &[SubSpec] = &[
    sub("on_demand", 0, 1, ArgType::Bool),
    sub("ask", 1, 1, ArgType::Word),
    sub("dns", 1, 2, ArgType::Word),
    sub("resolvers", 1, usize::MAX, ArgType::Word),
    // ★ G59 第 3 條：這份 DNS 憑證被**宣告**覆蓋哪些 zone。
    //   原生供應商（cloudflare / dnspod）必填——把「憑證能幹什麼」變成設定裡可讀的事實。
    sub("zones", 1, usize::MAX, ArgType::Word),
];

/// `log` 的子塊。✅ 欄位清單與格式由 **G113 + G114** 定稿（D7 結案）。
pub const LOG_SUBS: &[SubSpec] = &[
    sub("output", 1, 2, ArgType::Word),
    sub(
        "level",
        1,
        1,
        ArgType::Enum(&["debug", "info", "warn", "error"]),
    ),
    // ★ 白名單頭（**M2 批 L 第 ③ 步**）。⚠ 預設**一個頭都不記**，所以
    //   `min_args = 1`：寫了 `headers` 卻不給名字是個錯，不是「記零個」——
    //   後者用「別寫這一行」表達就夠了，而前者多半是漏了參數。
    sub("headers", 1, usize::MAX, ArgType::Word),
    sub("resp_headers", 1, usize::MAX, ArgType::Word),
];

/// 任何白名單裡都不許出現的頭名（**G114 那一半沒被推翻的理由**）。
///
/// ★ 小寫形態，比對時把使用者寫的也壓成小寫 —— HTTP 頭名本來就大小寫不敏感，
/// 而一條「寫 `Authorization` 會紅、寫 `authorization` 不會」的規則等於沒有規則。
pub const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    "set-cookie",
    "proxy-authorization",
];

/// `cache` 的子塊（M2 自研，M1 回落 nginx）。
pub const CACHE_SUBS: &[SubSpec] = &[
    // ⚠ ⚠ **`ttl` 是「備援」不是「覆蓋」**（G96）：只有上游**沒給**
    //   新鮮度資訊（`Cache-Control: max-age` / `s-maxage` / `Expires`）時才用它。
    //   ★ 取備援而不是覆蓋的理由很具體：覆蓋語義會讓一個帶 `no-store` 的回應
    //   也被存下來 —— 而那正是「偶爾給錯內容」裡最貴的一種（給錯人看到別人的頁面）。
    //   ★ 代價認下：從 nginx `proxy_cache_valid` 遷過來的人會發現它不覆蓋。
    sub("ttl", 1, 1, ArgType::Duration),
    // **單條目**大小上限：超過它的回應**不進快取**（照發給用戶端）。
    sub("max_size", 1, 1, ArgType::Size),
    // ★ M2 批 G 新增：**整個快取**的容量上限，達到就按 LRU 淘汰。
    //   ⚠ 它是必需的而不是可選的 —— 一個沒有上限的記憶體快取就是一處記憶體洩漏，
    //   而那種洩漏的現場是「跑幾天之後被 OOM 殺掉」，與快取本身看不出關係。
    sub("capacity", 1, 1, ArgType::Size),
    // ★ M2 批 H 新增：**磁碟後端**的根目錄。寫了它 ⇒ 快取寫入磁碟；不寫 ⇒ 記憶體後端。
    //
    // ⚠ ⚠ **名字取 `disk` 而不是 `path`**：`path` 在本 DSL 裡已經是**請求路徑**的意思
    //   （匹配器與 `{path}` 佔位符都用它），而一條子指令與一個佔位符同名、含義卻
    //   完全不同，是一處**讀起來毫無異常**的誤解來源。
    // ⚠ ⚠ **快取後端是處理程序級的**（與 `capacity` 同理，見 `serve()` 裡那段註解）：
    //   多個 `cache` 塊寫了**不同**的 `disk` 是編譯期錯誤（`FUL-DSL-0035`），
    //   ★ 而不是像 `capacity` 那樣悄悄取一個 —— 一個被悄悄忽略的目錄，
    //   現場是「我的快取怎麼沒落到那兒」，而設定裡一個字都看不出問題。
    sub("disk", 1, 1, ArgType::Word),
];

/// `cache { capacity … }` 沒寫時的預設整體容量。
///
/// ★ 有預設值而不是必填：`cache` 這條指令在 M1 就存在了，突然變成必填會讓
/// 既有設定編不過。⚠ 而預設值**必須在裝載紀錄裡說出來**（與 G88 的 hide 清單同一條紀律）。
pub const CACHE_DEFAULT_CAPACITY_BYTES: u64 = 256 * 1024 * 1024;

/// `cache { max_size … }` 沒寫時的單條目上限。
pub const CACHE_DEFAULT_MAX_SIZE_BYTES: u64 = 8 * 1024 * 1024;

/// `file_server` 的子塊（M2 自研，M1 回落 nginx）。
pub const FILE_SERVER_SUBS: &[SubSpec] = &[
    sub("root", 1, 1, ArgType::Word),
    sub("index", 1, usize::MAX, ArgType::Word),
    // ── M2 批 F（G87 / G88）───────────────────────────────────────────────
    // ⚠ 兩條布爾都用 `ArgType::Bool`，也就是**只認 `true`/`false`**，
    //   明確拒絕 `yes/no/on/off`（G87 拍板，`tls_insecure_skip_verify` 同款）。
    //   代價認下：從 nginx 的 `disable_symlinks off` 遷過來的人會寫 `off`，
    //   由 `check_arg_type` 那句「布爾只認 true 與 false」接住。
    sub("follow_symlinks", 1, 1, ArgType::Bool),
    sub("hide", 1, usize::MAX, ArgType::Word),
    sub("hide_defaults", 1, 1, ArgType::Bool),
    // ★ M2 批 I（G99）：**預壓縮旁檔案**。寫了 `precompressed br gzip` 之後，
    //   發 `/x.css` 時先看有沒有 `/x.css.br`（用戶端認 br 的話），有就直接發它。
    //   ⚠ ⚠ 取值與 `encode` 用**同一張表**（gzip / zstd / br）——
    //   兩處各寫一張的話，`encode` 加了第四種而這裡沒加，現場是
    //   「設定裡寫得下、就是不生效」，而不會有任何回報錯誤。
    sub(
        "precompressed",
        1,
        usize::MAX,
        ArgType::Enum(&["gzip", "zstd", "br"]),
    ),
];

/// `hide` 的預設清單（G88）。**按路徑段**匹配，命中回 **404 不是 403**。
///
/// ★ 回 403 等於確認「這個檔案在」——那是一次資訊洩漏。
/// ⚠ 它非空，所以裝載時必須把生效的清單打出來：**一個不說出來的非空預設就是一次靜默行為**
/// （批 20 為「靜默失能」專門堵過一次）。
pub const HIDE_DEFAULTS: &[&str] = &[".git", ".env", ".svn", ".hg", ".bzr", ".DS_Store"];

/// 某條鏈上指令允許的子塊規格。回傳 `None` 表示它不接受子塊。
pub const fn subs_of(d: ChainDirective) -> Option<&'static [SubSpec]> {
    match d {
        ChainDirective::ReverseProxy => Some(REVERSE_PROXY_SUBS),
        ChainDirective::Cache => Some(CACHE_SUBS),
        ChainDirective::FileServer => Some(FILE_SERVER_SUBS),
        // 容器的子塊裝的是別的指令，不是子指令；由 compile 單獨處理。
        ChainDirective::Handle | ChainDirective::Route => None,
        ChainDirective::Tracing
        | ChainDirective::Header
        | ChainDirective::Rewrite
        | ChainDirective::Encode
        | ChainDirective::Redir
        | ChainDirective::Respond
        // ★ `metrics` 一個子指令都沒有，也**不打算**有：抓取端要什麼由抓取端決定，
        //   而「在設定裡挑要暴露哪幾個族」會讓兩次抓取的欄位集由設定說了算 ——
        //   ⚠ 那種缺口的現場是「Grafana 上這條線斷了」，而服務本身一切正常。
        | ChainDirective::Metrics => None,
    }
}

/// 在一張子指令表裡查名字，並在查不到時給建議。
pub fn lookup_sub<'a>(
    table: &'a [SubSpec],
    name: &str,
) -> Result<&'a SubSpec, Option<&'static str>> {
    match table.iter().find(|s| s.name == name) {
        Some(s) => Ok(s),
        None => Err(suggest(name, table.iter().map(|s| s.name))),
    }
}

// ── 全域選項塊 ──────────────────────────────────────────────────────────────

/// 全域選項塊裡允許的鍵（DSL 參考 §一）。
pub const GLOBAL_OPTIONS: &[SubSpec] = &[
    sub("acme_email", 1, 1, ArgType::Word),
    sub("acme_ca", 1, 1, ArgType::Word),
    sub("admin", 1, 1, ArgType::Word),
    sub("default_sni", 1, 1, ArgType::Word),
    // ★ 自動把 HTTP 重定向到 HTTPS（G12 的後半句）。**預設開**，寫 `false` 關掉。
    //   ⚠ 是 `false` 不是 `off` —— DSL 的布爾只認 true/false（§七），
    //   這句話我第一次就寫錯了，而是編譯器自己的診斷把我攔住的。
    // ⚠ 它合成的是**真的站台塊**，`fulcrum compile` 與 `plan` 都看得見 ——
    //   而不是資料面裡一條看不見的特判。理由見 compile.rs 裡那段。
    sub("auto_http_redirect", 1, 1, ArgType::Bool),
    sub("grace_period", 1, 1, ArgType::Duration),
    // ★ ★ 執行緒數按**角色**分三格（G35 結案、G140 完成實作），⛔ **有意沒有一個籠統的
    //   全域 `threads`**：pingora 的執行緒不跨 service 共享（每個 service 各起一套
    //   runtime）⇒ 總執行緒數 ≈ Σ(各 service)，一個全域值設成核數會直接超訂。
    //   推導與初值在 docs/architecture/process-model.md 的「執行緒模型」一節。
    // ⚠ 用 `ArgType::Word` 而不是某個「整數」類型：本表今天沒有整數型（`weight`
    //   與 `passive_fail` 都走 Word + compile.rs 裡一條專門的診斷），
    //   ⇒ 照既有形狀走，值域判在 compile.rs，診斷是 FUL-DSL-0046。
    sub("threads_l7", 1, 1, ArgType::Word),
    sub("threads_l4", 1, 1, ArgType::Word),
    sub("threads_admin", 1, 1, ArgType::Word),
    // ★ ★ 回落後端在哪（§6.3 / G34）。**樞衡不拉起、不監管、不讀它們的設定**，
    //   所以它只需要知道一個位址。⚠ 這是「邊界顯式」那條約束（回落層第 2 條）的落法：
    //   哪些請求走回落必須在設定裡看得見，而**看得見的第一步是這一跳寫在設定裡**。
    // ⚠ ⚠ **`fallback_nginx` / `fallback_caddy` （M2 批 G）從這張表裡刪除**
    //   （G98）。★ 它們不是被靜默丟棄的：`compile.rs` 有一條**專門的診斷**告訴寫了它們
    //   的人這一層去哪了 —— 一個被刪掉的公開設定面，只回「不認識的全域選項」是不夠的。
    sub("proxy_protocol_from", 1, usize::MAX, ArgType::Word),
];

// ── 字面值檢核 ──────────────────────────────────────────────────────────────

/// 時長解析。★ **裸數字一律是錯誤**（DSL 參考 §七）。
pub fn parse_duration_ms(s: &str) -> Option<u64> {
    let (num, unit) = split_number_unit(s)?;
    if unit.is_empty() {
        return None; // 裸數字：不猜單位
    }
    let mult = match unit {
        "ms" => 1u64,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return None,
    };
    num.parse::<u64>().ok()?.checked_mul(mult)
}

/// 大小解析。十進位與二進位前綴都認，含義按 IEC。
pub fn parse_size_bytes(s: &str) -> Option<u64> {
    let (num, unit) = split_number_unit(s)?;
    let mult: u64 = match unit {
        "" | "B" => 1,
        "KB" => 1_000,
        "MB" => 1_000_000,
        "GB" => 1_000_000_000,
        "TB" => 1_000_000_000_000,
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        "TiB" => 1u64 << 40,
        _ => return None,
    };
    num.parse::<u64>().ok()?.checked_mul(mult)
}

/// 布爾。★ 只認這兩個，避開 YAML `no → false` 那類坑。
pub fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// 狀態碼或狀態碼族（`200` / `2xx`）。
pub fn valid_status_pattern(s: &str) -> bool {
    if let Some(prefix) = s.strip_suffix("xx") {
        return matches!(prefix, "1" | "2" | "3" | "4" | "5");
    }
    matches!(s.parse::<u16>(), Ok(n) if (100..=599).contains(&n))
}

/// 切成「數字部分 + 單位部分」。全是數字時單位是空串。
///
/// ★ **不在這裡判「沒有單位算不算錯」**——那是兩種類型各自的事：
/// 大小的裸數字是位元組（合法），時長的裸數字是錯誤（不猜單位）。
/// ⚠ 對全數字串直接回傳 `None` 是錯的：`max_size 512` 會被判成非法，
/// 而**時長那一側看起來完全正常**——同一個函式服務兩種類型時，
/// 把其中一種的規則寫進公共部分，另一種就會安靜地跟著變。
fn split_number_unit(s: &str) -> Option<(&str, &str)> {
    let idx = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if idx == 0 {
        return None; // 沒有數字部分
    }
    Some((&s[..idx], &s[idx..]))
}

/// 檢核一個參數是否符合它宣告的類型。出錯時回傳一句人話。
pub fn check_arg_type(ty: ArgType, value: &str) -> Result<(), (crate::diag::DiagCode, String)> {
    use crate::diag::DiagCode;
    match ty {
        ArgType::Word => Ok(()),
        ArgType::Duration => {
            if parse_duration_ms(value).is_some() {
                Ok(())
            } else if value.chars().all(|c| c.is_ascii_digit()) && !value.is_empty() {
                Err((
                    DiagCode::BAD_DURATION,
                    format!("时长必须带单位，写成 `{value}s` 或 `{value}ms`"),
                ))
            } else {
                Err((
                    DiagCode::BAD_DURATION,
                    "时长写法是 `500ms` `5s` `1m` `2h` `1d`".to_string(),
                ))
            }
        }
        ArgType::Size => {
            if parse_size_bytes(value).is_some() {
                Ok(())
            } else {
                Err((
                    DiagCode::BAD_SIZE,
                    "大小写法是 `10MB` `1GiB`（十进制与二进制前缀都认）".to_string(),
                ))
            }
        }
        ArgType::Bool => {
            if parse_bool(value).is_some() {
                Ok(())
            } else {
                Err((
                    DiagCode::BAD_BOOL,
                    "布尔只认 `true` 与 `false`（不接受 yes/no/on/off）".to_string(),
                ))
            }
        }
        ArgType::StatusPattern => {
            if valid_status_pattern(value) {
                Ok(())
            } else {
                Err((
                    DiagCode::BAD_STATUS,
                    "写一个状态码（`200`）或一族（`2xx`）".to_string(),
                ))
            }
        }
        ArgType::Enum(allowed) => {
            if allowed.contains(&value) {
                Ok(())
            } else {
                let mut msg = format!("只能是 {}", allowed.join(" / "));
                if let Some(s) = suggest(value, allowed.iter().copied()) {
                    msg.push_str(&format!("；你是不是想写 `{s}`？"));
                }
                Err((DiagCode::BAD_ENUM, msg))
            }
        }
    }
}

/// 一條子指令連同它出現的位置，供 compile 階段回報錯誤用。
#[derive(Debug, Clone)]
pub struct SubUse {
    pub name: String,
    pub span: Span,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 顺序表严格递增且名字唯一() {
        let mut last = 0u16;
        let mut names = std::collections::BTreeSet::new();
        for d in ChainDirective::ALL {
            assert!(
                d.order() > last,
                "{} 的序号 {} 没有严格大于前一条的 {last}",
                d.name(),
                d.order()
            );
            last = d.order();
            assert!(names.insert(d.name()), "指令名 {} 重复", d.name());
        }
        for d in SiteDirective::ALL {
            assert!(names.insert(d.name()), "指令名 {} 重复", d.name());
        }
    }

    #[test]
    fn 中间件全部排在容器与终结之前() {
        // 順序表的形狀本身就是一條契約：中介軟體 → 容器 → 終結。
        // 它爛掉的方式是「某天插了一條中介軟體在終結後面」，而那不會有任何症狀。
        let first_container = ChainDirective::ALL
            .iter()
            .find(|d| d.kind() == Kind::Container)
            .map(|d| d.order())
            .expect("表里应当有容器");
        let first_terminal = ChainDirective::ALL
            .iter()
            .find(|d| d.kind() == Kind::Terminal)
            .map(|d| d.order())
            .expect("表里应当有终结类");
        assert!(first_container < first_terminal);
        for d in ChainDirective::ALL {
            match d.kind() {
                Kind::Middleware => assert!(d.order() < first_container, "{}", d.name()),
                Kind::Container => assert!(d.order() < first_terminal, "{}", d.name()),
                Kind::Terminal => assert!(d.order() >= first_terminal, "{}", d.name()),
            }
        }
    }

    #[test]
    fn 容器必须排在终结类之前否则文档首页的例子就是错的() {
        // DSL 參考 §一的範例是：
        //     handle @internal { reverse_proxy … }
        //     respond 403
        // 只有 `handle` 跑在 `respond` **之前**，那個「備援 403」才是備援；
        // 否則 403 恆勝，handle 塊永遠進不去——而設定能裝載、請求能通、紀錄一行不報。
        assert!(ChainDirective::Handle.order() < ChainDirective::Respond.order());
        assert!(ChainDirective::Route.order() < ChainDirective::Respond.order());
    }

    /// `metrics` 的三項身分（**M2 批 M**，G116）。
    ///
    /// ⚠ 上面那兩條自測（序號嚴格遞增、終結類全排在中介軟體與容器之後）**已經自動
    /// 覆蓋了新加的這一行**，所以這裡只釘它自己的三項：認得出、序號 75、終結類。
    /// ★ 序號 75 是契約的一部分（它必須夾在 `respond` 70 與 `reverse_proxy` 80 之間）
    /// —— 寫死 70 < 75 < 80 而不是只寫 `== 75`，是因為**光有等號看不出它排在哪**：
    /// 有朝一日 `respond` 挪到 76，等號照樣綠，而那時 `metrics` 已經跑在它前面了。
    #[test]
    fn metrics是序号75的终结指令() {
        assert_eq!(
            ChainDirective::from_name("metrics"),
            Some(ChainDirective::Metrics)
        );
        assert_eq!(ChainDirective::Metrics.order(), 75);
        assert_eq!(ChainDirective::Metrics.kind(), Kind::Terminal);
        assert_eq!(ChainDirective::Metrics.owner(), Owner::SelfBuiltM2);
        assert!(ChainDirective::Respond.order() < ChainDirective::Metrics.order());
        assert!(ChainDirective::Metrics.order() < ChainDirective::ReverseProxy.order());
        // 它不收子塊 —— 與 `respond` / `redir` 同一檔。
        assert!(subs_of(ChainDirective::Metrics).is_none());
    }

    #[test]
    fn 时长不接受裸数字() {
        assert_eq!(parse_duration_ms("5s"), Some(5_000));
        assert_eq!(parse_duration_ms("500ms"), Some(500));
        assert_eq!(parse_duration_ms("2h"), Some(7_200_000));
        assert_eq!(parse_duration_ms("5"), None, "裸数字必须被拒");
        assert_eq!(parse_duration_ms("s"), None);
        assert_eq!(parse_duration_ms("5x"), None);
    }

    #[test]
    fn 布尔只认两个词() {
        assert_eq!(parse_bool("true"), Some(true));
        assert_eq!(parse_bool("false"), Some(false));
        for w in ["yes", "no", "on", "off", "True", "1", "0"] {
            assert_eq!(parse_bool(w), None, "{w} 不该被接受");
        }
    }

    #[test]
    fn 大小两种前缀都认() {
        assert_eq!(parse_size_bytes("10MB"), Some(10_000_000));
        assert_eq!(parse_size_bytes("1GiB"), Some(1 << 30));
        assert_eq!(parse_size_bytes("512"), Some(512));
        assert_eq!(parse_size_bytes("10Mb"), None);
    }

    #[test]
    fn 状态码族() {
        assert!(valid_status_pattern("200"));
        assert!(valid_status_pattern("2xx"));
        assert!(!valid_status_pattern("6xx"));
        assert!(!valid_status_pattern("99"));
        assert!(!valid_status_pattern("xx"));
    }

    #[test]
    fn 建议只在真的像的时候才给() {
        assert_eq!(suggest_directive("reverse-proxy"), Some("reverse_proxy"));
        assert_eq!(suggest_directive("headr"), Some("header"));
        // ★ 反向：完全不像的不給建議。亂指的 help 會把人帶到更遠的地方。
        assert_eq!(suggest_directive("完全不相干的东西"), None);
        assert_eq!(suggest_directive("xyzzy"), None);
    }

    #[test]
    fn 回落归属与文档一致() {
        // ⚠ ⚠ **這條測試換了它在驗什麼，兩版契約都留著：**
        //   · M1–批 E：`file_server` 與 `cache` 都回落給 nginx；
        //   · 批 F：`file_server` 改自研 ⇒ 反著釘「它必須沒有回落引擎」；
        //   · **批 G（現行）**：`cache` 也改自研 ⇒ **整個回落層刪除**，
        //     連 `fallback_engine()` 這個函式都沒了。
        //   ★ 於是它現在驗的是「歸屬表裡再沒有回落那一檔」——
        //   ⚠ 一條「加回回落」的改動會讓下面這個 `match` 編不過，那就是新的判據。
        for d in ChainDirective::ALL {
            let label = d.owner().doc_label();
            assert!(
                !label.contains("回落"),
                "`{}` 的归属是「{label}」—— 回落层已于批 G 删除",
                d.name()
            );
        }
    }

    /// ★ 三檔的 `doc_label()` 互不相同 —— 它是文件表那一列的**逐字契約**，
    /// 兩檔撞名等於兩種歸屬在文件裡長得一模一樣。
    #[test]
    fn 三档归属的文档标签互不相同() {
        let labels = [
            Owner::SelfBuilt.doc_label(),
            Owner::SelfBuiltM2.doc_label(),
            Owner::Reserved.doc_label(),
        ];
        let mut sorted = labels;
        sorted.sort_unstable();
        let before = sorted.len();
        let mut d = sorted.to_vec();
        d.dedup();
        assert_eq!(d.len(), before, "有两档的文档标签撞名了：{labels:?}");
    }
}
