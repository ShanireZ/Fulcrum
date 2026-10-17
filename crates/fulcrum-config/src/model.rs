//! 結構化設定 —— **唯一內部事實**（G11），格式是 **JSON**（G48）。
//!
//! ★ 兩個入口都合法：人寫 DSL，機器直接寫這一層。**DSL 是它的一個前端，不是它的替代。**
//! diff、版本化、原子回復全部建在這一層上，所以：
//!
//! - ★ ★ **`None` 照樣序列化成 `null`，不用 `skip_serializing_if`。**
//!   鍵集合穩定，兩個版本的 diff 才只反映真實變化；否則「把某項從有改成無」
//!   會表現為**刪掉一行**，與「這個版本的結構變了」在 diff 裡長得一模一樣。
//! - **不做反向生成**（結構化 → DSL）：註解、縮進、簡寫在往返中必然丟失。
//!
//! ⚠ 欄位會隨自研進度增加，
//! **穩定性承諾仍從 M4 起**（`G149`，原 `D9`，2026-09-07 結案）——
//! 現在還沒有承諾，`schema_version` 先佔住位置。

use crate::secret::Secret;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 當前結構化設定的版本號。改動欄位含義時要動它。
pub const SCHEMA_VERSION: u32 = 1;

/// 序列化成人讀的 JSON。
///
/// ★ 放在這裡而不是讓呼叫方自己 `serde_json::to_string_pretty`：
/// 「結構化那份是唯一內部事實」意味著**它長什麼樣是本層的責任**。
/// 由呼叫方各寫各的，縮進與鍵序遲早會分叉，而 diff 與原子回復全建在這份文字上。
pub fn to_pretty_json(cfg: &StructuredConfig) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(cfg)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructuredConfig {
    pub schema_version: u32,
    pub global: Global,
    pub defaults: Defaults,
    pub sites: Vec<SiteConfig>,
    pub l4: Option<L4Config>,
}

// ⚠ **回落層已整層刪除**（G98）：它的三個使用者（`l4` / `file_server` / `cache`）
// 逐塊改成自研之後一個不剩。代價寫在明處：**寫了 `fallback_nginx` /
// `fallback_caddy` 的設定會編譯不過** —— 而那兩條不是被靜默丟棄的，
// `compile.rs` 裡有一條專門的診斷告訴人它去哪了。

/// 預設回應（**G63**）。★ **預設值本身就是一份契約**，所以它進結構化設定，
/// 不只活在程式碼裡——機器寫這一層的時候，看得見自己繼承了什麼。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Defaults {
    /// 無站台匹配 → **421 Misdirected Request**。
    ///
    /// ★ 不靜默交給某個站台：後者是 nginx `default_server` 那類行為的溫床——
    /// 請求被交給一個使用者沒打算讓它去的地方，且沒有任何提示。
    pub no_site_match: u16,
    /// 站台內無路由匹配 → 404。
    pub no_route_match: u16,
    /// 上游全部不健康 → 502。
    pub all_upstreams_down: u16,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            no_site_match: 421,
            no_route_match: 404,
            all_upstreams_down: 502,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Global {
    pub acme_email: Option<String>,
    pub acme_ca: Option<String>,
    pub admin: Option<String>,
    pub default_sni: Option<String>,
    pub grace_period_ms: Option<u64>,
    /// 資料面（L7 反代 / 靜態檔案 / 快取 / **h3**）每個 service 的執行緒數（**G35 / G140**）。
    ///
    /// `None` = 用角色預設（見 `fulcrum_server::process::ThreadRole`）。
    ///
    /// ⚠ ⚠ ★ **執行緒不跨 service 共享** —— pingora 的 `ServerConf` 原話是
    /// "The threads are not shared across services"，每個 service 各起一套 runtime
    /// ⇒ **總執行緒數 ≈ Σ(各 service)**。這正是本儲存庫**沒有**一個籠統的全域 `threads`
    /// 而按角色分三格的理由：把一個全域值設成核數會直接超訂
    /// （4 核 × 4 service = 16 個 worker 執行緒）。見 `docs/architecture/process-model.md`。
    ///
    /// ★ h3 有意與 h1/h2 **同一個角色**（G140 ③）：它就是資料面入口，
    /// 與 h1/h2 服務同一批流量、走同一條執行鏈。
    pub threads_l7: Option<usize>,
    /// L4 面（TCP/UDP 透傳）每個 service 的執行緒數（**G35 / G140**）。`None` = 角色預設。
    pub threads_l4: Option<usize>,
    /// 管理面與後台任務的執行緒數（**G35 / G140**）。`None` = 角色預設。
    ///
    /// ★ ★ 它同時是 pingora `ServerConf.threads` 的取值 ⇒ **任何沒有被逐 service
    /// 覆蓋的東西都跟著它走**（後台服務就是這一類：`background_service()` 不設
    /// `threads`，於是 `Service::threads()` 回傳 `None` 並回落到全域值）。
    /// ⇒ 它是「其餘一切」那一格，⛔ 不只是 admin socket 那一個 service。
    pub threads_admin: Option<usize>,
    /// 自動把 HTTP 重定向到 HTTPS（G12 的後半句）。**預設 true**。
    ///
    /// ⚠ ⚠ `#[serde(default = "yes")]` 不是可有可無的：結構化設定是**公開入口**（G11），
    /// 機器直接寫它。一份**沒有這個欄位**的舊 JSON 灌進來時，
    /// 預設必須是 `true` —— 否則「升級一次樞衡，站台的 HTTP 跳轉就沒了」，
    /// 而設定一個字都沒改過。
    #[serde(default = "yes")]
    pub auto_http_redirect: bool,
    /// ★ ★ **PROXY protocol 的信任清單，管的是 HTTP 面的全部監聽連接埠**（**M2 批 D**）。
    ///
    /// 空 = **誰發來的 PROXY 頭都不看**（預設，也是 owner 拍板的口徑）。
    ///
    /// ⚠ ⚠ **「不在清單裡就當沒有這個頭」在這裡有一個字面的落法：一個位元組都不讀。**
    /// 與 XFF 那條（[安全基線]）不同的是，XFF 是 HTTP 頭、忽略它無害；
    /// 而 PROXY 頭是**連接開頭的位元組**，「讀掉丟棄」與「一個位元組都不讀」
    /// 是**給上游兩條完全不同的流**。取後者的理由寫在
    /// `fulcrum_runtime::proxyproto` 與 §10：
    /// v2 頭自帶長度欄位，「讀掉丟棄」必須先解析攻擊者控制的那兩個位元組才知道丟多少。
    ///
    /// ★ 它與 `l4` 裡那份清單**有意分開**：信任 A 發 PROXY 頭 ≠ 信任 A 對另一個連接埠
    /// 也這麼幹，更 ≠ 信任 A 寫的 XFF（那是請求級的，這是連接級的）。
    #[serde(default)]
    pub proxy_protocol_from: Vec<String>,
}

