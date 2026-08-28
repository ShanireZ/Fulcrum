//! 自研 HTTP 缓存（**M2 批 G**，G82–G84 + G95–G98）。
//!
//! ## 这一批做了什么
//!
//! **语义层**（RFC 9111：可缓存性 · 新鲜度 · 缓存键 · `Vary` · 条件重验证 ·
//! 上游响应头里的**全套** `Cache-Control` 指令，G97）+ **内存后端** +
//! **防惊群** + 管理面 `POST /purge`。
//!
//! **磁盘后端是批 H**（G95 的切法）：形状已由 G83/G84 定死 ——
//! 两级分片目录、meta 与 body 两文件、`tmp` 后 `rename`、启动不扫盘。
//!
//! ## ★ ★ ★ 判据为什么写成这个样子
//!
//! G82 拍板时把代价写在了明处：
//! > RFC 9111 语义、`Vary`、防惊群、元数据序列化 ≈4000 行全自己写，
//! > **而缓存的错表现为「偶尔给错内容」** —— 不像转发的错那样当场可见。
//!
//! ⇒ 每一条规则的判据都要说清**「写错了会怎样」**，而不只是「这样写是对的」。
//! 本模块四个子模块里每一条 `#[test]` 都照这个写。
//!
//! ## 我们是**共享**缓存
//!
//! 这一句改变好几处判断，逐条写在 [`policy`] 顶部 —— 其中最贵的一条是
//! **带 `Authorization` 的请求，其响应默认不可缓存**（RFC 9111 §3.5）：
//! 漏了它就是把一个人的私有页面发给下一个人，**而不会有任何报错**。

pub mod cc;
pub mod coalesce;
pub mod disk;
pub mod key;
pub mod policy;
pub mod store;

use log::{error, info, warn};
use std::path::Path;
use std::sync::Arc;

/// 解析一个 HTTP 日期成 unix 秒。
///
/// ★ ★ **直接用静态文件那一批写的解析器**（[`crate::files::httpdate`]，G93）——
/// 它已经把三种格式与 RFC 850 两位年那条规则都做了，还有 10 条单测钉着。
/// ⚠ 在这里再写一份的代价很具体：`Expires` 与 `Last-Modified` 是**同一种**日期，
/// 两份实现迟早在闰年或两位年上分家，而现场表现是「某些响应的缓存寿命算错了」。
/// ★ 这与批 C 复用 `wildcard_covers` 是同一条纪律：**让分家在结构上做不到**。
pub fn files_httpdate(s: &str) -> Option<i64> {
    crate::files::httpdate::parse(s)
}

/// 后端：内存（批 G）或磁盘（批 H），**全进程只有一个**。
///
/// ## ★ ★ ★ 为什么是一个 enum，而不是 `Box<dyn Store>`
///
/// 磁盘层要实现的就是 `MemStore` 那一组接口，而其中 [`store::MemStore::get`] 带一个泛型闭包 ——
/// ⚠ **它不是对象安全的**，`dyn` 要么把闭包装箱、要么把签名改掉。
/// ★ enum 换来两样东西：调用点不变（还是那七个同名方法），
/// 而且**再加一个后端时，漏掉哪一个方法是编译错误**。
///
/// ⚠ ⚠ 第三档 [`Backend::Off`] 不是凑数的，理由见 [`Backend::open`]。
#[derive(Debug)]
pub enum Backend {
    Mem(store::MemStore),
    Disk(disk::DiskStore),
    /// 配了 `disk` 而那个目录用不了 —— **不缓存，但照常转发**。
    Off,
}

impl Backend {
    /// 按配置挑一个后端。
    ///
    /// ## ★ ★ ★ 目录用不了的时候，三种做法里为什么取这一种
    ///
    /// | 做法 | 失败形态 |
    /// |---|---|
    /// | 退回内存 | ⚠ ⚠ 用户按**磁盘**写的 `capacity`（比如 50GB）会变成内存预算 ⇒ **OOM** |
    /// | `exit(1)` | ⚠ ⚠ 换代时新一代拒绝启动 = **服务整体中断**，而 `validate` 是绿的 |
    /// | **关掉缓存，照常转发** | 变慢，仅此而已 |
    ///
    /// ★ 而「关掉」必须**说得出来**，否则它就是本仓库反复点名的那种静默失能：
    /// 装载日志有一行 `error`，运行时 `X-Fulcrum-Cache` 那个头**不再出现** ——
    /// 两个独立的信号，一个给启动那一刻，一个给之后的任何时刻。
    pub fn open(disk_dir: Option<&str>, capacity: u64) -> Backend {
        let Some(dir) = disk_dir else {
            return Backend::Mem(store::MemStore::new(capacity));
        };
        match disk::DiskStore::open(Path::new(dir), capacity) {
            Ok(s) => Backend::Disk(s),
            Err(e) => {
                error!(
                    "★ 缓存磁盘目录 `{dir}` 用不了（{e}）—— 本进程不再缓存任何东西，\
                     但请求照常转发。修好目录（存在、可写、属主对）之后重启生效"
                );
                Backend::Off
            }
        }
    }

