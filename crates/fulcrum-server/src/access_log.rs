//! 结构化访问日志（**M2 批 L 第 ② + ③ 步**；D7 由 **G113 + G114** 结案）。
//!
//! 字段契约定稿在 [`docs/architecture/observability.md`](../../../docs/architecture/observability.md)
//! —— **那一份是权威**，本模块是它的实现。
//!
//! ★ 第 ② 步做的是**固定集**；第 ③ 步加的是**白名单头**（[`collect`]）
//! 与 **TLS 四格**（[`TlsFields`]）。⚠ ⚠ 后者有一处登记在案的缺口（**D27**）：
//! **h3 上四格全都取不到**，见 [`TlsFields`] 的类型文档。
//!
//! # ★ ★ 三件被有意分开的事
//!
//! | | 谁 |
//! |---|---|
//! | 一行长什么样（字段清单、顺序、缺省怎么表示）| 本模块的 [`Record::to_json_line`] |
//! | 一行**写到哪** | 本模块的 [`Sink`]（进程级注册表，按路径去重）|
//! | 一行**要不要写** | [`fulcrum_runtime::LogLevel::records`]（阈值）|
//!
//! ★ 分开的理由是它们各自会独立变化：加字段不该动写入口，换输出不该动字段清单。
//!
//! # ⚠ ⚠ 收尾这一处**同时**喂指标（**M2 批 M**）
//!
//! [`Record::finish`] 先无条件喂 `fulcrum_requests_total` 与
//! `fulcrum_request_duration_seconds`，再调 [`Record::emit`]（照旧按 `log` 早退）。
//! ★ 绑在一处的理由与代价都写在 `finish` 的文档上，一句话：
//! **两处各算一遍 `outcome` / `status` 迟早分家，而分家的表现是两个数字都言之凿凿、却对不上。**
//!
//! # ⚠ ⚠ 「取不到的字段不出现，而不是给 `null`」
//!
//! 那是契约里写死的一条，落法在 [`JsonLine`] 上：只有 `Some` 才会被写进去。
//! ★ 理由是 `null` 在 logfmt 里没有对应物 —— 而 G113 取「JSON 单一格式、字段扁平」
//! 的全部意义就是让「以后想加 logfmt」只需换一个序列化器。
//!
//! # ⚠ 代价写在明处：这一行是**同步**写的
//!
//! 每条请求收尾时在一把 `Mutex` 里做一次 `write_all`（一行几百字节，追加模式）。
//! ★ 与 §11 **D20**（磁盘缓存的同步 I/O）是同一类取舍，而这一处小得多：
//! 那边是一次冷盘**读**，这边是一次小**写**。⚠ 但它同样在请求路径上，
//! 所以照 D20 的口径记在这里 —— **M3 对拍时一起量**，别现在猜。

use fulcrum_runtime::{HeaderPick, LogLevel, LogOutput, LogRt};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

/// 一行访问日志的**写入口**。
///
/// ★ ★ 按**绝对路径**去重：多个站点写同一个文件时，它们共用同一个句柄与同一把锁
/// ⇒ 两条日志行不会互相插进对方中间。⚠ 各开各的话，交错是**随机出现**的，
/// 而那种坏法在小流量下永远不显形。
#[derive(Clone)]
pub(crate) enum Sink {
    Stderr,
    File(Arc<Mutex<std::fs::File>>),
}

/// 进程级的文件句柄注册表：**绝对路径 → 句柄**。
fn files() -> &'static Mutex<BTreeMap<String, Arc<Mutex<std::fs::File>>>> {
    static FILES: OnceLock<Mutex<BTreeMap<String, Arc<Mutex<std::fs::File>>>>> = OnceLock::new();
    FILES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// 把这份运行时图里配到的每一个日志文件都**在装载时打开一次**。
///
/// # ★ ★ ★ 为什么这一步必须在装载时做，而且失败要判红
///
/// 一个打不开的日志文件（路径打错、目录不存在、权限不够）如果拖到第一个请求才发现，
/// 现场是**服务完全正常、而日志一行都没有** —— 那正是本仓库反复点名的那种
/// 「没有任何东西会说出来的失效」，而且它偏偏发生在**观测**这一块上：
/// ⚠ ⚠ **一个用来「出了事你能知道」的东西，自己坏掉时没人知道。**
///
/// ⇒ 与 `plan_tls` 对「配置里给的 PEM 读不出来」的处置逐字同形：**硬错误，装不上**。
/// ★ 而它天然是原子的：`POST /load` 那条路在换之前调用本函数，失败就整份不生效。
pub(crate) fn open_all(rt: &fulcrum_runtime::Runtime) -> Result<usize, Vec<String>> {
    let mut errors = Vec::new();
    let mut opened = 0usize;
    for site in rt.sites() {
        let Some(cfg) = site.log.as_ref() else {
            continue;
        };
        let LogOutput::File(path) = &cfg.output else {
            continue;
        };
        match open_file(path) {
            Ok(true) => opened += 1,
            Ok(false) => {}
            Err(e) => errors.push(format!(
                "站点 {} 的 `log {{ output file {path} }}`：{e}",
                site.name
            )),
        }
    }
    if errors.is_empty() {
        Ok(opened)
    } else {
        Err(errors)
    }
}

/// 打开（或复用）一个日志文件。返回 `Ok(true)` 表示这一次真的新开了一个。
fn open_file(path: &str) -> std::io::Result<bool> {
    let mut map = files().lock().unwrap_or_else(|p| p.into_inner());
    if map.contains_key(path) {
        return Ok(false);
    }
    // ⚠ `append(true)`：换代时**两代会同时写同一个文件**（老一代还在排空），
    //   而 append 模式下每次 write 的定位与写入在内核里是原子的 ⇒ 不会互相覆盖。
    //   ★ 这一条是「零停机换代」这个产品性质对日志提出的要求，不是通用习惯。
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    map.insert(path.to_string(), Arc::new(Mutex::new(f)));
    Ok(true)
}

