//! 管理面（**G8 的全量原子 load** + **G74 的强制续期**），走 **Unix domain socket**（G14）。
//!
//! ```text
//! admin unix//run/fulcrum/admin.sock      ← DSL 里这一行开启它
//! ```
//!
//! ★ **只绑 Unix domain socket，权限交给文件系统 ACL**（G14）—— 堵掉 Caddy 那个短处
//! （Admin API 默认绑回环且无认证，同机任意进程可改配置）。
//! ⚠ 于是这里不需要发明一套 token 体系，也没有「忘了开认证」这种失败模式。
//!
//! # 提供两条命令
//!
//! | | 干什么 | 载荷 |
//! |---|---|---|
//! | `POST /load` | **全量原子 load**（G8）| 结构化配置 JSON（G48：与磁盘上那份**同一种**）|
//! | `POST /renew` | **强制续期一个域名**（G74）| `{"domain":"…"}`；
//!   加 `"force": true` 时**连退避一起清**（第二档）|
//!
//! ★ `/renew` 两档：默认档越过「还不到时候」但**不越过退避**（随手绕开退避的口子
//! 等于给「反复重签把 CA 配额烧光」开门，而配额按**账户**算）；
//! `"force": true` 把失败计数清零，用在**根因已经修好**之后，⚠ 每次都打 warn。
//!
//! ★ 载荷是**结构化 JSON 而不是 DSL**（G11 + G48）：机器写结构化层，
//! 而它与磁盘上那份是同一种格式。人要提交 DSL 的话先 `fulcrum compile`。
//!
//! # ⚠ ⚠ 「原子」到哪儿为止：**监听端口集不能变**
//!
//! Pingora 在**启动时**绑定监听器 ⇒ 改了监听地址的配置在同一个进程里不可能生效，
//! 所以这里**显式拒绝**并让调用方走 `systemctl reload`（真的换代，零停机）。
//!
//! > ★ 判据在**拒绝**上，不在「尽力而为」上：**配置变更是事务，不是文件写入**。
//! > 一个「端口没换成、别的换成了」的结果**既不是旧状态也不是新状态** ——
//! > 那是三种结局里最坏的一种。

use crate::dns;
use fulcrum_acme::AcmeManager;
use fulcrum_runtime::{Runtime, SharedRuntime};
use log::{error, info, warn};
use pingora_core::apps::{HttpServerApp, ReusedHttpStream};
use pingora_core::protocols::http::ServerSession;
use pingora_core::server::ShutdownWatch;
use std::sync::Arc;

/// 请求体上限。★ 一份结构化配置是几十 KB 量级；4 MiB 宽出两个数量级，
/// 而没有上限就等于让任何能写这个 socket 的进程把内存吃光。
const MAX_BODY: usize = 4 * 1024 * 1024;

/// socket 的权限。**0600 = 只有属主能读写**（G14：权限交给文件系统 ACL）。
///
/// ⚠ 这个数字就是整个管理面的**全部**访问控制。放宽它之前请先想清楚：
/// 能写这个 socket 的进程可以**换掉整份配置**。
pub const SOCKET_MODE: u32 = 0o600;