    /// 这个后端叫什么（装载日志与 `X-Fulcrum-Cache` 都取它）。
    ///
    /// ★ ★ 它说的是**真的挑中了哪一个**，不是配置里写了什么 ——
    /// 那正是上面那张表里「关掉」这一档能被看见的原因。
    pub fn label(&self) -> &'static str {
        match self {
            Backend::Mem(_) => "memory",
            Backend::Disk(_) => "disk",
            Backend::Off => "off",
        }
    }

    /// 给 `X-Fulcrum-Cache` 用的状态值：磁盘后端带一个后缀。
    ///
    /// ⚠ **判据要能分辨「从内存来的」与「从磁盘来的」**，而那件事靠状态码看不出来。
    /// ★ 但这个头只是**方便**的那一半：它信的是「本进程挑中了哪个后端」。
    /// 真正证明「东西在盘上」的判据是**把进程杀掉再起来，它还在** ——
    /// 那一条内存后端**不可能**通过。两条一起用。
    ///
    /// ⚠ 收 [`CacheState`] 而不是 `&str`，理由写在那个枚举上：**这一串与
    /// `fulcrum_cache_events_total{event}` 那一格是同一件事的两种粒度**，
    /// 而两处都从同一个枚举穷尽出来时，「加了一种状态却漏了一处」编不过。
    pub fn state(&self, base: CacheState) -> String {
        match self {
            Backend::Disk(_) => format!("{}-DISK", base.header_base()),
            _ => base.header_base().to_string(),
        }
    }

    // ── 七个操作。★ 与 `MemStore` 逐字同签名，逐条转发。────────────────────

    pub fn get<'a, F>(&self, primary: &str, get_header: F) -> store::Lookup
    where
        F: FnMut(&str) -> Option<&'a str>,
    {
        match self {
            Backend::Mem(s) => s.get(primary, get_header),
            Backend::Disk(s) => s.get(primary, get_header),
            Backend::Off => store::Lookup::Miss,
        }
    }

    pub fn put(&self, primary: &str, secondary: String, entry: store::Entry) {
        match self {
            Backend::Mem(s) => s.put(primary, secondary, entry),
            Backend::Disk(s) => s.put(primary, secondary, entry),
            Backend::Off => {}
        }
    }

    pub fn refresh(
        &self,
        primary: &str,
        secondary: &str,
        fresh_for: u64,
        now: i64,
        cc: cc::ResponseCc,
    ) {
        match self {
            Backend::Mem(s) => s.refresh(primary, secondary, fresh_for, now, cc),
            Backend::Disk(s) => s.refresh(primary, secondary, fresh_for, now, cc),
            Backend::Off => {}
        }
    }

    pub fn purge_primary(&self, primary: &str) -> usize {
        match self {
            Backend::Mem(s) => s.purge_primary(primary),
            Backend::Disk(s) => s.purge_primary(primary),
            Backend::Off => 0,
        }
    }

    pub fn purge_prefix(&self, prefix: &str) -> usize {
        match self {
            Backend::Mem(s) => s.purge_prefix(prefix),
            Backend::Disk(s) => s.purge_prefix(prefix),
            Backend::Off => 0,
        }
    }

    pub fn purge_all(&self) -> usize {
        match self {
            Backend::Mem(s) => s.purge_all(),
            Backend::Disk(s) => s.purge_all(),
            Backend::Off => 0,
        }
    }

    pub fn stats(&self) -> (u64, usize) {
        match self {
            Backend::Mem(s) => s.stats(),
            Backend::Disk(s) => s.stats(),
            Backend::Off => (0, 0),
        }
    }
}

