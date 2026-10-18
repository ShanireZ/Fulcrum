//! 管理面（**G8 的全量原子 load** + **G74 的強制續期**），走 **Unix domain socket**（G14）。
//!
//! ```text
//! admin unix//run/fulcrum/admin.sock      ← DSL 裡這一行開啟它
//! ```
//!
//! ★ **只綁 Unix domain socket，權限交給檔案系統 ACL**（G14）—— 堵掉 Caddy 那個短處
//! （Admin API 預設綁回環且無認證，同機任意處理程序可改設定）。
//! ⚠ 於是這裡不需要發明一套 token 體系，也沒有「忘了開認證」這種失敗模式。
//!
//! # 提供兩條命令
//!
//! | | 幹什麼 | 載荷 |
//! |---|---|---|
//! | `POST /load?overrides=keep\|clear` | **全量原子 load**（G8）；`overrides` **必填**、走查詢串，⛔ 不接受載荷信封（G120）| 結構化設定 JSON（G48：與磁碟上那份**同一種**）|
//! | `POST /renew` | **強制續期一個網域**（G74）| `{"domain":"…"}`；
//!   加 `"force": true` 時**連退避一起清**（第二檔）|
//!
//! ★ `/renew` 兩檔：預設檔越過「還不到時候」但**不越過退避**（隨手繞開退避的口子
//! 等於給「反覆重簽把 CA 配額燒光」開門，而配額按**帳戶**算）；
//! `"force": true` 把失敗計數清零，用在**根因已經修好**之後，⚠ 每次都打 warn。
//!
//! ★ 載荷是**結構化 JSON 而不是 DSL**（G11 + G48）：機器寫結構化層，
//! 而它與磁碟上那份是同一種格式。人要提交 DSL 的話先 `fulcrum compile`。
//!
//! # ⚠ ⚠ 「原子」到哪兒為止：**監聽連接埠集不能變**
//!
//! Pingora 在**啟動時**綁定監聽器 ⇒ 改了監聽位址的設定在同一個處理程序裡不可能生效，
//! 所以這裡**顯式拒絕**並讓呼叫方走 `systemctl reload`（真的換代，零停機）。
//!
//! > ★ 判據在**拒絕**上，不在「盡力而為」上：**設定變更是交易，不是檔案寫入**。
//! > 一個「連接埠沒換成、別的換成了」的結果**既不是舊狀態也不是新狀態** ——
//! > 那是三種結局裡最壞的一種。

use crate::dns;
use fulcrum_acme::AcmeManager;
use fulcrum_runtime::overrides::{OverrideKey, RuntimeAction, RuntimeOp, UpstreamOverride};
use fulcrum_runtime::{Runtime, SharedRuntime};
use fulcrum_tls::SniResolver;
use log::{error, info, warn};
use pingora_core::apps::{HttpServerApp, ReusedHttpStream};
use pingora_core::protocols::http::ServerSession;
use pingora_core::server::ShutdownWatch;
use std::sync::Arc;

/// 請求體上限。★ 一份結構化設定是幾十 KB 量級；4 MiB 寬出兩個數量級，
/// 而沒有上限就等於讓任何能寫這個 socket 的處理程序把記憶體吃光。
const MAX_BODY: usize = 4 * 1024 * 1024;

/// socket 的權限。**0600 = 只有屬主能讀寫**（G14：權限交給檔案系統 ACL）。
///
/// ⚠ 這個數字就是整個管理面的**全部**訪問控制。放寬它之前請先想清楚：
/// 能寫這個 socket 的處理程序可以**換掉整份設定**。
pub const SOCKET_MODE: u32 = 0o600;

/// 一份執行環境裡的 L4 監聽器集，**已排序**，用來比「換代才換得了的那部分變沒變」。
///
/// ★ 只取 `(协议, 监听地址原样)`：上游是可以隨時改的，改上游不需要重新 bind。
/// ⚠ 抽成函式而不是在兩處各寫一遍 —— 兩處各寫一遍時，
/// 「啟動時記的」與「load 時算的」會在下一次改動裡分家，而分家的表現是這道門**恆綠**。
fn l4_key_set(rt: &Runtime) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = rt
        .l4_listeners
        .iter()
        .map(|l| (l.proto.as_str().to_string(), l.listen.clone()))
        .collect();
    v.sort();
    v
}

pub struct AdminApp {
    rt: Arc<SharedRuntime>,
    /// 啟動時綁定的監聽連接埠集。★ 全量 load 拿它做「連接埠集有沒有變」的判據。
    listen_ports: Vec<(u16, bool)>,
    /// 啟動時綁定的 **L4** 監聽器集（`(协议, 监听地址原样)`）。
    ///
    /// ★ ★ M2 批 A加的，理由與上面那格逐字相同：**L4 連接埠同樣在
    /// 啟動時綁定**，`POST /load` 換不了它。⚠ 少了這一格，一份改了 `l4` 塊的設定
    /// 會 **200「已生效」而那個連接埠紋絲不動** —— 裝載成功、紀錄無異常、
    /// 只有真的去連那個連接埠才發現還是老上游。★ 這正是批 22 那條
    /// 「守衛接在 CLI 上，而管理面是另一個入口」的同族：**一個判據漏掉一個入口，
    /// 等於在那個入口上沒有判據。**
    l4_listeners: Vec<(String, String)>,
    acme: Option<Arc<AcmeManager>>,
    /// 快取後端（M2 批 G）。`None` = 這個處理程序沒建快取（測試夾具裡會是這樣）。
    ///
    /// ★ ★ `purge` 落在**這個** socket 上而不是新開一個連接埠（G84 拍板）：
    /// 與 `/load`、`/renew` 同一套 0600 權限（G14）。⚠ 一個「只是清快取」的
    /// 新入口聽起來無害，而它能被用來把上游打垮 —— 權限面必須只有一個。
    cache: Option<Arc<crate::cache::CacheHandle>>,
    /// 證書到期資料的源（**M2 批 N 任務 6**，`/stats` 的證書到期一節）。
    /// `None` = 這個處理程序沒有 TLS 解析器（測試夾具裡會是這樣）。
    ///
    /// ★ ★ ★ **選的是「給 `AdminApp::new` 加參數」這條路**（brief §4 兩條裡的
    /// 第①條），照抄 `acme` 這一格現成的形狀：production 那一側 `resolver` 與
    /// `metrics::LiveSources.resolver` 拿的是**同一個** `Arc<SniResolver>`
    /// （`lib.rs` 裡都是 `plan.resolver.clone()`）——「同一個物件、兩處持有」
    /// 不是第二份狀態，`acme` 欄位已經是這個形狀的先例（它同樣既在這裡、
    /// 也在 `LiveSources.acme` 裡）。⛔ 沒有走第②條（給 `metrics` 加一個公開
    /// 讀取口）：那條路會讓 `/stats` 在 `register_live()` 串接之前被調時讀到
    /// `None`——而 `AdminApp::new` 在 `lib.rs` 裡恰好排在 `register_live()`
    /// **之後**，看起來今天不會撞上，但那是兩處初始化順序的偶然重合，不是
    /// 結構上的保證；下一次誰調換了那兩行的順序，`/stats` 的證書到期就會在
    /// 沒有任何回報錯誤的情況下悄悄變回「一直沒資料」。「加參數」這條路沒有這個
    /// 時序相依——`AdminApp` 自己持有的 `Arc` 從建構那一刻起就是完整的。
    resolver: Option<Arc<SniResolver>>,
}

// ── `GET /stats` 的回應形狀（**M2 批 N 任務 6**）───────────────────────────
//
// ⚠ 這幾個結構體的欄位名**是 JSON 契約本身**——維運指令碼會直接解析，
// 改名字是破壞性變更。⇒ 與 `/runtime` 的 `Action`/`Req` 不同，這裡不加
// `#[serde(deny_unknown_fields)]`（那是收載荷用的）：這是**吐**載荷，讀的人
// 只應該忽略不認識的欄位，不該因為伺服器端以後新增一格就解析失敗。

/// 每個（站台, id, 上游位址）各一行。★ ★ **不按位址歸併**——R12 的同源判據
/// 要能把這一節按位址求和 `inflight` / 取合取 `healthy`，再與 `/metrics`
/// 逐項相等；歸併了就沒有東西可比。同一個位址被兩條 `reverse_proxy` 引用時，
/// 這裡理應出現兩行（各自獨立的 `inflight`/`healthy`，見 [`FanoutStat`]）。
#[derive(serde::Serialize)]
struct UpstreamStat {
    site: String,
    id: String,
    addr: String,
    inflight: usize,
    healthy: bool,
    /// 被動熔斷此刻把它摘著嗎（**G136**）。
    ///
    /// ★ ★ 它與 `healthy` **各佔一格**，因為那是兩套互不覆蓋的判定 ——
    /// ⚠ 少了這一格，維運會看到一個 `healthy: true` 的上游收不到流量，
    /// 而 `/stats` 裡沒有任何一個欄位說得出為什麼。
    passive_open: bool,
    /// 當前視窗內已經記了幾次失敗（**G136**）。
    ///
    /// ★ 它讓「快熔斷了」這件事在熔斷**之前**就看得見。
    passive_fails: u32,
}

/// 覆蓋層清單的一項（G18「永遠可見」）。★ 三格鍵**原樣**列出，不加工——
/// 維運照抄即可（brief §6）。
#[derive(serde::Serialize)]
struct OverrideStat {
    site: String,
    id: String,
    addr: String,
    disabled: bool,
    /// `None` = 只是 `disable`，沒有改權重。
    weight: Option<u32>,
    dangling: bool,
    /// 這一格**最後一次被設定**的時刻，**絕對 Unix 秒**（R6 留下的那條，owner 拍板）。
    ///
    /// ⚠ ⚠ 語義是「**最後一次改動**」而不是「首次被覆蓋」：維運問的那句話是
    /// 「這臺機器摘掉多久了」，問的是**當前形態**的年齡。⇒ 先改 `weight` 再 `disable`，
    /// 取「首次」的讀數會讓人以為它兩天前就摘了，而其實兩分鐘前才摘。
    ///
    /// ⛔ **不寫「已經過了多少秒」** —— 與 R5 在 [`CertStat::not_after_unix`] 上釘過的
    /// 是同一條：相對秒讀起來完全正常，而它在兩次抓取之間的含義會變。
    /// ★ 換算走 `metrics::unix_secs`，與 `config_loaded_at_unix` / `not_after_unix`
    /// **是同一份**，⛔ 這裡不自己算。
    set_at_unix: f64,
}

/// 一格覆蓋層鍵管著幾條 `reverse_proxy`（[`fulcrum_runtime::ProxyKeyFanout`]
/// 的 JSON 形態）。`proxies` 恆 ≥ 1。
#[derive(serde::Serialize, Clone)]
struct FanoutStat {
    site: String,
    id: String,
    addr: String,
    proxies: usize,
}

#[derive(serde::Serialize)]
struct CacheStat {
    bytes: u64,
    entries: usize,
}

#[derive(serde::Serialize)]
struct CertStat {
    domain: String,
    /// **絕對 Unix 秒**——與 `fulcrum_cert_expiry_seconds` 同一口徑（裁決 R5）。
    not_after_unix: f64,
}

/// `GET /stats` 整份回應體。
#[derive(serde::Serialize)]
struct StatsResponse {
    /// 世代標識（主 agent 裁決）：本處理程序的 pid。⛔ 不是一個新造的「設定世代
    /// 計數器」——`process.rs` 已經在寫 pid 檔案，它是現成且唯一的世代標識。
    pid: u32,
    /// 設定裝載時間，**絕對 Unix 秒**。來自 `SharedRuntime::loaded_at()`
    /// （裁決 R15），本檔案不另算一份。
    config_loaded_at_unix: f64,
    upstreams: Vec<UpstreamStat>,
    overrides: Vec<OverrideStat>,
    /// 全部鍵，恆 ≥ 1 條 `proxies`。
    fanout: Vec<FanoutStat>,
    /// ★ ★ `fanout` 裡 `proxies > 1` 的子集，單獨擺一份——R6 第二輪那個真實
    /// 風險（使用者寫兩條指同一臺的 `reverse_proxy`，可能以為 `disable` 只
    /// 影響一條）的唯一解藥是讓它**顯眼**，⛔ 不能只是混在 `fanout` 裡的
    /// 一個數字（brief §6）。
    fanout_shared: Vec<FanoutStat>,
    /// 快取後端的讀數。`Some` 時哪怕 `bytes`/`entries` 都是 0，也是一次真讀數，
    /// 不是預設值冒充。
    ///
    /// ⚠ ⚠ **當前構型下它恆為非 `null`**（owner 2026-09-03 拍板：`Option` 保留，
    /// 文件改成實話）。理由是**結構性的**：唯一的生產裝配點無條件建構快取句柄，
    /// 再無條件包成 `Some` ⇒ **真二進位吐不出 `"cache": null`**。
    /// ⇒ ⛔ **維運不要為 `null` 寫分支** —— 那會是一段永遠不執行的程式碼。
    /// ★ `Option` 留著是給「將來可能不帶該後端的構型」佔位的，**不是**今天的狀態描述；
    /// 而 `None` 今天只出現在單測的裝配裡。判據在 `tests/stats/run.sh`
    /// （⚠ 它只證得了**當前構型**，那也正是這條契約今天的全部內容）。
    cache: Option<CacheStat>,
    /// 已裝載證書的清單。`Some(vec![])` = 有解析器但零證書（比如整份設定沒有一個
    /// TLS 站台）—— ★ 那與「沒有解析器」是不同的狀態。
    ///
    /// ⚠ ⚠ **當前構型下它同樣恆為非 `null`**，理由與 `cache` 逐字相同（同一處裝配、
    /// 同樣無條件包 `Some`）。⇒ 今天分得開的只有 `[]` 與非空，⛔ 不是 `null` 與非 `null`。
    certs: Option<Vec<CertStat>>,
}

impl AdminApp {
    pub fn new(
        rt: Arc<SharedRuntime>,
        listen_ports: Vec<(u16, bool)>,
        acme: Option<Arc<AcmeManager>>,
        cache: Option<Arc<crate::cache::CacheHandle>>,
        resolver: Option<Arc<SniResolver>>,
    ) -> AdminApp {
        let mut listen_ports = listen_ports;
        listen_ports.sort_unstable();
        // ★ L4 那一格從**當前執行環境**取，而不是另外傳一份進來：
        //   多一個參數就多一處「呼叫方可能忘了給」，而忘了給的後果是這道門變成空的。
        let l4_listeners = l4_key_set(&rt.current());
        AdminApp {
            rt,
            listen_ports,
            l4_listeners,
            acme,
            cache,
            resolver,
        }
    }