/// 一份运行时里的 L4 监听器集，**已排序**，用来比「换代才换得了的那部分变没变」。
///
/// ★ 只取 `(协议, 监听地址原样)`：上游是可以随时改的，改上游不需要重新 bind。
/// ⚠ 抽成函数而不是在两处各写一遍 —— 两处各写一遍时，
/// 「启动时记的」与「load 时算的」会在下一次改动里分家，而分家的表现是这道门**恒绿**。
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
    /// 启动时绑定的监听端口集。★ 全量 load 拿它做「端口集有没有变」的判据。
    listen_ports: Vec<(u16, bool)>,
    /// 启动时绑定的 **L4** 监听器集（`(协议, 监听地址原样)`）。
    ///
    /// ★ ★ M2 批 A加的，理由与上面那格逐字相同：**L4 端口同样在
    /// 启动时绑定**，`POST /load` 换不了它。⚠ 少了这一格，一份改了 `l4` 块的配置
    /// 会 **200「已生效」而那个端口纹丝不动** —— 装载成功、日志无异常、
    /// 只有真的去连那个端口才发现还是老上游。★ 这正是批 22 那条
    /// 「守卫接在 CLI 上，而管理面是另一个入口」的同族：**一个判据漏掉一个入口，
    /// 等于在那个入口上没有判据。**
    l4_listeners: Vec<(String, String)>,
    acme: Option<Arc<AcmeManager>>,
    /// 缓存后端（M2 批 G）。`None` = 这个进程没建缓存（测试夹具里会是这样）。
    ///
    /// ★ ★ `purge` 落在**这个** socket 上而不是新开一个端口（G84 拍板）：
    /// 与 `/load`、`/renew` 同一套 0600 权限（G14）。⚠ 一个「只是清缓存」的
    /// 新入口听起来无害，而它能被用来把上游打垮 —— 权限面必须只有一个。
    cache: Option<Arc<crate::cache::CacheHandle>>,
}

impl AdminApp {
    pub fn new(
        rt: Arc<SharedRuntime>,
        listen_ports: Vec<(u16, bool)>,
        acme: Option<Arc<AcmeManager>>,
        cache: Option<Arc<crate::cache::CacheHandle>>,
    ) -> AdminApp {
        let mut listen_ports = listen_ports;
        listen_ports.sort_unstable();
        // ★ L4 那一格从**当前运行时**取，而不是另外传一份进来：
        //   多一个参数就多一处「调用方可能忘了给」，而忘了给的后果是这道门变成空的。
        let l4_listeners = l4_key_set(&rt.current());
        AdminApp {
            rt,
            listen_ports,
            l4_listeners,
            acme,
            cache,
        }
    }