/// 「这一条是从缓存发出去的」有哪几种 —— `X-Fulcrum-Cache` 那个头的**基值**
/// （不含后端后缀）。
///
/// # ★ ★ ★ 为什么是枚举而不是一个 `&str`
///
/// 同一件事要在两个粒度上说出来：响应头要分辨内存与磁盘（`HIT` / `HIT-DISK`），
/// 而 `fulcrum_cache_events_total{event}` 那张表把上界写死成四个值 ⇒ 两者折成一格。
/// ⚠ ⚠ 若把状态当字符串传，那个折叠就只能写成 `match s { … , _ => … }` ——
/// 而 `&str` 的 `match` **必须**有兜底臂，于是**将来新增的一种状态会静默地被算成别的**：
/// 指标数字照涨，只是涨错了格，没有任何东西会说出来（R7 明令禁止的形状）。
/// ★ 换成枚举之后，[`Backend::state`] 与 [`CacheState::event`] 两处一起穷尽 ——
/// 加一个变体时**两处都编不过**，那正是我们要的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheState {
    /// 新鲜，直接从缓存发出去。
    Hit,
    /// 陈了，带校验器问过上游拿到 304 之后，把缓存里那份发出去。
    Revalidated,
}

impl CacheState {
    /// `X-Fulcrum-Cache` 里那个基值。⚠ 这两个字符串是**对外契约**
    /// （判据与用户的告警规则都按它们写），改一个字就是改契约。
    fn header_base(self) -> &'static str {
        match self {
            CacheState::Hit => "HIT",
            CacheState::Revalidated => "REVALIDATED",
        }
    }

    /// 折成 `fulcrum_cache_events_total{event}` 的那一格（R7）。
    ///
    /// ⚠ 折得比响应头**粗**：`HIT` 与 `HIT-DISK` 在指标上是同一格。
    /// ★ 想知道「是哪一种命中」去看访问日志 —— 它记的是 `cache` 的原值。
    pub fn event(self) -> CacheEvent {
        match self {
            CacheState::Hit => CacheEvent::Hit,
            CacheState::Revalidated => CacheEvent::Stale,
        }
    }
}

/// `fulcrum_cache_events_total{event}` 的取值 —— **闭集，四个**（R7）。
///
/// ★ 四个是同一件事的四个去向，**单位都是「条」**：一条请求命中了 / 回源了 /
/// 重验证后发出去了，或者一条缓存条目被清掉了。
///
/// ⚠ 只有 `Hit` 与 `Stale` 在 `X-Fulcrum-Cache` 里露过面（见 [`CacheState`]）：
/// **回源那条路没有这个头**（契约里写死的，所以访问日志里也没有 `cache` 那一格），
/// 而 `Purge` 根本不发生在请求路径上。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheEvent {
    /// 从缓存发出去（含磁盘后端）。
    Hit,
    /// 回源：这条响应的内容来自上游。
    Miss,
    /// 重验证之后把缓存里那份发出去。
    Stale,
    /// 被清掉的条目数。
    Purge,
}

impl CacheEvent {
    /// 标签值。⚠ 与 [`CacheState::header_base`] 不同，这几个是**小写**的 ——
    /// Prometheus 标签值的习惯，而且它们与响应头本来就不是同一套词。
    pub fn label(self) -> &'static str {
        match self {
            CacheEvent::Hit => "hit",
            CacheEvent::Miss => "miss",
            CacheEvent::Stale => "stale",
            CacheEvent::Purge => "purge",
        }
    }

    /// 记 `n` 次。★ 收一个 `n` 是因为 `purge` 天生成批（一次清掉了 N 条）。
    pub fn record(self, n: u64) {
        crate::metrics::CACHE_EVENTS_TOTAL.inc_by(&[self.label()], n);
    }
}

/// 一个站点上生效的缓存实例。
///
/// ⚠ ⚠ **缓存是进程级的，不是每次装载新建的**：G8 的全量原子 load 会把整棵
/// 运行时图换掉，而**缓存内容不该跟着没**（换一次配置就清空缓存，等于每次
/// reload 都让上游挨一次满负荷）。⇒ 它挂在 `FulcrumApp` 上，跨 load 存活。
#[derive(Debug)]
pub struct CacheHandle {
    pub store: Backend,
    pub coalescer: coalesce::Coalescer,
}

impl CacheHandle {
    pub fn new(store: Backend) -> Arc<CacheHandle> {
        Arc::new(CacheHandle {
            store,
            coalescer: coalesce::Coalescer::new(),
        })
    }

