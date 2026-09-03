//! 连接计数的登记处（**M2 批 O**，G122 的**连接那半**）。
//!
//! 两个族的全部标签与计数都在这里；fork 那一侧（**改动 15**）只知道
//! 「这个监听器上多了一条 / 少了一条」，它不认识标签、也不知道 Prometheus 存在。
//!
//! # ★ ★ 为什么是「抓取时问活体」而不是事件点记账
//!
//! fork 够不到我们的 [`crate::metrics`] 模块（依赖方向是反的）⇒ 只能由它把数加到
//! 我们递过去的对象上，我们在渲染那一刻现问 —— 与 `upstream_inflight` 同一条路子。
//!
//! # ⚠ ⚠ 一个视图要按 `listen` 分格，⛔ 不能一格一视图
//!
//! `Listeners::set_connection_counter` 给**所有**端点设的是同一个实现，而一个
//! `Listeners` 可以有多个监听地址。⇒ [`ConnView::enter`] 必须按收到的 `listen` 查表。
//! ★ 那正是这一批否掉「fork 只暴露一对原子计数」那个方案的全部理由：
//! 那个方案的粒度只到 `Listeners`，而「一个 Service 只有一个地址」是**今天的巧合**。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Once, RwLock};

use log::warn;
use pingora_core::listeners::{ConnGuard, ConnectionCounter};

/// 接线漏了一个监听器时那一笔落在哪一格。
///
/// ★ ⛔ **不许丢掉那一笔**：丢掉的话，「接线漏了」表现为**指标上什么都没有**，
/// 而那与「那个端口没人连」长得一模一样。⇒ 记成一个记号，让它在正文里看得见。
/// ★ 尖括号形状随 G118 的 `<other>` / G121 的 `<none>` / G127 的 `<unknown>`：
/// 尖括号在合法的监听地址里不可能出现，撞不上真值。
pub const UNDECLARED: &str = "<undeclared>";

/// 这条连接是从哪个入口进来的（**闭集**，`fulcrum_connections_*` 的 `entrypoint` 标签）。
///
/// ⚠ ⚠ **⛔ 不叫 `proto`**：`fulcrum_requests_total` 已经有一个 `proto` 标签，取值是
/// `HTTP/1.1` 那一族。同名不同值域会让运维跨族写同一个过滤器时**拿到空集而不报错**。
/// ★ 本仓对这种「同名纯属巧合」有前例：见 `access_log::Record` 的 `site` / `site_addr`。
///
/// ⚠ 每个监听器**只属于一个** entrypoint ⇒ 这两个族的 series 数 = **监听器数**，
/// ⛔ 不是 5 × 监听器数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Entrypoint {
    /// pingora 的 `ListeningService` 上的数据面（h1/h2）。
    Http,
    /// 同一个 `ListeningService`，但那是管理面的 Unix socket（G14）。
    ///
    /// ★ 单列一格而不是并进 [`Entrypoint::Http`]：跨平面求和之前先按 entrypoint 过滤，
    /// 是抓取端唯一能把两个平面分开的办法。
    Admin,
    /// HTTP/3 入口（`quic::listener`，自己的 `recv_from` 循环）。
    Quic,
    /// L4 TCP 透传（`l4.rs` 自己的 accept 循环）。
    L4Tcp,
    /// L4 UDP 透传。⚠ 它**没有连接**，这一格数的是**会话**，见 [`ConnRegistry::set_active`]。
    L4Udp,
}

/// `fulcrum_connections_*{entrypoint}` 的**取值闭集**，`metrics.rs` 的基数表判据按它算上界。
///
/// ⚠ ⚠ ⛔ **不是手抄**：单测把 [`Entrypoint::ALL`] 逐个过 [`Entrypoint::as_str`]，
/// 与本常量互为子集；而 `as_str` 那条 match 是穷尽的 ⇒ 加一种入口就编不过。
pub const ENTRYPOINTS: &[&str] = &["http", "admin", "quic", "l4_tcp", "l4_udp"];

impl Entrypoint {
    /// ⚠ ⚠ **这个函数存在的唯一理由是让编译器守住 [`Entrypoint::ALL`] 的完整性。**
    /// 加一种入口 ⇒ 这条 match 编不过 ⇒ 人被逼着回来同时改 `ALL`。
    /// ⛔ 别给它加 `_ =>` 兜底臂，那样它就什么都不守了。
    /// ★ 只靠 [`Entrypoint::as_str`] 的穷尽性是不够的：那只逼人改 `as_str`，
    /// 而「顺手也改 `ALL`」是**一句写在注释里的话，没有门守着**。
    #[cfg(test)]
    pub(crate) fn 穷尽性自证(self) -> Self {
        match self {
            Entrypoint::Http => self,
            Entrypoint::Admin => self,
            Entrypoint::Quic => self,
            Entrypoint::L4Tcp => self,
            Entrypoint::L4Udp => self,
        }
    }