impl Sink {
    /// 按配置取一个写入口。★ 文件那条**只查不开** —— 开在 [`open_all`] 里，
    /// 而那一步失败时这份配置根本装不上。
    fn for_output(out: &LogOutput) -> Option<Sink> {
        match out {
            LogOutput::Stderr => Some(Sink::Stderr),
            LogOutput::File(p) => {
                let map = files().lock().unwrap_or_else(|e| e.into_inner());
                map.get(p).map(|f| Sink::File(f.clone()))
            }
        }
    }

    fn write_line(&self, line: &str) {
        match self {
            Sink::Stderr => {
                // ⚠ 一次 `write_all` 而不是 `eprintln!` 两次写：后者可能把
                //   行与换行分成两次 syscall，而两代进程同时写时那就会交错。
                let mut out = std::io::stderr().lock();
                let _ = out.write_all(line.as_bytes());
            }
            Sink::File(f) => {
                let mut g = f.lock().unwrap_or_else(|e| e.into_inner());
                let _ = g.write_all(line.as_bytes());
            }
        }
    }
}

/// 一行扁平 JSON 的拼装器。
///
/// ★ 不用 `serde_json::Map`，理由有两条，都不是风格：
/// ① **字段顺序**要按契约里那个顺序（`Map` 默认按字典序，读起来是乱的）；
/// ② **缺省要「不出现」而不是 `null`** —— 那是 `Option` 在这里唯一的表达方式。
/// ⚠ 值的转义仍然交给 `serde_json`：手写转义是本仓库明令不做的那一类。
struct JsonLine(String);

impl JsonLine {
    fn new() -> JsonLine {
        JsonLine(String::with_capacity(384))
    }

    fn sep(&mut self) {
        self.0.push(if self.0.is_empty() { '{' } else { ',' });
    }

    fn str(&mut self, k: &str, v: &str) {
        self.sep();
        let _ = write!(
            self.0,
            "{}:{}",
            serde_json::Value::from(k),
            serde_json::Value::from(v)
        );
    }

    fn opt_str(&mut self, k: &str, v: Option<&str>) {
        if let Some(v) = v {
            self.str(k, v);
        }
    }

    fn num(&mut self, k: &str, v: impl std::fmt::Display) {
        self.sep();
        let _ = write!(self.0, "{}:{v}", serde_json::Value::from(k));
    }

    fn finish(mut self) -> String {
        if self.0.is_empty() {
            self.0.push('{');
        }
        self.0.push_str("}\n");
        self.0
    }
}

/// 一次请求在日志里的样子。**由 [`crate::Downstream`] 持有，逐段填。**
///
/// ⚠ ⚠ 它**不是**「一份日志行的缓存」：`status` / `resp_size` 不在这里，
/// 它们在收尾时直接问 `ServerSession` —— ★ **能从被测对象本身问到的东西，
/// 就不要在旁边再记一份**，否则两份迟早会不一致，而不一致的那一天没有任何东西会说。
pub(crate) struct Record {
    /// 这个站点要不要记、记到哪。`None` = 不记（没配 `log`，或还没路由到站点）。
    pub target: Option<Arc<LogRt>>,
    pub started: SystemTime,
    /// ★ 取自**入口**，不是推断出来的 —— 见 [`crate::Downstream`] 的两个构造函数。
    pub proto: &'static str,
    pub method: String,
    pub host: String,
    /// **原始**请求目标（path + query），`rewrite` **之前**。
    pub uri: String,
    pub remote_ip: Option<IpAddr>,
    pub remote_port: u16,
    pub outcome: &'static str,
    pub site: Option<String>,
    /// 请求**实际匹配到的那条地址字面量**（G121），批 M 的 `fulcrum_requests_total`
    /// 取数点用它当 `site` 标签 —— **不进 `to_json_line`**，只是个中转站。
    ///
    /// ⚠ ⚠ 它与上面的 `site` 是两件不同的事，长得像是巧合：`site` = 站点的名字 =
    /// 第一个地址的原文（访问日志的字段契约，一个字不动）；这一格 = 命中的那一条，
    /// 只留主机名。两者同名纯属巧合，互不派生，见 [`fulcrum_runtime::Routed`] 的
    /// `site_addr` 字段文档。
    pub site_addr: Option<Arc<str>>,
    pub upstream: Option<String>,
    pub cache: Option<String>,
    /// TLS 那四格（**M2 批 L 第 ③ 步**）。`None` = 这条连接不是 TLS。
    ///
    /// ⚠ ⚠ **今天它在 h3 上恒为 `None`，而 h3 连接是 TLS 的** —— 见 [`TlsFields`]。
    pub tls: Option<TlsFields>,
    /// 白名单命中的请求头，**已经是最终形态**（日志键 + 值）。
    ///
    /// ★ 存成 `Vec<(String, String)>` 而不是在这里存一份头映射：
    /// 白名单是**装载时**就算完的（[`fulcrum_runtime::HeaderPick`]），
    /// 而「这条请求上有没有这个头」是**收尾时**问会话的 ——
    /// ⇒ 到这里两件事都已经有答案了，剩下的只是写出去。
    pub req_headers: Vec<(String, String)>,
    /// 白名单命中的响应头。同上。
    pub resp_headers: Vec<(String, String)>,
}