    /// 全量原子 load（G8）。
    fn load(&self, body: &[u8]) -> (u16, String) {
        let cfg: fulcrum_config::StructuredConfig = match serde_json::from_slice(body) {
            Ok(c) => c,
            Err(e) => {
                return (400, format!("载荷不是合法的结构化配置 JSON（G48）：{e}\n"));
            }
        };
        // ── ★ ★ ★ 脱敏产物不许被 load（批 22，真机实测补上）────────
        //
        //   `fulcrum compile` **默认**吐的就是脱敏产物，而这个接口的载荷正是它。
        //   ⚠ 在 example.com 上实测：不挡的话这里回 **200「已生效」** ——
        //   运维会理所当然地以为那份配置完整生效了，而里面的凭据是 `«已脱敏»`。
        //
        //   ★ 当时之所以没炸，只是因为 ACME 那条链是**启动时**建的、没读这份载荷；
        //   ⚠ 而「今天恰好没用到」不是判据 —— 它只是把爆炸推迟到有人让 ACME 读它的那天。
        //   ⇒ 在入口挡下，并且**说清楚怎么重新生成**。
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

        // ★ ★ **先整份建起来，建成了才换**。`Runtime::build` 里做了正则编译、
        //   CIDR 解析、上游地址解析——任何一条不过就整体不生效，
        //   而**旧配置一个字节都没动**。这就是 G8 说的「原子」。
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
        // ⚠ ⚠ **访问日志的文件要在换之前打开**（M2 批 L 第 ② 步）。
        //   ★ 放在这里而不是换完之后，是为了守住 G8 的「原子」：
        //   打不开就整份不生效，而**旧配置一个字节都没动**。
        //   ⇒ 一份把日志路径写错的配置会被**当场拒绝**，
        //   而不是「换上去了、服务正常、日志悄悄没了」。
        if let Err(errs) = crate::access_log::open_all(&next) {
            let mut out = String::from("访问日志的输出文件打不开，**没有任何改动生效**：\n");
            for e in &errs {
                out.push_str(&format!("  · {e}\n"));
            }
            return (400, out);
        }

        // ⚠ 监听端口集变了 = 这个进程做不到。**拒绝，而不是换一半。**
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
        // ⚠ **L4 监听器集同样换不了**（M2 批 A）。理由与上面那条逐字相同：
        //   端口在启动时绑定。★ 判据取的是 `(协议, 监听地址原样)` 的集合 ——
        //   **上游改了照样放行**，那是这道门有意不管的部分：换上游不需要重新 bind。
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
        // ── ⚠ ⚠ **缓存后端同样换不了**（M2 批 H）───────────────────────────
        //
        // 理由与上面两条同一族：它是**进程级**的，在启动时就打开了那个目录
        // （`DiskStore::open` 建目录、读索引、起后台维护任务）。
        // ★ ★ 不拦的话，一次 `POST /load` 会回 200，而**新写的 `disk` 目录一个字节
        //   都不会被用到** —— 运维那边看到的是「换成功了」，盘上那个目录始终是空的，
        //   ⚠ 而装载日志早在启动时就打过了，不会再说第二遍。
        //   那正是本仓库反复点名的静默失能，与「监听端口集变了」是同一件事的另一面。
        // ★ 判据取「有没有 `disk`、是哪一个」，**不看 `ttl` / `max_size` / `capacity`**：
        //   那三样是每请求现读 `CacheRt` 的，换配置立刻生效，这里有意不管。
        //   ⚠ `capacity` 是个例外中的例外：它在启动时定死了后端容量，改了不生效 ——
        //   已登记在 `PLAN.md` §11，不在本批处置。
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
        // ★ 未接线能力照旧在这里说一遍：换了配置之后「哪些写了但不生效」会变，
        //   而只在启动时打一次的话，换配置引入的新缺口没有任何人说。
        for (k, why) in next.unwired_in_use(&cfg) {
            warn!("load：`{k}` 在新配置里被用到，但还没接线 —— {why}");
        }
        // ★ ★ **换之前先把域名上游解析一遍。**
        //   ⚠ 少了这一步，新建出来的 `Upstream` 里那个槽是空的，于是
        //   **一次 load 会让所有域名上游短暂地「没有地址」** —— `pick()` 跳过它们，
        //   客户端拿到 502，直到后台任务下一次打点。那是一次自找的抖动。
        //   ★ 这里是阻塞 DNS，但它跑在**管理面**的请求上，不是数据面 ——
        //   一次有意的配置操作等几十毫秒是应该的。
        dns::resolve_now(&next, "全量 load");
        self.rt.swap(Arc::new(next));
        info!("全量 load 生效：{sites} 个站点（G8：原子换整份）");
        (200, format!("已生效：{sites} 个站点\n"))
    }