    /// 全部入口。⚠ 完整性由 [`Entrypoint::穷尽性自证`] 那条穷尽 match 守着。
    pub const ALL: [Entrypoint; 5] = [
        Entrypoint::Http,
        Entrypoint::Admin,
        Entrypoint::Quic,
        Entrypoint::L4Tcp,
        Entrypoint::L4Udp,
    ];

    /// 标签里那个词。
    ///
    /// ⚠ `match` 是**穷尽**的 ⇒ 将来加一种入口时**这里编不过**，
    /// 而不是静默地多出一个没人认得的标签值。
    pub fn as_str(self) -> &'static str {
        match self {
            Entrypoint::Http => "http",
            Entrypoint::Admin => "admin",
            Entrypoint::Quic => "quic",
            Entrypoint::L4Tcp => "l4_tcp",
            Entrypoint::L4Udp => "l4_udp",
        }
    }
}

/// 一格的键：**入口种类 + 监听地址原样**。
///
/// ★ 两截缺一不可：只按 `listen` 分的话，G110 下同一个端口号上的 TCP 与 QUIC
/// 会合并成一条 series；只按 `ep` 分的话，一个 `Listeners` 的多个地址会合并。
type CellKey = (Entrypoint, Box<str>);

type CellMap = BTreeMap<CellKey, Arc<ConnCell>>;

/// 渲染那一刻，一格的读数：入口 · 监听地址 · `total` · `active`。
pub type ConnReading = (Entrypoint, Box<str>, u64, i64);

/// 一个监听器的两个数。
#[derive(Debug, Default)]
struct ConnCell {
    /// 累计接进来过多少条。**只增**。
    total: AtomicU64,
    /// 此刻有多少条还活着。⚠ 含**还在握手**的那些（`enter` 在握手之前）。
    active: AtomicI64,
}

/// 全部监听器的连接计数。
///
/// ★ ★ 格子由 [`ConnRegistry::view`] / [`ConnRegistry::guard_for`] 在**接线那一刻**声明 ——
/// ⛔ **有意不预先枚举一遍监听器**：那会是第二条枚举路径，而
/// `fulcrum_runtime::Runtime::all_proxy_targets` 的文档正是为这个形状写的警告
/// （「⚠ ⚠ **有意不自己再走一遍**……走法分家的表现是某条在某一张清单里不存在」）。
///
/// ⚠ 写只发生在启动期（接线），读发生在每条连接与每次抓取 ⇒ 读锁无竞争。
#[derive(Debug, Default)]
pub struct ConnRegistry {
    cells: RwLock<CellMap>,
}

impl ConnRegistry {
    pub fn new() -> Arc<ConnRegistry> {
        Arc::new(ConnRegistry::default())
    }

    /// 声明一个格子并把它取出来（幂等）。
    fn declare(&self, ep: Entrypoint, listen: &str) -> Arc<ConnCell> {
        {
            let map = self.cells.read().unwrap_or_else(|e| e.into_inner());
            if let Some(c) = map.get(&(ep, Box::from(listen))) {
                return c.clone();
            }
        }
        let mut map = self.cells.write().unwrap_or_else(|e| e.into_inner());
        map.entry((ep, Box::from(listen)))
            .or_insert_with(|| Arc::new(ConnCell::default()))
            .clone()
    }

