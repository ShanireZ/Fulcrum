//! HTTP/3 入口（**M2 批 J**）—— QUIC 传输层 + `quiche::h3` 语义层。
//!
//! | 决策 | 内容 |
//! |---|---|
//! | **G103** | HTTP/3 取 `quiche`（传输层与语义层在同一个 crate 里）|
//! | **G104** | TLS 统一 BoringSSL；两个入口共用**同一个** `select_certificate_callback` |
//! | **G105** | 语义层用 `quiche::h3`，**不自研**（手写 QPACK 违反安全基线第 5 条）|
//! | **G109** | 升级窗口内的连接归属 = **按 DCID 跨进程转交**（见 [`gen_id`]）|
//! | **G110** | HTTP/3 跟 `tls` 自动开 + 发 `Alt-Svc`（✅ **已接线**）|
//!
//! # 模块分工
//!
//! - [`gen_id`]：代标识、SCID 的形状、数据报归属判定。
//! - [`retry`]：地址验证的 token（抗放大，同时保证「DCID 是我们选的」）。
//! - [`h3_session`]：**h3 → 现有执行链的那座桥**（实现 `SessionCustom`）。
//! - [`listener`]：自建 `Service` + 参与 socket 移交 + Retry / 版本协商 / 归属分发。
//! - [`h3_conn`]：`quiche::h3` 事件循环。
//! - [`relay`]：换代时的**跨进程转交通道**（unix datagram，路径由 `gen_id` 推导，
//!   两代之间不需要任何握手；通道**单向**）。
//!
//! ★ ★ 端到端判据 `tests/h3/run.sh` 的客户端是 **curl 的 OpenSSL-QUIC 栈**，
//! 与本模块用的 quiche 没有一行共同代码 ⇒ 那是唯一的**互操作**判据。
//! ⚠ `#[cfg(test)]` 里那两条走真 UDP 的用的是 quiche 自己的客户端，
//! **两边一起理解错时它们会一起绿**。
//!
//! ⚠ ⚠ ★ **一句不再成立的警告比没有警告更糟** —— 「批 J 不提供换代零中断」曾在
//! [`gen_id`] 与 [`listener`] 里各写了一遍，批 K 接上转交之后那两处是**一起**改的。

pub mod gen_id;
pub mod h3_conn;
pub mod h3_session;
pub mod listener;
pub mod relay;
pub mod retry;
