//! M0 接缝验证 rig 的可复用部分。
//!
//! ★ 这个 lib 是 **为 M1 spike 加的**，本身不改变 M0 的任何行为——
//! 四个模块原封不动，只是从 `main.rs` 的私有 `mod` 提到 crate 根上，
//! 好让 [`m1-systemd`](../../m1-systemd) 复用同一份流量服务（HTTP / 裸 TCP / 裸 UDP）
//! 与同一个探针，而不是复制一份。
//!
//! ★ ★ **为什么坚持复用而不是复制**：本仓库已经在 `tests/m0/lifecycle.sh` 上吃过一次亏——
//! 两个场景的收尾逻辑是复制粘贴的，于是它们分头长歪，同一个缺陷在其中一份里躲过了整整一轮复审。
//! 流量服务这三块是 M1 判「零停机」的判据本身，**判据有两份就等于没有判据**。
//!
//! M1 需要的是**另一个 seam**（systemd 的 MainPID 交接），不是另一套流量。

pub mod fd_inspect;
pub mod http_app;
pub mod raw_tcp;
pub mod raw_udp;
