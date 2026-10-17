//! 連接計數的登記處（**M2 批 O**，G122 的**連接那半**）。
//!
//! 兩個族的全部標籤與計數都在這裡；fork 那一側（**改動 15**）只知道
//! 「這個監聽器上多了一條 / 少了一條」，它不認識標籤、也不知道 Prometheus 存在。
//!
//! # ★ ★ 為什麼是「抓取時問活體」而不是事件點記賬
//!
//! fork 夠不到我們的 [`crate::metrics`] 模組（相依方向是反的）⇒ 只能由它把數加到
//! 我們遞過去的物件上，我們在繪製那一刻現問 —— 與 `upstream_inflight` 同一條路子。
//!
//! # ⚠ ⚠ 一個視圖要按 `listen` 分格，⛔ 不能一格一視圖
//!
//! `Listeners::set_connection_counter` 給**所有**端點設的是同一個實作，而一個
//! `Listeners` 可以有多個監聽位址。⇒ [`ConnView::enter`] 必須按收到的 `listen` 查表。
//! ★ 那正是這一批否掉「fork 只暴露一對原子計數」那個方案的全部理由：
//! 那個方案的粒度只到 `Listeners`，而「一個 Service 只有一個位址」是**今天的巧合**。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Once, RwLock};

use log::warn;
use pingora_core::listeners::{ConnGuard, ConnectionCounter};

/// 串接漏了一個監聽器時那一筆落在哪一格。
///
/// ★ ⛔ **不許丟掉那一筆**：丟掉的話，「串接漏了」表現為**指標上什麼都沒有**，
/// 而那與「那個連接埠沒人連」長得一模一樣。⇒ 記成一個記號，讓它在正文裡看得見。
/// ★ 尖括號形狀隨 G118 的 `<other>` / G121 的 `<none>` / G127 的 `<unknown>`：
/// 尖括號在合法的監聽位址裡不可能出現，撞不上真值。
pub const UNDECLARED: &str = "<undeclared>";

/// 這條連接是從哪個入口進來的（**閉集**，`fulcrum_connections_*` 的 `entrypoint` 標籤）。
///
/// ⚠ ⚠ **⛔ 不叫 `proto`**：`fulcrum_requests_total` 已經有一個 `proto` 標籤，取值是
/// `HTTP/1.1` 那一族。同名不同值域會讓維運跨族寫同一個過濾器時**拿到空集而不回報錯誤**。
/// ★ 本儲存庫對這種「同名純屬巧合」有前例：見 `access_log::Record` 的 `site` / `site_addr`。
///
/// ⚠ 每個監聽器**只屬於一個** entrypoint ⇒ 這兩個族的 series 數 = **監聽器數**，
/// ⛔ 不是 5 × 監聽器數。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Entrypoint {
    /// pingora 的 `ListeningService` 上的資料面（h1/h2）。
    Http,
    /// 同一個 `ListeningService`，但那是管理面的 Unix socket（G14）。
    ///
    /// ★ 單列一格而不是並進 [`Entrypoint::Http`]：跨平面求和之前先按 entrypoint 過濾，
    /// 是抓取端唯一能把兩個平面分開的辦法。
    Admin,
    /// HTTP/3 入口（`quic::listener`，自己的 `recv_from` 循環）。
    Quic,
    /// L4 TCP 透傳（`l4.rs` 自己的 accept 循環）。
    L4Tcp,
    /// L4 UDP 透傳。⚠ 它**沒有連接**，這一格數的是**工作階段**，見 [`ConnRegistry::set_active`]。
    L4Udp,
}

/// `fulcrum_connections_*{entrypoint}` 的**取值閉集**，`metrics.rs` 的基數表判據按它算上界。
///
/// ⚠ ⚠ ⛔ **不是手抄**：單測把 [`Entrypoint::ALL`] 逐個過 [`Entrypoint::as_str`]，
/// 與本常數互為子集；而 `as_str` 那條 match 是窮盡的 ⇒ 加一種入口就編不過。
pub const ENTRYPOINTS: &[&str] = &["http", "admin", "quic", "l4_tcp", "l4_udp"];

impl Entrypoint {
    /// ⚠ ⚠ **這個函式存在的唯一理由是讓編譯器守住 [`Entrypoint::ALL`] 的完整性。**
    /// 加一種入口 ⇒ 這條 match 編不過 ⇒ 人被逼著回來同時改 `ALL`。
    /// ⛔ 別給它加 `_ =>` 備援臂，那樣它就什麼都不守了。
    /// ★ 只靠 [`Entrypoint::as_str`] 的窮盡性是不夠的：那只逼人改 `as_str`，
    /// 而「順手也改 `ALL`」是**一句寫在註解裡的話，沒有門守著**。
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

