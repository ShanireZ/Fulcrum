//! 请求视图：路由决策需要知道的全部东西，**和 Pingora 无关**。
//!
//! ★ ★ 这一层刻意不引用 `pingora-core`。理由不是洁癖：
//! 路由语义（哪个站点、哪条匹配器、跑到第几步、终结成什么）是本项目**最需要被测**的逻辑，
//! 而它一旦长在 `HttpServerApp` 里，测一条路由就要起监听、发真流量、解析响应。
//! 那样的测试贵、慢、而且**红了指不到具体哪一条规则**。
//!
//! 代价是数据面那一侧要做一次转换（`&RequestHeader` → `RequestCtx`），
//! 而它是零拷贝的：全部字段都是借用。

use std::net::IpAddr;

/// 请求头查询。数据面给一个 Pingora 的实现，测试给 [`HeaderList`]。
pub trait Headers {
    /// 取一个头的值。★ 名字**大小写不敏感**（HTTP 的规定，不是可选项）。
    fn get(&self, name: &str) -> Option<&str>;
}

/// 一组 `(名字, 值)`。
///
/// ★ 为什么要包一层而不直接 `impl Headers for [(&str, &str)]`：
/// `&[T]` 本身已经是**胖指针**，没法再变成 `&dyn Headers`（那也是胖指针，
/// 但要求数据指针是细的）。包一个 sized 的壳是唯一的走法。
/// ⚠ 顺带避开另一个坑：切片自带一个 `get`，与 trait 里的同名方法**在切片上会赢**，
/// 于是 `h.get("x")` 会静静地变成按下标取元素。
pub struct HeaderList<'a>(pub &'a [(&'a str, &'a str)]);

impl Headers for HeaderList<'_> {
    fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| *v)
    }
}

/// 路由决策要用到的请求侧事实。
#[derive(Clone, Copy)]
pub struct RequestCtx<'a> {
    /// Host（**不含端口**）。空串表示请求没带 Host。
    pub host: &'a str,
    /// 请求打到的**本地**端口——站点索引按它来分，不是按 Host 里写的那个。
    pub port: u16,
    pub scheme: &'a str,
    pub method: &'a str,
    /// 路径，**不含查询串**。
    pub path: &'a str,
    /// 查询串，**不含 `?`**。
    pub query: &'a str,
    /// ⚠ **`+ Sync` 不是装饰**：数据面在 `await` 之间拿着这个 ctx，
    /// 而 Pingora 的 `HttpServerApp` 要求 future 是 `Send`。
    /// 少了这条界，报出来的是一句「future cannot be sent between threads safely」，
    /// **指向 trait 定义而不是这一行**——查起来很绕。
    pub headers: &'a (dyn Headers + Sync),
    /// ★ ★ **客户端 IP 的取值口径见安全基线：不能取 XFF 最左项**（客户端可伪造）。
    /// 本层只负责用它，取值是数据面的责任——所以这里是一个已经定好的值，
    /// 而不是一个「从头里现算」的函数。
    pub remote_ip: Option<IpAddr>,
    pub remote_port: u16,
}

impl RequestCtx<'_> {
    /// `{uri}`：路径 + 查询串。
    pub fn uri(&self) -> String {
        if self.query.is_empty() {
            self.path.to_string()
        } else {
            format!("{}?{}", self.path, self.query)
        }
    }
}

/// 响应侧的事实。只有到了写响应那一刻才知道，所以它是**后来才有**的。
///
/// ★ 这就是为什么 `header` 那些取值在路由阶段**不展开**、只带着模板走：
/// `{status}` 与 `{upstream}` 在路由阶段还不存在，提前展开只能得到空串——
/// 而空串正是 G61 点名要避免的那种无声失败。
#[derive(Default)]
pub struct ResponseCtx<'a> {
    pub status: Option<u16>,
    /// 本次选中的上游地址。
    pub upstream: Option<&'a str>,
    pub headers: Option<&'a (dyn Headers + Sync)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 头名大小写不敏感() {
        let h = HeaderList(&[("Content-Type", "text/html")]);
        assert_eq!(h.get("content-type"), Some("text/html"));
        assert_eq!(h.get("CONTENT-TYPE"), Some("text/html"));
        assert_eq!(h.get("x-nope"), None);
    }

    #[test]
    fn uri_拼装() {
        let h = HeaderList(&[]);
        let ctx = RequestCtx {
            host: "a.com",
            port: 443,
            scheme: "https",
            method: "GET",
            path: "/x",
            query: "",
            headers: &h,
            remote_ip: None,
            remote_port: 0,
        };
        assert_eq!(ctx.uri(), "/x");
        let ctx2 = RequestCtx {
            query: "a=1",
            ..ctx
        };
        assert_eq!(ctx2.uri(), "/x?a=1");
    }
}