    /// 拿到磁盘后端（不是磁盘后端就 `None`）。
    ///
    /// ★ 后台维护任务用它决定起不起 —— **它是磁盘后端才有的事**。
    /// ⚠ 无条件起一个每秒醒一次什么都不做的任务，会让「有没有磁盘缓存」
    /// 在 `top` 里看不出区别，也会让日志里那句「后台维护已启动」变成一句空话。
    pub fn disk(&self) -> Option<&disk::DiskStore> {
        match &self.store {
            Backend::Disk(s) => Some(s),
            _ => None,
        }
    }
}

/// 磁盘缓存的后台维护（**G84**：渐进重建索引 + 收孤儿 + save 索引）。
///
/// ★ ★ 它挂成 Pingora 的 `BackgroundService`，与 DNS 重解析、健康检查、ACME 同款 ——
/// **不是**一个裸的 `tokio::spawn`。⚠ 差别很具体：`BackgroundService` 拿得到
/// `ShutdownWatch`，于是它能在停机时把索引 **save** 下去（G84 那一半的自然时刻），
/// 而一个裸 spawn 的任务在换代时是被**直接丢掉**的。
pub struct CacheMaintenanceService {
    handle: Arc<CacheHandle>,
    tick: std::time::Duration,
}

impl CacheMaintenanceService {
    /// 一个一级分片目录一跳（最多 256 个）⇒ 一整轮约 4 分钟。
    ///
    /// ⚠ 有意**不是**「一跳走完全盘」：那只是把启动扫盘挪了个时刻做，
    /// 而 G84 拒绝的是那件事的**代价**（一次随缓存大小线性增长的长停顿），
    /// 不只是它发生在哪一刻。
    pub const TICK: std::time::Duration = std::time::Duration::from_secs(1);

    pub fn new(handle: Arc<CacheHandle>) -> CacheMaintenanceService {
        CacheMaintenanceService {
            handle,
            tick: Self::TICK,
        }
    }
}

#[async_trait::async_trait]
impl pingora_core::services::background::BackgroundService for CacheMaintenanceService {
    async fn start(&self, mut shutdown: pingora_core::server::ShutdownWatch) {
        let Some(_) = self.handle.disk() else {
            return;
        };
        info!(
            "缓存：磁盘索引后台渐进重建已启动（每 {}s 一个一级分片目录；G84：启动不扫盘）",
            self.tick.as_secs()
        );
        let mut passes = 0u64;
        let mut since_save = 0u64;
        loop {
            if *shutdown.borrow() {
                break;
            }
            // ★ `select!` 而不是先 sleep 再看信号（与 DNS 重解析同一条纪律）：
            //   一个 sleep 会把优雅停机拖长它那么久。
            tokio::select! {
                _ = tokio::time::sleep(self.tick) => {}
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { break; }
                }
            }
            let h = Arc::clone(&self.handle);
            // ⚠ ⚠ **走盘是阻塞 I/O**，必须扔进 blocking 池 —— 与 `getaddrinfo`
            //   那条一模一样的理由：直接在 runtime 线程上遍历一个大目录，
            //   会把排在同一条线程上的**请求**一起拖住。
            let done = match tokio::task::spawn_blocking(move || {
                h.disk().map(|d| d.rebuild_step()).unwrap_or(true)
            })
            .await
            {
                Ok(d) => d,
                Err(e) => {
                    warn!("缓存：后台重建那一跳没跑成：{e}");
                    continue;
                }
            };
            since_save += 1;
            if done {
                passes += 1;
                // ★ 只在**第一轮**走完时说一句：之后每 4 分钟一行就是噪音，
                //   而第一轮走完这件事有意义 —— 它是「索引从此是全的」那一刻。
                if passes == 1 {
                    let (used, n) = self.handle.store.stats();
                    info!("缓存：磁盘索引首轮渐进重建完成 —— {n} 条 / {used}B");
                }
            }
            // ★ 定期 save：崩溃（不是优雅停机）时最多丢一分钟的索引更新，
            //   而丢了的后果只是下次启动占用算得偏小，由渐进重建补回来。
            if since_save >= 60 {
                since_save = 0;
                let h = Arc::clone(&self.handle);
                let _ = tokio::task::spawn_blocking(move || {
                    if let Some(d) = h.disk() {
                        d.save_index();
                    }
                })
                .await;
            }
        }
        // ★ ★ **停机时把索引存下去**（G84 的 save 那一半）。
        //   ⚠ 这一步是这个 service 相对于裸 `tokio::spawn` 唯一真正多出来的能力，
        //   而它决定了下一代启动时占用算得准不准。
        if let Some(d) = self.handle.disk() {
            d.save_index();
            let (used, n) = self.handle.store.stats();
            info!("缓存：收到停机信号，淘汰索引已存盘（{n} 条 / {used}B）");
        }
    }
}