    /// 全部入口。⚠ 完整性由 [`Entrypoint::穷尽性自证`] 那條窮盡 match 守著。
    pub const ALL: [Entrypoint; 5] = [
        Entrypoint::Http,
        Entrypoint::Admin,
        Entrypoint::Quic,
        Entrypoint::L4Tcp,
        Entrypoint::L4Udp,
    ];

    /// 標籤裡那個詞。
    ///
    /// ⚠ `match` 是**窮盡**的 ⇒ 將來加一種入口時**這裡編不過**，
    /// 而不是靜默地多出一個沒人認得的標籤值。
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

/// 一格的鍵：**入口種類 + 監聽位址原樣**。
///
/// ★ 兩截缺一不可：只按 `listen` 分的話，G110 下同一個連接埠號上的 TCP 與 QUIC
/// 會合併成一條 series；只按 `ep` 分的話，一個 `Listeners` 的多個位址會合並。
type CellKey = (Entrypoint, Box<str>);

type CellMap = BTreeMap<CellKey, Arc<ConnCell>>;

/// 繪製那一刻，一格的讀數：入口 · 監聽位址 · `total` · `active`。
pub type ConnReading = (Entrypoint, Box<str>, u64, i64);

/// 一個監聽器的兩個數。
#[derive(Debug, Default)]
struct ConnCell {
    /// 累計接進來過多少條。**只增**。
    total: AtomicU64,
    /// 此刻有多少條還活著。⚠ 含**還在握手**的那些（`enter` 在握手之前）。
    active: AtomicI64,
}

/// 全部監聽器的連接計數。
///
/// ★ ★ 格子由 [`ConnRegistry::view`] / [`ConnRegistry::guard_for`] 在**串接那一刻**宣告 ——
/// ⛔ **有意不預先枚舉一遍監聽器**：那會是第二條枚舉路徑，而
/// `fulcrum_runtime::Runtime::all_proxy_targets` 的文件正是為這個形狀寫的警告
/// （「⚠ ⚠ **有意不自己再走一遍**……走法分家的表現是某條在某一張清單裡不存在」）。
///
/// ⚠ 寫只發生在啟動期（串接），讀發生在每條連接與每次抓取 ⇒ 讀鎖無競爭。
#[derive(Debug, Default)]
pub struct ConnRegistry {
    cells: RwLock<CellMap>,
}

impl ConnRegistry {
    pub fn new() -> Arc<ConnRegistry> {
        Arc::new(ConnRegistry::default())
    }

    /// 宣告一個格子並把它取出來（冪等）。
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