    /// 强制续期（G74）。
    /// 清缓存（**M2 批 G**，G84：`purge` 走管理面）。
    ///
    /// 载荷三选一：
    /// - `{"key": "<主键>"}` —— 清那一条 URL（含它 `Vary` 出来的全部次级键）
    /// - `{"prefix": "<主键前缀>"}` —— 清一批
    /// - `{"all": true}` —— 全清
    ///
    /// ⚠ ⚠ **主键不是 URL**：它是 `方法\u{1}scheme\u{1}host\u{1}路径\u{1}查询串`。
    /// ★ 直接收 URL 听起来友好，而它要求管理面**重新实现一遍**缓存键的算法 ——
    /// 两份算法迟早分家，而分家的现场是「purge 说清了 1 条，但下次请求还是命中」。
    /// ⇒ 这里收**主键本身**，并接受 `method` / `scheme` 几个字段替调用方拼。
    fn purge(&self, body: &[u8]) -> (u16, String) {
        #[derive(serde::Deserialize)]
        struct Req {
            #[serde(default)]
            key: Option<String>,
            #[serde(default)]
            prefix: Option<String>,
            #[serde(default)]
            all: bool,
            /// 便利字段：给了 `url` 就按这几样替调用方拼主键。
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
            // ★ 409 而不是 404：路径是对的，只是这个进程没有缓存。
            //   ⚠ 回 404 的话，读起来像「这个版本没有 purge 这个功能」。
            return (
                409,
                "这个进程没有缓存后端（配置里没有 `cache` 指令），没什么可清的\n".to_string(),
            );
        };

        // ⚠ ⚠ **磁盘后端下这几条会走遍整个缓存目录树**（`purge_prefix` / `purge_all`
        //   以**盘**为准而不是以索引为准，理由见 `disk::DiskStore::purge_prefix`）。
        //   ★ 那是阻塞 I/O，可能要好几秒 —— `block_in_place` 告诉 tokio 这条工作
        //   线程要卡一会儿，让它先把别的任务挪走。⚠ 少了它，一次 purge 会把
        //   **排在同一条线程上的请求**一起堵住，而现场是「按了一下清缓存，
        //   网站卡了三秒」—— 没有任何东西会把这两件事联系起来。
        //   ★ 内存后端下它几乎没有代价（那几个操作是纯内存的）。
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
        let (used, left) = cache.store.stats();
        // ★ **清掉 0 条也是 200**：purge 的语义是「让它不在」，而它本来就不在
        //   同样满足这个语义。⚠ 回 404 会让「清一个从没被缓存过的 URL」
        //   看起来像失败，于是脚本里到处是重试。
        (
            200,
            format!("清掉 {n} 条；现在还剩 {left} 条 / {used} 字节\n"),
        )
    }

