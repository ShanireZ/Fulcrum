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
//! | `POST /load?overrides=keep\|clear` | **全量原子 load**（G8）；`overrides` **必填**、走查询串，⛔ 不接受载荷信封（G120）| 结构化配置 JSON（G48：与磁盘上那份**同一种**）|
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
use fulcrum_runtime::overrides::{OverrideKey, RuntimeAction, RuntimeOp, UpstreamOverride};
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

    /// 全量原子 load（G8）+ `overrides` 两档（**M2 批 N 任务 5**，G120 / R9）。
    ///
    /// # `?overrides=keep|clear`：必填，走**查询串**
    ///
    /// ⛔ ⛔ 不接受把它塞进载荷（`{"overrides":…,"config":{…}}` 的信封）——
    /// 载荷是结构化配置，与磁盘上那份**同一种**（G48），多包一层信封就是两种
    /// 格式分了家。缺这个参数、或写了 `keep`/`clear` 之外的值 ⇒ **400，且旧
    /// 配置一个字节都不动**——这里在碰载荷之前就返回，原子性是结构上必然的。
    ///
    /// ★ ★ 不给默认值就是 G120 的全部内容：发布流水线要 `clear`（发布 = 回到
    /// 期望状态），事故处理中的人要 `keep`（一次无关的发布不该把刚摘掉的坏
    /// 节点放回去）——两种现实都对且互相冲突，任何默认值都会在另一种场景里
    /// 悄悄做错事。
    ///
    /// `keep` 什么都不用做——`SharedRuntime::swap` 内部已经调了
    /// `retain_after_swap`，悬空覆盖天然留着，这里只需把 `dangling` 的挑出来
    /// 点名（裁决 R8）。`clear` 调
    /// [`fulcrum_runtime::overrides::OverrideLayer::clear_all`] 撤销全部格子上
    /// 的覆盖（⛔ 不是物理删格子——见该方法文档）。⚠ ⚠ **两档都必须在回话里
    /// 逐项列出**（G120 明写 `clear`，裁决 R8 明写 `keep` 的悬空）：只给一个
    /// 数字不算数，理由是要避开 HAProxy 那个「runtime 改动在 reload 后无声无息
    /// 消失」的短处。
    ///
    /// ⚠ `dangling` 是相对**新运行时**算的 ⇒ 两档都必须在 `self.rt.swap()`
    /// **之后**判定——下面的实现把这一段放在 `swap` 之后正是为此。
    fn load(&self, body: &[u8], query: Option<&str>) -> (u16, String) {
        /// `overrides` 查询参数的两档。⛔ 只在这个函数内部用——这个端点自己的
        /// 字面量，仿照 `runtime()`/`purge()` 把小形状钉在唯一的调用点旁边。
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum Directive {
            /// 覆盖层原样留着；新运行时里键落不到任何上游的那些标 `dangling`，
            /// 回话逐条点名。
            Keep,
            /// 覆盖层清空；回话逐项列出被清掉的。
            Clear,
        }
        /// 从 `uri.query()` 解析。缺参数、或值不是 `keep`/`clear` 都是
        /// `None`——两者在契约上是同一件事：拒绝，不猜。
        fn parse_overrides(query: Option<&str>) -> Option<Directive> {
            for pair in query.unwrap_or("").split('&') {
                if let Some(v) = pair.strip_prefix("overrides=") {
                    return match v {
                        "keep" => Some(Directive::Keep),
                        "clear" => Some(Directive::Clear),
                        _ => None,
                    };
                }
            }
            None
        }

        let Some(directive) = parse_overrides(query) else {
            return (
                400,
                "缺 `?overrides=keep` 或 `?overrides=clear`（G120：必填参数，没有\
                 默认值——发布流水线要 `clear`，事故处理中的人要 `keep`，两种现实\
                 都对且互相冲突，任何默认值都会在另一种场景里悄悄做错事），\
                 **没有任何改动生效**。走查询串，不接受载荷里的字段：\
                 `POST /load?overrides=keep` 或 `POST /load?overrides=clear`\n"
                    .to_string(),
            );
        };

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
        // ⚠ `swap` 收 `Runtime` 而不是 `Arc<Runtime>`（批 N 任务 3）：装进来之前
        //   要把临时覆盖层的格子挂上去，而那要 `&mut` —— 那是结构保护，别绕过去。
        self.rt.swap(next);

        // ── G120 / R8：`overrides` 落地 ────────────────────────────────────
        //   ⚠ ⚠ 必须在 `swap` **之后**：`dangling` 是相对新运行时算的
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

        info!("全量 load 生效：{sites} 个站点（G8：原子换整份）");
        (200, format!("已生效：{sites} 个站点\n{overrides_report}"))
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
        // ── 缓存事件：`purge`（R7）────────────────────────────────────────
        //
        // ★ 记的是**被清掉的条目数**，不是「purge 被调了几次」——
        //   问「清掉了多少」的人远多于问「这个接口被调了几次」的人。
        // ⚠ ⚠ 正因如此，这一格与另外三个**不在同一个分母里**（D31）：
        //   `hit`/`miss`/`stale` 数的是「一条请求」，本格数的是「一条缓存条目」
        //   ⇒ **`sum(cache_events_total)` 不是任何一个有意义的量**。
        //   ★ 别把它写成「四个事件单位都是『条』」——「条」这个量词对两者都成立，
        //     那种说法自洽、读起来毫无破绽，而它断言的等价关系是假的。
        //     口径的权威在 `docs/architecture/observability.md` 与 `PLAN.md` §11 D31。
        // ⚠ 一条都没清掉时也照记（`+0`）：那让这条 series **存在**，而
        //   「这个进程从来没 purge 过」与「purge 过、只是没清到东西」是两件事，
        //   在抓取端要分得开。
        crate::cache::CacheEvent::Purge.record(n as u64);
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

    /// `POST /runtime`：增量通道，G8 的**动词**那半（**M2 批 N 任务 4**，裁决 R10）。
    ///
    /// 管理面收动词，直接改任务 3 建好的覆盖层格子——★ ★ 不重建 `Runtime`、不 swap：
    /// 覆盖格子是 `Arc<UpstreamOverride>`，与活着的 `Upstream` 身上那一份**是同一个
    /// 对象**（任务 3 判据 1，`Arc::ptr_eq` 钉住）。改登记处那一格，当场就作用到
    /// 正在跑的上游身上——这是 R7 的全部意义。
    ///
    /// # ★ ★ ★ 全有或全无
    ///
    /// 分两个阶段：
    /// 1. **纯解析**——校验 `verb` 认不认识、`weight` 的值域。值域**不在这里另写
    ///    一份**：借 [`UpstreamOverride::set_weight`] 自己的判断，对着一格临时的
    ///    `UpstreamOverride::default()` 跑一遍，只借判断、不碰任何真实状态。
    /// 2. **寻址 + 施加**——在 [`fulcrum_runtime::overrides::OverrideLayer::apply_all`]
    ///    的**同一次持锁**内完成。⛔ 不许拆成「先 `get()` 拿 `Arc`、再在锁外面改」：
    ///    中间可能被一次 `/load` 抢先把格子收走，改的就是一个孤儿——命令回 200，
    ///    覆盖静默丢失。`apply_all` 把「查存在」与「改」焊在同一把锁里，这也顺带
    ///    让「全有或全无」成为**结构上的**：一次持锁，要么全改，要么一条不改。
    ///
    /// 两个阶段的任何一步不过，**一条都不生效**。
    fn runtime(&self, body: &[u8]) -> (u16, String) {
        /// ⛔ **只认这一种形状**：`{"actions":[…]}`。不接受「单条不带数组」的
        /// 第二种写法——两种形状是两个解析器，两个解析器迟早分家。
        ///
        /// ★ ★ ★ `deny_unknown_fields`（修复轮 1，评审 M6）：拼错的字段名要 400，
        /// ⛔ 不许静默丢掉。`/load` 的载荷（`StructuredConfig`）不能这样收紧——
        /// 那是编译器的产物、另有消费者，严格会变成陷阱；但 `/runtime` 的字段集
        /// （`verb`/`site`/`id`/`upstream`/`weight`）是**这个端点自己的字面量**，
        /// 严格是对的。★ 决定性理由：这一笔是这个端点刚诞生的那一刻——
        /// 还没有任何外部调用方，现在收紧免费，晚一步就是破坏性变更。
        /// 与"别的动词带 weight 就拒绝"（见下面 `if a.verb != "set_weight" …`）
        /// 是同一条纪律：本任务选择在这个端点上不静默丢字段。
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
            /// **选填**，不写 = 空串（★ 主 agent 裁决：`id` 不做任何校验，只逐字
            /// 精确匹配登记处的键）。
            #[serde(default)]
            id: String,
            /// ⛔ ⛔ ⛔ **逐字精确匹配，不做归一化**（主 agent 裁决，按 R6 ⑤）：
            /// 管理面对着的是运行时，运行时里那个上游就叫它归一化之后的名字；
            /// 写配置原文（比如 `backend`）指不到时，400 报文会把该站点下全部
            /// 合法的键列出来，照抄即可——做归一化会让那份清单变成死代码。
            upstream: String,
            /// 只对 `verb: "set_weight"` 有意义。
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

        // ── 阶段一：纯解析 + 值域校验，不碰登记处 ────────────────────────
        let mut resolved: Vec<RuntimeAction> = Vec::with_capacity(req.actions.len());
        for a in &req.actions {
            // ⚠ 「别的动词带了 weight 怎么办」是本任务自己的决定（brief 要求钉一条
            //   判据）：选**拒绝**而不是**忽略**——静默丢掉调用方明确给的字段，
            //   与本仓反复堵的那类「配置照过、命令回 200、其实没全生效」是同一族。
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
                    // ★ ⛔ 值域不在这里另写一份：借 `UpstreamOverride::set_weight`
                    //   自己的判断，对着一格临时格子跑一遍——真正的那次施加在
                    //   阶段二，落在登记处自己的锁里。
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

        // ── 阶段二：寻址 + 施加，`OverrideLayer::apply_all` 一次持锁做完 ─────
        let live = self.rt.current().override_keys();
        match self.rt.overrides().apply_all(&live, &resolved) {
            Ok(()) => (200, format!("已生效：{} 条动作\n", resolved.len())),
            Err(missing) => {
                let mut out = String::from("指不到这些上游，一条都没生效：\n");
                for k in &missing {
                    out.push_str(&format!("  · {k}\n"));
                }
                // ★ R6 ⑤ 明确要求：逐条列出**该站点下**全部合法的键，运维照抄即可。
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
        // ⚠ `overrides` 走查询串（R9），不是路径的一部分——`uri.path()` 已实测
        //   是 path-only，加查询串不影响上面 `path` 的路由判定。这里同样先
        //   `.to_string()`：下面还要 `&mut session` 读 body，不能带着借用过去。
        let query = session.req_header().uri.query().map(|s| s.to_string());

        // 读 body，带上界。
        let mut body = Vec::new();
        loop {
            match session.read_request_body().await {
                Ok(Some(chunk)) => {
                    if body.len() + chunk.len() > MAX_BODY {
                        // ★ ★ ★ R11「每一次响应」就是每一次：这条 413 在
                        //   路由分发**之外**——历史上最容易漏掉计数行的调用点
                        //   （brief 点名）。`reply()` 把 `&self.rt` 设成必填参数，
                        //   让「这个调用点忘了带」在结构上做不到。
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

        let (status, text) = match (method.as_str(), path.as_str()) {
            ("POST", "/load") => self.load(&body, query.as_deref()),
            ("POST", "/renew") => self.renew(&body),
            ("POST", "/purge") => self.purge(&body),
            ("POST", "/runtime") => self.runtime(&body),
            // ⚠ 不认识的路径回 404 而不是 200：一个「什么都收下」的管理面
            //   会让打错的命令看起来成功了。
            _ => unknown_route(method.as_str(), path.as_str()),
        };
        reply(&mut session, status, &text, &self.rt).await;
        None
    }
}

/// `_ =>` 分支的说明文字：不认识的路径回 404 而不是 200——一个「什么都收下」的
/// 管理面会让打错的命令看起来成功了。
///
/// ★ ★ 抽成自由函数是为了让「`/runtime` 必须出现在这份『可用』清单里」
/// （**M2 批 N 任务 4** 判据 11）不必驱动一整条真实的 HTTP 连接就能测——
/// 少了它，打错命令的人看不到这个新入口存在。
fn unknown_route(method: &str, path: &str) -> (u16, String) {
    (
        404,
        format!(
            "不认识：{method} {path}\n可用：POST /load（全量原子 load）、POST /renew（强制续期）、POST /purge（清缓存）、POST /runtime（增量改覆盖层）\n"
        ),
    )
}

/// 写回响应——★ ★ ★ 也是 R11「管理面的每一次响应都带覆盖层计数」（G18）落地
/// 的**唯一**地方。
///
/// ⚠ ⚠ `rt` 是**必填**参数，不是「算好了顺手传一个」：`process_new_http` 里
/// `reply()` 有两个调用点（413 那条在路由分发之外），让每个 handler 各自拼
/// 一次计数行会是 N 处抄件——加第五条路径时必然漏一次。把它做进 `reply()`
/// 自己，「某个调用点忘了带」在结构上做不到——与本仓把 `Upstream::weight`
/// 设成私有字段是同一条纪律。
async fn reply(session: &mut ServerSession, status: u16, text: &str, rt: &Arc<SharedRuntime>) {
    let mut h = match pingora_http::ResponseHeader::build(status, None) {
        Ok(h) => h,
        Err(e) => {
            error!("管理面建不出响应头：{e}");
            return;
        }
    };
    // ── R11：尾部追加覆盖层计数，200/400/404/409/413 一个不漏 ────────────
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
        let (code, out) = a.load(&body, Some("overrides=clear"));
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
        let (status, text) = a.load(
            &json_of("http://b.com:8080 {\n  respond 204\n}\n"),
            Some("overrides=clear"),
        );
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
        let (status, text) = a.load(
            &json_of("http://b.com:9090 {\n  respond 200\n}\n"),
            Some("overrides=clear"),
        );
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

    // ★ 反向那一半：**只改新鲜度、不动后端**必须照常换得进去。
    //   ⚠ 少了它，一个「只要配置里有 `cache` 就拒」的实现会让上面那条全绿，
    //   而它把一次完全正当的 `ttl` 调整也拦了。
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
        let (status, text) = a.load(
            serde_json::to_vec(&cfg).unwrap().as_slice(),
            Some("overrides=clear"),
        );
        assert_eq!(status, 400, "{text}");
        assert!(text.contains("没有任何改动生效"), "{text}");
        assert!(Arc::ptr_eq(&before, &a.rt.current()));
    }

    // ── 任务 5：`overrides` 必填参数（G120 / R9）───────────────────────────

    #[test]
    fn load缺overrides参数400且旧配置一个字节都没动() {
        // ⚠ ⚠ 判据写法纪律：body 单独看完全合法、会真的生效（与「端口集不变时
        //   换得动」用的是同一份夹具）——唯一的毛病是**没带 `?overrides=`**。
        //   否则 400 可能来自别的原因，这条判据就 confound 了，测不到「缺
        //   overrides 本身会被拒」这件事。
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
        // 同上一条同一条纪律：body 单独看完全合法，唯一的毛病是查询串里的值。
        let a = app(vec![(8080, false)]);
        let before = a.rt.current();
        let (status, text) = a.load(
            &json_of("http://b.com:8080 {\n  respond 204\n}\n"),
            Some("overrides=bogus"),
        );
        assert_eq!(status, 400, "{text}");
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

    // ── `POST /runtime`（M2 批 N 任务 4，裁决 R10）──────────────────────────
    //
    // 夹具两个站点：`pool.example` 一条 handle 写了 `id`、另一条没写但指向
    // **同一台机器**（判据 6 要用——两者必须是不同的格子）；`solo.example`
    // 单站点单上游单目标（其余判据用，`pick_index_by(None)` 的 `Some`/`None`
    // 就是「在不在可用集里」最直接的读数）。地址全用 IP 字面量，免得测试还要
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
        )
    }

    /// 按站点名 + id 从当前运行时里找回那一条 `reverse_proxy` 的目标。
    /// 找不到就直接 panic——那说明夹具写错了，不是被测代码的问题。
    fn find_target<'a>(rt: &'a Runtime, site: &str, id: &str) -> &'a fulcrum_runtime::ProxyTarget {
        rt.keyed_proxies()
            .into_iter()
            .find(|p| p.site == site && p.id == id)
            .unwrap_or_else(|| panic!("夹具里应该有「{site}·{id}」这个目标"))
            .target
    }

    // ── 判据 1 / 2：disable / enable 走管理面这条路 ─────────────────────────

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
        // ★ 复用同一个 `t`（同一个 `Arc<UpstreamOverride>`）再问一次——这正是
        //   R7 的意义：改登记处那一格，当场作用到正在跑的上游身上，不必重新
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

    // ── 判据 3：set_weight ────────────────────────────────────────────────

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

    // ── 判据 4：全有或全无 ────────────────────────────────────────────────

    #[test]
    fn runtime_全有或全无_第二条指不到时第一条也不生效() {
        let a = app_runtime();
        let rt = a.rt.current();
        let solo = find_target(&rt, "http://solo.example:8090", "");
        let pool_a = find_target(&rt, "http://pool.example:8080", "pool_a");
        assert_eq!(solo.pick_index_by(None), Some(0));
        assert_eq!(pool_a.pick_index_by(None), Some(0));

        // ⚠ 第二条的地址用 TEST-NET-3（203.0.113.0/24）——不会出现在任何配置
        //   或错误消息模板的字面量里（判据写法纪律，brief §3 末段）。
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

    // ── 判据 5：400 报文逐条列出该站点下的键 ─────────────────────────────

    #[test]
    fn runtime_指不到时400报文逐条列出该站点的键() {
        // ★ ★ ★ 修复轮 1，评审 I3：夹具必须选一个**有两个键**的站点——
        //   `pool.example` 下 `pool_a` 与空串 `id` 各占一格，两把键的地址相同
        //   （`10.40.0.1:1`），只有 `id` 不同。⚠ 换成这个夹具之前用的是
        //   `solo.example`（站下只有一个键），"逐条列出**全部**键"这句话
        //   从来没被测到——一个只打印第一条的实现照样绿。
        let a = app_runtime();
        let (status, text) = a.runtime(
            br#"{"actions":[{"verb":"disable","site":"http://pool.example:8080","upstream":"203.0.113.250:65000"}]}"#,
        );
        assert_eq!(status, 400, "{text}");
        // ⚠ ⚠ 判据写法纪律：真断言落在 `OverrideKey` 自己的 `Display` 输出上——
        //   不手写格式字符串（那样会在 `Display` 改动时悄悄失去意义），
        //   也不会出现在任何错误消息模板的固定字面量里。
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

    // ── 判据 6：选填 id 命中不同的格子 ───────────────────────────────────

    #[test]
    fn runtime_选填id命中不同格子() {
        let a = app_runtime();
        let rt = a.rt.current();
        let 带id = find_target(&rt, "http://pool.example:8080", "pool_a");
        let 不带id = find_target(&rt, "http://pool.example:8080", "");
        assert_eq!(带id.pick_index_by(None), Some(0));
        assert_eq!(不带id.pick_index_by(None), Some(0));

        // 不写 id ⇒ 命中空串那一格，`带id` 那一格不该被碰到。
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

        // 写了对应的 id ⇒ 命中另一格。
        let (status, text) = a.runtime(
            br#"{"actions":[{"verb":"disable","site":"http://pool.example:8080","id":"pool_a","upstream":"10.40.0.1:1"}]}"#,
        );
        assert_eq!(status, 200, "{text}");
        assert_eq!(带id.pick_index_by(None), None, "写了 id 现在也该摘掉了");
    }

    // ── 判据 7：set_weight 的值域 ─────────────────────────────────────────

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

    // ── 判据 8：不认识的 verb ─────────────────────────────────────────────

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

    // ── 判据 9：报文形状不对 ─────────────────────────────────────────────

    #[test]
    fn runtime_报文形状不对400且不生效() {
        let a = app_runtime();
        let rt = a.rt.current();
        let t = find_target(&rt, "http://solo.example:8090", "");
        // 单条不带数组——⛔ 不接受的第二种写法。
        let (status, _) = a.runtime(
            br#"{"verb":"disable","site":"http://solo.example:8090","upstream":"10.40.0.9:9"}"#,
        );
        assert_eq!(status, 400);
        assert_eq!(t.pick_index_by(None), Some(0));
        // JSON 坏掉。
        let (status, _) = a.runtime(b"{ not json");
        assert_eq!(status, 400);
        assert_eq!(t.pick_index_by(None), Some(0));
    }

    // ── 判据 10：指不到不留垃圾格子 ───────────────────────────────────────

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

    // ── 判据 11：404 的「可用」清单里有 /runtime ─────────────────────────

    #[test]
    fn 不认识的路径的可用清单里有_runtime() {
        let (status, text) = unknown_route("GET", "/nope");
        assert_eq!(status, 404);
        assert!(text.contains("/runtime"), "{text}");
    }

    // ── 判据 12：结构判据（TOCTOU）───────────────────────────────────────

    #[test]
    fn admin_rs这条路径不经过overridelayer_get() {
        // ★ ★ ★ 时序竞态难直接测，退而求其次：断言这个文件的源码里既真的走了
        //   `apply_all`（正面：结构上确实换了新路），也压根不出现
        //   `OverrideLayer::get()` 的调用形状（反面：没有偷偷绕回旧的两步式）。
        //   ⛔ 别写成「碰巧现在没并发所以过了」的行为判据——那是一把在好坏两种
        //   情况下读数相同的尺。
        //
        // ⚠ ⚠ 只查**产品代码**那一半（`#[cfg(test)]` 之前）——`include_str!` 会把
        //   这条判据自己也读进来，而这条判据的断言文字里恰好逐字写着
        //   `"overrides().get("` 这个串；不切掉测试模块的话，这条判据会读到
        //   **它自己的源码**当成「出现了那个调用」，永远红，属于本批判据写法
        //   纪律点名的那类自我碰撞（brief §3 末段的同一族陷阱）。
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

    // ── 补充判据：weight 只对 set_weight 有意义（本任务自己的决定，钉住）──

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

    // ── 补充判据：悬空的键端到端也不能被 /runtime 寻址 ──────────────────
    //
    // ★ overrides.rs 自己的判据已经在 `OverrideLayer::apply_all` 那一层验过
    //   这件事；这里再从 `admin.rs` 的 `runtime()` 走一遍完整路径，确认
    //   `live` 确实来自 `self.rt.current()`（正在服务的那一份），不是随便
    //   哪一份快照。
    #[test]
    fn runtime_悬空的键不能被寻址() {
        let a = app_runtime();
        let (status, text) = a.runtime(
            br#"{"actions":[{"verb":"disable","site":"http://solo.example:8090","upstream":"10.40.0.9:9"}]}"#,
        );
        assert_eq!(status, 200, "{text}");
        // 换一份不再有 solo.example 的配置——那一格现在悬空：设过覆盖，
        // 登记处按 R8 把它留着，但当前运行时已经不认它。
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

    // ── 补充判据：拼错的字段名要 400，不许静默丢掉（修复轮 1，评审 M6）──────
    //
    // ⚠ ⚠ ⚠ 两条判据都必须挑一个**除了那个拼错的字段之外，其余部分单独看
    //   也完全合法、会真的生效**的动作/报文——否则拼错的字段被静默丢掉之后，
    //   请求会因为**别的、无关的原因**恰好也是 400（比如 `set_weight` 缺
    //   `weight` 字段本来就 400），判据看起来红过，其实从没测到
    //   `deny_unknown_fields` 本身。这里用 `disable`（不需要 `weight`）+
    //   一个多余字段，`deny_unknown_fields` 不生效的话这条请求会正常
    //   200 并且真的把上游摘掉。

    #[test]
    fn runtime_拼错字段名400且不生效() {
        let a = app_runtime();
        let rt = a.rt.current();
        let t = find_target(&rt, "http://solo.example:8090", "");
        assert_eq!(t.pick_index_by(None), Some(0), "夹具本身应该是可用的");
        // 这条 `disable` 本身的三个必填字段都对——`wieght` 是多出来的、
        // 这条动作根本用不到的字段。没有 `deny_unknown_fields` 的话它会被
        // 静默丢掉，这条 disable 照常生效（200，上游被摘掉）。
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
        // 外层 `Req` 同样要 `deny_unknown_fields`：`actions` 本身是完全合法的
        // 一条 `disable`，多出来的顶层字段 `extra` 才是要挡的东西——
        // 没有 `deny_unknown_fields` 的话它会被静默丢掉，这条请求照常生效。
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

    // ── 任务 5：`/load` 的 `overrides` 两档接上真实的覆盖层（G120 / R8）──────
    //
    // ★ 复用上面 `POST /runtime` 那组的 `RUNTIME_DSL` / `app_runtime` /
    //   `find_target`：重新 `/load` 同一份 DSL 时监听端口集不变（装得进去），
    //   正好用来验证 `overrides` 两档在真实覆盖层上的行为——不止「回了 200」。
    //   ⚠ ⚠ 硬要求（brief）：这两档的**真正**判据必须从 `tests/serve/run.sh`
    //   那条真 socket 上打；这里是**另外**补一遍语义（直接调 handler，不经过
    //   `?overrides=` 的查询串解析那一步），不能取代 E2E。

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

        // ★ ★ 判据挂在数据面上：**新**运行时里同一个上游，覆盖必须仍然生效。
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
        // ⚠ ⚠ 判据写法纪律：真断言落在 `OverrideKey` 自己的 `Display` 输出上，
        //   不手写格式字符串。
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

        // 换一份不再有 `reverse_proxy 10.40.0.9:9` 的配置——这一格现在悬空。
        // ⚠ 监听端口集要保持 [(8080,false),(8090,false)] 不变，否则会撞 409：
        //   两个站点名照旧，只把 solo.example 的 body 换成裸 `respond`。
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
}