    /// 取一個格子；沒宣告過就落到 [`UNDECLARED`] 那一格並**喊一次**。
    fn cell(&self, ep: Entrypoint, listen: &str) -> Arc<ConnCell> {
        {
            let map = self.cells.read().unwrap_or_else(|e| e.into_inner());
            if let Some(c) = map.get(&(ep, Box::from(listen))) {
                return c.clone();
            }
        }
        // ⚠ **只喊一次**：這是每條連接都會走的路，不限流的話一次設定錯誤會讓
        //   紀錄本身變成第二個故障。
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

    /// 給 fork 那一側用的視圖：**宣告這一格**，並回傳一個綁定了 `ep` 的計數器。
    ///
    /// ★ ★ ★ `entrypoint` 由視圖帶、`listen` 由 fork 遞 ⇒ **貼錯標籤在結構上做不到**。
    pub fn view(self: &Arc<Self>, ep: Entrypoint, listen: &str) -> Arc<dyn ConnectionCounter> {
        self.declare(ep, listen);
        Arc::new(ConnView {
            reg: self.clone(),
            ep,
        })
    }

    /// 給我們自己那三條循環用：**宣告這一格**，並回傳一個已經綁好 `(ep, listen)` 的句柄。
    ///
    /// ★ ★ 綁一次、用很多次 ⇒ 「宣告的那一格」與「寫入的那一格」**在結構上是同一個**，
    /// ⛔ 不會出現「宣告了 A 卻往 B 上寫」那種在今天的設定上全綠的錯法。
    pub fn bind(self: &Arc<Self>, ep: Entrypoint, listen: &str) -> BoundConn {
        BoundConn {
            counter: self.view(ep, listen),
            cell: self.declare(ep, listen),
            listen: Arc::from(listen),
        }
    }

    /// 繪製那一刻的讀數，**按鍵排序**（`BTreeMap` 天然如此）。
    ///
    /// ⚠ 順序確定這件事沒有任何斷言會因它變紅，代價是**兩次抓取的 diff 裡全是噪聲** ——
    /// 而人正是靠 diff 看指標出沒出新東西的（`metrics` 模組頂部那條紀律）。
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

/// 已經綁好 `(entrypoint, listen)` 的一格 —— 我們自己那三條 accept 循環拿的是它。
///
/// ★ 它與 fork 那條路**共用同一個 [`ConnGuard`]** ⇒ 減一仍然只有那一個呼叫點。
#[derive(Clone)]
pub struct BoundConn {
    counter: Arc<dyn ConnectionCounter>,
    cell: Arc<ConnCell>,
    listen: Arc<str>,
}

impl BoundConn {
    /// 拿一個守衛：建構即 `+1`，`Drop` 即 `-1`。
    ///
    /// ⚠ ⚠ 呼叫方**必須把它綁到一個有名字的變數上**（`let _g = …;`）——
    /// 寫成裸 `let _ = …;` 會當場 drop，於是 `active` 恆為 0 而 `total` 照漲，
    /// **且不會有任何東西紅**。
    pub fn guard(&self) -> ConnGuard {
        ConnGuard::new(self.counter.clone(), self.listen.clone())
    }

    /// 把 `active` **設成**這個數（⛔ 不是加上）。★ 只服務 L4 UDP 的工作階段表派生。
    pub fn set_active(&self, n: usize) {
        self.cell.active.store(n as i64, Ordering::Relaxed);
    }

    /// 只把 `total` 加一（⛔ 不動 `active`）。★ 只服務 L4 UDP 的「新建了一條工作階段」。
    pub fn bump_total(&self) {
        self.cell.total.fetch_add(1, Ordering::Relaxed);
    }
}

/// 綁定了一個 `entrypoint` 的計數器視圖 —— fork 那一側拿到的就是它。
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
        // ★ 「建成就出樣本」：宣告完立刻就該有一條 0/0，而不是等第一條連接。
        //   ⇒ 「監聽器在、只是沒人連」與「設定里根本沒有它」因此分得開。
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
        // ★ ★ counter 只增、gauge 會降 —— 一條斷言同時釘住兩者。
        assert_eq!(
            reg.snapshot(),
            vec![(Entrypoint::Http, "0.0.0.0:80".into(), 1, 0)]
        );
    }

    #[test]
    fn 同一个视图上不同的_listen_分成两格() {
        // ⚠ ⚠ 這一條守的是**方案選型的全部理由**：一個 `Listeners` 可以有多個監聽
        //   位址，而它們共用同一個視圖 ⇒ 視圖必須按 `listen` 分格，
        //   ⛔ 不能「一個視圖一個格子」。少了這一條，那個錯法在今天的設定上全綠。
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
        // ★ 丟掉的話，一個「串接漏了一個監聽器」的缺陷會表現為**指標上什麼都沒有** ——
        //   而那與「那個連接埠沒人連」長得一模一樣。⇒ 記成一個撞不上真位址的記號，
        //   與 G118 的 `<other>` / G121 的 `<none>` / G127 的 `<unknown>` 同一族寫法。
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
        // ⚠ 閉集：加一種入口時 `as_str` 的 `match` 編不過，而不是靜默多出一個值。
        assert_eq!(Entrypoint::Http.as_str(), "http");
        assert_eq!(Entrypoint::Admin.as_str(), "admin");
        assert_eq!(Entrypoint::Quic.as_str(), "quic");
        assert_eq!(Entrypoint::L4Tcp.as_str(), "l4_tcp");
        assert_eq!(Entrypoint::L4Udp.as_str(), "l4_udp");
    }

    #[test]
    fn set_active_是覆盖不是累加() {
        // ★ ★ UDP 那一格是從 `sessions.len()` **派生**的 ⇒ 它必須是「設成這個數」，
        //   ⛔ 不是「加上這個數」。累加的話它只漲不降 —— 而那正是「不用 +1/-1」
        //   要躲開的東西，寫成累加等於把躲開的坑原樣挖回來。
        let reg = ConnRegistry::new();
        let b = reg.bind(Entrypoint::L4Udp, "0.0.0.0:53");
        b.set_active(3);
        b.set_active(1);
        assert_eq!(reg.snapshot()[0].3, 1);
    }

    #[test]
    fn bump_total_只动_total_不动_active() {
        // ★ UDP 的 total 是事件點（新建工作階段那一處），而 active 是派生的 ——
        //   兩者有意走不同的路，這一條把「別順手把 active 也加了」釘住。
        let reg = ConnRegistry::new();
        let b = reg.bind(Entrypoint::L4Udp, "0.0.0.0:53");
        b.bump_total();
        b.bump_total();
        let s = reg.snapshot();
        assert_eq!(s[0].2, 2, "total 该涨 2");
        assert_eq!(s[0].3, 0, "active 一点都不该动");
    }
}