/// `serde(default)` 用的 `true`。★ 單獨一個函式是因為 serde 的 default 只能給函式名。
fn yes() -> bool {
    true
}

/// `proxy_protocol` 不帶參數時寫哪個版本（**owner 拍板：v2**，M2 批 D）。
///
/// ★ ★ **它住在這裡，而 `fulcrum_runtime::proxyproto::Version::default()` 引用它** ——
/// 不是兩處各寫一個 `"v2"` 再拿一條契約測試去釘。⚠ 兩份「一開始總是一致的」值，
/// 會在**下一次改動**時分家，而這一條分家的現場表現是
/// 「DSL 裡省略參數與顯式寫 v2 行為不同」—— 沒有任何一道門會說出來。
/// ★ 這與批 C 重複使用 `wildcard_covers`（G66）是同一條紀律：**讓分家在結構上做不到**。
pub const DEFAULT_PROXY_PROTOCOL: &str = "v2";

/// `proxy_protocol` 認得的版本。
///
/// ★ ★ 它有**兩個**使用者：`reverse_proxy` 的子指令表（走 `ArgType::Enum`）
/// 與 `l4` 塊裡那段手寫的檢核（`l4` 的子指令不走子指令表）。
/// ⚠ 讓它們各寫一份 `["v1", "v2"]` 的代價很具體：**將來加 v3 時只改一處**，
/// 而兩處的現場表現是「同一個詞在 HTTP 面能寫、在 L4 面回報錯誤」。
pub const PROXY_PROTOCOL_VERSIONS: &[&str] = &["v1", "v2"];