    /// 全量原子 load（G8）+ `overrides` 兩檔（**M2 批 N 任務 5**，G120 / R9）。
    ///
    /// # `?overrides=keep|clear`：必填，走**查詢串**
    ///
    /// ⛔ ⛔ 不接受把它塞進載荷（`{"overrides":…,"config":{…}}` 的信封）——
    /// 載荷是結構化設定，與磁碟上那份**同一種**（G48），多包一層信封就是兩種
    /// 格式分了家。缺這個參數、或寫了 `keep`/`clear` 之外的值 ⇒ **400，且舊
    /// 設定一個位元組都不動**——這裡在碰載荷之前就回傳，原子性是結構上必然的。
    ///
    /// ★ ★ 不給預設值就是 G120 的全部內容：發佈流水線要 `clear`（發佈 = 回到
    /// 期望狀態），事故處理中的人要 `keep`（一次無關的發佈不該把剛摘掉的壞
    /// 節點放回去）——兩種現實都對且互相沖突，任何預設值都會在另一種場景裡
    /// 悄悄做錯事。
    ///
    /// `keep` 什麼都不用做——`SharedRuntime::swap` 內部已經調了
    /// `retain_after_swap`，懸空覆蓋天然留著，這裡只需把 `dangling` 的挑出來
    /// 點名（裁決 R8）。`clear` 調
    /// [`fulcrum_runtime::overrides::OverrideLayer::clear_all`] 撤銷全部格子上
    /// 的覆蓋（⛔ 不是物理刪格子——見該方法文件）。⚠ ⚠ **兩檔都必須在回話裡
    /// 逐項列出**（G120 明寫 `clear`，裁決 R8 明寫 `keep` 的懸空）：只給一個
    /// 數字不算數，理由是要避開 HAProxy 那個「runtime 改動在 reload 後無聲無息
    /// 消失」的短處。
    ///
    /// ⚠ `dangling` 是相對**新執行環境**算的 ⇒ 兩檔都必須在 `self.rt.swap()`
    /// **之後**判定——下面的實作把這一段放在 `swap` 之後正是為此。
    fn load(&self, body: &[u8], query: Option<&str>) -> (u16, String) {
        /// `overrides` 查詢參數的兩檔。⛔ 只在這個函式內部用——這個端點自己的
        /// 字面值，仿照 `runtime()`/`purge()` 把小形狀釘在唯一的呼叫點旁邊。
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum Directive {
            /// 覆蓋層原樣留著；新執行環境裡鍵落不到任何上游的那些標 `dangling`，
            /// 回話逐條點名。
            Keep,
            /// 覆蓋層清空；回話逐項列出被清掉的。
            Clear,
        }
        /// `parse_overrides` 解析不出一個乾淨檔位時，三種情形各自要說不同的話
        /// （修復輪 1，評審 F3）——⛔ 不許把「重複」也歸進「值不認識」：維運看到
        /// 「不認識 `keep,clear`」會去查文件，看到「寫了兩次」才會去查自己的
        /// 請求，兩句話導向的排查動作不一樣。
        enum OverridesError {
            /// 一個 `overrides=` 都沒有。
            Missing,
            /// 有且只有一個，但值不是 `keep`/`clear`。
            Invalid,
            /// ⚠ ⚠ ⚠ 出現了不止一個 `overrides=`（不論值是否相同）——
            /// 修復輪 1，評審 F3：G120 的立身之本是「兩種現實互相沖突就不猜」，
            /// 而 `?overrides=keep&overrides=clear` 正是這個形狀的教科書執行個體；
            /// 原實作在第一個 `overrides=` 上就 `return`，把衝突**靜默判成
            /// 第一個**，與「不給預設值」這條紀律背道而馳。⇒ 一律拒絕，
            /// 不去猜"最後一個才算數"還是"第一個才算數"。
            Duplicate,
        }
        /// 從 `uri.query()` 解析。★ ★ 掃完**全部**鍵值對，⛔ 不在第一次命中
        /// 就 `return`——那樣才發現得了第二個 `overrides=`。
        fn parse_overrides(query: Option<&str>) -> Result<Directive, OverridesError> {
            let mut found: Option<&str> = None;
            for pair in query.unwrap_or("").split('&') {
                if let Some(v) = pair.strip_prefix("overrides=") {
                    if found.is_some() {
                        return Err(OverridesError::Duplicate);
                    }
                    found = Some(v);
                }
            }
            match found {
                None => Err(OverridesError::Missing),
                Some("keep") => Ok(Directive::Keep),
                Some("clear") => Ok(Directive::Clear),
                Some(_) => Err(OverridesError::Invalid),
            }
        }

        let directive = match parse_overrides(query) {
            Ok(d) => d,
            Err(OverridesError::Missing) => {
                return (
                    400,
                    "缺 `?overrides=keep` 或 `?overrides=clear`（G120：必填参数，没有\
                     默认值——发布流水线要 `clear`，事故处理中的人要 `keep`，两种现实\
                     都对且互相冲突，任何默认值都会在另一种场景里悄悄做错事），\
                     **没有任何改动生效**。走查询串，不接受载荷里的字段：\
                     `POST /load?overrides=keep` 或 `POST /load?overrides=clear`\n"
                        .to_string(),
                );
            }
            Err(OverridesError::Invalid) => {
                return (
                    400,
                    "`overrides` 只认 `keep` 或 `clear`，**没有任何改动生效**。\n".to_string(),
                );
            }
            Err(OverridesError::Duplicate) => {
                return (
                    400,
                    "`overrides` 出现了不止一次——这是**重复**，不是「值不认识」\
                     （修复轮 1，评审 F3）：G120 的立身之本是两种现实互相冲突就\
                     不猜，`?overrides=keep&overrides=clear` 正是这个形状的教科书\
                     实例，**没有任何改动生效**。一次请求只认一个 `overrides`。\n"
                        .to_string(),
                );
            }
        };

        let cfg: fulcrum_config::StructuredConfig = match serde_json::from_slice(body) {
            Ok(c) => c,
            Err(e) => {
                return (400, format!("载荷不是合法的结构化配置 JSON（G48）：{e}\n"));
            }
        };
        // ── ★ ★ ★ 脫敏產物不許被 load（批 22，真機實測補上）────────
        //
        //   `fulcrum compile` **預設**吐的就是脫敏產物，而這個介面的載荷正是它。
        //   ⚠ 在 example.com 上實測：不擋的話這裡回 **200「已生效」** ——
        //   維運會理所當然地以為那份設定完整生效了，而裡面的憑證是 `«已脱敏»`。
        //
        //   ★ 當時之所以沒炸，只是因為 ACME 那條鏈是**啟動時**建的、沒讀這份載荷；
        //   ⚠ 而「今天恰好沒用到」不是判據 —— 它只是把爆炸推遲到有人讓 ACME 讀它的那天。
        //   ⇒ 在入口擋下，並且**說清楚怎麼重新生成**。
        let redacted = fulcrum_config::secret_guard::redacted_secrets(&cfg);
        if !redacted.is_empty() {
            return (
                400,
                format!(
                    "这些站点的凭据是**脱敏过的**，没有任何改动生效：{}\n                     ★ 它多半来自 `fulcrum compile` 的默认产物 —— 那份 JSON 给人看没问题，\
                     但不能拿来 load。要带凭据的产物：`fulcrum compile <配置> --with-secrets`。\n                     ⚠ 不挡的话，`«已脱敏»` 会被当成凭据发给 CA，\
                     而现场表现是「凭据不对」——与真的凭据写错长得一模一样。\n",
                    redacted.join(" ")
                ),
            );
        }

        // ★ ★ **先整份建起來，建成了才換**。`Runtime::build` 裡做了正則編譯、
        //   CIDR 解析、上游位址解析——任何一條不過就整體不生效，
        //   而**舊設定一個位元組都沒動**。這就是 G8 說的「原子」。
        let next = match Runtime::build(&cfg) {
            Ok(rt) => rt,
            Err(errs) => {
                let mut out = String::from("配置建不起来，**没有任何改动生效**：\n");
                for e in &errs {
                    out.push_str(&format!("  · {e}\n"));
                }
                return (400, out);
            }
        };
        // ⚠ ⚠ **訪問紀錄的檔案要在換之前打開**（M2 批 L 第 ② 步）。
        //   ★ 放在這裡而不是換完之後，是為了守住 G8 的「原子」：
        //   打不開就整份不生效，而**舊設定一個位元組都沒動**。
        //   ⇒ 一份把紀錄路徑寫錯的設定會被**當場拒絕**，
        //   而不是「換上去了、服務正常、紀錄悄悄沒了」。
        if let Err(errs) = crate::access_log::open_all(&next) {
            let mut out = String::from("访问日志的输出文件打不开，**没有任何改动生效**：\n");
            for e in &errs {
                out.push_str(&format!("  · {e}\n"));
            }
            return (400, out);
        }

        // ⚠ 監聽連接埠集變了 = 這個處理程序做不到。**拒絕，而不是換一半。**
        let mut next_ports = next.listen_ports.clone();
        next_ports.sort_unstable();
        if next_ports != self.listen_ports {
            let fmt = |v: &Vec<(u16, bool)>| {
                v.iter()
                    .map(|(p, tls)| format!("{p}{}", if *tls { "(tls)" } else { "" }))
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            return (
                409,
                format!(
                    "监听端口集变了，本进程换不了（Pingora 在启动时绑定）：\n\
                     当前：{}\n提交：{}\n\
                     请改用 `systemctl reload fulcrum` —— 那是真的换代，零停机。\n\
                     ★ 这里拒绝而不是「换掉能换的那部分」：一个端口没换成、别的换成了的结果，\n\
                     既不是旧状态也不是新状态（G8 / 设计原则第 4 条）。\n",
                    fmt(&self.listen_ports),
                    fmt(&next_ports)
                ),
            );
        }
        // ⚠ **L4 監聽器集同樣換不了**（M2 批 A）。理由與上面那條逐字相同：
        //   連接埠在啟動時綁定。★ 判據取的是 `(协议, 监听地址原样)` 的集合 ——
        //   **上游改了照樣放行**，那是這道門有意不管的部分：換上游不需要重新 bind。
        let next_l4 = l4_key_set(&next);
        if next_l4 != self.l4_listeners {
            let fmt = |v: &Vec<(String, String)>| {
                if v.is_empty() {
                    "（没有 L4 监听器）".to_string()
                } else {
                    v.iter()
                        .map(|(p, l)| format!("{p} {l}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                }
            };
            return (
                409,
                format!(
                    "L4 监听器集变了，本进程换不了（监听端口在启动时绑定）：\n\
                     当前：{}\n提交：{}\n\
                     请改用 `systemctl reload fulcrum` —— 那是真的换代，零停机。\n\
                     ★ 只有**监听器**换不了；`proxy` 后面的上游随时可以改，那条路不经过 bind。\n",
                    fmt(&self.l4_listeners),
                    fmt(&next_l4)
                ),
            );
        }
        // ── ⚠ ⚠ **快取後端同樣換不了**（M2 批 H）───────────────────────────
        //
        // 理由與上面兩條同一族：它是**處理程序級**的，在啟動時就打開了那個目錄
        // （`DiskStore::open` 建目錄、讀索引、起後台維護任務）。
        // ★ ★ 不攔的話，一次 `POST /load` 會回 200，而**新寫的 `disk` 目錄一個位元組
        //   都不會被用到** —— 維運那邊看到的是「換成功了」，盤上那個目錄始終是空的，
        //   ⚠ 而裝載紀錄早在啟動時就打過了，不會再說第二遍。
        //   那正是本儲存庫反覆點名的靜默失能，與「監聽連接埠集變了」是同一件事的另一面。
        // ★ 判據取「有沒有 `disk`、是哪一個」，**不看 `ttl` / `max_size` / `capacity`**：
        //   那三樣是每請求現讀 `CacheRt` 的，換設定立刻生效，這裡有意不管。
        //   ⚠ `capacity` 是個例外中的例外：它在啟動時定死了後端容量，改了不生效 ——
        //   已登記在 `PLAN.md` §11，不在本批處置。
        let cur_disk = self
            .rt
            .current()
            .cache_settings()
            .iter()
            .find_map(|(_, c)| c.disk_dir.clone());
        let next_disk = next
            .cache_settings()
            .iter()
            .find_map(|(_, c)| c.disk_dir.clone());
        if cur_disk != next_disk {
            let fmt = |d: &Option<String>| match d {
                Some(v) => format!("磁盘 `{v}`"),
                None => "内存（没写 `disk`）".to_string(),
            };
            return (
                409,
                format!(
                    "缓存后端变了，本进程换不了（目录在启动时打开）：\n\
                     当前：{}\n提交：{}\n\
                     请改用 `systemctl reload fulcrum` —— 那是真的换代，零停机。\n\
                     ★ 这里拒绝而不是「悄悄不换」：一个回了 200、而新目录一个字节都没写过的 \
                     load，比一个红的 load 难查得多。\n",
                    fmt(&cur_disk),
                    fmt(&next_disk)
                ),
            );
        }
        let sites = cfg.sites.len();
        // ★ 未串接能力照舊在這裡說一遍：換了設定之後「哪些寫了但不生效」會變，
        //   而只在啟動時打一次的話，換設定引入的新缺口沒有任何人說。
        for (k, why) in next.unwired_in_use(&cfg) {
            warn!("load：`{k}` 在新配置里被用到，但还没接线 —— {why}");
        }
        // ★ ★ **換之前先把網域上游解析一遍。**
        //   ⚠ 少了這一步，新建出來的 `Upstream` 裡那個槽是空的，於是
        //   **一次 load 會讓所有網域上游短暫地「沒有位址」** —— `pick()` 跳過它們，
        //   用戶端拿到 502，直到後台任務下一次打點。那是一次自找的抖動。
        //   ★ 這裡是阻塞 DNS，但它跑在**管理面**的請求上，不是資料面 ——
        //   一次有意的設定操作等幾十毫秒是應該的。
        dns::resolve_now(&next, "全量 load");
        // ⚠ `swap` 收 `Runtime` 而不是 `Arc<Runtime>`（批 N 任務 3）：裝進來之前
        //   要把臨時覆蓋層的格子掛上去，而那要 `&mut` —— 那是結構保護，別繞過去。
        self.rt.swap(next);

        // ── G120 / R8：`overrides` 完成實作 ────────────────────────────────────
        //   ⚠ ⚠ 必須在 `swap` **之後**：`dangling` 是相對新執行環境算的
        //   （`SharedRuntime::override_entries` 走的是 `current()`）。
        let overrides_report = match directive {
            Directive::Keep => {
                let dangling: Vec<_> = self
                    .rt
                    .override_entries()
                    .into_iter()
                    .filter(|e| e.dangling)
                    .collect();
                if dangling.is_empty() {
                    "overrides=keep：覆盖层原样留着，没有悬空的。\n".to_string()
                } else {
                    let mut out = format!(
                        "overrides=keep：覆盖层原样留着，其中 {} 项现在悬空（键落不到\
                         任何上游，仍然生效中——这不是删，是 keep 的意义所在）：\n",
                        dangling.len()
                    );
                    for e in &dangling {
                        out.push_str(&format!("  · {}\n", e.key));
                    }
                    out
                }
            }
            Directive::Clear => {
                let live = self.rt.current().override_keys();
                let cleared = self.rt.overrides().clear_all(&live);
                if cleared.is_empty() {
                    "overrides=clear：覆盖层本来就是空的，没有可清的。\n".to_string()
                } else {
                    let mut out = format!("overrides=clear：清掉了 {} 项覆盖：\n", cleared.len());
                    for e in &cleared {
                        out.push_str(&format!("  · {}\n", e.key));
                    }
                    out
                }
            }
        };

        // ── D19：快取容量跟著換，並**把生效值說出來** ──────────────────────
        //
        // ★ ★ ★ 缺陷原本的形狀：`ttl` / `max_size` 每請求現讀 `CacheRt`、改了立刻生效，
        //   而 `capacity` 在**啟動時**被抄進了後端的一個欄位 ⇒ 一次 `POST /load`
        //   回 200、`plan` 顯示新值，**實際仍是舊容量，而沒有任何東西會說**。
        //   ⚠ 兩條子指令寫在同一個 `cache { … }` 塊裡，設定檔案上看不出任何區別。
        //
        // ⚠ 它必須在 `swap` **之後**：要按**新**執行環境算容量。
        // ★ 推導只有一處（`cache::effective_capacity`），與 `serve()` 建後端時用的是
        //   同一個函式 —— 抄一遍的話，「啟動時算出來的」與「load 之後算出來的」
        //   會有一天不是同一個數，而兩處各自都自洽。
        let cache_report = match &self.cache {
            Some(c) => {
                let want = crate::cache::effective_capacity(&self.rt.current().cache_settings());
                c.store.set_capacity(want);
                match c.store.capacity() {
                    // ★ ★ 報的是**問後端要回來的那個數**，⛔ 不是剛才那個 `want` ——
                    //   回顯自己剛寫進去的值，對「寫進去了沒有」這件事零判別力。
                    Some(now) => format!(
                        "缓存容量：生效 {now}B（各 `cache` 块取最大值）。\
                         ⚠ 调小是**惰性**的：到下一次写入之前，占用仍可能高于新上限。\n"
                    ),
                    None => "缓存容量：后端关着（配了 `disk` 而那个目录用不了），\
                             容量无从谈起 —— 启动日志里那行 error 说了为什么。\n"
                        .to_string(),
                }
            }
            // ⚠ 沒有快取後端時**一個字都不說**：一句「本處理程序沒有快取」會在每一次
            //   load 的回話裡重複出現，而它與這次 load 做了什麼毫無關係。
            None => String::new(),
        };

        info!("全量 load 生效：{sites} 个站点（G8：原子换整份）");
        (
            200,
            format!("已生效：{sites} 个站点\n{overrides_report}{cache_report}"),
        )
    }

    /// 強制續期（G74）。
    /// 清快取（**M2 批 G**，G84：`purge` 走管理面）。
    ///
    /// 載荷三選一：
    /// - `{"key": "<主键>"}` —— 清那一條 URL（含它 `Vary` 出來的全部次級鍵）
    /// - `{"prefix": "<主键前缀>"}` —— 清一批
    /// - `{"all": true}` —— 全清
    ///
    /// ⚠ ⚠ **主鍵不是 URL**：它是 `方法\u{1}scheme\u{1}host\u{1}路径\u{1}查询串`。
    /// ★ 直接收 URL 聽起來友好，而它要求管理面**重新實作一遍**快取鍵的演算法 ——
    /// 兩份演算法遲早分家，而分家的現場是「purge 說清了 1 條，但下次請求還是命中」。
    /// ⇒ 這裡收**主鍵本身**，並接受 `method` / `scheme` 幾個欄位替呼叫方拼。
    fn purge(&self, body: &[u8]) -> (u16, String) {
        #[derive(serde::Deserialize)]
        struct Req {
            #[serde(default)]
            key: Option<String>,
            #[serde(default)]
            prefix: Option<String>,
            #[serde(default)]
            all: bool,
            /// 便利欄位：給了 `url` 就按這幾樣替呼叫方拼主鍵。
            #[serde(default)]
            url: Option<UrlKey>,
        }
        #[derive(serde::Deserialize)]
        struct UrlKey {
            #[serde(default = "get_method")]
            method: String,
            #[serde(default = "https_scheme")]
            scheme: String,
            host: String,
            path: String,
            #[serde(default)]
            query: String,
        }
        fn get_method() -> String {
            "GET".to_string()
        }
        fn https_scheme() -> String {
            "https".to_string()
        }

        let req: Req = match serde_json::from_slice(body) {
            Ok(r) => r,
            Err(e) => {
                return (
                    400,
                    format!(
                        "载荷要是 {{\"key\":…}} / {{\"prefix\":…}} / {{\"all\":true}} / {{\"url\":{{…}}}}：{e}\n"
                    ),
                );
            }
        };
        let Some(cache) = &self.cache else {
            // ★ 409 而不是 404：路徑是對的，只是這個處理程序沒有快取。
            //   ⚠ 回 404 的話，讀起來像「這個版本沒有 purge 這個功能」。
            return (
                409,
                "这个进程没有缓存后端（配置里没有 `cache` 指令），没什么可清的\n".to_string(),
            );
        };

        // ⚠ ⚠ **磁碟後端下這幾條會走遍整個快取目錄樹**（`purge_prefix` / `purge_all`
        //   以**盤**為準而不是以索引為準，理由見 `disk::DiskStore::purge_prefix`）。
        //   ★ 那是阻塞 I/O，可能要好幾秒 —— `block_in_place` 告訴 tokio 這條工作
        //   執行緒要卡一會兒，讓它先把別的任務挪走。⚠ 少了它，一次 purge 會把
        //   **排在同一條執行緒上的請求**一起堵住，而現場是「按了一下清快取，
        //   網站卡了三秒」—— 沒有任何東西會把這兩件事聯繫起來。
        //   ★ 記憶體後端下它幾乎沒有代價（那幾個操作是純記憶體的）。
        let n = tokio::task::block_in_place(|| {
            if req.all {
                Some(cache.store.purge_all())
            } else if let Some(u) = &req.url {
                let k =
                    crate::cache::key::primary(&u.method, &u.scheme, &u.host, &u.path, &u.query);
                Some(cache.store.purge_primary(&k))
            } else if let Some(k) = &req.key {
                Some(cache.store.purge_primary(k))
            } else {
                req.prefix.as_ref().map(|pfx| cache.store.purge_prefix(pfx))
            }
        });
        let Some(n) = n else {
            return (
                400,
                "要给 `key` / `prefix` / `url` / `all` 之一\n".to_string(),
            );
        };
        // ── 被清掉的條目數：自成一族（G123，D31 結案）──────────────────────
        //
        // ★ 記的是**被清掉的條目數**，不是「purge 被調了幾次」——
        //   問「清掉了多少」的人遠多於問「這個介面被調了幾次」的人。
        // ⚠ ⚠ 正因如此它**自成一族**，而不是 `cache_events_total` 的第四個值：
        //   那個族數的是「一條請求」，本族數的是「一條快取條目」
        //   ⇒ 混在一起時 `sum(cache_events_total)` 不是任何一個有意義的量。
        //   ★ 拆開之後那個陷阱**結構上不存在**了，不再靠誰記得住。
        //   口徑的權威在 `docs/architecture/observability.md` 與 `PLAN.md` §10 G123。
        // ⚠ 一條都沒清掉時也照記（`+0`）：那讓這條 series **存在**，而
        //   「這個處理程序從來沒 purge 過」與「purge 過、只是沒清到東西」是兩件事，
        //   在抓取端要分得開。
        crate::cache::record_purged_entries(n as u64);
        let (used, left) = cache.store.stats();
        // ★ **清掉 0 條也是 200**：purge 的語義是「讓它不在」，而它本來就不在
        //   同樣滿足這個語義。⚠ 回 404 會讓「清一個從沒被快取過的 URL」
        //   看起來像失敗，於是指令碼裡到處是重試。
        (
            200,
            format!("清掉 {n} 条；现在还剩 {left} 条 / {used} 字节\n"),
        )
    }

    fn renew(&self, body: &[u8]) -> (u16, String) {
        #[derive(serde::Deserialize)]
        struct Req {
            domain: String,
            /// ★ 預設 `false` —— **預設不越過退避**，見 `request_renew` 的兩檔表。
            #[serde(default)]
            force: bool,
        }
        let req: Req = match serde_json::from_slice(body) {
            Ok(r) => r,
            Err(e) => return (400, format!("载荷要是 {{\"domain\":\"…\"}}：{e}\n")),
        };
        let Some(acme) = &self.acme else {
            return (
                409,
                "这个进程没有开自动签发（配置里没有需要自动签的站点），没什么可续的\n".to_string(),
            );
        };
        if acme.request_renew(&req.domain, req.force) {
            let tail = if req.force {
                "★ 带了 `\"force\": true`：**失败计数会被清零**，退避随之消失，本轮立刻试。\n\
                 ⚠ 根因没修好的话，下一次失败会从头开始重新长 —— 它不是「重试按钮」。\n"
            } else {
                "⚠ 它**不越过退避**——上一次刚失败过的话会先等退避到点。\n\
                 ★ 根因已经修好、想立刻重试，加 `\"force\": true`。\n"
            };
            (
                202,
                format!(
                    "已排上：{}。★ 巡检已被叫醒；结果看日志里那条「ACME 签发成功」。\n{tail}",
                    req.domain
                ),
            )
        } else {
            (
                404,
                format!(
                    "{} 不在本进程的自动签发目标里，什么都没做。\n\
                     ★ 目标是从配置里算出来的：站点要走自动 HTTPS，通配符还要配 `tls {{ dns … }}`。\n",
                    req.domain
                ),
            )
        }
    }

    /// `POST /runtime`：增量通道，G8 的**動詞**那半（**M2 批 N 任務 4**，裁決 R10）。
    ///
    /// 管理面收動詞，直接改任務 3 建好的覆蓋層格子——★ ★ 不重建 `Runtime`、不 swap：
    /// 覆蓋格子是 `Arc<UpstreamOverride>`，與活著的 `Upstream` 身上那一份**是同一個
    /// 物件**（任務 3 判據 1，`Arc::ptr_eq` 釘住）。改登記處那一格，當場就作用到
    /// 正在跑的上游身上——這是 R7 的全部意義。
    ///
    /// # ★ ★ ★ 全有或全無
    ///
    /// 分兩個階段：
    /// 1. **純解析**——檢核 `verb` 認不認識、`weight` 的值域。值域**不在這裡另寫
    ///    一份**：借 [`UpstreamOverride::set_weight`] 自己的判斷，對著一格臨時的
    ///    `UpstreamOverride::default()` 跑一遍，只借判斷、不碰任何真實狀態。
    /// 2. **尋址 + 施加**——在 [`fulcrum_runtime::overrides::OverrideLayer::apply_all`]
    ///    的**同一次持鎖**內完成。⛔ 不許拆成「先 `get()` 拿 `Arc`、再在鎖外面改」：
    ///    中間可能被一次 `/load` 搶先把格子收走，改的就是一個孤兒——命令回 200，
    ///    覆蓋靜默丟失。`apply_all` 把「查存在」與「改」焊在同一把鎖裡，這也順帶
    ///    讓「全有或全無」成為**結構上的**：一次持鎖，要麼全改，要麼一條不改。
    ///
    /// 兩個階段的任何一步不過，**一條都不生效**。
    fn runtime(&self, body: &[u8]) -> (u16, String) {
        /// ⛔ **只認這一種形狀**：`{"actions":[…]}`。不接受「單條不帶陣列」的
        /// 第二種寫法——兩種形狀是兩個解析器，兩個解析器遲早分家。
        ///
        /// ★ ★ ★ `deny_unknown_fields`（修復輪 1，評審 M6）：拼錯的欄位名要 400，
        /// ⛔ 不許靜默丟掉。`/load` 的載荷（`StructuredConfig`）不能這樣收緊——
        /// 那是編譯器的產物、另有消費者，嚴格會變成陷阱；但 `/runtime` 的欄位集
        /// （`verb`/`site`/`id`/`upstream`/`weight`）是**這個端點自己的字面值**，
        /// 嚴格是對的。★ 決定性理由：這一筆是這個端點剛誕生的那一刻——
        /// 還沒有任何外部呼叫方，現在收緊免費，晚一步就是破壞性變更。
        /// 與"別的動詞帶 weight 就拒絕"（見下面 `if a.verb != "set_weight" …`）
        /// 是同一條紀律：本任務選擇在這個端點上不靜默丟欄位。
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Req {
            actions: Vec<Action>,
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Action {
            verb: String,
            site: String,
            /// **選填**，不寫 = 空串（★ 主 agent 裁決：`id` 不做任何檢核，只逐字
            /// 精確匹配登記處的鍵）。
            #[serde(default)]
            id: String,
            /// ⛔ ⛔ ⛔ **逐字精確匹配，不做歸一化**（主 agent 裁決，按 R6 ⑤）：
            /// 管理面對著的是執行環境，執行環境裡那個上游就叫它歸一化之後的名字；
            /// 寫設定原文（比如 `backend`）指不到時，400 訊息會把該站台下全部
            /// 合法的鍵列出來，照抄即可——做歸一化會讓那份清單變成死程式碼。
            upstream: String,
            /// 只對 `verb: "set_weight"` 有意義。
            #[serde(default)]
            weight: Option<u32>,
        }

        let req: Req = match serde_json::from_slice(body) {
            Ok(r) => r,
            Err(e) => {
                return (
                    400,
                    format!("载荷要是 {{\"actions\":[…]}}（不接受单条不带数组的写法）：{e}\n"),
                );
            }
        };
        if req.actions.is_empty() {
            return (400, "`actions` 不能是空数组\n".to_string());
        }

        // ── 階段一：純解析 + 值域檢核，不碰登記處 ────────────────────────
        let mut resolved: Vec<RuntimeAction> = Vec::with_capacity(req.actions.len());
        for a in &req.actions {
            // ⚠ 「別的動詞帶了 weight 怎麼辦」是本任務自己的決定（brief 要求釘一條
            //   判據）：選**拒絕**而不是**忽略**——靜默丟掉呼叫方明確給的欄位，
            //   與本儲存庫反覆堵的那類「設定照過、命令回 200、其實沒全生效」是同一族。
            if a.verb != "set_weight" && a.weight.is_some() {
                return (
                    400,
                    format!(
                        "verb `{}` 不接受 `weight` 字段（站点 {}·上游 {}）——\
                         `weight` 只对 `set_weight` 有意义\n",
                        a.verb, a.site, a.upstream
                    ),
                );
            }
            let op = match a.verb.as_str() {
                "disable" => RuntimeOp::SetDisabled(true),
                "enable" => RuntimeOp::SetDisabled(false),
                "set_weight" => {
                    let Some(w) = a.weight else {
                        return (
                            400,
                            format!(
                                "verb `set_weight` 要带 `weight`（站点 {}·上游 {}）\n",
                                a.site, a.upstream
                            ),
                        );
                    };
                    // ★ ⛔ 值域不在這裡另寫一份：借 `UpstreamOverride::set_weight`
                    //   自己的判斷，對著一格臨時格子跑一遍——真正的那次施加在
                    //   階段二，落在登記處自己的鎖裡。
                    if let Err(e) = UpstreamOverride::default().set_weight(w) {
                        return (400, format!("{e}\n"));
                    }
                    RuntimeOp::SetWeight(w)
                }
                other => return (400, format!("不认识的 verb：`{other}`\n")),
            };
            resolved.push(RuntimeAction {
                key: OverrideKey::new(&a.site, &a.id, &a.upstream),
                op,
            });
        }

        // ── 階段二：尋址 + 施加，`OverrideLayer::apply_all` 一次持鎖做完 ─────
        let live = self.rt.current().override_keys();
        match self.rt.overrides().apply_all(&live, &resolved) {
            Ok(()) => (200, format!("已生效：{} 条动作\n", resolved.len())),
            Err(missing) => {
                let mut out = String::from("指不到这些上游，一条都没生效：\n");
                for k in &missing {
                    out.push_str(&format!("  · {k}\n"));
                }
                // ★ R6 ⑤ 明確要求：逐條列出**該站台下**全部合法的鍵，維運照抄即可。
                let mut sites: Vec<&str> = missing.iter().map(|k| k.site.as_str()).collect();
                sites.sort_unstable();
                sites.dedup();
                for site in sites {
                    out.push_str(&format!("站点 {site} 下现在有这些键：\n"));
                    let mut found = false;
                    for k in live.iter().filter(|k| k.site == site) {
                        out.push_str(&format!("  · {k}\n"));
                        found = true;
                    }
                    if !found {
                        out.push_str("  （这个站点现在没有任何上游键——站点名是不是也写错了？）\n");
                    }
                }
                (400, out)
            }
        }
    }

    /// `GET /stats`：唯讀快照（**M2 批 N 任務 6**，G16 / G18「永遠可見」那半，
    /// R12「與 `/metrics` 同源」）。
    ///
    /// ⚠ ⚠ ⚠ **每次都從 `self.rt.current()` 現讀，絕不快取一份 `Runtime`**——
    /// `metrics.rs` 的 [`crate::metrics::LiveSources`] 類型文件已經點過名：
    /// 握著快照等於「指標停在換設定之前」。`self.rt` 與
    /// `metrics::LiveSources.runtime` 是**同一個** `Arc<SharedRuntime>`
    /// （production 那一側 `lib.rs` 兩處都是 `shared.clone()`），只要這裡現讀，
    /// 就與 `/metrics` 讀的是同一組原子量——R12 的「同源」由此成立，不需要
    /// 這裡與 `metrics.rs` 共用一份歸併程式碼（那是兩回事：R12 要求「同一個
    /// 原子量源」，不要求「同一段歸併演算法」；本節按站台 × 上游列、不歸併，
    /// 歸併只發生在 `/metrics` 那一側，與判據指令碼裡）。
    fn stats(&self) -> String {
        // ★ ★ 一次呼叫只取一份快照——理由見上面的方法文件；下面對 `rt` 的
        //   全部讀取都基於這同一個 `Arc<Runtime>`，不會跨兩份設定。
        //   ⚠ ⚠ ⚠ 這句話必須是**真的**：`overrides` 那一節走
        //   `SharedRuntime::override_entries_of(&rt)`（吃這裡已經拿到的
        //   `rt`），⛔ 不許調不吃參數的 `override_entries()`——那個內部會
        //   再調一次 `self.rt.current()`，一次並行的 `POST /load` 落在兩次
        //   之間，就會吐出「`dangling` 按新設定算、別的欄位按舊設定列」的
        //   一份 JSON（修復輪 1，評審 I3）。
        let rt = self.rt.current();

        // ── 每個（站台, id, 上游位址）各一行（R12：不按位址歸併）────────────
        let mut upstreams = Vec::new();
        for kp in rt.keyed_proxies() {
            for up in &kp.target.upstreams {
                upstreams.push(UpstreamStat {
                    site: kp.site.to_string(),
                    id: kp.id.to_string(),
                    addr: up.addr.clone(),
                    inflight: up.inflight(),
                    healthy: up.is_healthy(),
                    passive_open: up.passive_open(),
                    passive_fails: up.passive_fails(),
                });
            }
        }

        // ── overrides 清單：取數口是 SharedRuntime::override_entries_of ─────
        //   ⛔ 別在這裡自己拼 live 再調 OverrideLayer::entries——道理與
        //   `SharedRuntime::override_entries` 文件上那句逐字相同。
        //   ⚠ ⚠ 傳 `&rt`（已經拿在手裡的那一份），不調不吃參數的
        //   `override_entries()`——那個會再調一次 `current()`，見上面
        //   函式開頭那段註解（修復輪 1，評審 I3）。
        let overrides: Vec<OverrideStat> = self
            .rt
            .override_entries_of(&rt)
            .into_iter()
            .map(|e| OverrideStat {
                site: e.key.site,
                id: e.key.id,
                addr: e.key.addr,
                disabled: e.disabled,
                weight: e.weight,
                dangling: e.dangling,
                // ★ 從**那一格自己**帶過來的時刻換算，⛔ 這裡不取 `now()`：
                //   在這兒取一次 now() 會讓每一行都印同一個數，而那對每一格都是錯的。
                set_at_unix: crate::metrics::unix_secs(e.set_at),
            })
            .collect();

        // ── fanout：唯一取數口是 Runtime::proxy_key_fanout ──────────────────
        //   ⛔ 別再寫一份「數一數同鍵有幾條」的遍歷——那與任務 3 的判據
        //   6/7 守的是同一張圖，兩份拼法遲早分家。
        let fanout: Vec<FanoutStat> = rt
            .proxy_key_fanout()
            .into_iter()
            .map(|f| FanoutStat {
                site: f.site.to_string(),
                id: f.id.to_string(),
                addr: f.addr.to_string(),
                proxies: f.proxies,
            })
            .collect();
        let fanout_shared: Vec<FanoutStat> =
            fanout.iter().filter(|f| f.proxies > 1).cloned().collect();

        // ── 快取佔用：`None` = 沒有快取後端，不是 0 冒充 ────────────────────
        let cache = self.cache.as_ref().map(|c| {
            let (bytes, entries) = c.store.stats();
            CacheStat { bytes, entries }
        });

        // ── 證書到期：`None` = 沒有 TLS 解析器；`Some(vec![])` = 有解析器
        //    但零證書——兩者是不同的狀態，別用同一個空值表示 ────────────────
        let certs = self.resolver.as_ref().map(|r| {
            r.expiries()
                .into_iter()
                .map(|(domain, not_after)| CertStat {
                    domain,
                    not_after_unix: crate::metrics::unix_secs(not_after),
                })
                .collect::<Vec<_>>()
        });

        let resp = StatsResponse {
            pid: std::process::id(),
            // R15：裝載時刻只有一份，存在 SharedRuntime 上，這裡唯讀不算。
            config_loaded_at_unix: crate::metrics::unix_secs(self.rt.loaded_at()),
            upstreams,
            overrides,
            fanout,
            fanout_shared,
            cache,
            certs,
        };
        // ★ 這裡不會因為資料形狀 panic：全部欄位都是 String/bool/數字/Option/
        //   Vec，`unix_secs` 回的是有窮 f64（1970 之前也只是負數，不是 NaN/Inf）。
        //   `unwrap_or_else` 純粹是防禦性收尾，不是預期會走到的分支。
        serde_json::to_string(&resp).unwrap_or_else(|e| {
            error!("/stats 序列化失败（不该发生）：{e}");
            "{}".to_string()
        })
    }
}

/// 寫 JSON 回應——★ ★ **只給 `GET /stats` 用**，與 [`reply`] 分開的原因是
/// 兩者的回應契約本來就不同：`reply` 的正文是 text/plain，並且**必須**在
/// 尾部追加 R11 那句覆蓋層計數（G18）；`/stats` 的正文是 JSON，追加一行純
/// 文字會把它撕成一份不合法的 JSON。⚠ ⚠ 這不是遺漏 R11——`/stats` 自己的
/// `overrides` 一節已經把覆蓋層的**全量**清單擺出來了（比 R11 那一行「N 項
/// 生效中，M 項懸空」詳細得多），G18「永遠可見」在這裡由 JSON 正文字身
/// 承擔，不需要再借 `reply()` 那一行文字。
async fn reply_json(session: &mut ServerSession, status: u16, json: &str) {
    let mut h = match pingora_http::ResponseHeader::build(status, None) {
        Ok(h) => h,
        Err(e) => {
            error!("管理面建不出响应头：{e}");
            return;
        }
    };
    let body = bytes::Bytes::from(json.to_string());
    let _ = h.insert_header("Content-Type", "application/json; charset=utf-8");
    let _ = h.insert_header("Content-Length", body.len().to_string());
    if session.write_response_header(Box::new(h)).await.is_err() {
        return;
    }
    let _ = session.write_response_body(body, true).await;
}

#[async_trait::async_trait]
impl HttpServerApp for AdminApp {
    async fn process_new_http(
        self: &Arc<Self>,
        mut session: ServerSession,
        _shutdown: &ShutdownWatch,
    ) -> Option<ReusedHttpStream> {
        match session.read_request().await {
            Ok(true) => {}
            _ => return None,
        }
        // 管理面不做 keep-alive：它一天被調幾次，省下的那點握手不值得多一條狀態。
        session.set_keepalive(None);

        let method = session.req_header().method.clone();
        let path = session.req_header().uri.path().to_string();
        // ⚠ `overrides` 走查詢串（R9），不是路徑的一部分——`uri.path()` 已實測
        //   是 path-only，加查詢串不影響上面 `path` 的路由判定。這裡同樣先
        //   `.to_string()`：下面還要 `&mut session` 讀 body，不能帶著借用過去。
        let query = session.req_header().uri.query().map(|s| s.to_string());

        // 讀 body，帶上界。
        let mut body = Vec::new();
        loop {
            match session.read_request_body().await {
                Ok(Some(chunk)) => {
                    if body.len() + chunk.len() > MAX_BODY {
                        // ★ ★ ★ R11「每一次回應」就是每一次：這條 413 在
                        //   路由散佈**之外**——歷史上最容易漏掉計數行的呼叫點
                        //   （brief 點名）。`reply()` 把 `&self.rt` 設成必填參數，
                        //   讓「這個呼叫點忘了帶」在結構上做不到。
                        reply(&mut session, 413, "载荷太大\n", &self.rt).await;
                        return None;
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(e) => {
                    warn!("管理面读 body 失败：{e}");
                    return None;
                }
            }
        }

        // ── GET /stats：唯讀快照，走**獨立**的 JSON 寫出口 ────────────────
        //
        // ★ ★ 排在下面那個 `match` 之外、單獨判斷，而不是塞進同一個
        //   `match` 再把 `reply()` 換成 `reply_json()`：`/stats` 的回應契約
        //   本來就與另外四條不同（JSON、不追加 R11 計數行），硬塞進同一條
        //   散佈路徑只會讓 `(u16, String)` 這個回傳類型背上第二種含義。
        //   ⚠ ⚠ 但它仍然經過上面同一段真實的 HTTP 解析（method/path/body
        //   讀取）——不是繞開路由散佈另起一條近路，任務 4 那次「16 條判據
        //   全部直接調 handler，路由被刪都測不出來」的教訓在這裡不成立：
        //   下面的分支與 `unknown_route` 共用同一份 `path.as_str()` 匹配，
        //   刪掉這一段的話請求會落進 `_ => unknown_route(...)`，回 404，
        //   而 404 的判據（見下方 `unknown_route`）會紅。
        if method.as_str() == "GET" && path.as_str() == "/stats" {
            reply_json(&mut session, 200, &self.stats()).await;
            return None;
        }

        let (status, text) = match (method.as_str(), path.as_str()) {
            ("POST", "/load") => self.load(&body, query.as_deref()),
            ("POST", "/renew") => self.renew(&body),
            ("POST", "/purge") => self.purge(&body),
            ("POST", "/runtime") => self.runtime(&body),
            // ⚠ 不認識的路徑回 404 而不是 200：一個「什麼都收下」的管理面
            //   會讓打錯的命令看起來成功了。
            _ => unknown_route(method.as_str(), path.as_str()),
        };
        reply(&mut session, status, &text, &self.rt).await;
        None
    }
}

/// `_ =>` 分支的說明文字：不認識的路徑回 404 而不是 200——一個「什麼都收下」的
/// 管理面會讓打錯的命令看起來成功了。
///
/// ★ ★ 抽成自由函式是為了讓「`/runtime` 必須出現在這份『可用』清單裡」
/// （**M2 批 N 任務 4** 判據 11）不必驅動一整條真實的 HTTP 連接就能測——
/// 少了它，打錯命令的人看不到這個新入口存在。
fn unknown_route(method: &str, path: &str) -> (u16, String) {
    (
        404,
        format!(
            "不认识：{method} {path}\n可用：POST /load（全量原子 load）、POST /renew（强制续期）、POST /purge（清缓存）、POST /runtime（增量改覆盖层）、GET /stats（只读快照）\n"
        ),
    )
}

/// 寫回回應——★ ★ ★ 也是 R11「管理面的每一次回應都帶覆蓋層計數」（G18）完成實作
/// 的**唯一**地方。
///
/// ⚠ ⚠ `rt` 是**必填**參數，不是「算好了順手傳一個」：`process_new_http` 裡
/// `reply()` 有兩個呼叫點（413 那條在路由散佈之外），讓每個 handler 各自拼
/// 一次計數行會是 N 處抄件——加第五條路徑時必然漏一次。把它做進 `reply()`
/// 自己，「某個呼叫點忘了帶」在結構上做不到——與本儲存庫把 `Upstream::weight`
/// 設成私有欄位是同一條紀律。
async fn reply(session: &mut ServerSession, status: u16, text: &str, rt: &Arc<SharedRuntime>) {
    let mut h = match pingora_http::ResponseHeader::build(status, None) {
        Ok(h) => h,
        Err(e) => {
            error!("管理面建不出响应头：{e}");
            return;
        }
    };
    // ── R11：尾部追加覆蓋層計數，200/400/404/409/413 一個不漏 ────────────
    let (n, m) = rt.override_counts();
    let mut full = text.to_string();
    full.push_str(&format!("当前有 {n} 项临时覆盖生效中（其中 {m} 项悬空）\n"));
    let body = bytes::Bytes::from(full);
    let _ = h.insert_header("Content-Type", "text/plain; charset=utf-8");
    let _ = h.insert_header("Content-Length", body.len().to_string());
    if session.write_response_header(Box::new(h)).await.is_err() {
        return;
    }
    let _ = session.write_response_body(body, true).await;
}

/// 從 `admin` 那一行裡摳出 socket 路徑。只認 `unix/<路径>`（G14）。
///
/// ⚠ **不認 `:2019` 這種連接埠寫法**，而且要回報錯誤而不是靜靜忽略：
/// 一個「寫了連接埠、結果綁了個 socket」或者「寫了什麼都不生效」的管理面，
/// 是最壞的一種——維運以為它開著。
pub fn socket_path(spec: &str) -> Result<String, String> {
    match spec.strip_prefix("unix/") {
        Some(p) if !p.is_empty() => Ok(p.to_string()),
        _ => Err(format!(
            "`admin {spec}` 不认得。管理面只绑 Unix socket（G14），写成 `admin unix/<路径>`，\
             例如 `admin unix//run/fulcrum/admin.sock`"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 只认_unix_前缀的写法() {
        assert_eq!(
            socket_path("unix//run/fulcrum/admin.sock").unwrap(),
            "/run/fulcrum/admin.sock"
        );
        // ★ ★ 判據的另一半：連接埠寫法必須**回報錯誤**，不能靜靜忽略。
        //   一個寫了 `admin :2019` 卻什麼都不發生的管理面，維運會以為它開著——
        //   而那正是 G14 要堵的 Caddy 那個短處的鏡像版本。
        for bad in [":2019", "127.0.0.1:2019", "unix/", "tcp//x", ""] {
            let e = socket_path(bad).unwrap_err();
            assert!(e.contains("G14"), "`{bad}` 的错误应当点名 G14：{e}");
        }
    }

    #[test]
    fn 脱敏产物_load_必须被拒而不是回_200() {
        // ★ ★ ★ **這條判據是真機實測補上的。**
        //
        //   `fulcrum compile` **預設**吐脫敏產物，而這個介面的載荷正是它 ——
        //   實測不擋的話回的是 **200「已生效：4 個站台」**，
        //   而裡面的憑證是 `«已脱敏»`。⚠ 維運會理所當然地以為設定完整生效了。
        //
        //   ⚠ 當時之所以沒炸，只因為 ACME 那條鏈是**啟動時**建的、沒讀這份載荷 ——
        //   而「今天恰好沒用到」不是判據，它只是把爆炸推遲到有人讓 ACME 讀它的那天。
        let a = app(vec![(8080, false)]);
        let dsl = format!(
            "http://a.com:8080 {{\n  respond 200\n}}\n\
             *.a.com {{\n  tls {{\n    dns dnspod {}\n    zones a.com\n    resolvers 1.1.1.1\n  }}\n  respond 200\n}}\n",
            fulcrum_config::secret::REDACTED
        );
        let (code, body) = a.load(&json_of(&dsl), Some("overrides=clear"));
        assert_eq!(code, 400, "脱敏产物竟然被接受了：{body}");
        assert!(
            body.contains("--with-secrets"),
            "错误里要说清怎么重新生成：{body}"
        );
        assert!(body.contains("没有任何改动生效"), "要说清没生效：{body}");
    }

    #[test]
    fn 带真凭据的载荷照常_load_得进去() {
        // ⚠ 與上一條配對：只測「脫敏的會被拒」的話，
        //   一個把**所有**帶憑證的設定都拒掉的實作會讓上面那條照常綠，
        //   而那樣 `POST /load` 對原生 DNS 供應商就整個不能用了。
        // ⚠ 連接埠集要給全：`*.a.com` 帶來 443，而**自動 HTTP 重定向**（批 21）
        //   還會合成一個 :80 的站台 —— 少給一個，這條測試會紅在 409 上，
        //   而那與它想驗的事情毫無關係。
        let a = app(vec![(80, false), (443, true), (8080, false)]);
        let dsl = "http://a.com:8080 {\n  respond 200\n}\n\
                   *.a.com {\n  tls {\n    dns dnspod 12345,abcdefabcdef\n    zones a.com\n    resolvers 1.1.1.1\n  }\n  respond 200\n}\n";
        // ★ 載荷要帶真值，所以生成時進 reveal 作用域 —— 這正是 `compile --with-secrets` 做的事。
        let cfg = fulcrum_config::compile_str("t.Fulcrumfile", dsl)
            .config
            .unwrap();
        let body = fulcrum_config::secret::reveal(|| serde_json::to_vec(&cfg).unwrap());
        let (code, out) = a.load(&body, Some("overrides=clear"));
        assert_eq!(code, 200, "带真凭据的载荷被拒了：{out}");
    }

    // ── D19：`POST /load` 改了 `capacity` 要真的生效，並把生效值說出來 ───────
    //
    // ★ ★ ★ 這條是整個 D19 的**承重判據**：上面兩個 store 的測試只證明了
    //   「容量改得動」，⚠ 而缺陷在於**沒有人去改它** —— `load` 換的是 `self.rt`，
    //   後端從不重建 ⇒ 只補 `set_capacity` 而忘了在 `load` 裡調，那兩條照樣全綠。
    //
    // ★ 回話那半（候選 ③）不是錦上添花：實測 `capacity` **今天沒有任何執行環境出口**
    //   —— 啟動紀錄說一次「生效容量」，之後 metrics 裡沒有、API 裡也沒有。
    //   ⇒ 光改成線上生效，維運仍然無從核對它到底生效了沒。
    #[test]
    fn load_改了_capacity_后端容量跟着变_且回话说出生效值() {
        let (a, cache) = app_with_cache(vec![(8080, false)], 1024);
        assert_eq!(cache.store.capacity(), Some(1024), "前提：起始容量 1024");

        let dsl = "http://a.com:8080 {\n  reverse_proxy 127.0.0.1:9\n  \
                   cache {\n    capacity 4096\n  }\n}\n";
        let (code, body) = a.load(&json_of(dsl), Some("overrides=clear"));
        assert_eq!(code, 200, "{body}");
        assert_eq!(
            cache.store.capacity(),
            Some(4096),
            "★ 承重：load 之后后端容量该跟着配置走 —— 这一格红就是 D19 本身"
        );
        assert!(
            body.contains("4096"),
            "回话要说出生效容量（候选 ③），实得：{body}"
        );
    }

    // ⚠ 反方向：**沒改容量的 load 不許把話說反**。
    //   只測「改了會變」的話，一個把容量無條件重置成預設值的實作照樣綠。
    #[test]
    fn load_没改_capacity_时容量不动() {
        let (a, cache) = app_with_cache(vec![(8080, false)], 4096);
        let dsl = "http://a.com:8080 {\n  reverse_proxy 127.0.0.1:9\n  \
                   cache {\n    capacity 4096\n  }\n}\n";
        let (code, body) = a.load(&json_of(dsl), Some("overrides=clear"));
        assert_eq!(code, 200, "{body}");
        assert_eq!(cache.store.capacity(), Some(4096), "没改就不该动");
    }

    fn app_with_cache(
        ports: Vec<(u16, bool)>,
        capacity: u64,
    ) -> (AdminApp, Arc<crate::cache::CacheHandle>) {
        let cfg =
            fulcrum_config::compile_str("t.Fulcrumfile", "http://a.com:8080 {\n  respond 200\n}\n")
                .config
                .unwrap();
        let rt = Arc::new(Runtime::build(&cfg).unwrap());
        let cache = crate::cache::CacheHandle::new(crate::cache::Backend::open(None, capacity));
        (
            AdminApp::new(
                SharedRuntime::new(rt),
                ports,
                None,
                Some(cache.clone()),
                None,
            ),
            cache,
        )
    }

    fn app(ports: Vec<(u16, bool)>) -> AdminApp {
        let cfg =
            fulcrum_config::compile_str("t.Fulcrumfile", "http://a.com:8080 {\n  respond 200\n}\n")
                .config
                .unwrap();
        let rt = Arc::new(Runtime::build(&cfg).unwrap());
        AdminApp::new(SharedRuntime::new(rt), ports, None, None, None)
    }

    fn json_of(dsl: &str) -> Vec<u8> {
        let cfg = fulcrum_config::compile_str("t.Fulcrumfile", dsl)
            .config
            .expect("样例配置应当能编译");
        serde_json::to_vec(&cfg).unwrap()
    }

    #[test]
    fn 端口集不变时换得动() {
        let a = app(vec![(8080, false)]);
        let before = a.rt.current();
        let (status, text) = a.load(
            &json_of("http://b.com:8080 {\n  respond 204\n}\n"),
            Some("overrides=clear"),
        );
        assert_eq!(status, 200, "{text}");
        // ★ ★ 判據是**換到了**，不是「回傳了 200」。
        //   ⚠ 斷言 `listen_ports == [(8080,false)]` 是不行的 —— 那個值在新舊兩份裡
        //   **是一樣的**，一個「收下但什麼都不做」的實作照樣綠。
        //   ★ **判據不能掛在兩邊都相同的量上。**
        assert!(
            !Arc::ptr_eq(&before, &a.rt.current()),
            "load 返回 200，但那一份配置根本没被换掉"
        );
        assert_eq!(a.rt.current().listen_ports, vec![(8080, false)]);
    }

    #[test]
    fn 端口集变了就拒绝而且旧配置一个字节都没动() {
        let a = app(vec![(8080, false)]);
        let before = a.rt.current();
        let (status, text) = a.load(
            &json_of("http://b.com:9090 {\n  respond 200\n}\n"),
            Some("overrides=clear"),
        );
        assert_eq!(status, 409, "{text}");
        assert!(text.contains("systemctl reload"), "要说清怎么办：{text}");
        // ⚠ 這一條才是「原子」的判據：**拒絕之後舊的還在**。
        //   一個「先換後檢核」的實作在上一條上表現完全相同。
        assert!(Arc::ptr_eq(&before, &a.rt.current()), "旧配置被动过了");
    }

    // ── M2 批 H：快取後端換不了 ────────────────────────────────────────
    //
    // ⚠ ⚠ 不攔的話，`POST /load` 會回 **200**，而新寫的 `disk` 目錄一個位元組都不會被用到
    //   —— 維運看到「換成功了」，盤上那個目錄始終是空的，而裝載紀錄早在啟動時
    //   就打過了、不會再說第二遍。★ 這是靜默失能，不是小瑕疵。
    #[test]
    fn 换缓存后端要被拒而且旧配置一个字节都没动() {
        let a = app(vec![(8080, false)]);
        let before = a.rt.current();
        let (status, text) = a.load(
            &json_of(
                "http://a.com:8080 {\n  cache {\n    disk /var/cache/fulcrum\n  }\n  \
                 reverse_proxy 127.0.0.1:1\n}\n",
            ),
            Some("overrides=clear"),
        );
        assert_eq!(status, 409, "换后端居然被放行了：{text}");
        assert!(text.contains("缓存后端变了"), "要说清是哪件事：{text}");
        assert!(text.contains("systemctl reload"), "要说清怎么办：{text}");
        assert!(Arc::ptr_eq(&before, &a.rt.current()), "旧配置被动过了");
    }

    // ★ 反向那一半：**只改新鮮度、不動後端**必須照常換得進去。
    //   ⚠ 少了它，一個「只要設定裡有 `cache` 就拒」的實作會讓上面那條全綠，
    //   而它把一次完全正當的 `ttl` 調整也攔了。
    #[test]
    fn 只改缓存参数不动后端照常换得动() {
        let a = app(vec![(8080, false)]);
        let before = a.rt.current();
        let (status, text) = a.load(
            &json_of(
                "http://a.com:8080 {\n  cache {\n    ttl 5m\n  }\n  reverse_proxy 127.0.0.1:1\n}\n",
            ),
            Some("overrides=clear"),
        );
        assert_eq!(status, 200, "{text}");
        assert!(
            !Arc::ptr_eq(&before, &a.rt.current()),
            "回了 200，而那一份配置根本没被换掉"
        );
    }

    #[test]
    fn 载荷坏掉时也一个字节都没动() {
        let a = app(vec![(8080, false)]);
        let before = a.rt.current();
        let (status, _) = a.load(b"{ not json", Some("overrides=clear"));
        assert_eq!(status, 400);
        assert!(Arc::ptr_eq(&before, &a.rt.current()));
    }

    #[test]
    fn 建不起来的配置也一个字节都没动() {
        // ★ 結構化層是公開入口（G11），所以它能塞進 DSL 塞不進的東西——
        //   這裡用一個非法 CIDR，`Runtime::build` 會拒絕它。
        let a = app(vec![(8080, false)]);
        let before = a.rt.current();
        let mut cfg = fulcrum_config::compile_str(
            "t.Fulcrumfile",
            "http://a.com:8080 {\n  @m {\n    remote_ip 10.0.0.0/8\n  }\n  respond 200\n}\n",
        )
        .config
        .unwrap();
        // 把 CIDR 改壞 —— 只有結構化層進得來。
        let json = serde_json::to_string(&cfg)
            .unwrap()
            .replace("10.0.0.0/8", "10.0.0.0/99");
        cfg = serde_json::from_str(&json).unwrap();
        let (status, text) = a.load(
            serde_json::to_vec(&cfg).unwrap().as_slice(),
            Some("overrides=clear"),
        );
        assert_eq!(status, 400, "{text}");
        assert!(text.contains("没有任何改动生效"), "{text}");
        assert!(Arc::ptr_eq(&before, &a.rt.current()));
    }

    // ── 任務 5：`overrides` 必填參數（G120 / R9）───────────────────────────

    #[test]
    fn load缺overrides参数400且旧配置一个字节都没动() {
        // ⚠ ⚠ 判據寫法紀律：body 單獨看完全合法、會真的生效（與「連接埠集不變時
        //   換得動」用的是同一份夾具）——唯一的毛病是**沒帶 `?overrides=`**。
        //   否則 400 可能來自別的原因，這條判據就 confound 了，測不到「缺
        //   overrides 本身會被拒」這件事。
        let a = app(vec![(8080, false)]);
        let before = a.rt.current();
        let (status, text) = a.load(&json_of("http://b.com:8080 {\n  respond 204\n}\n"), None);
        assert_eq!(status, 400, "{text}");
        assert!(
            text.contains("overrides"),
            "要点名是 overrides 缺了：{text}"
        );
        assert!(Arc::ptr_eq(&before, &a.rt.current()), "旧配置被动过了");
    }

    #[test]
    fn load的overrides参数值不对400且旧配置一个字节都没动() {
        // 同上一條同一條紀律：body 單獨看完全合法，唯一的毛病是查詢串裡的值。
        let a = app(vec![(8080, false)]);
        let before = a.rt.current();
        let (status, text) = a.load(
            &json_of("http://b.com:8080 {\n  respond 204\n}\n"),
            Some("overrides=bogus"),
        );
        assert_eq!(status, 400, "{text}");
        assert!(Arc::ptr_eq(&before, &a.rt.current()), "旧配置被动过了");
    }

    // ★ ★ ★ 修復輪 1，評審 F3（提級 Minor）：`overrides=` 出現不止一次
    //   （不論值是否衝突）要被拒絕，不許靜默取第一個。
    #[test]
    fn load的overrides参数重复400且旧配置一个字节都没动() {
        // ⚠ ⚠ 判據寫法紀律：body 單獨看完全合法（與「連接埠集不變時換得動」
        //   同一份），唯一的毛病是查詢串裡 `overrides` 寫了兩次。
        let a = app(vec![(8080, false)]);
        let before = a.rt.current();
        let (status, text) = a.load(
            &json_of("http://b.com:8080 {\n  respond 204\n}\n"),
            Some("overrides=keep&overrides=clear"),
        );
        assert_eq!(status, 400, "{text}");
        assert!(
            text.contains("重复"),
            "要说清是「重复」而不是「值不认识」：{text}"
        );
        assert!(Arc::ptr_eq(&before, &a.rt.current()), "旧配置被动过了");
    }

    #[test]
    fn 没开自动签发时强制续期给一条能看懂的错() {
        let a = app(vec![(8080, false)]);
        let (status, text) = a.renew(br#"{"domain":"a.com"}"#);
        assert_eq!(status, 409, "{text}");
        assert!(text.contains("没有开自动签发"), "{text}");
    }

    #[test]
    fn 强制续期的载荷坏掉时报_400() {
        let a = app(vec![(8080, false)]);
        assert_eq!(a.renew(b"{}").0, 400);
        assert_eq!(a.renew(b"nope").0, 400);
    }

    // ── `POST /runtime`（M2 批 N 任務 4，裁決 R10）──────────────────────────
    //
    // 夾具兩個站台：`pool.example` 一條 handle 寫了 `id`、另一條沒寫但指向
    // **同一臺機器**（判據 6 要用——兩者必須是不同的格子）；`solo.example`
    // 單站台單上游單目標（其餘判據用，`pick_index_by(None)` 的 `Some`/`None`
    // 就是「在不在可用集裡」最直接的讀數）。位址全用 IP 字面值，免得測試還要
    // 走 DNS 解析。
    const RUNTIME_DSL: &str = "\
http://pool.example:8080 {
  handle /a/* {
    reverse_proxy 10.40.0.1:1 {
      id pool_a
    }
  }
  handle /b/* {
    reverse_proxy 10.40.0.1:1
  }
}
http://solo.example:8090 {
  reverse_proxy 10.40.0.9:9
}
";

    fn app_runtime() -> AdminApp {
        let cfg = fulcrum_config::compile_str("t.Fulcrumfile", RUNTIME_DSL)
            .config
            .unwrap();
        let rt = Arc::new(Runtime::build(&cfg).unwrap());
        AdminApp::new(
            SharedRuntime::new(rt),
            vec![(8080, false), (8090, false)],
            None,
            None,
            None,
        )
    }

    /// 按站台名 + id 從當前執行環境裡找回那一條 `reverse_proxy` 的目標。
    /// 找不到就直接 panic——那說明夾具寫錯了，不是被測程式碼的問題。
    fn find_target<'a>(rt: &'a Runtime, site: &str, id: &str) -> &'a fulcrum_runtime::ProxyTarget {
        rt.keyed_proxies()
            .into_iter()
            .find(|p| p.site == site && p.id == id)
            .unwrap_or_else(|| panic!("夹具里应该有「{site}·{id}」这个目标"))
            .target
    }

    // ── 判據 1 / 2：disable / enable 走管理面這條路 ─────────────────────────

    #[test]
    fn runtime_disable走管理面让上游掉出可用集() {
        let a = app_runtime();
        let rt = a.rt.current();
        let t = find_target(&rt, "http://solo.example:8090", "");
        assert_eq!(t.pick_index_by(None), Some(0), "夹具本身应该是可用的");
        let (status, text) = a.runtime(
            br#"{"actions":[{"verb":"disable","site":"http://solo.example:8090","upstream":"10.40.0.9:9"}]}"#,
        );
        assert_eq!(status, 200, "{text}");
        // ★ 重複使用同一個 `t`（同一個 `Arc<UpstreamOverride>`）再問一次——這正是
        //   R7 的意義：改登記處那一格，當場作用到正在跑的上游身上，不必重新
        //   拿一份 `Runtime`。
        assert_eq!(
            t.pick_index_by(None),
            None,
            "走管理面 disable 之后，这个上游应该掉出可用集"
        );
    }

    #[test]
    fn runtime_enable把disable撤回来() {
        let a = app_runtime();
        let rt = a.rt.current();
        let t = find_target(&rt, "http://solo.example:8090", "");
        let (s1, t1) = a.runtime(
            br#"{"actions":[{"verb":"disable","site":"http://solo.example:8090","upstream":"10.40.0.9:9"}]}"#,
        );
        assert_eq!(s1, 200, "{t1}");
        assert_eq!(t.pick_index_by(None), None, "先确认摘掉了");
        let (status, text) = a.runtime(
            br#"{"actions":[{"verb":"enable","site":"http://solo.example:8090","upstream":"10.40.0.9:9"}]}"#,
        );
        assert_eq!(status, 200, "{text}");
        assert_eq!(
            t.pick_index_by(None),
            Some(0),
            "enable 应该把 disable 撤回来"
        );
    }

    // ── 判據 3：set_weight ────────────────────────────────────────────────

    #[test]
    fn runtime_set_weight改到upstream_weight返回新值() {
        let a = app_runtime();
        let rt = a.rt.current();
        let t = find_target(&rt, "http://solo.example:8090", "");
        assert_eq!(
            t.upstreams[0].weight(),
            1,
            "夹具没写 weight，配置权重缺省是 1"
        );
        let (status, text) = a.runtime(
            br#"{"actions":[{"verb":"set_weight","site":"http://solo.example:8090","upstream":"10.40.0.9:9","weight":7}]}"#,
        );
        assert_eq!(status, 200, "{text}");
        assert_eq!(
            t.upstreams[0].weight(),
            7,
            "set_weight 之后 Upstream::weight() 要返回新值"
        );
    }

    // ── 判據 4：全有或全無 ────────────────────────────────────────────────

    #[test]
    fn runtime_全有或全无_第二条指不到时第一条也不生效() {
        let a = app_runtime();
        let rt = a.rt.current();
        let solo = find_target(&rt, "http://solo.example:8090", "");
        let pool_a = find_target(&rt, "http://pool.example:8080", "pool_a");
        assert_eq!(solo.pick_index_by(None), Some(0));
        assert_eq!(pool_a.pick_index_by(None), Some(0));

        // ⚠ 第二條的位址用 TEST-NET-3（203.0.113.0/24）——不會出現在任何設定
        //   或錯誤訊息範本的字面值裡（判據寫法紀律，brief §3 末段）。
        let body = br#"{"actions":[
            {"verb":"disable","site":"http://solo.example:8090","upstream":"10.40.0.9:9"},
            {"verb":"disable","site":"http://solo.example:8090","upstream":"203.0.113.250:65000"},
            {"verb":"disable","site":"http://pool.example:8080","id":"pool_a","upstream":"10.40.0.1:1"}
        ]}"#;
        let (status, text) = a.runtime(body);
        assert_eq!(status, 400, "{text}");
        assert_eq!(
            solo.pick_index_by(None),
            Some(0),
            "第一条也不该生效——全有或全无"
        );
        assert_eq!(pool_a.pick_index_by(None), Some(0), "第三条同样不该生效");
    }

    // ── 判據 5：400 訊息逐條列出該站台下的鍵 ─────────────────────────────

    #[test]
    fn runtime_指不到时400报文逐条列出该站点的键() {
        // ★ ★ ★ 修復輪 1，評審 I3：夾具必須選一個**有兩個鍵**的站台——
        //   `pool.example` 下 `pool_a` 與空串 `id` 各佔一格，兩把鍵的位址相同
        //   （`10.40.0.1:1`），只有 `id` 不同。⚠ 換成這個夾具之前用的是
        //   `solo.example`（站下只有一個鍵），"逐條列出**全部**鍵"這句話
        //   從來沒被測到——一個只列印第一條的實作照樣綠。
        let a = app_runtime();
        let (status, text) = a.runtime(
            br#"{"actions":[{"verb":"disable","site":"http://pool.example:8080","upstream":"203.0.113.250:65000"}]}"#,
        );
        assert_eq!(status, 400, "{text}");
        // ⚠ ⚠ 判據寫法紀律：真斷言落在 `OverrideKey` 自己的 `Display` 輸出上——
        //   不手寫格式字串（那樣會在 `Display` 改動時悄悄失去意義），
        //   也不會出現在任何錯誤訊息範本的固定字面值裡。
        let 带id = OverrideKey::new("http://pool.example:8080", "pool_a", "10.40.0.1:1");
        let 不带id = OverrideKey::new("http://pool.example:8080", "", "10.40.0.1:1");
        assert!(
            text.contains(&带id.to_string()),
            "400 报文应该列出 pool.example 下带 id 的那把键，运维照抄即可：{text}"
        );
        assert!(
            text.contains(&不带id.to_string()),
            "400 报文应该**同时**列出 pool.example 下不带 id 的那把键——\
             两把键地址相同、只有 id 不同，漏列其中一把就是「逐条列出全部」没做到：{text}"
        );
    }

    // ── 判據 6：選填 id 命中不同的格子 ───────────────────────────────────

    #[test]
    fn runtime_选填id命中不同格子() {
        let a = app_runtime();
        let rt = a.rt.current();
        let 带id = find_target(&rt, "http://pool.example:8080", "pool_a");
        let 不带id = find_target(&rt, "http://pool.example:8080", "");
        assert_eq!(带id.pick_index_by(None), Some(0));
        assert_eq!(不带id.pick_index_by(None), Some(0));

        // 不寫 id ⇒ 命中空串那一格，`带id` 那一格不該被碰到。
        let (status, text) = a.runtime(
            br#"{"actions":[{"verb":"disable","site":"http://pool.example:8080","upstream":"10.40.0.1:1"}]}"#,
        );
        assert_eq!(status, 200, "{text}");
        assert_eq!(
            不带id.pick_index_by(None),
            None,
            "没写 id 应该命中空串那一格"
        );
        assert_eq!(
            带id.pick_index_by(None),
            Some(0),
            "写了 id 的那一格不该被这条没写 id 的动作碰到——两者不是同一格"
        );

        // 寫了對應的 id ⇒ 命中另一格。
        let (status, text) = a.runtime(
            br#"{"actions":[{"verb":"disable","site":"http://pool.example:8080","id":"pool_a","upstream":"10.40.0.1:1"}]}"#,
        );
        assert_eq!(status, 200, "{text}");
        assert_eq!(带id.pick_index_by(None), None, "写了 id 现在也该摘掉了");
    }

    // ── 判據 7：set_weight 的值域 ─────────────────────────────────────────

    #[test]
    fn runtime_set_weight越界0和65536都400且不生效() {
        let a = app_runtime();
        let rt = a.rt.current();
        let t = find_target(&rt, "http://solo.example:8090", "");
        for bad in [0u32, 65536] {
            let body = format!(
                r#"{{"actions":[{{"verb":"set_weight","site":"http://solo.example:8090","upstream":"10.40.0.9:9","weight":{bad}}}]}}"#
            );
            let (status, text) = a.runtime(body.as_bytes());
            assert_eq!(status, 400, "weight={bad}：{text}");
            assert_eq!(
                t.upstreams[0].weight(),
                1,
                "越界的 weight 不该生效：weight={bad}"
            );
        }
    }

    // ── 判據 8：不認識的 verb ─────────────────────────────────────────────

    #[test]
    fn runtime_不认识的verb400且不生效() {
        let a = app_runtime();
        let rt = a.rt.current();
        let t = find_target(&rt, "http://solo.example:8090", "");
        let (status, text) = a.runtime(
            br#"{"actions":[{"verb":"reboot","site":"http://solo.example:8090","upstream":"10.40.0.9:9"}]}"#,
        );
        assert_eq!(status, 400, "{text}");
        assert_eq!(t.pick_index_by(None), Some(0), "不认识的 verb 不该生效");
    }

    // ── 判據 9：訊息形狀不對 ─────────────────────────────────────────────

    #[test]
    fn runtime_报文形状不对400且不生效() {
        let a = app_runtime();
        let rt = a.rt.current();
        let t = find_target(&rt, "http://solo.example:8090", "");
        // 單條不帶陣列——⛔ 不接受的第二種寫法。
        let (status, _) = a.runtime(
            br#"{"verb":"disable","site":"http://solo.example:8090","upstream":"10.40.0.9:9"}"#,
        );
        assert_eq!(status, 400);
        assert_eq!(t.pick_index_by(None), Some(0));
        // JSON 壞掉。
        let (status, _) = a.runtime(b"{ not json");
        assert_eq!(status, 400);
        assert_eq!(t.pick_index_by(None), Some(0));
    }

    // ── 判據 10：指不到不留垃圾格子 ───────────────────────────────────────

    #[test]
    fn runtime_指不到不留垃圾格子() {
        let a = app_runtime();
        let before = a.rt.overrides().slot_count();
        let (status, _) = a.runtime(
            br#"{"actions":[{"verb":"disable","site":"http://solo.example:8090","upstream":"203.0.113.250:65000"}]}"#,
        );
        assert_eq!(status, 400);
        assert_eq!(
            a.rt.overrides().slot_count(),
            before,
            "一次失败的 /runtime 之后登记处的格子数不该变"
        );
    }

    // ── 判據 11：404 的「可用」清單裡有 /runtime ─────────────────────────

    #[test]
    fn 不认识的路径的可用清单里有_runtime() {
        let (status, text) = unknown_route("GET", "/nope");
        assert_eq!(status, 404);
        assert!(text.contains("/runtime"), "{text}");
    }

    // ── 判據 12：結構判據（TOCTOU）───────────────────────────────────────

    #[test]
    fn admin_rs这条路径不经过overridelayer_get() {
        // ★ ★ ★ 時序競態難直接測，退而求其次：斷言這個檔案的原始碼裡既真的走了
        //   `apply_all`（正面：結構上確實換了新路），也壓根不出現
        //   `OverrideLayer::get()` 的呼叫形狀（反面：沒有偷偷繞回舊的兩步式）。
        //   ⛔ 別寫成「碰巧現在沒並行所以過了」的行為判據——那是一把在好壞兩種
        //   情況下讀數相同的尺。
        //
        // ⚠ ⚠ 只查**產品程式碼**那一半（`#[cfg(test)]` 之前）——`include_str!` 會把
        //   這條判據自己也讀進來，而這條判據的斷言文字裡恰好逐字寫著
        //   `"overrides().get("` 這個串；不切掉測試模組的話，這條判據會讀到
        //   **它自己的原始碼**當成「出現了那個呼叫」，永遠紅，屬於本批判據寫法
        //   紀律點名的那類自我碰撞（brief §3 末段的同一族陷阱）。
        let whole = include_str!("admin.rs");
        let prod = whole
            .split("#[cfg(test)]")
            .next()
            .expect("这个文件里应该有 #[cfg(test)]");
        assert!(
            prod.contains("overrides().apply_all("),
            "runtime() 应该经过 `OverrideLayer::apply_all`（查存在与改在同一次持锁内完成）"
        );
        assert!(
            !prod.contains("overrides().get("),
            "admin.rs 的产品代码里不许出现 `OverrideLayer::get()` 的调用——那会把「查存在」\
             与「改」拆成两步，中间可能被一次 `/load` 的 retain_after_swap 抢先把格子收走，\
             改的就是一个已经没人认的孤儿"
        );
    }

    // ── 補充判據：weight 只對 set_weight 有意義（本任務自己的決定，釘住）──

    #[test]
    fn runtime_disable带weight字段直接400而不是被忽略() {
        let a = app_runtime();
        let rt = a.rt.current();
        let t = find_target(&rt, "http://solo.example:8090", "");
        let (status, text) = a.runtime(
            br#"{"actions":[{"verb":"disable","site":"http://solo.example:8090","upstream":"10.40.0.9:9","weight":3}]}"#,
        );
        assert_eq!(status, 400, "{text}");
        assert!(text.contains("set_weight"), "要说清为什么：{text}");
        assert_eq!(t.pick_index_by(None), Some(0), "不该生效");
    }

    #[test]
    fn runtime_set_weight缺weight字段400() {
        let a = app_runtime();
        let (status, text) = a.runtime(
            br#"{"actions":[{"verb":"set_weight","site":"http://solo.example:8090","upstream":"10.40.0.9:9"}]}"#,
        );
        assert_eq!(status, 400, "{text}");
    }

    #[test]
    fn runtime_actions空数组400() {
        let a = app_runtime();
        let (status, text) = a.runtime(br#"{"actions":[]}"#);
        assert_eq!(status, 400, "{text}");
    }

    // ── 補充判據：懸空的鍵端到端也不能被 /runtime 尋址 ──────────────────
    //
    // ★ overrides.rs 自己的判據已經在 `OverrideLayer::apply_all` 那一層驗過
    //   這件事；這裡再從 `admin.rs` 的 `runtime()` 走一遍完整路徑，確認
    //   `live` 確實來自 `self.rt.current()`（正在服務的那一份），不是隨便
    //   哪一份快照。
    #[test]
    fn runtime_悬空的键不能被寻址() {
        let a = app_runtime();
        let (status, text) = a.runtime(
            br#"{"actions":[{"verb":"disable","site":"http://solo.example:8090","upstream":"10.40.0.9:9"}]}"#,
        );
        assert_eq!(status, 200, "{text}");
        // 換一份不再有 solo.example 的設定——那一格現在懸空：設過覆蓋，
        // 登記處按 R8 把它留著，但當前執行環境已經不認它。
        let cfg2 = fulcrum_config::compile_str(
            "t.Fulcrumfile",
            "http://pool.example:8080 {\n  reverse_proxy 10.40.0.1:1\n}\n",
        )
        .config
        .unwrap();
        let next = Runtime::build(&cfg2).unwrap();
        a.rt.swap(next);
        let (status, text) = a.runtime(
            br#"{"actions":[{"verb":"enable","site":"http://solo.example:8090","upstream":"10.40.0.9:9"}]}"#,
        );
        assert_eq!(status, 400, "悬空的键不该能被 /runtime 寻址到：{text}");
    }

    // ── 補充判據：拼錯的欄位名要 400，不許靜默丟掉（修復輪 1，評審 M6）──────
    //
    // ⚠ ⚠ ⚠ 兩條判據都必須挑一個**除了那個拼錯的欄位之外，其餘部分單獨看
    //   也完全合法、會真的生效**的動作/訊息——否則拼錯的欄位被靜默丟掉之後，
    //   請求會因為**別的、無關的原因**恰好也是 400（比如 `set_weight` 缺
    //   `weight` 欄位本來就 400），判據看起來紅過，其實從沒測到
    //   `deny_unknown_fields` 本身。這裡用 `disable`（不需要 `weight`）+
    //   一個多餘欄位，`deny_unknown_fields` 不生效的話這條請求會正常
    //   200 並且真的把上游摘掉。

    #[test]
    fn runtime_拼错字段名400且不生效() {
        let a = app_runtime();
        let rt = a.rt.current();
        let t = find_target(&rt, "http://solo.example:8090", "");
        assert_eq!(t.pick_index_by(None), Some(0), "夹具本身应该是可用的");
        // 這條 `disable` 本身的三個必填欄位都對——`wieght` 是多出來的、
        // 這條動作根本用不到的欄位。沒有 `deny_unknown_fields` 的話它會被
        // 靜默丟掉，這條 disable 照常生效（200，上游被摘掉）。
        let (status, text) = a.runtime(
            br#"{"actions":[{"verb":"disable","site":"http://solo.example:8090","upstream":"10.40.0.9:9","wieght":7}]}"#,
        );
        assert_eq!(status, 400, "{text}");
        assert_eq!(
            t.pick_index_by(None),
            Some(0),
            "带着多余字段的动作不该生效——上游不该被摘掉"
        );
    }

    #[test]
    fn runtime_外层多出字段400且不生效() {
        let a = app_runtime();
        let rt = a.rt.current();
        let t = find_target(&rt, "http://solo.example:8090", "");
        assert_eq!(t.pick_index_by(None), Some(0), "夹具本身应该是可用的");
        // 外層 `Req` 同樣要 `deny_unknown_fields`：`actions` 本身是完全合法的
        // 一條 `disable`，多出來的頂層欄位 `extra` 才是要擋的東西——
        // 沒有 `deny_unknown_fields` 的話它會被靜默丟掉，這條請求照常生效。
        let (status, text) = a.runtime(
            br#"{"actions":[{"verb":"disable","site":"http://solo.example:8090","upstream":"10.40.0.9:9"}],"extra":"field"}"#,
        );
        assert_eq!(status, 400, "{text}");
        assert_eq!(
            t.pick_index_by(None),
            Some(0),
            "带着多余顶层字段的整份请求不该生效"
        );
    }

    // ── 任務 5：`/load` 的 `overrides` 兩檔接上真實的覆蓋層（G120 / R8）──────
    //
    // ★ 重複使用上面 `POST /runtime` 那組的 `RUNTIME_DSL` / `app_runtime` /
    //   `find_target`：重新 `/load` 同一份 DSL 時監聽連接埠集不變（裝得進去），
    //   正好用來驗證 `overrides` 兩檔在真實覆蓋層上的行為——不止「回了 200」。
    //   ⚠ ⚠ 硬要求（brief）：這兩檔的**真正**判據必須從 `tests/serve/run.sh`
    //   那條真 socket 上打；這裡是**另外**補一遍語義（直接調 handler，不經過
    //   `?overrides=` 的查詢串解析那一步），不能取代 E2E。

    #[test]
    fn load的overrides等于keep时覆盖还在且仍作用在数据面上() {
        let a = app_runtime();
        let k = OverrideKey::new("http://solo.example:8090", "", "10.40.0.9:9");
        a.rt.overrides().slot(&k).set_disabled(true);
        {
            let rt = a.rt.current();
            let t = find_target(&rt, "http://solo.example:8090", "");
            assert_eq!(t.pick_index_by(None), None, "夹具前提：已经摘掉了");
        }

        let (status, text) = a.load(&json_of(RUNTIME_DSL), Some("overrides=keep"));
        assert_eq!(status, 200, "{text}");

        // ★ ★ 判據掛在資料面上：**新**執行環境裡同一個上游，覆蓋必須仍然生效。
        let rt = a.rt.current();
        let t = find_target(&rt, "http://solo.example:8090", "");
        assert_eq!(
            t.pick_index_by(None),
            None,
            "overrides=keep 之后，覆盖应该仍然作用在新运行时的数据面上"
        );
    }

    #[test]
    fn load的overrides等于clear时覆盖被清且回话逐项列出() {
        let a = app_runtime();
        let k = OverrideKey::new("http://solo.example:8090", "", "10.40.0.9:9");
        a.rt.overrides().slot(&k).set_disabled(true);

        let (status, text) = a.load(&json_of(RUNTIME_DSL), Some("overrides=clear"));
        assert_eq!(status, 200, "{text}");
        // ⚠ ⚠ 判據寫法紀律：真斷言落在 `OverrideKey` 自己的 `Display` 輸出上，
        //   不手寫格式字串。
        assert!(
            text.contains(&k.to_string()),
            "clear 的回话应该逐项列出被清掉的那一项：{text}"
        );

        let rt = a.rt.current();
        let t = find_target(&rt, "http://solo.example:8090", "");
        assert_eq!(
            t.pick_index_by(None),
            Some(0),
            "overrides=clear 之后覆盖应该没了，上游恢复可用"
        );
    }

    #[test]
    fn load的overrides等于keep时悬空的覆盖被点名() {
        let a = app_runtime();
        let k = OverrideKey::new("http://solo.example:8090", "", "10.40.0.9:9");
        a.rt.overrides().slot(&k).set_disabled(true);

        // 換一份不再有 `reverse_proxy 10.40.0.9:9` 的設定——這一格現在懸空。
        // ⚠ 監聽連接埠集要保持 [(8080,false),(8090,false)] 不變，否則會撞 409：
        //   兩個站台名照舊，只把 solo.example 的 body 換成裸 `respond`。
        let (status, text) = a.load(
            &json_of(
                "http://pool.example:8080 {\n  reverse_proxy 10.40.0.1:1\n}\n\
                 http://solo.example:8090 {\n  respond 200\n}\n",
            ),
            Some("overrides=keep"),
        );
        assert_eq!(status, 200, "{text}");
        assert!(
            text.contains(&k.to_string()),
            "keep 的回话应该把悬空的覆盖逐条点名：{text}"
        );
        assert!(text.contains("悬空"), "要说清这是悬空：{text}");
    }

    // ★ ★ ★ 修復輪 1，評審 F2（Important）：`clear`/`keep` 的「逐項列出」此前
    //   只擺過**單項**——`任务5_clear_all返回值只含…`（`overrides.rs`）驗的是
    //   `clear_all` 自己的 `Vec`，碰不到 `admin.rs` 這兩個格式化循環
    //   （`for e in &cleared`／`for e in &dangling`）；一個「只
    //   `push_str(cleared[0])`」的實作——表頭 `清掉了 {} 项` 仍然用 `.len()`
    //   算，讀起來完全正常——七格門禁原本會全綠。這裡用 `RUNTIME_DSL` 現成的
    //   三把互不相同的鍵（`pool.example` 帶 `id` / 不帶 `id` + `solo.example`），
    //   逐一斷言三個 `Display` 串都出現在回話裡。

    #[test]
    fn load的overrides等于clear时多项覆盖全部逐项列出_不只第一项() {
        let a = app_runtime();
        let k_pool_a = OverrideKey::new("http://pool.example:8080", "pool_a", "10.40.0.1:1");
        let k_pool_bare = OverrideKey::new("http://pool.example:8080", "", "10.40.0.1:1");
        let k_solo = OverrideKey::new("http://solo.example:8090", "", "10.40.0.9:9");
        a.rt.overrides().slot(&k_pool_a).set_disabled(true);
        a.rt.overrides().slot(&k_pool_bare).set_weight(7).unwrap();
        a.rt.overrides().slot(&k_solo).set_disabled(true);

        let (status, text) = a.load(&json_of(RUNTIME_DSL), Some("overrides=clear"));
        assert_eq!(status, 200, "{text}");
        for k in [&k_pool_a, &k_pool_bare, &k_solo] {
            assert!(
                text.contains(&k.to_string()),
                "clear 的回话应该列出**每一项**被清掉的，漏了 {k}：{text}"
            );
        }
    }

    #[test]
    fn load的overrides等于keep时多项悬空全部逐条点名_不只第一项() {
        let a = app_runtime();
        let k_pool_a = OverrideKey::new("http://pool.example:8080", "pool_a", "10.40.0.1:1");
        let k_pool_bare = OverrideKey::new("http://pool.example:8080", "", "10.40.0.1:1");
        let k_solo = OverrideKey::new("http://solo.example:8090", "", "10.40.0.9:9");
        a.rt.overrides().slot(&k_pool_a).set_disabled(true);
        a.rt.overrides().slot(&k_pool_bare).set_weight(7).unwrap();
        a.rt.overrides().slot(&k_solo).set_disabled(true);

        // 換一份兩個站台都還在（保住監聽連接埠集 [(8080,false),(8090,false)]）、
        // 但一個上游都沒有的設定——三把鍵因此**全部**懸空。
        let (status, text) = a.load(
            &json_of(
                "http://pool.example:8080 {\n  respond 200\n}\n\
                 http://solo.example:8090 {\n  respond 200\n}\n",
            ),
            Some("overrides=keep"),
        );
        assert_eq!(status, 200, "{text}");
        for k in [&k_pool_a, &k_pool_bare, &k_solo] {
            assert!(
                text.contains(&k.to_string()),
                "keep 的回话应该把**每一项**悬空覆盖都点名，漏了 {k}：{text}"
            );
        }
    }

    // ── `GET /stats`（M2 批 N 任務 6，G16 / G18 / R12 / R15）────────────────
    //
    // ★ ★ 這裡補的是「直接調 handler、逐欄位核對最終 JSON 文字」那一半；
    //   brief §7 點名「必須走真路徑」的那一半在 `tests/serve/run.sh` 的
    //   `[3.6/4]` 一節（真 socket，`GET /stats` 與 `GET /metrics` 兩個出口
    //   都打）。⛔ 兩邊都要有，這裡**不能取代** E2E——任務 4 那次「16 條判據
    //   全部直接調 handler，把路由那一行刪掉全部照樣綠」的教訓點名過這件事。

    /// 一把共享鍵（兩條 `reverse_proxy` 都沒寫 `id`，指同一臺 `10.70.0.1:1`）+
    /// 一把獨立鍵（寫了 `id solo`，指另一臺 `10.70.0.2:2`）。★ 與 R6 第二輪
    /// 那個真實風險（使用者以為 `disable` 只影響一條 `reverse_proxy`）逐字同形。
    const STATS_DSL: &str = "\
http://pool.example:8080 {
  handle /a/* {
    reverse_proxy 10.70.0.1:1
  }
  handle /b/* {
    reverse_proxy 10.70.0.1:1
  }
  handle /c/* {
    reverse_proxy 10.70.0.2:2 {
      id solo
    }
  }
}
";

    fn app_stats() -> AdminApp {
        let cfg = fulcrum_config::compile_str("t.Fulcrumfile", STATS_DSL)
            .config
            .unwrap();
        let rt = Arc::new(Runtime::build(&cfg).unwrap());
        AdminApp::new(
            SharedRuntime::new(rt),
            vec![(8080, false)],
            None,
            None,
            None,
        )
    }

    /// 解析 `/stats` 的 JSON 正文。★ ★ ★ 判據寫法紀律（brief §8 第 2 條）：
    /// 斷言要打在**這份解析結果**（最終 JSON 文字）上，不是打在
    /// `override_entries()`/`proxy_key_fanout()` 這類取數函式的回傳值上——
    /// 任務 5 那次「驗 `clear_all()` 回傳 `Vec` 的判據碰不到 `admin.rs` 裡把它
    /// 拼成文字的循環」的教訓點名過這個形狀。
    fn stats_json(a: &AdminApp) -> serde_json::Value {
        let text = a.stats();
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("/stats 不是合法 JSON：{e}\n{text}"))
    }

    #[test]
    fn stats_是合法json且带着全部顶层字段() {
        let a = app_stats();
        let v = stats_json(&a);
        for key in [
            "pid",
            "config_loaded_at_unix",
            "upstreams",
            "overrides",
            "fanout",
            "fanout_shared",
            "cache",
            "certs",
        ] {
            assert!(v.get(key).is_some(), "缺顶层字段 `{key}`：{v}");
        }
    }

    #[test]
    fn stats的pid等于本进程pid_不是新造的世代计数器() {
        let a = app_stats();
        let v = stats_json(&a);
        assert_eq!(v["pid"].as_u64(), Some(std::process::id() as u64), "{v}");
    }

    // ── 快取佔用 / 證書到期：明確的預設，不是 0 冒充（brief §8 判據 7）───────

    #[test]
    fn stats没配缓存和证书时是明确缺省_不是0冒充() {
        let a = app_stats(); // cache=None，resolver=None（app_stats 的建構就是這樣）。
        let v = stats_json(&a);
        assert!(
            v["cache"].is_null(),
            "没建缓存后端时 cache 应该是 null：{v}"
        );
        assert!(
            v["certs"].is_null(),
            "没有 TLS 解析器时 certs 应该是 null：{v}"
        );
    }

    #[test]
    fn stats配了缓存时哪怕是0也是真读数不是null() {
        let cfg = fulcrum_config::compile_str("t.Fulcrumfile", STATS_DSL)
            .config
            .unwrap();
        let rt = Arc::new(Runtime::build(&cfg).unwrap());
        let cache = crate::cache::CacheHandle::new(crate::cache::Backend::open(None, 4096));
        let a = AdminApp::new(
            SharedRuntime::new(rt),
            vec![(8080, false)],
            None,
            Some(cache),
            None,
        );
        let v = stats_json(&a);
        assert!(
            !v["cache"].is_null(),
            "配了缓存后端，cache 不该是 null：{v}"
        );
        assert_eq!(v["cache"]["bytes"].as_u64(), Some(0), "{v}");
        assert_eq!(v["cache"]["entries"].as_u64(), Some(0), "{v}");
    }

    #[test]
    fn stats配了解析器但零证书时certs是空数组不是null() {
        let cfg = fulcrum_config::compile_str("t.Fulcrumfile", STATS_DSL)
            .config
            .unwrap();
        let rt = Arc::new(Runtime::build(&cfg).unwrap());
        let resolver = Arc::new(SniResolver::new());
        let a = AdminApp::new(
            SharedRuntime::new(rt),
            vec![(8080, false)],
            None,
            None,
            Some(resolver),
        );
        let v = stats_json(&a);
        assert!(!v["certs"].is_null(), "配了解析器，certs 不该是 null：{v}");
        assert_eq!(v["certs"].as_array().map(|a| a.len()), Some(0), "{v}");
    }

    #[test]
    fn stats证书到期读的是真解析器里的真证书_口径与fulcrum_cert_expiry_seconds相同() {
        let cfg = fulcrum_config::compile_str("t.Fulcrumfile", STATS_DSL)
            .config
            .unwrap();
        let rt = Arc::new(Runtime::build(&cfg).unwrap());
        let resolver = Arc::new(SniResolver::new());
        let ck = crate::metrics::自签("stats-cert.example");
        let 到期 = ck.not_after;
        resolver.install(&["stats-cert.example".to_string()], ck);
        let a = AdminApp::new(
            SharedRuntime::new(rt),
            vec![(8080, false)],
            None,
            None,
            Some(resolver),
        );
        let v = stats_json(&a);
        let certs = v["certs"].as_array().expect("certs 应该是数组");
        assert_eq!(certs.len(), 1, "{v}");
        assert_eq!(certs[0]["domain"], "stats-cert.example", "{v}");
        // ★ 與 fulcrum_cert_expiry_seconds 同一口徑：絕對 Unix 秒（裁決 R5）。
        let want = 到期
            .duration_since(std::time::UNIX_EPOCH)
            .expect("自签证书的 notAfter 在 1970 之后")
            .as_secs_f64();
        assert_eq!(certs[0]["not_after_unix"].as_f64(), Some(want), "{v}");
    }

    // ── overrides 一節：三格鍵原樣 + disabled/weight/dangling（brief §6 / §8 判據 5）──

    #[test]
    fn stats的overrides一节三格键原样_且disabled_weight_dangling各自正确() {
        let a = app_stats();
        // 兩把鍵分別檢驗 §6 點名的兩種獨立形態：只 disable（weight=None）／
        // 只 set_weight（disabled=false）。
        let k_shared = OverrideKey::new("http://pool.example:8080", "", "10.70.0.1:1");
        let k_solo = OverrideKey::new("http://pool.example:8080", "solo", "10.70.0.2:2");
        a.rt.overrides().slot(&k_shared).set_disabled(true);
        a.rt.overrides().slot(&k_solo).set_weight(9).unwrap();

        let v = stats_json(&a);
        let overrides = v["overrides"].as_array().expect("overrides 应该是数组");
        assert_eq!(overrides.len(), 2, "{v}");

        let find = |site: &str, id: &str, addr: &str| {
            overrides
                .iter()
                .find(|o| o["site"] == site && o["id"] == id && o["addr"] == addr)
                .unwrap_or_else(|| panic!("没找到键 (site={site}, id={id}, addr={addr})：{v}"))
        };
        // ⚠ 三格鍵必須**原樣**——維運照抄即可，不加工。
        let shared_row = find("http://pool.example:8080", "", "10.70.0.1:1");
        assert_eq!(shared_row["disabled"], true, "{v}");
        assert!(
            shared_row["weight"].is_null(),
            "只 disable 过，weight 该是 null：{v}"
        );
        assert_eq!(shared_row["dangling"], false, "{v}");

        let solo_row = find("http://pool.example:8080", "solo", "10.70.0.2:2");
        assert_eq!(solo_row["disabled"], false, "{v}");
        assert_eq!(solo_row["weight"].as_u64(), Some(9), "{v}");
        assert_eq!(solo_row["dangling"], false, "{v}");
    }

    // ── overrides 一節：**每項覆蓋帶設定時間**（R6 留下的那條，owner 拍板）──────
    //
    // ★ 語義是「**最後一次改動**」，⛔ 不是「首次被覆蓋」。語義本身由
    //   `fulcrum-runtime` 的 `tests/overrides.rs` 那三條釘住；這裡釘的是
    //   **它真的進了這份 JSON、而且換算沒錯**（brief §8 第 2 條：斷言打在最終文字上）。

    #[test]
    fn stats的overrides每一项都带着设置时间且是绝对unix秒() {
        let a = app_stats();
        let k = OverrideKey::new("http://pool.example:8080", "", "10.70.0.1:1");
        let 之前 = crate::metrics::unix_secs(std::time::SystemTime::now());
        a.rt.overrides().slot(&k).set_disabled(true);
        let 之后 = crate::metrics::unix_secs(std::time::SystemTime::now());

        let v = stats_json(&a);
        let row = &v["overrides"].as_array().expect("overrides 应该是数组")[0];
        let t = row["set_at_unix"]
            .as_f64()
            .unwrap_or_else(|| panic!("set_at_unix 应该是数字：{v}"));
        // ⚠ ⛔ 不斷言等於某個具體值：那要求判據與被測物件共用時鐘且零延遲。
        assert!(
            t >= 之前 && t <= 之后,
            "set_at_unix={t} 不在 [{之前}, {之后}] 里：{v}"
        );
        // ★ ★ **它必須是絕對時刻，⛔ 不是「已經過了多少秒」** —— 與 R5 在
        //   `not_after_unix` 上釘過的是同一條：相對秒讀起來完全正常，
        //   而它在兩次抓取之間的含義會變。判據：這個數落在「今天」的量級上，
        //   一個「已過秒數」的實作會是個接近 0 的小數。
        assert!(
            t > 1_700_000_000.0,
            "set_at_unix={t} 看起来不是绝对 Unix 秒（像是「已经过了多少秒」）：{v}"
        );
    }

    /// ★ ★ **同一次抓取裡，兩格覆蓋各有各的時間** —— ⛔ 不是同一個值。
    ///
    /// ⚠ 少了它，一個「在 `stats()` 裡取一次 `now()` 給所有行」的實作在上面那條
    /// 區間判據上是**綠的**，而它給出的讀數對每一格都是錯的。
    #[test]
    fn stats的overrides两格覆盖各有各的时间_不是同一个数() {
        let a = app_stats();
        let k1 = OverrideKey::new("http://pool.example:8080", "", "10.70.0.1:1");
        let k2 = OverrideKey::new("http://pool.example:8080", "solo", "10.70.0.2:2");
        a.rt.overrides().slot(&k1).set_disabled(true);
        std::thread::sleep(std::time::Duration::from_millis(5));
        a.rt.overrides().slot(&k2).set_weight(9).unwrap();

        let v = stats_json(&a);
        let rows = v["overrides"].as_array().expect("overrides 应该是数组");
        let t = |id: &str| {
            rows.iter()
                .find(|o| o["id"] == id)
                .unwrap_or_else(|| panic!("没找到 id={id}：{v}"))["set_at_unix"]
                .as_f64()
                .unwrap_or_else(|| panic!("set_at_unix 应该是数字：{v}"))
        };
        assert!(
            t("solo") > t(""),
            "后设的那格时间没有更晚：先设的 {}、后设的 {}：{v}",
            t(""),
            t("solo")
        );
    }

    // ★ ★ ★ 判據寫法紀律（brief §8 三條硬要求第 3 條）：總數與懸空數必須不
    //   相等，否則把 `dangling` 算反、或兩格印同一個值，判據照樣綠。
    #[test]
    fn stats的overrides总数与悬空数不相等时两个数不会读串() {
        let a = app_stats();
        let k_shared = OverrideKey::new("http://pool.example:8080", "", "10.70.0.1:1");
        let k_solo = OverrideKey::new("http://pool.example:8080", "solo", "10.70.0.2:2");
        a.rt.overrides().slot(&k_shared).set_disabled(true);
        a.rt.overrides().slot(&k_solo).set_disabled(true);

        // 換一份只留 solo 那條路的設定——共享鍵那把因此懸空，solo 那把仍然活著。
        let (status, text) = a.load(
            &json_of(
                "http://pool.example:8080 {\n  \
                 handle /c/* {\n    reverse_proxy 10.70.0.2:2 {\n      id solo\n    }\n  }\n  \
                 handle {\n    respond 200\n  }\n}\n",
            ),
            Some("overrides=keep"),
        );
        assert_eq!(status, 200, "{text}");

        let v = stats_json(&a);
        let overrides = v["overrides"].as_array().expect("overrides 应该是数组");
        assert_eq!(overrides.len(), 2, "总数应该是 2：{v}");
        let dangling_count = overrides.iter().filter(|o| o["dangling"] == true).count();
        assert_eq!(dangling_count, 1, "悬空数应该是 1（只有共享键那把）：{v}");
        assert_ne!(
            overrides.len(),
            dangling_count,
            "总数与悬空数必须不同，否则写反也测不出来"
        );

        let shared_row = overrides
            .iter()
            .find(|o| {
                o["site"] == "http://pool.example:8080"
                    && o["id"] == ""
                    && o["addr"] == "10.70.0.1:1"
            })
            .expect("共享键那一项应该还在（悬空但没删——R8）");
        assert_eq!(shared_row["dangling"], true, "{v}");
        let solo_row = overrides
            .iter()
            .find(|o| o["id"] == "solo")
            .expect("solo 那一项应该还在");
        assert_eq!(solo_row["dangling"], false, "{v}");
    }

    // ── fanout：撞鍵 ⇒ proxies==2，且顯眼地擺在 fanout_shared（brief §6 / §8 判據 6）──
    //
    // ★ ★ ★ 判據寫法紀律：總鍵數與 shared 那一項的 `proxies` 必須不相等——
    //   這裡特地加一把 `third` 鍵，把 fanout 總數拉到 3，與 proxies=2 錯開
    //   （只有共享 + solo 兩把鍵的話，總數與 proxies 會恰好都是 2）。
    #[test]
    fn stats的fanout两条reverse_proxy撞同一个键时proxies等于2_且显眼摆在fanout_shared() {
        let cfg = fulcrum_config::compile_str(
            "t.Fulcrumfile",
            "http://pool.example:8080 {\n  \
             handle /a/* {\n    reverse_proxy 10.70.0.1:1\n  }\n  \
             handle /b/* {\n    reverse_proxy 10.70.0.1:1\n  }\n  \
             handle /c/* {\n    reverse_proxy 10.70.0.2:2 {\n      id solo\n    }\n  }\n  \
             handle /d/* {\n    reverse_proxy 10.70.0.3:3 {\n      id third\n    }\n  }\n\
             }\n",
        )
        .config
        .unwrap();
        let rt = Arc::new(Runtime::build(&cfg).unwrap());
        let a = AdminApp::new(
            SharedRuntime::new(rt),
            vec![(8080, false)],
            None,
            None,
            None,
        );

        let v = stats_json(&a);
        let fanout = v["fanout"].as_array().expect("fanout 应该是数组");
        assert_eq!(
            fanout.len(),
            3,
            "三把互不相同的键（共享地址一把 + 两把独立）：{v}"
        );

        let shared_row = fanout
            .iter()
            .find(|f| {
                f["site"] == "http://pool.example:8080"
                    && f["id"] == ""
                    && f["addr"] == "10.70.0.1:1"
            })
            .expect("共享地址那把键应该在 fanout 里");
        assert_eq!(shared_row["proxies"].as_u64(), Some(2), "{v}");
        for id in ["solo", "third"] {
            let row = fanout.iter().find(|f| f["id"] == id).unwrap();
            assert_eq!(row["proxies"].as_u64(), Some(1), "{id} 应该 proxies=1：{v}");
        }

        // ★ ★ 顯眼那一半：fanout_shared 只含 proxies>1 的那一項，不是混在
        //   fanout 裡的一個數字（brief §6）。
        let shared = v["fanout_shared"]
            .as_array()
            .expect("fanout_shared 应该是数组");
        assert_eq!(shared.len(), 1, "{v}");
        assert_eq!(shared[0]["proxies"].as_u64(), Some(2), "{v}");
        assert_eq!(shared[0]["addr"], "10.70.0.1:1", "{v}");

        assert_ne!(
            fanout.len() as u64,
            shared_row["proxies"].as_u64().unwrap(),
            "总键数（3）与 proxies（2）必须不相等——判据写法纪律"
        );
    }

    // ── 設定裝載時間（裁決 R15，brief §8 判據 3）─────────────────────────────

    #[test]
    fn stats的配置装载时间在load之后会变() {
        let a = app_stats();
        let before = stats_json(&a)["config_loaded_at_unix"]
            .as_f64()
            .expect("config_loaded_at_unix 应该是数字");
        // ★ 睡一下：確保兩次 `SystemTime::now()` 不會因為時鐘粒度巧合相等——
        //   這是測試自己的可靠性保證，與被測邏輯無關。
        std::thread::sleep(std::time::Duration::from_millis(5));
        let (status, text) = a.load(
            &json_of("http://pool.example:8080 {\n  respond 204\n}\n"),
            Some("overrides=clear"),
        );
        assert_eq!(status, 200, "{text}");
        let after = stats_json(&a)["config_loaded_at_unix"]
            .as_f64()
            .expect("config_loaded_at_unix 应该是数字");
        assert!(
            after > before,
            "load 之后配置装载时间应该变新：before={before} after={after}"
        );
    }

    #[test]
    fn stats的配置装载时间只从sharedruntime读_adminrs不自己另算一份() {
        // ★ ★ 結構判據（同「admin_rs這條路徑不經過overridelayer_get」那條的
        //   紀律）：正面——`stats()` 委託 `self.rt.loaded_at()`；反面——本檔案
        //   的產品程式碼不出現 `SystemTime::now()`（那會是第二份狀態，與 R15 /
        //   `overrides` 欄位上那句紀律同源）。
        let whole = include_str!("admin.rs");
        let prod = whole
            .split("#[cfg(test)]")
            .next()
            .expect("这个文件里应该有 #[cfg(test)]");
        assert!(
            prod.contains("self.rt.loaded_at()"),
            "stats() 应该委托 SharedRuntime::loaded_at()，不是自己再算一份"
        );
        assert!(
            !prod.contains("SystemTime::now()"),
            "admin.rs 的产品代码不许自己读时钟——配置装载时间只有 SharedRuntime 上那一份"
        );
    }

    // ★ ★ ★ 結構判據（修復輪 1，評審 I3）：`stats()` 曾經在已經持有 `rt`
    //   之後，又通過不吃參數的 `override_entries()` 內部再調一次
    //   `self.rt.current()`——一次並行的 `POST /load` 落在兩次之間，
    //   `overrides` 一節的 `dangling` 就會按**新**設定算，而 `upstreams`/
    //   `fanout` 按**舊**設定列。正面：`stats()` 用
    //   `override_entries_of(&rt)`；反面：`stats()` 自己的函式體裡不再出現
    //   不吃參數的那個版本（只看 `stats()` 到下一個函式 `reply_json` 之間的
    //   原始碼——`admin.rs` 別處的 `load()` 仍然合法地用著不吃參數那個版本，
    //   不能把整份 `prod` 一起判）。
    #[test]
    fn stats的overrides走override_entries_of不重复取快照() {
        let whole = include_str!("admin.rs");
        let prod = whole
            .split("#[cfg(test)]")
            .next()
            .expect("这个文件里应该有 #[cfg(test)]");
        let stats_start = prod
            .find("fn stats(&self) -> String {")
            .expect("stats() 函数应该存在");
        let stats_and_after = &prod[stats_start..];
        let stats_end = stats_and_after.find("async fn reply_json").expect(
            "reply_json 应该紧跟在 stats() 后面——如果这里报错，说明两者\
                     之间插了别的函数，把下面的边界跟着改一下",
        );
        let stats_body = &stats_and_after[..stats_end];
        assert!(
            stats_body.contains("override_entries_of(&rt)"),
            "stats() 应该调 override_entries_of(&rt)，复用已经持有的那份快照"
        );
        assert!(
            !stats_body.contains(".override_entries()"),
            "stats() 不许调不吃参数的 override_entries()——那会在已经持有 `rt` \
             之后又让 SharedRuntime 内部再取一次 current()"
        );
    }

    // ── 404 的「可用」清單裡有 GET /stats（brief §8 判據 8）──────────────────

    #[test]
    fn 不认识的路径的可用清单里有_stats() {
        let (status, text) = unknown_route("POST", "/nope");
        assert_eq!(status, 404);
        assert!(text.contains("GET /stats"), "{text}");
    }

    // ── R12：與 /metrics 同源（brief §5 / §8 判據 2，最重的一條）─────────────
    //
    // ★ ★ ★ 判據寫法紀律：inflight 求和與 healthy 取合取都要走**非退化**值——
    //   兩個 Upstream 物件各自 `acquire()` 不同的次數（3 與 2，和=5，互不相等，
    //   也都不是容易與布爾值混淆的 0/1）、一個健康一個不健康（取合取=false，
    //   與「取析取」「只取第一個」都不同）。⛔ ⛔ 不許只斷言「兩邊都不是 0」。

    #[test]
    fn stats按地址归并后与metrics逐项相等_且走非退化值() {
        let cfg = fulcrum_config::compile_str(
            "t.Fulcrumfile",
            "http://pool.example:8080 {\n  \
             handle /a/* {\n    reverse_proxy 10.80.0.1:1\n  }\n  \
             handle /b/* {\n    reverse_proxy 10.80.0.1:1\n  }\n\
             }\n",
        )
        .config
        .unwrap();
        let rt = Arc::new(Runtime::build(&cfg).unwrap());
        let a = AdminApp::new(
            SharedRuntime::new(rt),
            vec![(8080, false)],
            None,
            None,
            None,
        );

        let live_rt = a.rt.current();
        let mut matches: Vec<&fulcrum_runtime::ProxyTarget> = live_rt
            .keyed_proxies()
            .into_iter()
            .filter(|p| p.site == "http://pool.example:8080" && p.id.is_empty())
            .map(|p| p.target)
            .collect();
        assert_eq!(matches.len(), 2, "夹具应该有两条撞键的 reverse_proxy");
        let target_b = matches.remove(1);
        let target_a = matches.remove(0);

        // 非退化：3 與 2（和=5，互不相等，也不是 0/1）。
        for _ in 0..3 {
            target_a.upstreams[0].acquire();
        }
        for _ in 0..2 {
            target_b.upstreams[0].acquire();
        }
        // 一個健康一個不健康——取合取 = false，取析取會把這裡算成 true。
        target_a.upstreams[0].set_healthy(false);
        assert!(
            target_b.upstreams[0].is_healthy(),
            "夹具前提：另一个默认健康"
        );

        // ── /stats：按站台 × 上游列，不歸併 ──────────────────────────────
        let v = stats_json(&a);
        let rows: Vec<&serde_json::Value> = v["upstreams"]
            .as_array()
            .expect("upstreams 应该是数组")
            .iter()
            .filter(|u| u["addr"] == "10.80.0.1:1")
            .collect();
        assert_eq!(
            rows.len(),
            2,
            "同一个地址被两条 reverse_proxy 引用，应该出两行：{v}"
        );
        let stats_inflight_sum: u64 = rows.iter().map(|r| r["inflight"].as_u64().unwrap()).sum();
        let stats_healthy_and: bool = rows.iter().all(|r| r["healthy"].as_bool().unwrap());

        // ── /metrics：真的調 metrics::snapshot()，不是重新實作一遍歸併 ──────
        let metrics_text = crate::metrics::render_snapshot_for_test(&crate::metrics::LiveSources {
            runtime: Some(a.rt.clone()),
            resolver: None,
            acme: None,
            // ★ 本條判據只對上游那兩個族，連接那兩個族與它無關 ⇒ 不接。
            conn: None,
        });
        let metrics_inflight =
            metrics_gauge_value(&metrics_text, "fulcrum_upstream_inflight", "10.80.0.1:1");
        let metrics_healthy =
            metrics_gauge_value(&metrics_text, "fulcrum_upstream_healthy", "10.80.0.1:1");

        // ── ★ ★ ★ 同源判據本身，**排在最前面**（修復輪 1，評審 I2）──────────
        //
        //   ⚠ ⚠ ⚠ 順序是有意的：這兩條必須在任何「/stats 側讀數是不是等於
        //   夾具擺的字面值 5/false」這類判據**之前**跑到。反證時注入的是
        //   「/stats 不讀真原子量」，它會**同時**讓 `stats_inflight_sum` 偏離
        //   字面值 5**和**讓這兩條同源斷言不相等——如果字面值斷言排在前面，
        //   反證會在那一條先 abort，這兩條「真正的」R12 判據本次根本沒被
        //   執行到（本儲存庫 `PLAN.md` Global Constraint 8 點名過的同一個坑：
        //   交叉斷言被等值斷言擋在後面，從沒被驗過）。⇒ 同源判據必須是
        //   **第一個**會讀到 `stats_inflight_sum`/`stats_healthy_and` 的斷言。
        assert_eq!(
            stats_inflight_sum as f64, metrics_inflight,
            "R12：/stats 按地址求和之后应该与 /metrics 逐项相等（inflight）：{v}\n{metrics_text}"
        );
        assert_eq!(
            if stats_healthy_and { 1.0 } else { 0.0 },
            metrics_healthy,
            "R12：/stats 按地址取合取之后应该与 /metrics 逐项相等（healthy）：{v}\n{metrics_text}"
        );

        // ── 非退化值檢查，**排在同源判據之後**：確認兩邊落的都是我們擺的
        //    3+2=5／一健康一不健康，不是巧合的退化值（0/0、true/true）。
        assert_eq!(stats_inflight_sum, 5, "3+2=5，非退化值：{v}");
        assert!(!stats_healthy_and, "一个不健康 ⇒ 取合取应该是 false：{v}");
        assert_eq!(metrics_inflight, 5.0, "{metrics_text}");
        assert_eq!(metrics_healthy, 0.0, "{metrics_text}");
    }

    /// 從 Prometheus text exposition 裡取一個 gauge 的值。
    fn metrics_gauge_value(text: &str, name: &str, label_value: &str) -> f64 {
        let needle = format!("{name}{{upstream=\"{label_value}\"}} ");
        let line = text
            .lines()
            .find(|l| l.starts_with(&needle))
            .unwrap_or_else(|| panic!("没找到 `{needle}` 这一行：{text}"));
        line[needle.len()..].trim().parse().unwrap()
    }

    // ── R12 的另一半：不快取 Runtime 快照（brief §8 判據 9）──────────────────

    #[test]
    fn stats不缓存runtime快照_load换掉上游之后立刻反映新的那份() {
        let a = app_stats(); // STATS_DSL：pool.example:8080 上有 10.70.0.1:1 與 10.70.0.2:2
        let before = stats_json(&a);
        let before_addrs: std::collections::BTreeSet<String> = before["upstreams"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u["addr"].as_str().unwrap().to_string())
            .collect();
        assert!(before_addrs.contains("10.70.0.1:1"), "{before}");

        // 換一份完全不同的上游（同一個站台、同一個連接埠集，不撞 409）。
        let (status, text) = a.load(
            &json_of("http://pool.example:8080 {\n  reverse_proxy 10.90.0.9:9\n}\n"),
            Some("overrides=clear"),
        );
        assert_eq!(status, 200, "{text}");

        let after = stats_json(&a);
        let after_addrs: std::collections::BTreeSet<String> = after["upstreams"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u["addr"].as_str().unwrap().to_string())
            .collect();
        assert!(
            !after_addrs.contains("10.70.0.1:1"),
            "★ ★ ★ R12：/stats 不该还看得到换掉之前的老上游——{after}"
        );
        assert!(
            after_addrs.contains("10.90.0.9:9"),
            "★ ★ ★ R12：/stats 应该立刻反映刚换上的新上游——{after}"
        );
    }
}