// ★ 缓存的装载摘要住在 `lib.rs` 的 `log_load_summary` 里（与 hide 清单、UNWIRED 公告同一处）：
//   装载结论只有一个出口，而不是两个各说各的。

#[cfg(test)]
mod tests {
    //! **M2 批 M**（裁决 R7）：`X-Fulcrum-Cache` 的**完整取值集合** ↔ 四个缓存事件。

    use super::*;

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!(
            "fulcrum-cache-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir(&p).expect("临时目录撞名了 —— 那说明命名不够唯一");
        p
    }

    #[test]
    fn 状态值的完整取值集合_以及它们各自折到哪个事件() {
        // ★ ★ 一次列全：两个状态 × 三种后端 = `X-Fulcrum-Cache` 能出现的**全部**字符串。
        //   ⚠ 只列「我见过的那几个」不算数 —— R7 要求的是**取值集合**，
        //     而集合是由这两个枚举与后端那三档共同定死的。
        let dir = tmp_dir("state");
        let mem = Backend::Mem(store::MemStore::new(1 << 20));
        let off = Backend::Off;
        let disk = Backend::Disk(disk::DiskStore::open(&dir, 1 << 20).expect("打不开磁盘缓存"));

        assert_eq!(mem.state(CacheState::Hit), "HIT");
        assert_eq!(off.state(CacheState::Hit), "HIT");
        assert_eq!(disk.state(CacheState::Hit), "HIT-DISK");
        assert_eq!(mem.state(CacheState::Revalidated), "REVALIDATED");
        assert_eq!(off.state(CacheState::Revalidated), "REVALIDATED");
        assert_eq!(disk.state(CacheState::Revalidated), "REVALIDATED-DISK");

        // ★ ★ 折叠的两个方向都要判：
        //   ① 后端后缀**不改变**事件那一格（`HIT` 与 `HIT-DISK` 同一个 `hit`）；
        //   ② 状态**改变**它。
        //   ⚠ 少了 ②，一个「全折成 `hit`」的实现照样能让 ① 全绿。
        assert_eq!(CacheState::Hit.event().label(), "hit");
        assert_eq!(CacheState::Revalidated.event().label(), "stale");
        assert_ne!(
            CacheState::Hit.event(),
            CacheState::Revalidated.event(),
            "命中与重验证被折成了同一格 —— 那两件事在指标上就再也分不开了"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 四个事件的标签值就是那张表写死的四个_而且互不相同() {
        // ⚠ 上界是这四个：加一个变体，`label` 的 `match` 当场编不过。
        //   ★ 这条判据钉住的是**标签值本身** —— 改一个字就是改抓取端的查询语句。
        assert_eq!(CacheEvent::Hit.label(), "hit");
        assert_eq!(CacheEvent::Miss.label(), "miss");
        assert_eq!(CacheEvent::Stale.label(), "stale");
        assert_eq!(CacheEvent::Purge.label(), "purge");

        // ★ ★ 反向那半：四个标签值**互不相同**。
        //   ⚠ 一次复制粘贴就能让两个变体共用一个词，而上面四条只要有一条跟着改
        //     就照样全绿 —— 现场是两件事被加进同一格，数字看起来完全正常。
        let mut labels: Vec<&str> = [
            CacheEvent::Hit,
            CacheEvent::Miss,
            CacheEvent::Stale,
            CacheEvent::Purge,
        ]
        .iter()
        .map(|e| e.label())
        .collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels, ["hit", "miss", "purge", "stale"]);
    }

    #[test]
    fn 四个事件都进得了那张进程级的表() {
        // ★ 只判「这一格存在」，不判读数：别的判据也在往同一张表里写，
        //   而**存在**是单调的 ⇒ 这条判据不会因为测试跑的顺序而红。
        // ⚠ `record(0)` 照样把这条 series 建出来 —— counter 在 Prometheus 里
        //   本来就该尽早存在（「族在、值是 0」与「族根本没出现过」是两件事）。
        let all = [
            CacheEvent::Hit,
            CacheEvent::Miss,
            CacheEvent::Stale,
            CacheEvent::Purge,
        ];
        for e in all {
            e.record(0);
        }
        let out = crate::metrics::render();
        for e in all {
            assert!(
                out.contains(&format!(
                    "fulcrum_cache_events_total{{event=\"{}\"}} ",
                    e.label()
                )),
                "缺 {} 这一格：\n{out}",
                e.label()
            );
        }
    }
}