/// 一条连接的 TLS 信息（**M2 批 L 第 ③ 步**）。
///
/// # h1/h2 四格、h3 三格 —— 少的那一格是有意的
///
/// `version` / `cipher` 来自 `ServerSession::digest()` 里的 `SslDigest`，
/// `sni` / `alpn` 来自 [`crate::tls::HandshakeInfo`] 塞进 `SslDigest.extension` 的那一份。
/// ★ h3 走同一段代码：[`crate::quic::h3_session::quic_digest`] 在连接那一层现造一个同类型的
/// `Digest`，于是 `digest()` 在 h3 上也有值 —— 两条路在这一层**没有分叉**。
///
/// ⚠ **`tls_cipher` 在 h3 上恒为空**，那一格因此不出现：quiche 的 `Handshake::cipher()`
/// 锁在私有 `mod tls` 里，取不到。⇒ 宁可缺一格，也不写一个编出来的值。
/// ★ 守它的是 [`tests/log/run.sh`] 第八步：h1/h2 四格都在、h3 三格都在，
/// **且 h3 的日志行里 `tls_cipher` 必须不出现** —— 那条反向断言防的是有人给它编一个值。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct TlsFields {
    /// `TLSv1.3` 这种。⚠ 空串 = 问不出来 ⇒ 那一格不出现。
    pub version: String,
    /// `TLS_AES_256_GCM_SHA384` 这种。⚠ 空串同上。
    pub cipher: String,
    /// 客户端发的 SNI。`None` = 它没发（按 IP 直连就没有）。
    pub sni: Option<String>,
    /// 协商出来的 ALPN（`h2` / `http/1.1` / `acme-tls/1`）。`None` = 没协商出来。
    pub alpn: Option<String>,
}

/// 按白名单从一份头映射里取值。
///
/// ★ ★ 一个函数同时服务请求头与响应头 —— 两者的规范化规则**逐字相同**
/// （小写、`-` 换 `_`、加前缀），差别只在前缀，而前缀已经在
/// [`HeaderPick::key`] 里算完了。⚠ 各写一份的话，将来改多值连接符会漏掉一处。
///
/// ⚠ ⚠ **值用 lossy 转 UTF-8，而不是「转不了就丢掉」**：头值是 opaque 字节
/// （RFC 9110 的 `obs-text` 允许 0x80–0xFF），而「丢掉」在日志里的样子是
/// **这个头不存在** —— 那是一句假话。★ 转义交给 `serde_json`，与 `uri` 那格同源。
pub(crate) fn collect(picks: &[HeaderPick], map: &http::HeaderMap) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for p in picks {
        // ⚠ 多值头按 `, ` 连接（RFC 9110 §5.3：同名字段行等价于逗号连接的一行）。
        let joined = map
            .get_all(p.lookup.as_str())
            .iter()
            .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
            .collect::<Vec<_>>()
            .join(", ");
        // ★ 「在白名单里」与「这条请求上真的有」是两件事 —— 后者不成立就不出现。
        if map.get(p.lookup.as_str()).is_some() {
            out.push((p.key.clone(), joined));
        }
    }
    out
}

impl Record {
    pub(crate) fn new(proto: &'static str) -> Record {
        Record {
            target: None,
            started: SystemTime::now(),
            proto,
            method: String::new(),
            host: String::new(),
            uri: String::new(),
            remote_ip: None,
            remote_port: 0,
            // ⚠ 这个默认值只在「连路由都没走到」时才会被写出来
            //   （读不到请求头那一类）。★ 它有意不是空串：一个空的 `outcome`
            //   在日志里读起来像「字段丢了」，而这里的事实是「什么都没发生」。
            outcome: "aborted",
            site: None,
            site_addr: None,
            upstream: None,
            cache: None,
            tls: None,
            req_headers: Vec::new(),
            resp_headers: Vec::new(),
        }
    }