    /// 取一个格子；没声明过就落到 [`UNDECLARED`] 那一格并**喊一次**。
    fn cell(&self, ep: Entrypoint, listen: &str) -> Arc<ConnCell> {
        {
            let map = self.cells.read().unwrap_or_else(|e| e.into_inner());
            if let Some(c) = map.get(&(ep, Box::from(listen))) {
                return c.clone();
            }
        }
        // ⚠ **只喊一次**：这是每条连接都会走的路，不限流的话一次配置错误会让
        //   日志本身变成第二个故障。
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            warn!(
                "连接计数：监听器 {}/{listen} 没有被声明过 —— 这一笔记进 {UNDECLARED}。\
                 ★ 这说明接线漏了一处，不是运行时的正常状态。",
                ep.as_str()
            );
        });
        self.declare(ep, UNDECLARED)
    }

    /// 给 fork 那一侧用的视图：**声明这一格**，并返回一个绑定了 `ep` 的计数器。
    ///
    /// ★ ★ ★ `entrypoint` 由视图带、`listen` 由 fork 递 ⇒ **贴错标签在结构上做不到**。
    pub fn view(self: &Arc<Self>, ep: Entrypoint, listen: &str) -> Arc<dyn ConnectionCounter> {
        self.declare(ep, listen);
        Arc::new(ConnView {
            reg: self.clone(),
            ep,
        })
    }

    /// 给我们自己那三条循环用：**声明这一格**，并返回一个已经绑好 `(ep, listen)` 的句柄。
    ///
    /// ★ ★ 绑一次、用很多次 ⇒ 「声明的那一格」与「写入的那一格」**在结构上是同一个**，
    /// ⛔ 不会出现「声明了 A 却往 B 上写」那种在今天的配置上全绿的错法。
    pub fn bind(self: &Arc<Self>, ep: Entrypoint, listen: &str) -> BoundConn {
        BoundConn {
            counter: self.view(ep, listen),
            cell: self.declare(ep, listen),
            listen: Arc::from(listen),
        }
    }

    /// 渲染那一刻的读数，**按键排序**（`BTreeMap` 天然如此）。
    ///
    /// ⚠ 顺序确定这件事没有任何断言会因它变红，代价是**两次抓取的 diff 里全是噪声** ——
    /// 而人正是靠 diff 看指标出没出新东西的（`metrics` 模块顶部那条纪律）。
    pub fn snapshot(&self) -> Vec<ConnReading> {
        let map = self.cells.read().unwrap_or_else(|e| e.into_inner());
        map.iter()
            .map(|((ep, listen), c)| {
                (
                    *ep,
                    listen.clone(),
                    c.total.load(Ordering::Relaxed),
                    c.active.load(Ordering::Relaxed),
                )
            })
            .collect()
    }
}

/// 已经绑好 `(entrypoint, listen)` 的一格 —— 我们自己那三条 accept 循环拿的是它。
///
/// ★ 它与 fork 那条路**共用同一个 [`ConnGuard`]** ⇒ 减一仍然只有那一个调用点。
#[derive(Clone)]
pub struct BoundConn {
    counter: Arc<dyn ConnectionCounter>,
    cell: Arc<ConnCell>,
    listen: Arc<str>,
}

impl BoundConn {
    /// 拿一个守卫：构造即 `+1`，`Drop` 即 `-1`。
    ///
    /// ⚠ ⚠ 调用方**必须把它绑到一个有名字的变量上**（`let _g = …;`）——
    /// 写成裸 `let _ = …;` 会当场 drop，于是 `active` 恒为 0 而 `total` 照涨，
    /// **且不会有任何东西红**。
    pub fn guard(&self) -> ConnGuard {
        ConnGuard::new(self.counter.clone(), self.listen.clone())
    }

    /// 把 `active` **设成**这个数（⛔ 不是加上）。★ 只服务 L4 UDP 的会话表派生。
    pub fn set_active(&self, n: usize) {
        self.cell.active.store(n as i64, Ordering::Relaxed);
    }

    /// 只把 `total` 加一（⛔ 不动 `active`）。★ 只服务 L4 UDP 的「新建了一条会话」。
    pub fn bump_total(&self) {
        self.cell.total.fetch_add(1, Ordering::Relaxed);
    }
}

/// 绑定了一个 `entrypoint` 的计数器视图 —— fork 那一侧拿到的就是它。
#[derive(Debug)]
struct ConnView {
    reg: Arc<ConnRegistry>,
    ep: Entrypoint,
}

impl ConnectionCounter for ConnView {
    fn enter(&self, listen: &str) {
        let c = self.reg.cell(self.ep, listen);
        c.total.fetch_add(1, Ordering::Relaxed);
        c.active.fetch_add(1, Ordering::Relaxed);
    }