/// ⚠ ⚠ **`Default` 必須手寫，不能 `derive`。**
///
/// `derive(Default)` 給 `bool` 的是 **`false`** —— 而這個欄位的預設值是 **`true`**。
/// 一旦 derive，一份**沒寫全域塊**的設定（最常見的那種）就會悄悄關掉 HTTP 跳轉，
/// 而 DSL 參考 §二印著「自動把 HTTP 重定向過來」。
/// ★ 這正是本儲存庫那條「預設值本身就是契約」（§八 / G63）的又一處落點。
impl Default for Global {
    fn default() -> Self {
        Global {
            acme_email: None,
            acme_ca: None,
            admin: None,
            default_sni: None,
            grace_period_ms: None,
            // ★ 三個都是 `None` = 「用角色預設」，⛔ 不是「1」：
            //   把預設值寫死在這裡，它就住了兩處（這裡一份、算角色預設那裡一份），
            //   而兩份遲早分家 —— 本儲存庫 2026-09-05 當天剛栽過一次同形狀的（G135 的
            //   `effective_capacity` 抄了三處）。⇒ 預設只存在於 `ThreadRole` 那一處。
            threads_l7: None,
            threads_l4: None,
            threads_admin: None,
            auto_http_redirect: true,
            // ★ 預設**空**：不信任任何人發來的 PROXY 頭。
            //   ⚠ 這與 `auto_http_redirect` 那條相反 —— 那條的預設是 `true`
            //   （關掉它是「設定一個字沒改，站台的跳轉沒了」）；
            //   而這條的預設必須是「誰都不信」，因為一個預設信任某個網段的代理伺服器，
            //   等於讓那個網段裡的任何人自稱自己是任何 IP。
            proxy_protocol_from: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SiteConfig {
    pub addresses: Vec<Address>,
    pub matchers: BTreeMap<String, Matcher>,
    pub tls: TlsConfig,
    pub log: Option<LogConfig>,
    /// 已經按執行順序表排好的執行鏈（G49）。
    pub chain: Vec<Step>,
    /// `handle_errors { … }` 的內容。空 = 用 [`Defaults`]。
    pub error_handler: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Address {
    /// DSL 裡寫的原文。
    pub raw: String,
    pub scheme: String,
    /// 空串 = 任意 Host。
    pub host: String,
    pub port: u16,
    /// `*.example.com`。★ 需要 DNS-01（G54/G57），M1 即可用（G58）。
    pub wildcard: bool,
    /// 是否自動簽證書並把 HTTP 重定向過來（G12）。`http://` 前綴會關掉它。
    pub auto_https: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Matcher {
    /// ★ 同一塊內**多條件是 AND**；同一條件寫多個值是 OR。
    pub conditions: Vec<Condition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    pub kind: String,
    pub values: Vec<String>,
    /// `not { … }` 包著的。
    pub negate: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum TlsMode {
    /// 自動 HTTPS（預設）。
    Automatic,
    /// `tls internal` —— 自簽。
    Internal,
    /// `tls <cert> <key>` —— 使用者自帶證書。
    Manual { cert: String, key: String },
    /// `http://` 位址：不簽、不升級。
    Off,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TlsConfig {
    #[serde(flatten)]
    pub mode: TlsMode,
    pub on_demand: bool,
    /// On-Demand 的存取資格端點。★ **G15：沒配它就拒絕啟動**，
    /// 所以這裡是 `Option` 只是為了表達「DSL 裡沒寫」，不是「可以不寫」。
    pub ask: Option<String>,
    /// DNS-01 用哪家（G57：原生 `cloudflare` / `dnspod`，其餘走 exec hook）。
    pub dns_provider: Option<String>,
    /// `dns exec <程序>` 裡那個處理程序路徑。★ 只有 `exec` 用得上。
    #[serde(default)]
    /// `dns` 的第二個參數：exec 的處理程序路徑，或原生供應商的憑證。
    ///
    /// ⚠ ⚠ 此後它是 [`Secret`]：**字面值憑證現在寫得下**（owner 拍板，
    /// Caddy 形狀），而代價是這份設定本身變成了秘密 —— 所以預設序列化、
    /// `Debug`、`Display` 一律脫敏，露真值要顯式進 `secret::reveal` 作用域。
    /// ★ `dns exec /path/to/hook` 那種也走這個類型，但路徑不是秘密（無前綴 ⇒ 字面值
    /// ⇒ 會被脫敏）—— ⚠ 這是有意的取捨：**一條路徑也可能洩露資訊**，
    /// 而它作為「線索」的價值遠低於憑證洩露的代價；要看它就 `--with-secrets`。
    pub dns_arg: Option<Secret>,
    /// 檢核 TXT 可見性時問哪些權威 NS（`host:port`）。
    ///
    /// ★ ★ **G58 要求它必須有**：確認 TXT 可見只能靠真去問，
    /// **絕不能只 sleep 一個固定秒數**。所以配了 `dns` 卻沒配這個是**編譯期錯誤**，
    /// 形狀照 G15（`on_demand` 沒配 `ask` 就拒絕啟動）。
    #[serde(default)]
    pub resolvers: Vec<String>,
    /// 這份 DNS 憑證**被宣告**覆蓋哪些 zone（**G59 第 3 條**）。
    ///
    /// ★ ★ 它不是「憑證實際能幹什麼」——DNSPod 的 token 是帳號級的，
    /// 實際權限就是帳號下的全部網域。宣告的價值在於：**越權那一刻是我們自己拒絕的，
    /// 而且拒絕的理由在設定裡看得見**。
    /// ⚠ 原生供應商（`cloudflare` / `dnspod`）沒寫它是**編譯期錯誤**，形狀照 G15。
    #[serde(default)]
    pub zones: Vec<String>,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            mode: TlsMode::Automatic,
            on_demand: false,
            ask: None,
            dns_provider: None,
            dns_arg: None,
            resolvers: Vec::new(),
            zones: Vec::new(),
        }
    }
}

/// ✅ 欄位清單與格式已由 **G113 + G114** 定稿（D7 結案）。
/// 權威是 [`docs/architecture/observability.md`](../../../docs/architecture/observability.md)。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LogConfig {
    pub output: Option<String>,
    pub level: Option<String>,
    /// 請求頭白名單（**M2 批 L 第 ③ 步**）。★ 預設**一個頭都不記**。
    ///
    /// ⚠ 這裡存的是**使用者寫的原文**，規範化（小寫、`-` 換 `_`、加前綴）在執行環境圖那一層做
    /// —— 結構化設定是「設定的樣子」，不是「紀錄的樣子」。
    #[serde(default)]
    pub headers: Vec<String>,
    /// 回應頭白名單（**M2 批 L 第 ③ 步**）。同上。
    #[serde(default)]
    pub resp_headers: Vec<String>,
}

/// 執行鏈上的一步。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    /// 執行順序表裡的序號（G49）。★ 它進結構化設定，是為了讓「它實際跑在第幾步」
    /// 成為**可被讀到的事實**，而不是只能靠背那張表。
    pub order: u16,
    pub matcher: Option<MatcherRef>,
    #[serde(flatten)]
    pub body: StepBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatcherRef {
    /// `@name`
    Named(String),
    /// 行內簡寫，**只能是路徑**（G50）。
    Path(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "directive")]
pub enum StepBody {
    /// 預留：M1 語法上認得，不產生任何行為。
    Tracing,
    Header {
        ops: Vec<HeaderOp>,
    },
    Rewrite {
        to: String,
    },
    Encode {
        encodings: Vec<String>,
    },
    Cache {
        /// **備援**新鮮度（G96）：只有上游沒給 `max-age`/`s-maxage`/`Expires` 時才用它。
        /// `None` = 上游沒說就**不快取**。
        ttl_ms: Option<u64>,
        /// 單條目大小上限。`None` ⇒ [`fulcrum_config::directive::CACHE_DEFAULT_MAX_SIZE_BYTES`]。
        max_size_bytes: Option<u64>,
        /// 整個快取的容量上限，達到就按 LRU 淘汰。
        /// `None` ⇒ [`fulcrum_config::directive::CACHE_DEFAULT_CAPACITY_BYTES`]。
        #[serde(default)]
        capacity_bytes: Option<u64>,
        /// 磁碟後端的根目錄（M2 批 H）。`None` ⇒ 記憶體後端。
        ///
        /// ⚠ 它是**處理程序級**的：多個 `cache` 塊必須寫同一個值（編譯期檢查）。
        #[serde(default)]
        disk_dir: Option<String>,
    },
    /// 互斥容器：多個 `handle` 裡只有第一個匹配的會執行。
    Handle {
        arms: Vec<HandleArm>,
    },
    /// ★ 保序容器（G49 的逃生口）：塊內**按書寫順序**執行。
    ///
    /// ⚠ 讀這份 JSON 的人注意：`steps` 裡每一步仍然帶著自己的 `order`（那是它在
    /// 順序表裡的位置，是指令的固有屬性），**但陣列順序才是執行順序**。
    /// 對 `steps` 按 `order` 排序會把逃生口的語義抹掉。
    Route {
        steps: Vec<Step>,
    },
    Redir {
        to: String,
        code: u16,
    },
    Respond {
        status: u16,
        body: Option<String>,
    },
    /// Prometheus 抓取端點（**M2 批 M**，G116）。**一個欄位都沒有。**
    ///
    /// ★ 它是**終結類**（序號 75），不是站台級屬性也不是獨立監聽器：
    /// 訪問控制、TLS、訪問紀錄、壓縮因此全部重複使用現有機制，
    /// 而 G14「管理面只綁 Unix socket」的口徑一個字不動 —— 指標面不屬於管理面。
    ///
    /// ⚠ 結構化設定是**公開入口**（G11），所以這個無欄位變體的 JSON 形狀也是契約：
    /// 內部標籤制 ⇒ 它序列化成 `{"order":75,"matcher":null,"directive":"metrics"}`,
    /// **沒有第二個鍵**。★ 與 `Tracing` 同款，不是新寫法。
    Metrics,
    ReverseProxy {
        /// 這一條 `reverse_proxy` 的**穩定 id**（`id` 子指令，**M2 批 N 任務 2.8**，
        /// 裁決 R6 ⇒ G125）。`None` = 沒寫。
        ///
        /// # 它是幹什麼用的
        ///
        /// 管理面的臨時覆蓋層（G18）要尋址「**這個站台塊裡的那臺機器**」，鍵是
        /// `(站点名, id, 归一化后的上游地址)`。沒有 id 時同一站台裡兩條 `reverse_proxy`
        /// 寫了同一個上游就分不開 —— 它們**共享同一個覆蓋格子**，一次 `disable`
        /// 把它們一起摘掉（裁決 R6 ③ **第二輪**）。⇒ 想分開就寫 `id`。
        ///
        /// ⚠ ⚠ **鍵相同不是錯誤、也不給告警**：「一個後端掛在幾組 `handle` 路由後面」
        /// 是反代最常見的寫法 ⇒ **現有設定一個位元組都不用改**。
        /// 「這一格管著幾條」由 `fulcrum_runtime::Runtime::proxy_key_fanout` 算出來，
        /// 經 `/stats` 顯示（G18「不持久化但永遠可見」）。
        ///
        /// # ⚠ ⚠ 鍵相不相同這件事**看不見於這一層**
        ///
        /// 鍵裡的上游位址取**歸一化之後**的那個串（`fulcrum_runtime::normalize_upstream`
        /// 把 `backend` 補成 `backend:80`）⇒ 兩條 `reverse_proxy` 一條寫 `backend`、
        /// 另一條寫 `backend:80`，**原文不同而鍵相同**。在這一層拿原文 token 比對
        /// 會把那一對算成兩格，而它們在執行環境是同一臺機器
        /// ⇒ 鍵只在 `fulcrum_runtime` 建圖那條路上成形。
        ///
        /// # ⚠ ⚠ 序列化：`None` 時**整個鍵都不出現**
        ///
        /// 與本模組 `Global` 那條「`None` 寫成 `null` 好讓鍵集合穩定」**有意相反**，
        /// 理由與 [`UpstreamSpec`] 的「權重為 1 寫回裸字串」逐字相同：
        /// **沒寫 `id` 的設定 `compile` 出來的 JSON 一個位元組都不許變** ——
        /// 現有夾具、磁碟上那份結構化設定、`POST /load` 的舊載荷全靠它。
        /// ⇒ 這一批「有沒有順手改掉別的東西」因此看得出來。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// 上游清單。★ 每一項自帶**設定權重**（[`UpstreamSpec`]，**M2 批 N**）。
        ///
        /// ⚠ ⚠ 取「每項自帶權重」而不是「再加一個平行的 `weights: Vec<u32>`」：
        /// 兩個等長向量遲早不等長，而不等長的表現是**權重悄悄錯位**，不是回報錯誤。
        upstreams: Vec<UpstreamSpec>,
        lb_policy: String,
        health: HealthCheck,
        /// ★ 上游網域定期重解析（G17）。**這條直接消滅 nginx OSS 那個經典事故源**。
        dns_refresh_ms: u64,
        passive: Passive,
        header_up: Vec<HeaderOp>,
        header_down: Vec<HeaderOp>,
        transport: String,
        tls_insecure_skip_verify: bool,
        /// **發**給上游一個 PROXY 頭，`"v1"` / `"v2"`。`None` = 不發（**M2 批 D**）。
        ///
        /// ★ 它與 `header_up X-Forwarded-For` 解決的是同一件事，但**層不同**：
        /// XFF 是 HTTP 頭（上游要會讀它、且要信任我們），PROXY 頭在**連接開頭**，
        /// 上游把它當成 socket 層的事實 —— ⇒ 對**不解析 HTTP 的上游**（資料庫、
        /// 電子郵件、自己也是 L4 代理的那種）它是唯一可用的那個。
        #[serde(default)]
        proxy_protocol: Option<String>,
    },
    FileServer {
        /// ⚠ **M2 批 F 起必填、且必須是絕對路徑**（G91）。類型仍是 `Option`，
        /// 是因為編譯出錯時也要能把這一步造出來接著往下查——`None` 只出現在
        /// 已經報過 `MISSING_REQUIRED_SUB` 的那條路上，執行環境看不到它。
        root: Option<String>,
        browse: bool,
        index: Vec<String>,
        /// G87。預設 **true**（跟隨），與 nginx `disable_symlinks off`、Caddy 一致。
        ///
        /// ⚠ ⚠ 結構化設定是**公開入口**（G11），預設是 `true`
        /// ⇒ 必須 `#[serde(default = "yes")]`，裸 `#[serde(default)]` 會給出 `false`，
        /// 那等於**從 JSON 進來的設定默默換了一種行為**。
        #[serde(default = "yes")]
        follow_symlinks: bool,
        /// G88。使用者寫的那幾段，**追加**在預設表之後（不是替換）。
        #[serde(default)]
        hide: Vec<String>,
        /// G88。預設 **true**（預設表生效）。寫 `false` 關掉預設表。
        ///
        /// ★ 它把「追加」的代價補掉了：**一個關不掉的預設就是一條隱藏規則**。
        #[serde(default = "yes")]
        hide_defaults: bool,
        /// 預壓縮旁檔案認哪幾種編碼（**M2 批 I**，G99）。空 = 不找旁檔案。
        ///
        /// ★ 與 `encode` 用同一張取值表（gzip / zstd / br），但兩者是**獨立**的：
        /// 只配 `precompressed` = 只發已經壓好的旁檔案、從不現壓；
        /// 只配 `encode` = 全部現壓；兩個都配 = 有旁檔案用旁檔案、沒有才現壓。
        #[serde(default)]
        precompressed: Vec<String>,
    },
}

impl StepBody {
    pub fn directive_name(&self) -> &'static str {
        match self {
            StepBody::Tracing => "tracing",
            StepBody::Header { .. } => "header",
            StepBody::Rewrite { .. } => "rewrite",
            StepBody::Encode { .. } => "encode",
            StepBody::Cache { .. } => "cache",
            StepBody::Handle { .. } => "handle",
            StepBody::Route { .. } => "route",
            StepBody::Redir { .. } => "redir",
            StepBody::Respond { .. } => "respond",
            StepBody::Metrics => "metrics",
            StepBody::ReverseProxy { .. } => "reverse_proxy",
            StepBody::FileServer { .. } => "file_server",
        }
    }
}

/// 沒寫 `weight` 的上游是這個權重（**M2 批 N**，裁決 R1）。
pub const DEFAULT_UPSTREAM_WEIGHT: u32 = 1;

/// 權重值域的下界（裁決 R3）。
///
/// ★ ★ **`0` 有意不合法**：「這臺不參與排程」**只有一種表達方式** —— 管理面覆蓋層的
/// `disable`。讓 `weight 0` 也表示摘掉，就是兩條路做同一件事，
/// 而兩條路遲早分家（一條改了、另一條沒改，且沒有任何東西會說）。
pub const MIN_UPSTREAM_WEIGHT: u32 = 1;

/// 權重值域的上界（裁決 R3）。
///
/// ★ 它與管理面 `set_weight` 的值域**是同一對常數**，不是兩處各寫一個 65535。
pub const MAX_UPSTREAM_WEIGHT: u32 = 65_535;

/// 執行緒數值域的下界（**G140**）。
///
/// ★ ★ **`0` 有意不合法，理由與 [`MIN_UPSTREAM_WEIGHT`] 同源但更硬**：pingora 拿這個數
/// 去建 tokio runtime，`0` 會**在啟動時 panic** —— ⇒ 這裡不攔，症狀就是「改了一個數字，
/// 服務起不來，而回報錯誤出自 tokio 深處、一個字都不提設定」。
pub const MIN_THREADS: usize = 1;

/// 執行緒數值域的上界（**G140**）。
///
/// ★ 它不是一條效能建議，是一道**手滑防線**：`threads_l7 1000000` 會讓 pingora 真的去
/// 建那麼多執行緒。⚠ ⛔ **別把這個數讀成「推薦上限」** —— 合適的取值要等 M3 的對拍資料
/// （`docs/architecture/process-model.md` 明寫「初值不是結論」）。
pub const MAX_THREADS: usize = 1024;

/// `passive_fail` 值域的下界（**G136**）。
///
/// ★ ★ **`0` 有意不合法，理由與 [`MIN_UPSTREAM_WEIGHT`] 逐字同源**：
/// 「這條 `reverse_proxy` 不做被動熔斷」**只有一種表達方式** —— 不寫 `passive_fail`。
pub const MIN_PASSIVE_FAIL: u32 = 1;

/// `passive_fail` 值域的上界（**G136**）。
///
/// ⚠ 上界的價值**不在於擋住一個荒謬的大數**（閾值 65535 只是永遠熔不斷，不危險），
/// 而在於讓「不是一個整數」這一類手滑走同一條診斷 —— 那一類才是真正危險的：
/// 在 G136 之前它靜默變成 `None`，而 `None` 的語義恰好是**整個特性關掉**。
pub const MAX_PASSIVE_FAIL: u32 = 65_535;

/// `reverse_proxy { id … }` 的長度上限（owner 拍板，R6 那三條小項之三）。
pub const MAX_PROXY_ID_LEN: usize = 64;

/// 這個 `id` 在取值域裡嗎：`[A-Za-z0-9_.-]`，長度 `1..=`[`MAX_PROXY_ID_LEN`]。
///
/// # ★ ★ ★ 收緊的理由只有一條半，⛔ 別把它記成「基數風險」
///
/// 1. **硬的那條 —— 與備援記號撞車**：本儲存庫用 `<other>` / `<none>` / `<unknown>` /
///    `<undeclared>` 表示「取不到 / 備援」，靠的是**尖括號在真值裡不可能出現**。
///    而在這條之前，`id <none>` 是合法的。
/// 2. **軟的那半條 —— 可讀性**：`id` 裡的換行會讓 `/stats` 與紀錄上那一行
///    在人眼裡斷成兩行。
///
/// ⛔ **不是**指標基數：G126 明寫 `fulcrum_overrides_active` **無標籤**，
/// `id` 一個字都不進指標。★ 一個假理由會讓下一個人理直氣壯地把限制放寬回去。
///
/// # ⚠ 空串在這裡回 `false`，但它有**自己的**診斷
///
/// `id ""` 與「根本沒寫」是同一個鍵（[`crate::diag::DiagCode::EMPTY_PROXY_ID`]
/// 那段解釋是它的全部價值）⇒ 呼叫方要**先**判空串，⛔ 別讓這條泛泛的
/// 「不合法」把那句話頂掉。
///
/// # ⚠ 兩條路都要調它
///
/// DSL 那條在 `compile.rs`，**結構化設定**那條（`POST /load`，G11 的公開入口，
/// 不經過 `fulcrum compile`）在 `fulcrum-runtime` 建圖時。
/// ★ 兩處調的是**這一個**函式，不是兩份手寫的平行邏輯 —— 與
/// [`MIN_UPSTREAM_WEIGHT`] / [`MAX_UPSTREAM_WEIGHT`] 同一條紀律。
pub fn is_valid_proxy_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_PROXY_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
}

/// 那句「合法的 id 長什麼樣」的話，**只有一份**。
///
/// ★ 兩條路的診斷/裝載錯誤都拼它 —— ⛔ 兩處各寫一句，改的時候必定只改一處。
pub fn proxy_id_shape() -> String {
    format!("合法的 `id` 只含 `A-Za-z0-9_.-`，长度 1–{MAX_PROXY_ID_LEN}（例 `pool_web` / `web-1`）")
}

/// 一條 `reverse_proxy` 上的一個上游：位址 + **設定權重**（**M2 批 N**，裁決 R1/R2）。
///
/// # ★ ★ 序列化契約（G11 的公開入口，兩個方向都釘住）
///
/// | 情形 | JSON |
/// |---|---|
/// | 反序列化：裸字串 `"10.0.0.1:8080"` | 收，權重 = [`DEFAULT_UPSTREAM_WEIGHT`] |
/// | 反序列化：物件 `{"addr":"10.0.0.1:8080","weight":3}` | 收 |
/// | 序列化：權重 == 1 | **寫回裸字串** |
/// | 序列化：權重 != 1 | 寫成物件 |
///
/// ⇒ ① 舊載荷繼續能 `POST /load`（那是個活著的介面，不是內部結構）；
/// ② 沒配 `weight` 的設定 `compile` 出來的 JSON **一個位元組都不變**。
/// ⚠ ②不是「省事」，它是**判據的一部分**：現有夾具、`plan` 的輸出與磁碟上那份結構化設定
/// 全都因此逐字不變，於是這一批**有沒有順手改掉別的東西**看得出來。
///
/// ⚠ 代價寫在明處：**`weight 1` 與不寫 `weight` 在結構化設定裡完全同形**，
/// 裝載時也就分不出來。★ 這不是缺口 —— 全部權重都是 1 的排程與今天逐字相同，
/// 「分不出來」的那兩種情形本來就沒有任何行為差別。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamSpec {
    /// DSL 裡 `reverse_proxy` 那一行上寫的**原文 token**。
    ///
    /// ⚠ ⚠ **這一層不做任何歸一化**（`backend` 不會變成 `backend:80`）：
    /// 歸一化住在 `fulcrum_runtime::normalize_upstream`，在這邊再寫一份「差不多的」
    /// 就是分家，而分家的現場是「寫了 `weight backend 3`，設定照過，權重沒生效」。
    /// ⇒ `weight` 的位址比對因此是**逐字相同**，對不上是裝載期錯誤。
    pub addr: String,
    /// 設定權重，值域 `[MIN_UPSTREAM_WEIGHT, MAX_UPSTREAM_WEIGHT]`。
    pub weight: u32,
}

impl UpstreamSpec {
    /// 一個預設權重的上游。
    pub fn new(addr: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            weight: DEFAULT_UPSTREAM_WEIGHT,
        }
    }
}