    /// 拼出那一行（**不含**写出去这一步）。
    ///
    /// ★ 拆成一个纯函数，是为了让判据可以直接量它 —— 端到端只能证「日志里有这么一行」，
    /// 证不了「每一个字段都按契约来」。
    pub(crate) fn to_json_line(&self, status: u16, resp_size: usize, now: SystemTime) -> String {
        let mut j = JsonLine::new();
        j.str(
            "ts",
            &fulcrum_runtime::template::format_rfc3339_millis(self.started),
        );
        j.str("level", LogLevel::name_for(status));
        j.str("proto", self.proto);
        j.str("method", &self.method);
        j.str("host", &self.host);
        j.str("uri", &self.uri);
        j.num("status", status);
        j.num("resp_size", resp_size);
        // ⚠ 三位小数：毫秒以下没有意义，而 `{:.3}` 让它在日志里是定长的。
        let ms = now
            .duration_since(self.started)
            .map(|d| d.as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        j.num("duration_ms", format!("{ms:.3}"));
        j.str("outcome", self.outcome);
        j.opt_str("site", self.site.as_deref());
        j.str(
            "remote_ip",
            &self
                .remote_ip
                .map(|i| i.to_string())
                .unwrap_or_else(|| "-".to_string()),
        );
        j.num("remote_port", self.remote_port);
        j.opt_str("upstream", self.upstream.as_deref());
        j.opt_str("cache", self.cache.as_deref());
        // ── TLS 那四格（**M2 批 L 第 ③ 步**）─────────────────────────────
        //
        // ⚠ 空串当成「问不出来」而不是写一个空值：`tls_cipher=""` 在日志里
        //   读起来像「协商出了一个名字叫空的套件」，而事实是这条连接没有 TLS
        //   或者那个字段拿不到。★ 与「取不到的字段不出现」是同一条契约。
        if let Some(t) = self.tls.as_ref() {
            if !t.version.is_empty() {
                j.str("tls_version", &t.version);
            }
            if !t.cipher.is_empty() {
                j.str("tls_cipher", &t.cipher);
            }
            j.opt_str("tls_sni", t.sni.as_deref());
            j.opt_str("tls_alpn", t.alpn.as_deref());
        }
        // ── 白名单头（同上）。★ 顺序 = 配置里写的顺序，不是字典序 ──────
        //   ⚠ 两组分开写，是为了让 `req_hdr_*` 与 `resp_hdr_*` 在一行里挨着 ——
        //     它们本来就来自两个不同的东西，混在一起读起来要靠前缀去分。
        for (k, v) in &self.req_headers {
            j.str(k, v);
        }
        for (k, v) in &self.resp_headers {
            j.str(k, v);
        }
        j.finish()
    }

    /// 记一行 —— 如果这个站点配了 `log`，而且这条状态码过得了阈值。
    pub(crate) fn emit(&self, status: u16, resp_size: usize) {
        let Some(cfg) = self.target.as_ref() else {
            return;
        };
        if !cfg.level.records(status) {
            return;
        }
        let Some(sink) = Sink::for_output(&cfg.output) else {
            // ⚠ 走到这里意味着装载时开过的那个句柄不见了 —— `open_all` 保证不会。
            //   ★ 不 panic、也不静默：说一句，然后这条请求照常收尾。
            log::error!(
                "访问日志的写入口不见了（{:?}）—— 这一条没记下来",
                cfg.output
            );
            return;
        };
        sink.write_line(&self.to_json_line(status, resp_size, SystemTime::now()));
    }

    /// 一次请求的**收尾**：先喂指标，再记那一行（**M2 批 M**，G116–G121）。
    ///
    /// # ★ ★ ★ 为什么两件事必须绑在同一个方法上
    ///
    /// `fulcrum_requests_total` 与 `fulcrum_request_duration_seconds` 的取数点
    /// **只能有这一处**。⚠ 指标与日志各算一遍 `outcome` / `status` 的话迟早分家，
    /// 而**分家的表现是两个数字都言之凿凿、却对不上**（D18 / G66 的形状）——
    /// 两边各自都自洽，于是没有任何东西会说出来。
    /// ⇒ 同一个 [`Record`]、同一份 `status` / `resp_size`，一处算完两处用。
    ///
    /// # ⚠ ⚠ 指标在前，而且**不受站点的 `log` 配置管**
    ///
    /// [`Record::emit`] 在「这个站点没配 `log`」与「这条状态码过不了阈值」时早退，
    /// 而**没配日志的站点照样要有指标**：把指标也挂到 `log` 底下，代价是一整个站点
    /// 在监控里凭空消失，而配置里看不出任何问题 —— 那是更坏的一侧。
    /// ⇒ 「与访问日志同一段代码」说的是**同一份取值**，不是「同一个早退分支」。
    ///
    /// ★ 代价写在明处：**「指标增量 == 访问日志行数」这条一致性只在配了
    /// `log { level info }` 的站点上成立**，判据场景里写死这一点。
    ///
    /// ⚠ [`Record::emit`] 从此**只剩这一个调用方**。
    pub(crate) fn finish(&self, status: u16, resp_size: usize) {
        self.meter(status, SystemTime::now());
        self.emit(status, resp_size);
    }

    /// 把这一条喂给指标注册表。
    ///
    /// ★ 拆成一个收 `now` 的函数，理由与 [`Record::to_json_line`] 一样：
    /// 判据要能不靠真实时钟量它。
    fn meter(&self, status: u16, now: SystemTime) {
        // ★ `site` 取**请求实际命中的那条地址字面量**（G121）；没匹配到站点时取
        //   `<none>`（R2）—— 于是 `fulcrum_requests_total` 是**真的总数**，
        //   一致性门才有东西可对。⚠ 尖括号形式在合法地址字面量里不可能出现，
        //   与 G118 的 `<other>` 是同一族写法。
        let site = self.site_addr.as_deref().unwrap_or("<none>");
        // ⚠ 标签的个数与顺序照 `metrics::FAMILIES` 的声明，对不上就地 panic。
        crate::metrics::REQUESTS_TOTAL.inc(&[site, self.outcome, status_class(status), self.proto]);
        // ⚠ 单位是**秒**，与桶边界和名字里的 `_seconds` 同一口径。
        // ⚠ `duration_since` 失败（时钟回拨）按 0 记 —— 与 `to_json_line` 里
        //   `duration_ms` 的处置**逐字一致**：两处对同一件事必须给同一个答案。
        let secs = now
            .duration_since(self.started)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        crate::metrics::REQUEST_DURATION_SECONDS.observe(&[site, self.outcome], secs);
    }
}

/// `fulcrum_requests_total{status_class}` 那一格：状态码的**类**，闭集 **6 个值**（R1）。
///
/// # ★ `status == 0` 取 `none`，不是 `0xx`，也不是「不计」
///
/// 0 = **一个响应头都没写出去**；按契约那不是「未知」，是**什么都没发生**。
/// ⚠ 它可达，而且会被记进访问日志（`LogLevel::All.records(0)` 为真）——
/// 站点匹配上了、执行链跑完了、下游在我们写响应之前断开时就是这个形状。
/// ⇒ 闭集是 `1xx` `2xx` `3xx` `4xx` `5xx` `none`，**6 个不是 5 个**。
///
/// # ⚠ 不是合法状态码的那些也归 `none`，而不是各开一格
///
/// 1–99 与 600 以上不是 HTTP 状态码，只可能来自上游给的一个编出来的数。
/// ★ 给它们开新格，就是把这一格的**上界交回给别人** —— 而基数那张表管的正是这件事。
///
/// ⚠ 有意**不写 `_ =>` 兜底臂**：范围铺满整个 `u16`，闭集是不是穷尽的由编译器证。
fn status_class(status: u16) -> &'static str {
    match status {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        0..=99 | 600.. => "none",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn rec() -> Record {
        let mut r = Record::new("HTTP/1.1");
        r.started = SystemTime::UNIX_EPOCH + Duration::from_millis(1_787_000_000_123);
        r.method = "GET".into();
        r.host = "a.example".into();
        r.uri = "/x?y=1".into();
        r.remote_ip = Some("192.0.2.7".parse().unwrap());
        r.remote_port = 56324;
        r.outcome = "reverse_proxy";
        r.site = Some("a.example".into());
        r
    }

    #[test]
    fn 一行是合法的扁平_json_而且字段按契约来() {
        let r = rec();
        let s = r.to_json_line(200, 12, r.started + Duration::from_micros(1500));
        assert!(s.ends_with("}\n"), "一行一条，必须以换行收尾：{s}");
        let v: serde_json::Value = serde_json::from_str(s.trim_end()).expect("必须是合法 JSON");
        let o = v.as_object().expect("顶层必须是对象");
        // ★ ★ **扁平**：一个嵌套对象/数组都不许有 —— 那是 G113 的全部意义
        //   （以后想加 logfmt 时只换序列化器，字段清单一个字不动）。
        for (k, val) in o {
            assert!(
                !val.is_object() && !val.is_array(),
                "字段 `{k}` 不是标量 —— 契约要求扁平"
            );
        }
        // ★ ★ 断言的是**本层的责任**：`ts` 是「这条请求开始的那一刻」。
        //   ⚠ 写一个字面量就等于把 `format_rfc3339_millis` 的正确性也抄进本层，
        //   而那件事在 `fulcrum-runtime` 里已经有六个已知时刻钉着 ——
        //   抄一份的唯一效果是它哪天漂了会有两个地方一起红，且都指不到根因。
        assert_eq!(
            o["ts"],
            fulcrum_runtime::template::format_rfc3339_millis(r.started)
        );
        assert!(
            o["ts"].as_str().unwrap().ends_with("Z"),
            "契约要求 UTC 且带 Z"
        );
        assert_eq!(o["level"], "info");
        assert_eq!(o["proto"], "HTTP/1.1");
        assert_eq!(o["method"], "GET");
        assert_eq!(o["host"], "a.example");
        assert_eq!(o["uri"], "/x?y=1");
        assert_eq!(o["status"], 200);
        assert_eq!(o["resp_size"], 12);
        assert_eq!(o["outcome"], "reverse_proxy");
        assert_eq!(o["site"], "a.example");
        assert_eq!(o["remote_ip"], "192.0.2.7");
        assert_eq!(o["remote_port"], 56324);
        assert_eq!(o["duration_ms"], 1.5);
    }

    // ★ ★ ★ R3 的反向判据（任务 2 · G121）：`site_addr` 是 `fulcrum_requests_total`
    //   取数的中转站，**不属于**访问日志的字段契约。少了这一条，日后有人往
    //   `to_json_line` 里顺手补一行 `j.opt_str("site_addr", ...)`，会静默地把
    //   访问日志字段契约改掉——而这正是任务 2 明令「一个字都不许改」的那一处。
    #[test]
    fn site_addr_不进访问日志的_json_行() {
        let mut r = rec();
        r.site_addr = Some(Arc::from("a.example"));
        let s = r.to_json_line(200, 0, r.started);
        let v: serde_json::Value = serde_json::from_str(s.trim_end()).unwrap();
        let o = v.as_object().unwrap();
        assert!(
            !o.contains_key("site_addr"),
            "`site_addr` 不该出现在访问日志的 JSON 行里——它只是指标取数的中转站"
        );
        // `site` 字段本身不受影响，仍然是契约里那个「站点名字 = 第一个地址原文」。
        assert_eq!(o["site"], "a.example");
    }

    #[test]
    fn 取不到的字段不出现_而不是给_null() {
        let r = rec();
        let s = r.to_json_line(200, 0, r.started);
        let v: serde_json::Value = serde_json::from_str(s.trim_end()).unwrap();
        let o = v.as_object().unwrap();
        // ★ ★ ★ 这一条守的是契约里那句话本身。⚠ 若换成 `null`，
        //   `upstream=` 与「这条请求没有上游」在 logfmt 里就分不开了。
        assert!(!o.contains_key("upstream"), "没有上游时这个键不该出现");
        assert!(!o.contains_key("cache"), "没过缓存时这个键不该出现");
        // 而给了值的那些必须出现。
        let mut r2 = rec();
        r2.upstream = Some("10.0.0.1:8080".into());
        r2.cache = Some("HIT".into());
        let v2: serde_json::Value =
            serde_json::from_str(r2.to_json_line(200, 0, r2.started).trim_end()).unwrap();
        assert_eq!(v2["upstream"], "10.0.0.1:8080");
        assert_eq!(v2["cache"], "HIT");
    }

    #[test]
    fn level_按状态码派生_与阈值无关() {
        let r = rec();
        let lv = |st: u16| {
            let s = r.to_json_line(st, 0, r.started);
            let v: serde_json::Value = serde_json::from_str(s.trim_end()).unwrap();
            v["level"].as_str().unwrap().to_string()
        };
        assert_eq!(lv(200), "info");
        assert_eq!(lv(304), "info");
        assert_eq!(lv(404), "warn");
        assert_eq!(lv(421), "warn");
        assert_eq!(lv(502), "error");
    }

    #[test]
    fn 阈值决定哪些行被写出来() {
        // ⚠ 这是与上一条**不同**的一件事，而它们的取值名长得一样 ——
        //   契约里为此专门写了一段。
        assert!(LogLevel::All.records(200) && LogLevel::All.records(500));
        assert!(!LogLevel::Warn.records(200));
        assert!(LogLevel::Warn.records(404) && LogLevel::Warn.records(500));
        assert!(!LogLevel::Error.records(404));
        assert!(LogLevel::Error.records(500));
    }

    #[test]
    fn 值里的引号与换行被转义_而不是把那一行拆成两行() {
        // ⚠ ⚠ `uri` 是**客户端给的**。一个带引号或换行的请求目标，
        //   若不转义就能**伪造出一整行日志** —— 那是日志注入。
        //   ★ 这条判据的对照物是「解析得回来」：只要 `serde_json` 认得，
        //     就说明那些字节没有跑到 JSON 结构外面去。
        let mut r = rec();
        r.uri = "/a\"b\nc\\d".into();
        let s = r.to_json_line(200, 0, r.started);
        assert_eq!(
            s.matches('\n').count(),
            1,
            "整条日志只许有行尾那一个换行：{s:?}"
        );
        let v: serde_json::Value = serde_json::from_str(s.trim_end()).expect("必须仍是合法 JSON");
        assert_eq!(v["uri"], "/a\"b\nc\\d", "转义之后必须还原得回来");
    }

    #[test]
    fn 没配_log_的站点一行都不写() {
        // ★ `target` 为 `None` 时 `emit` 直接返回 —— 判据取「它不会 panic 也不写」，
        //   真正「有没有写出去」由第二十三个场景在真文件上量。
        let r = rec();
        assert!(r.target.is_none());
        r.emit(200, 0); // 不该有任何副作用
    }

    #[test]
    fn 未成形的请求也有一个说得出口的_outcome() {
        // ⚠ 读不到请求头那一类：什么都没发生，而字段不能是空串
        //   （空串在日志里读起来像「字段丢了」）。
        let r = Record::new("HTTP/1.1");
        let s = r.to_json_line(400, 0, r.started);
        let v: serde_json::Value = serde_json::from_str(s.trim_end()).unwrap();
        assert_eq!(v["outcome"], "aborted");
    }

    #[test]
    fn 多个站点写同一个路径时共用同一个句柄() {
        // ★ ★ 各开各的话，两条日志行互相插进对方中间是**随机出现**的，
        //   而那种坏法在小流量下永远不显形。
        let dir = std::env::temp_dir().join(format!("fulcrum-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.json");
        let path = p.to_string_lossy().to_string();
        assert!(open_file(&path).unwrap(), "第一次应当真的新开一个");
        assert!(!open_file(&path).unwrap(), "第二次必须复用，不许再开一个");
        let a = Sink::for_output(&LogOutput::File(path.clone())).expect("拿得到");
        let b = Sink::for_output(&LogOutput::File(path.clone())).expect("拿得到");
        match (a, b) {
            (Sink::File(x), Sink::File(y)) => {
                assert!(Arc::ptr_eq(&x, &y), "两次拿到的必须是同一个句柄")
            }
            _ => panic!("应当是文件那一支"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 打不开的路径是错误_不是静默降级() {
        // ★ ★ ★ 一个用来「出了事你能知道」的东西，自己坏掉时必须有人知道。
        let bad = "/proc/这个目录不存在/x.json";
        assert!(open_file(bad).is_err(), "打不开必须是 Err");
    }
}

#[cfg(test)]
mod tests_batch_l3 {
    //! **M2 批 L 第 ③ 步**：白名单头与 TLS 四格。
    //!
    //! ★ 单开一个模块，是因为上面那一组是「一行长什么样」的判据，
    //! 而这一组多了一件事：**取值从哪儿来**（头映射 / `SslDigest`）。

    use super::*;
    use std::time::Duration;

    fn rec() -> Record {
        let mut r = Record::new("HTTP/1.1");
        r.started = SystemTime::UNIX_EPOCH + Duration::from_millis(1_787_000_000_123);
        r.method = "GET".into();
        r.host = "a.example".into();
        r.uri = "/x".into();
        r.outcome = "respond";
        r
    }

    fn parse(r: &Record) -> serde_json::Value {
        let s = r.to_json_line(200, 0, r.started);
        assert_eq!(s.matches('\n').count(), 1, "整条日志只许有行尾那一个换行");
        serde_json::from_str(s.trim_end()).expect("必须是合法 JSON")
    }

    fn pick(name: &str, prefix: &str) -> HeaderPick {
        HeaderPick {
            lookup: name.to_ascii_lowercase(),
            key: format!("{prefix}{}", name.to_ascii_lowercase().replace('-', "_")),
        }
    }

    #[test]
    fn tls_四格按契约出现() {
        let mut r = rec();
        r.tls = Some(TlsFields {
            version: "TLSv1.3".into(),
            cipher: "TLS_AES_256_GCM_SHA384".into(),
            sni: Some("a.example".into()),
            alpn: Some("h2".into()),
        });
        let v = parse(&r);
        assert_eq!(v["tls_version"], "TLSv1.3");
        assert_eq!(v["tls_cipher"], "TLS_AES_256_GCM_SHA384");
        assert_eq!(v["tls_sni"], "a.example");
        assert_eq!(v["tls_alpn"], "h2");
    }

    #[test]
    fn 不是_tls_的连接四格一个都不出现() {
        // ★ ★ 反证。⚠ 少了它，一条「恒写 tls_version」的实现会让上面那条全绿，
        //   而它在每一条明文请求上都说了一句假话。
        let v = parse(&rec());
        for k in ["tls_version", "tls_cipher", "tls_sni", "tls_alpn"] {
            assert!(v.as_object().unwrap().get(k).is_none(), "{k} 不该出现");
        }
    }

    #[test]
    fn 问不出来的那一格不出现_而不是给一个空串() {
        // ⚠ `tls_cipher=""` 在日志里读起来像「协商出了一个名字是空的套件」。
        //   ★ 上游 `SslDigest::from_ssl` 在拿不到 cipher 时给的正是空串 ——
        //     这一条守的是「那个空串不会原样落进日志」。
        let mut r = rec();
        r.tls = Some(TlsFields {
            version: "TLSv1.3".into(),
            cipher: String::new(),
            sni: None,
            alpn: None,
        });
        let v = parse(&r);
        assert_eq!(v["tls_version"], "TLSv1.3");
        let o = v.as_object().unwrap();
        assert!(o.get("tls_cipher").is_none(), "空 cipher 不该出现");
        assert!(o.get("tls_sni").is_none(), "没发 SNI 时不该出现");
        assert!(o.get("tls_alpn").is_none(), "没协商 ALPN 时不该出现");
    }

    #[test]
    fn 白名单头按契约的键名出现() {
        let mut r = rec();
        r.req_headers = vec![("req_hdr_user_agent".into(), "curl/8".into())];
        r.resp_headers = vec![("resp_hdr_content_type".into(), "text/plain".into())];
        let v = parse(&r);
        assert_eq!(v["req_hdr_user_agent"], "curl/8");
        assert_eq!(v["resp_hdr_content_type"], "text/plain");
        // ★ ★ 仍然**扁平** —— G113 的全部意义。
        for (k, val) in v.as_object().unwrap() {
            assert!(
                !val.is_object() && !val.is_array(),
                "字段 `{k}` 不是标量 —— 契约要求扁平"
            );
        }
    }

    #[test]
    fn 多值头按逗号空格连接() {
        // RFC 9110 §5.3：同名字段行等价于用逗号连接的一行。
        let mut m = http::HeaderMap::new();
        m.append("accept", "text/html".parse().unwrap());
        m.append("accept", "application/json".parse().unwrap());
        let got = collect(&[pick("Accept", "req_hdr_")], &m);
        assert_eq!(
            got,
            vec![(
                "req_hdr_accept".to_string(),
                "text/html, application/json".to_string()
            )]
        );
    }

    #[test]
    fn 白名单里有而这条请求上没有的头不出现() {
        // ⚠ 「在白名单里」与「这条请求上真的有」是两件事 —— 契约里分开写的那一句。
        let m = http::HeaderMap::new();
        assert!(collect(&[pick("User-Agent", "req_hdr_")], &m).is_empty());
    }

    #[test]
    fn 不在白名单里的头一个都不取() {
        // ★ ★ ★ 反证：默认**一个头都不记**，而这条守的是「白名单真的在筛」。
        //   ⚠ 少了它，一条「把整个头映射倒进日志」的实现在别的判据下全绿 ——
        //     而那正好是 G114 那半条理由要防的东西（凭据进日志）。
        let mut m = http::HeaderMap::new();
        m.insert("authorization", "Bearer 秘密".parse().unwrap());
        m.insert("user-agent", "curl/8".parse().unwrap());
        let got = collect(&[pick("User-Agent", "req_hdr_")], &m);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "req_hdr_user_agent");
    }

    #[test]
    fn 头值里的引号与换行被转义_而不是把那一行拆成两行() {
        // ⚠ ⚠ 头值是**客户端给的** —— 与 `uri` 那一格同一个威胁：一个带引号或
        //   换行的头值若不转义就能**伪造出一整行日志**。
        let mut r = rec();
        r.req_headers = vec![("req_hdr_x_evil".into(), "a\"b\nc\\d".into())];
        let v = parse(&r);
        assert_eq!(v["req_hdr_x_evil"], "a\"b\nc\\d", "转义之后必须还原得回来");
    }

    #[test]
    fn 不是_utf8_的头值转成有损字符_而不是被当成不存在() {
        // ★ ★ 头值是 opaque 字节（RFC 9110 的 `obs-text` 允许 0x80–0xFF）。
        //   ⚠ 「转不了就丢掉」在日志里的样子是**这个头不存在** —— 那是一句假话。
        let mut m = http::HeaderMap::new();
        m.insert(
            "x-raw",
            http::HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap(),
        );
        let got = collect(&[pick("X-Raw", "req_hdr_")], &m);
        assert_eq!(got.len(), 1, "这个头确实在这条请求上");
        assert!(!got[0].1.is_empty());
    }
}

#[cfg(test)]
mod tests_batch_m {
    //! **M2 批 M**：收尾这一处**同时**喂指标（裁决 R1 / R2 / R4）。
    //!
    //! ⚠ ⚠ 这一组走的是**真的**进程级指标注册表 —— 于是每条判据用的 `site` 标签值
    //! 都只此一处用过，否则同一个测试二进制里几条测试会把数写进对方的断言里。

    use super::*;
    use std::time::Duration;

    fn rec(site_addr: &str) -> Record {
        let mut r = Record::new("HTTP/1.1");
        r.started = SystemTime::now();
        r.method = "GET".into();
        r.host = "a.example".into();
        r.uri = "/x".into();
        r.outcome = "respond";
        r.site_addr = Some(Arc::from(site_addr));
        r
    }

    /// 从进程级注册表里读这一格的读数；没有这条 series 就是 0。
    fn counter(site: &str, outcome: &str, class: &str) -> u64 {
        let want = format!(
            "fulcrum_requests_total{{site=\"{site}\",outcome=\"{outcome}\",\
             status_class=\"{class}\",proto=\"HTTP/1.1\"}} "
        );
        let out = crate::metrics::render();
        out.lines()
            .find_map(|l| l.strip_prefix(&want))
            .map(|v| v.parse().expect("读数不是个数"))
            .unwrap_or(0)
    }

    fn lines(path: &str) -> usize {
        std::fs::read_to_string(path)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    #[test]
    fn status_class_是六个值的闭集_而_0_取_none_r1() {
        // ★ R1 的**前提**：`status = 0` 可达，而且会被记进访问日志。
        //   ⚠ 少了这一句，「0 该不该有一格」就成了一个凭空的选择题。
        assert!(LogLevel::All.records(0));

        // ★ 含两个端点：`0`（什么都没发生）与 `599`。
        assert_eq!(status_class(0), "none");
        assert_eq!(status_class(99), "none");
        assert_eq!(status_class(100), "1xx");
        assert_eq!(status_class(199), "1xx");
        assert_eq!(status_class(200), "2xx");
        assert_eq!(status_class(299), "2xx");
        assert_eq!(status_class(300), "3xx");
        assert_eq!(status_class(399), "3xx");
        assert_eq!(status_class(400), "4xx");
        assert_eq!(status_class(499), "4xx");
        assert_eq!(status_class(500), "5xx");
        assert_eq!(status_class(599), "5xx");
        // ⚠ 600 以上不是 HTTP 状态码 —— 归 `none`，不许各开一格。
        assert_eq!(status_class(600), "none");
        assert_eq!(status_class(u16::MAX), "none");

        // ★ ★ 反向那半：整个 `u16` 值域上**只出现这 6 个标签值**。
        //   ⚠ 上面每一条都只问「这个码是不是那个类」，问不出「有没有第 7 个类」——
        //     一个多出 `0xx` 的实现在它们那里完全看不出来。
        let seen: std::collections::BTreeSet<&str> = (0..=u16::MAX).map(status_class).collect();
        assert_eq!(
            seen.into_iter().collect::<Vec<_>>(),
            ["1xx", "2xx", "3xx", "4xx", "5xx", "none"]
        );
    }

    #[test]
    fn 站点没配_log_时指标照样加一_而日志一行都不写_r4() {
        // ★ ★ ★ R4 的正反两半，而且**在同一个文件上**对照 ——
        //   「文件是空的」这一半单独看什么都不证明：夹具没接上时它同样是空的。
        let dir = std::env::temp_dir().join(format!("fulcrum-metrics-r4-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("r4.json").to_string_lossy().to_string();
        let _ = std::fs::remove_file(&path);
        open_file(&path).expect("测试用的日志文件要开得起来");
        let cfg = LogRt {
            output: LogOutput::File(path.clone()),
            level: LogLevel::All,
            req_headers: Vec::new(),
            resp_headers: Vec::new(),
        };

        // ① 没配 `log`（`target` 是 `None`）：指标 +1，而文件一行都不多。
        let a = rec("<ut-m-nolog>");
        assert!(a.target.is_none());
        a.finish(200, 12);
        assert_eq!(lines(&path), 0, "没配 `log` 的站点不许写出任何一行");
        assert_eq!(
            counter("<ut-m-nolog>", "respond", "2xx"),
            1,
            "没配 `log` 的站点必须照样有指标 —— 那是 R4 取这一侧的全部理由"
        );

        // ② 配了 `log { level info }`：指标 +1，而且这一次**真的写出去了一行**。
        //    ★ 这一半证明上面那个 0 不是夹具坏了。
        let mut b = rec("<ut-m-log>");
        b.target = Some(Arc::new(cfg.clone()));
        b.finish(200, 12);
        assert_eq!(lines(&path), 1, "配了 `log` 就该有一行 —— 夹具是通的");
        assert_eq!(counter("<ut-m-log>", "respond", "2xx"), 1);

        // ③ 阈值挡住（`level error` 遇上 200）：日志仍然不写，指标仍然 +1。
        //    ⚠ 这是 `emit` 的**第二个**早退分支，与 ① 那个是两条不同的路。
        let mut c = rec("<ut-m-thresh>");
        c.target = Some(Arc::new(LogRt {
            level: LogLevel::Error,
            ..cfg.clone()
        }));
        c.finish(200, 12);
        assert_eq!(lines(&path), 1, "阈值挡住的那一条不许被写出去");
        assert_eq!(counter("<ut-m-thresh>", "respond", "2xx"), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 没匹配到站点的请求也进总数_site_取尖括号_none_r2() {
        // ★ R2：这样 `fulcrum_requests_total` 才是**真的总数**，一致性门才有东西可对。
        //   ⚠ 尖括号形式在合法的地址字面量里不可能出现 ⇒ 它撞不上真站点。
        let mut r = rec("<不该用到>");
        r.site_addr = None;
        r.outcome = "no_site_match";
        r.finish(421, 0);
        assert_eq!(counter("<none>", "no_site_match", "4xx"), 1);
    }

    #[test]
    fn 耗时按秒记_而算不出来时按_0_记_与日志那一格逐字一致() {
        // 正向：已知的 250ms ⇒ `_sum` 是 0.25 秒（**秒**，不是毫秒）。
        let mut r = rec("<ut-m-dur>");
        r.started = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        r.meter(200, r.started + Duration::from_millis(250));
        let out = crate::metrics::render();
        assert!(
            out.contains(
                "fulcrum_request_duration_seconds_sum{site=\"<ut-m-dur>\",outcome=\"respond\"} 0.25\n"
            ),
            "{out}"
        );

        // ★ ★ 反向：`now` 早于 `started`（时钟回拨）⇒ `duration_since` 失败 ⇒ 按 **0** 记。
        //   ⚠ 这一条同时钉住「两处对同一件事给同一个答案」：`to_json_line` 的
        //     `duration_ms` 在同一份输入上也必须是 0。
        let mut b = rec("<ut-m-back>");
        b.started = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let 回拨 = b.started - Duration::from_secs(5);
        b.meter(200, 回拨);
        let out = crate::metrics::render();
        assert!(
            out.contains(
                "fulcrum_request_duration_seconds_sum{site=\"<ut-m-back>\",outcome=\"respond\"} 0\n"
            ),
            "{out}"
        );
        let v: serde_json::Value =
            serde_json::from_str(b.to_json_line(200, 0, 回拨).trim_end()).unwrap();
        assert_eq!(v["duration_ms"], 0.0, "日志那一格与指标必须给同一个答案");
    }

    #[test]
    fn 标签取的是命中的地址字面量与那条请求自己的协议() {
        // ⚠ `site` 取 `site_addr`（G121）而**不是** `site`（那是站点的名字，R3），
        //   `proto` 取 `Record` 入口填的那个 —— 两者都不是推断出来的。
        let mut r = Record::new("HTTP/3.0");
        r.started = SystemTime::now();
        r.outcome = "file_server";
        r.site = Some("http://first.example:9900".into());
        r.site_addr = Some(Arc::from("<ut-m-h3>"));
        r.finish(503, 0);
        let out = crate::metrics::render();
        assert!(
            out.contains(
                "fulcrum_requests_total{site=\"<ut-m-h3>\",outcome=\"file_server\",\
                 status_class=\"5xx\",proto=\"HTTP/3.0\"} 1\n"
            ),
            "{out}"
        );
        // ★ 反向：站点的名字**不该**出现在指标里 —— 它是访问日志那一侧的东西。
        assert!(
            !out.contains("first.example"),
            "站点名字漏进了指标标签：{out}"
        );
    }
}