    fn leave(&self, listen: &str) {
        self.reg
            .cell(self.ep, listen)
            .active
            .fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 声明过的格子从一开始就有样本_而且守卫一进一出() {
        let reg = ConnRegistry::new();
        let v = reg.view(Entrypoint::Http, "0.0.0.0:80");
        // ★ 「建成就出样本」：声明完立刻就该有一条 0/0，而不是等第一条连接。
        //   ⇒ 「监听器在、只是没人连」与「配置里根本没有它」因此分得开。
        assert_eq!(
            reg.snapshot(),
            vec![(Entrypoint::Http, "0.0.0.0:80".into(), 0, 0)]
        );
        v.enter("0.0.0.0:80");
        assert_eq!(
            reg.snapshot(),
            vec![(Entrypoint::Http, "0.0.0.0:80".into(), 1, 1)]
        );
        v.leave("0.0.0.0:80");
        // ★ ★ counter 只增、gauge 会降 —— 一条断言同时钉住两者。
        assert_eq!(
            reg.snapshot(),
            vec![(Entrypoint::Http, "0.0.0.0:80".into(), 1, 0)]
        );
    }

    #[test]
    fn 同一个视图上不同的_listen_分成两格() {
        // ⚠ ⚠ 这一条守的是**方案选型的全部理由**：一个 `Listeners` 可以有多个监听
        //   地址，而它们共用同一个视图 ⇒ 视图必须按 `listen` 分格，
        //   ⛔ 不能「一个视图一个格子」。少了这一条，那个错法在今天的配置上全绿。
        let reg = ConnRegistry::new();
        let v = reg.view(Entrypoint::L4Tcp, "0.0.0.0:1");
        reg.view(Entrypoint::L4Tcp, "0.0.0.0:2");
        v.enter("0.0.0.0:1");
        v.enter("0.0.0.0:2");
        v.enter("0.0.0.0:2");
        let s = reg.snapshot();
        assert_eq!(s[0], (Entrypoint::L4Tcp, "0.0.0.0:1".into(), 1, 1));
        assert_eq!(s[1], (Entrypoint::L4Tcp, "0.0.0.0:2".into(), 2, 2));
    }

    #[test]
    fn 没声明过的_listen_记进_undeclared_而不是丢掉() {
        // ★ 丢掉的话，一个「接线漏了一个监听器」的缺陷会表现为**指标上什么都没有** ——
        //   而那与「那个端口没人连」长得一模一样。⇒ 记成一个撞不上真地址的记号，
        //   与 G118 的 `<other>` / G121 的 `<none>` / G127 的 `<unknown>` 同一族写法。
        let reg = ConnRegistry::new();
        let v = reg.view(Entrypoint::Http, "0.0.0.0:80");
        v.enter("0.0.0.0:999");
        let s = reg.snapshot();
        assert!(
            s.iter()
                .any(|(_, l, t, a)| &**l == UNDECLARED && *t == 1 && *a == 1),
            "没声明过的 listen 应当记进 <undeclared> 那一格，实际拿到 {s:?}"
        );
    }

    #[test]
    fn entrypoint_的标签值逐个钉住() {
        // ⚠ 闭集：加一种入口时 `as_str` 的 `match` 编不过，而不是静默多出一个值。
        assert_eq!(Entrypoint::Http.as_str(), "http");
        assert_eq!(Entrypoint::Admin.as_str(), "admin");
        assert_eq!(Entrypoint::Quic.as_str(), "quic");
        assert_eq!(Entrypoint::L4Tcp.as_str(), "l4_tcp");
        assert_eq!(Entrypoint::L4Udp.as_str(), "l4_udp");
    }

    #[test]
    fn set_active_是覆盖不是累加() {
        // ★ ★ UDP 那一格是从 `sessions.len()` **派生**的 ⇒ 它必须是「设成这个数」，
        //   ⛔ 不是「加上这个数」。累加的话它只涨不降 —— 而那正是「不用 +1/-1」
        //   要躲开的东西，写成累加等于把躲开的坑原样挖回来。
        let reg = ConnRegistry::new();
        let b = reg.bind(Entrypoint::L4Udp, "0.0.0.0:53");
        b.set_active(3);
        b.set_active(1);
        assert_eq!(reg.snapshot()[0].3, 1);
    }

    #[test]
    fn bump_total_只动_total_不动_active() {
        // ★ UDP 的 total 是事件点（新建会话那一处），而 active 是派生的 ——
        //   两者有意走不同的路，这一条把「别顺手把 active 也加了」钉住。
        let reg = ConnRegistry::new();
        let b = reg.bind(Entrypoint::L4Udp, "0.0.0.0:53");
        b.bump_total();
        b.bump_total();
        let s = reg.snapshot();
        assert_eq!(s[0].2, 2, "total 该涨 2");
        assert_eq!(s[0].3, 0, "active 一点都不该动");
    }
}