impl Serialize for UpstreamSpec {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct as _;
        if self.weight == DEFAULT_UPSTREAM_WEIGHT {
            // ★ 這一支是「沒配 weight 的設定一個位元組都不變」的全部實作。
            return s.serialize_str(&self.addr);
        }
        let mut st = s.serialize_struct("UpstreamSpec", 2)?;
        st.serialize_field("addr", &self.addr)?;
        st.serialize_field("weight", &self.weight)?;
        st.end()
    }
}

impl<'de> Deserialize<'de> for UpstreamSpec {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // ⚠ 手寫 visitor 而不是 `#[serde(untagged)]`：untagged 在兩支都不匹配時
        //   只會說「data did not match any variant」，而這裡是 `POST /load` 的入口 ——
        //   一條說不清哪裡錯了的 400 會把排查成本整個推給對面。
        d.deserialize_any(UpstreamSpecVisitor)
    }
}

struct UpstreamSpecVisitor;

impl<'de> serde::de::Visitor<'de> for UpstreamSpecVisitor {
    type Value = UpstreamSpec;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("一个上游地址字符串，或 {\"addr\":\"host:port\",\"weight\":正整数}")
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
        Ok(UpstreamSpec::new(v))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut m: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error as _;
        let mut addr: Option<String> = None;
        let mut weight: Option<u32> = None;
        while let Some(k) = m.next_key::<String>()? {
            match k.as_str() {
                "addr" => {
                    if addr.is_some() {
                        return Err(A::Error::duplicate_field("addr"));
                    }
                    addr = Some(m.next_value()?);
                }
                "weight" => {
                    if weight.is_some() {
                        return Err(A::Error::duplicate_field("weight"));
                    }
                    weight = Some(m.next_value()?);
                }
                // ⚠ 不靜默吞掉不認識的鍵：一個拼錯的 `weigth` 會讓權重回落成 1，
                //   而那份設定在每一處都顯得正常。
                other => {
                    return Err(A::Error::unknown_field(other, &["addr", "weight"]));
                }
            }
        }
        let addr = addr.ok_or_else(|| A::Error::missing_field("addr"))?;
        let weight = weight.unwrap_or(DEFAULT_UPSTREAM_WEIGHT);
        if !(MIN_UPSTREAM_WEIGHT..=MAX_UPSTREAM_WEIGHT).contains(&weight) {
            // ★ 與 DSL 那條診斷（`FUL-DSL-0040`）同一個值域：機器寫這一層時
            //   也得撞上同一堵牆，否則「DSL 拒絕、JSON 收下」就是兩套規則。
            return Err(A::Error::custom(format!(
                "上游 `{addr}` 的 weight 是 {weight}，要在 {MIN_UPSTREAM_WEIGHT}–{MAX_UPSTREAM_WEIGHT} 之间；\
                 0 不合法 —— 把一个上游摘出调度用管理面的 `disable`"
            )));
        }
        Ok(UpstreamSpec { addr, weight })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandleArm {
    pub matcher: Option<MatcherRef>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeaderOp {
    /// `set` / `add` / `remove`
    pub op: String,
    pub name: String,
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthCheck {
    pub uri: Option<String>,
    pub interval_ms: u64,
    pub timeout_ms: u64,
    pub status: String,
}

impl Default for HealthCheck {
    fn default() -> Self {
        Self {
            uri: None,
            interval_ms: 10_000,
            timeout_ms: 3_000,
            status: "2xx".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Passive {
    /// 視窗內失敗幾次就熔斷。**`None` = 這條 `reverse_proxy` 完全不做被動熔斷**（G136）。
    ///
    /// ★ ★ 它是 `Option` 而下面兩格不是，因為這兩件事**性質不同**：`None` 在這裡是
    /// **語義**（關掉），⛔ 不是「沒寫所以取預設」。⇒ 既有設定一個字不改、行為一個字不變。
    /// ★ 同一條先例在 `health_uri`：不寫它就完全不探測，其餘 `health_*` 有預設也不會
    /// 讓探測發生。
    pub fail_threshold: Option<u32>,
    /// 數失敗的視窗，預設 **10s**（G136）。
    ///
    /// ★ ★ **預設值在這一層就落實成具體數字，⛔ 不留 `Option` 讓執行環境 `unwrap_or`**：
    /// 那樣預設值會住在兩處，而兩處遲早分家、分家那天沒有任何東西會說。
    /// ⇒ 與隔壁 [`HealthCheck::interval_ms`] 同一形狀；`plan` / `compile` 的產物
    /// （G11/G48 的公開契約）因此說得出**真正生效**的值。
    pub window_ms: u64,
    /// 熔斷之後歇多久，然後放**一個**半開探針，預設 **30s**（G136）。
    ///
    /// ★ 它與 [`window_ms`](Self::window_ms) 有意是兩個旋鈕，⛔ 不是 nginx `fail_timeout`
    /// 那樣一格兩用：兩者問的不是同一個問題 —— 視窗問「失敗要多密才算一個模式」，
    /// 冷卻問「給上游多久緩過來」，而一個真壞了的上游很少在 10s 內自愈。
    /// ★ 拉長冷卻的代價被半開釘死在「每週期 1 個探針」，不會因為拉長而變貴。
    pub cooldown_ms: u64,
}

impl Default for Passive {
    /// ⚠ ⚠ **預設是「關閉」** —— `fail_threshold: None`。
    /// 下面兩格的數字只有在 `passive_fail` 寫了之後才有意義。
    fn default() -> Self {
        Self {
            fail_threshold: None,
            window_ms: 10_000,
            cooldown_ms: 30_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L4Config {
    pub listeners: Vec<L4Listener>,
    // ★ ★ ★ **`fallback` 這一格在被整條刪掉了。**
    //
    //   M1（G60）：整個 L4 面編譯成「交給 Caddy」的回落標記；
    //   批 A：TCP 自研 ⇒ 它變成 `Option`，只有 `udp` 還標回落；
    //   批 B：UDP 也自研 ⇒ **沒有任何一條 L4 設定需要回落了**，這一格恆為 `None`。
    //
    //   ⚠ 留一個恆為 `None` 的欄位不是「保守」，它是**一句會被當真的假話**：
    //   `plan` 與 `compile` 的產物是公開契約（G11/G48），讀的人會以為那條路還在。
    //   ★ 這也是回落層第 3 條約束（「拆除回落是里程碑的一部分，不是以後再說」）
    //   第一次真的被兌現 —— 而兌現的方式是**刪掉資料結構**，不是留著不填。
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L4Listener {
    /// `tcp` / `udp`
    pub proto: String,
    pub listen: String,
    /// **備援**上游：沒有任何 `sni` / `alpn` 規則命中時用它。
    ///
    /// ⚠ 它可以是空的 —— 那表示「沒匹配上就關掉連接」。★ 空與「沒寫規則」是兩回事：
    /// 後者是純透傳（批 A 的形狀），前者是「只服務我認得的那幾個名字」。
    pub upstreams: Vec<String>,
    /// SNI / ALPN 分流規則（**M2 批 C**）。★ **按書寫順序匹配，第一個命中即用**。
    ///
    /// ⚠ L4 沒有站台塊那張執行順序表（那張表是給中介軟體排序的），
    /// 所以這裡的順序就是**使用者寫的順序** —— 與 `route { … }` 那個保序塊同源。
    #[serde(default)]
    pub rules: Vec<L4Rule>,
    /// **收**：信任這些來源發來的 PROXY 頭（**M2 批 D**）。空 = 誰都不信。
    ///
    /// ★ 每個監聽器**各有一份**，而不是共用 `l4` 塊級的一份：
    /// 3306 前面掛的那臺 LB 與 443 前面掛的那臺通常不是同一臺。
    #[serde(default)]
    pub proxy_protocol_from: Vec<String>,
    /// **發**：給上游發一個 PROXY 頭，`"v1"` / `"v2"`。`None` = 不發（**M2 批 D**）。
    ///
    /// ⚠ ⚠ **收與發是兩件獨立的事，不是一個開關的兩半**：
    /// 只收不發（樞衡自己要用那個 IP）、只發不收（樞衡是第一跳）、
    /// 收了再發（鏈式傳遞）三種都是真實用法。★ 因此它們是兩個欄位。
    #[serde(default)]
    pub proxy_protocol: Option<String>,
}

/// 一條 L4 分流規則（`sni a.com b.com { proxy … }`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L4Rule {
    /// `sni` / `alpn`。
    pub kind: String,
    /// 要匹配的值。**同一條裡多個值是 OR**（與站台塊內的匹配器同一條口徑）。
    pub values: Vec<String>,
    pub upstreams: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_ones_g63_wrote_down() {
        let d = Defaults::default();
        assert_eq!(
            d.no_site_match, 421,
            "无站点匹配必须是 421，不是静默交给某个站点"
        );
        assert_eq!(d.no_route_match, 404);
        assert_eq!(d.all_upstreams_down, 502);
    }

    #[test]
    fn none_serializes_as_null_so_the_key_set_is_stable() {
        // ★ diff 穩定是 G48 選 JSON 的理由之一。鍵集合會隨取值消失的話，
        //   「把某項從有改成無」在 diff 裡看起來就像「結構變了」。
        let g = Global::default();
        let j = serde_json::to_string(&g).unwrap();
        assert!(j.contains("\"acme_email\":null"), "{j}");
    }

    // ── 上游權重的序列化契約（M2 批 N 任務 1，裁決 R2）────────────────────

    #[test]
    fn 裸字符串上游收得下并且权重是一() {
        // ★ 這一條守的是**舊載荷**：`POST /load` 是活著的介面，
        //   在 `weight` 出現之前發出去的那些 JSON 必須繼續能進來。
        let u: UpstreamSpec = serde_json::from_str("\"10.0.0.1:8080\"").unwrap();
        assert_eq!(u.addr, "10.0.0.1:8080");
        assert_eq!(u.weight, DEFAULT_UPSTREAM_WEIGHT);
    }

    #[test]
    fn 对象形态的上游收得下并且权重留住了() {
        let u: UpstreamSpec = serde_json::from_str(r#"{"addr":"x:1","weight":3}"#).unwrap();
        assert_eq!(u.addr, "x:1");
        assert_eq!(u.weight, 3);
        // 往回傳去仍是物件，且權重還是 3。
        let j = serde_json::to_string(&u).unwrap();
        assert_eq!(j, r#"{"addr":"x:1","weight":3}"#);
    }

    #[test]
    fn 权重为一时序列化回裸字符串而不是对象() {
        // ★ ★ **反向判據**：少了它，一個「永遠寫物件」的實作照樣能讓上面兩條綠，
        //   而它會讓現有的每一份夾具、每一份磁碟上的結構化設定集體漂移。
        let j = serde_json::to_string(&UpstreamSpec::new("x:1")).unwrap();
        assert_eq!(j, "\"x:1\"", "权重为 1 必须写回裸字符串");
        assert!(
            !j.contains("weight"),
            "权重为 1 时不许出现 weight 这个键：{j}"
        );
        assert!(!j.contains('{'), "权重为 1 时不许序列化成对象：{j}");
    }

    #[test]
    fn 裸字符串往返之后仍是裸字符串() {
        let j = "\"10.0.0.1:8080\"";
        let u: UpstreamSpec = serde_json::from_str(j).unwrap();
        assert_eq!(serde_json::to_string(&u).unwrap(), j);
    }

    #[test]
    fn json_那一侧的权重值域与_dsl_那一侧是同一堵墙() {
        // ⚠ 機器直接寫結構化設定也是**公開入口**（G11）：DSL 拒絕而 JSON 收下的話，
        //   就是兩套規則，而 `weight 0` 在其中一條路上會悄悄變成「摘掉」。
        for bad in [
            r#"{"addr":"x:1","weight":0}"#,
            r#"{"addr":"x:1","weight":65536}"#,
        ] {
            let e = serde_json::from_str::<UpstreamSpec>(bad).unwrap_err();
            assert!(
                e.to_string().contains("disable"),
                "0 / 越界要说清怎么改对，实际：{e}"
            );
        }
        // 拼錯的鍵不許被靜默吞掉 —— 吞掉的表現是權重回落成 1，而設定看起來完全正常。
        let e = serde_json::from_str::<UpstreamSpec>(r#"{"addr":"x:1","weigth":3}"#).unwrap_err();
        assert!(e.to_string().contains("weigth"), "{e}");
    }

    // ── `reverse_proxy` 的穩定 id（M2 批 N 任務 2.8，裁決 R6 ⇒ G125）──────────

    /// 一條最樸素的 `reverse_proxy`，`id` 由呼叫方給。
    fn rp(id: Option<&str>) -> StepBody {
        StepBody::ReverseProxy {
            id: id.map(str::to_string),
            upstreams: vec![UpstreamSpec::new("10.0.0.1:8080")],
            lb_policy: "round_robin".into(),
            health: HealthCheck::default(),
            dns_refresh_ms: 30_000,
            passive: Passive::default(),
            header_up: Vec::new(),
            header_down: Vec::new(),
            transport: "http".into(),
            tls_insecure_skip_verify: false,
            proxy_protocol: None,
        }
    }

    /// 沒有 `id` 的那份產物**逐字**長這樣。
    const RP_NO_ID: &str = concat!(
        r#"{"directive":"reverse_proxy","upstreams":["10.0.0.1:8080"],"#,
        r#""lb_policy":"round_robin","#,
        r#""health":{"uri":null,"interval_ms":10000,"timeout_ms":3000,"status":"2xx"},"#,
        r#""dns_refresh_ms":30000,"#,
        r#""passive":{"fail_threshold":null,"window_ms":10000,"cooldown_ms":30000},"#,
        r#""header_up":[],"header_down":[],"transport":"http","#,
        r#""tls_insecure_skip_verify":false,"proxy_protocol":null}"#,
    );

    #[test]
    fn 没写_id_的产物逐字不变() {
        // ★ ★ **硬判據**（任務 2.8 §2 ②）：`id` 這一格必須**完全消失**，
        //   不是寫成 `"id":null`。現有夾具、磁碟上那份結構化設定、`POST /load`
        //   的舊載荷全都靠它逐字不變 —— 於是這一批「有沒有順手改掉別的東西」看得出來。
        // ⚠ 這與本模組 `none_serializes_as_null_so_the_key_set_is_stable`
        //   （`Global` 那條「鍵集合要穩定」）**有意相反**，與 `UpstreamSpec`
        //   的「權重為 1 寫回裸字串」是同一條取捨。
        let j = serde_json::to_string(&rp(None)).unwrap();
        assert_eq!(j, RP_NO_ID, "没写 id 的 reverse_proxy 产物变了");
        assert!(
            !j.contains("\"id\""),
            "没写 id 时不许出现 id 这个键（哪怕值是 null）：{j}"
        );
    }

    #[test]
    fn 写了_id_的产物带得上并且读得回() {
        let j = serde_json::to_string(&rp(Some("pool_web"))).unwrap();
        // ★ 反向判據：少了它，一個「永遠不序列化 id」的實作照樣能讓上面那條綠，
        //   而那樣的 id 一 round-trip 就沒了。
        assert_eq!(
            j,
            RP_NO_ID.replacen(
                r#""directive":"reverse_proxy","#,
                r#""directive":"reverse_proxy","id":"pool_web","#,
                1
            ),
            "写了 id 的产物不对"
        );
        let back: StepBody = serde_json::from_str(&j).unwrap();
        assert_eq!(back, rp(Some("pool_web")));
    }

    #[test]
    fn 旧载荷没有_id_这个字段也收得下() {
        // ★ `POST /load` 是活著的介面：在 `id` 出現之前發出去的那些 JSON 必須繼續能進來。
        let back: StepBody = serde_json::from_str(RP_NO_ID).unwrap();
        assert_eq!(back, rp(None));
        // 往回傳去仍然一個位元組不差。
        assert_eq!(serde_json::to_string(&back).unwrap(), RP_NO_ID);
    }

    #[test]
    fn round_trips_through_json() {
        let cfg = StructuredConfig {
            schema_version: SCHEMA_VERSION,
            global: Global::default(),
            defaults: Defaults::default(),
            sites: vec![SiteConfig {
                addresses: vec![Address {
                    raw: "example.com".into(),
                    scheme: "https".into(),
                    host: "example.com".into(),
                    port: 443,
                    wildcard: false,
                    auto_https: true,
                }],
                matchers: BTreeMap::new(),
                tls: TlsConfig::default(),
                log: None,
                chain: vec![Step {
                    order: 70,
                    matcher: None,
                    body: StepBody::Respond {
                        status: 200,
                        body: None,
                    },
                }],
                error_handler: Vec::new(),
            }],
            l4: None,
        };
        let j = serde_json::to_string(&cfg).unwrap();
        let back: StructuredConfig = serde_json::from_str(&j).unwrap();
        assert_eq!(cfg, back, "机器写这一层，往返必须无损");
    }
}
