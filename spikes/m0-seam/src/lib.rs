//! M0 接縫驗證 rig 的可重複使用部分。
//!
//! ★ 這個 lib 是 **為 M1 spike 加的**，本身不改變 M0 的任何行為——
//! 四個模組原封不動，只是從 `main.rs` 的私有 `mod` 提到 crate 根上，
//! 好讓 [`m1-systemd`](../../m1-systemd) 重複使用同一份流量服務（HTTP / 裸 TCP / 裸 UDP）
//! 與同一個探針，而不是複製一份。
//!
//! ★ ★ **為什麼堅持重複使用而不是複製**：本儲存庫已經在 `tests/m0/lifecycle.sh` 上吃過一次虧——
//! 兩個場景的收尾邏輯是複製粘貼的，於是它們分頭長歪，同一個缺陷在其中一份裡躲過了整整一輪複審。
//! 流量服務這三塊是 M1 判「零停機」的判據本身，**判據有兩份就等於沒有判據**。
//!
//! M1 需要的是**另一個 seam**（systemd 的 MainPID 交接），不是另一套流量。

pub mod fd_inspect;
pub mod http_app;
pub mod raw_tcp;
pub mod raw_udp;