    fn renew(&self, body: &[u8]) -> (u16, String) {
        #[derive(serde::Deserialize)]
        struct Req {
            domain: String,
            /// ★ 缺省 `false` —— **默认不越过退避**，见 `request_renew` 的两档表。
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
        // 管理面不做 keep-alive：它一天被调几次，省下的那点握手不值得多一条状态。
        session.set_keepalive(None);

        let method = session.req_header().method.clone();
        let path = session.req_header().uri.path().to_string();

        // 读 body，带上界。
        let mut body = Vec::new();
        loop {
            match session.read_request_body().await {
                Ok(Some(chunk)) => {
                    if body.len() + chunk.len() > MAX_BODY {
                        reply(&mut session, 413, "载荷太大\n").await;
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

        let (status, text) = match (method.as_str(), path.as_str()) {
            ("POST", "/load") => self.load(&body),
            ("POST", "/renew") => self.renew(&body),
            ("POST", "/purge") => self.purge(&body),
            // ⚠ 不认识的路径回 404 而不是 200：一个「什么都收下」的管理面
            //   会让打错的命令看起来成功了。
            _ => (
                404,
                format!(
                    "不认识：{method} {path}\n可用：POST /load（全量原子 load）、POST /renew（强制续期）、POST /purge（清缓存）\n"
                ),
            ),
        };
        reply(&mut session, status, &text).await;
        None
    }
}

async fn reply(session: &mut ServerSession, status: u16, text: &str) {
    let mut h = match pingora_http::ResponseHeader::build(status, None) {
        Ok(h) => h,
        Err(e) => {
            error!("管理面建不出响应头：{e}");
            return;
        }
    };
    let body = bytes::Bytes::from(text.to_string());
    let _ = h.insert_header("Content-Type", "text/plain; charset=utf-8");
    let _ = h.insert_header("Content-Length", body.len().to_string());
    if session.write_response_header(Box::new(h)).await.is_err() {
        return;
    }
    let _ = session.write_response_body(body, true).await;
}

/// 从 `admin` 那一行里抠出 socket 路径。只认 `unix/<路径>`（G14）。
///
/// ⚠ **不认 `:2019` 这种端口写法**，而且要报错而不是静静忽略：
/// 一个「写了端口、结果绑了个 socket」或者「写了什么都不生效」的管理面，
/// 是最坏的一种——运维以为它开着。
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
        // ★ ★ 判据的另一半：端口写法必须**报错**，不能静静忽略。
        //   一个写了 `admin :2019` 却什么都不发生的管理面，运维会以为它开着——
        //   而那正是 G14 要堵的 Caddy 那个短处的镜像版本。
        for bad in [":2019", "127.0.0.1:2019", "unix/", "tcp//x", ""] {
            let e = socket_path(bad).unwrap_err();
            assert!(e.contains("G14"), "`{bad}` 的错误应当点名 G14：{e}");
        }
    }

    #[test]
    fn 脱敏产物_load_必须被拒而不是回_200() {
        // ★ ★ ★ **这条判据是真机实测补上的。**
        //
        //   `fulcrum compile` **默认**吐脱敏产物，而这个接口的载荷正是它 ——
        //   实测不挡的话回的是 **200「已生效：4 个站点」**，
        //   而里面的凭据是 `«已脱敏»`。⚠ 运维会理所当然地以为配置完整生效了。
        //
        //   ⚠ 当时之所以没炸，只因为 ACME 那条链是**启动时**建的、没读这份载荷 ——
        //   而「今天恰好没用到」不是判据，它只是把爆炸推迟到有人让 ACME 读它的那天。
        let a = app(vec![(8080, false)]);
        let dsl = format!(
            "http://a.com:8080 {{\n  respond 200\n}}\n\
             *.a.com {{\n  tls {{\n    dns dnspod {}\n    zones a.com\n    resolvers 1.1.1.1\n  }}\n  respond 200\n}}\n",
            fulcrum_config::secret::REDACTED
        );
        let (code, body) = a.load(&json_of(&dsl));
        assert_eq!(code, 400, "脱敏产物竟然被接受了：{body}");
        assert!(
            body.contains("--with-secrets"),
            "错误里要说清怎么重新生成：{body}"
        );
        assert!(body.contains("没有任何改动生效"), "要说清没生效：{body}");
    }

    #[test]
    fn 带真凭据的载荷照常_load_得进去() {
        // ⚠ 与上一条配对：只测「脱敏的会被拒」的话，
        //   一个把**所有**带凭据的配置都拒掉的实现会让上面那条照常绿，
        //   而那样 `POST /load` 对原生 DNS 供应商就整个不能用了。
        // ⚠ 端口集要给全：`*.a.com` 带来 443，而**自动 HTTP 重定向**（批 21）
        //   还会合成一个 :80 的站点 —— 少给一个，这条测试会红在 409 上，
        //   而那与它想验的事情毫无关系。
        let a = app(vec![(80, false), (443, true), (8080, false)]);
        let dsl = "http://a.com:8080 {\n  respond 200\n}\n\
                   *.a.com {\n  tls {\n    dns dnspod 12345,abcdefabcdef\n    zones a.com\n    resolvers 1.1.1.1\n  }\n  respond 200\n}\n";
        // ★ 载荷要带真值，所以生成时进 reveal 作用域 —— 这正是 `compile --with-secrets` 做的事。
        let cfg = fulcrum_config::compile_str("t.Fulcrumfile", dsl)
            .config
            .unwrap();
        let body = fulcrum_config::secret::reveal(|| serde_json::to_vec(&cfg).unwrap());
        let (code, out) = a.load(&body);
        assert_eq!(code, 200, "带真凭据的载荷被拒了：{out}");
    }

    fn app(ports: Vec<(u16, bool)>) -> AdminApp {
        let cfg =
            fulcrum_config::compile_str("t.Fulcrumfile", "http://a.com:8080 {\n  respond 200\n}\n")
                .config
                .unwrap();
        let rt = Arc::new(Runtime::build(&cfg).unwrap());
        AdminApp::new(SharedRuntime::new(rt), ports, None, None)
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
        let (status, text) = a.load(&json_of("http://b.com:8080 {\n  respond 204\n}\n"));
        assert_eq!(status, 200, "{text}");
        // ★ ★ 判据是**换到了**，不是「返回了 200」。
        //   ⚠ 断言 `listen_ports == [(8080,false)]` 是不行的 —— 那个值在新旧两份里
        //   **是一样的**，一个「收下但什么都不做」的实现照样绿。
        //   ★ **判据不能挂在两边都相同的量上。**
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
        let (status, text) = a.load(&json_of("http://b.com:9090 {\n  respond 200\n}\n"));
        assert_eq!(status, 409, "{text}");
        assert!(text.contains("systemctl reload"), "要说清怎么办：{text}");
        // ⚠ 这一条才是「原子」的判据：**拒绝之后旧的还在**。
        //   一个「先换后校验」的实现在上一条上表现完全相同。
        assert!(Arc::ptr_eq(&before, &a.rt.current()), "旧配置被动过了");
    }

    // ── M2 批 H：缓存后端换不了 ────────────────────────────────────────
    //
    // ⚠ ⚠ 不拦的话，`POST /load` 会回 **200**，而新写的 `disk` 目录一个字节都不会被用到
    //   —— 运维看到「换成功了」，盘上那个目录始终是空的，而装载日志早在启动时
    //   就打过了、不会再说第二遍。★ 这是静默失能，不是小瑕疵。
    #[test]
    fn 换缓存后端要被拒而且旧配置一个字节都没动() {
        let a = app(vec![(8080, false)]);
        let before = a.rt.current();
        let (status, text) = a.load(&json_of(
            "http://a.com:8080 {\n  cache {\n    disk /var/cache/fulcrum\n  }\n  \
             reverse_proxy 127.0.0.1:1\n}\n",
        ));
        assert_eq!(status, 409, "换后端居然被放行了：{text}");
        assert!(text.contains("缓存后端变了"), "要说清是哪件事：{text}");
        assert!(text.contains("systemctl reload"), "要说清怎么办：{text}");
        assert!(Arc::ptr_eq(&before, &a.rt.current()), "旧配置被动过了");
    }

    // ★ 反向那一半：**只改新鲜度、不动后端**必须照常换得进去。
    //   ⚠ 少了它，一个「只要配置里有 `cache` 就拒」的实现会让上面那条全绿，
    //   而它把一次完全正当的 `ttl` 调整也拦了。
    #[test]
    fn 只改缓存参数不动后端照常换得动() {
        let a = app(vec![(8080, false)]);
        let before = a.rt.current();
        let (status, text) = a.load(&json_of(
            "http://a.com:8080 {\n  cache {\n    ttl 5m\n  }\n  reverse_proxy 127.0.0.1:1\n}\n",
        ));
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
        let (status, _) = a.load(b"{ not json");
        assert_eq!(status, 400);
        assert!(Arc::ptr_eq(&before, &a.rt.current()));
    }

    #[test]
    fn 建不起来的配置也一个字节都没动() {
        // ★ 结构化层是公开入口（G11），所以它能塞进 DSL 塞不进的东西——
        //   这里用一个非法 CIDR，`Runtime::build` 会拒绝它。
        let a = app(vec![(8080, false)]);
        let before = a.rt.current();
        let mut cfg = fulcrum_config::compile_str(
            "t.Fulcrumfile",
            "http://a.com:8080 {\n  @m {\n    remote_ip 10.0.0.0/8\n  }\n  respond 200\n}\n",
        )
        .config
        .unwrap();
        // 把 CIDR 改坏 —— 只有结构化层进得来。
        let json = serde_json::to_string(&cfg)
            .unwrap()
            .replace("10.0.0.0/8", "10.0.0.0/99");
        cfg = serde_json::from_str(&json).unwrap();
        let (status, text) = a.load(serde_json::to_vec(&cfg).unwrap().as_slice());
        assert_eq!(status, 400, "{text}");
        assert!(text.contains("没有任何改动生效"), "{text}");
        assert!(Arc::ptr_eq(&before, &a.rt.current()));
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
}
