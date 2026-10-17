//! HTTP/3 入口（**M2 批 J**）—— QUIC 傳輸層 + `quiche::h3` 語義層。
//!
//! | 決策 | 內容 |
//! |---|---|
//! | **G103** | HTTP/3 取 `quiche`（傳輸層與語義層在同一個 crate 裡）|
//! | **G104** | TLS 統一 BoringSSL；兩個入口共用**同一個** `select_certificate_callback` |
//! | **G105** | 語義層用 `quiche::h3`，**不自研**（手寫 QPACK 違反安全基線第 5 條）|
//! | **G109** | 升級視窗內的連接歸屬 = **按 DCID 跨處理程序轉交**（見 [`gen_id`]）|
//! | **G110** | HTTP/3 跟 `tls` 自動開 + 發 `Alt-Svc`（✅ **已串接**）|
//!
//! # 模組分工
//!
//! - [`gen_id`]：代標識、SCID 的形狀、資料報歸屬判定。
//! - [`retry`]：位址驗證的 token（抗放大，同時保證「DCID 是我們選的」）。
//! - [`h3_session`]：**h3 → 現有執行鏈的那座橋**（實作 `SessionCustom`）。
//! - [`listener`]：自建 `Service` + 參與 socket 移交 + Retry / 版本協商 / 歸屬散布。
//! - [`h3_conn`]：`quiche::h3` 事件循環。
//! - [`relay`]：換代時的**跨處理程序轉交通道**（unix datagram，路徑由 `gen_id` 推導，
//!   兩代之間不需要任何握手；通道**單向**）。
//!
//! ★ ★ 端到端判據 `tests/h3/run.sh` 的用戶端是 **curl 的 OpenSSL-QUIC 棧**，
//! 與本模組用的 quiche 沒有一行共同程式碼 ⇒ 那是唯一的**互操作**判據。
//! ⚠ `#[cfg(test)]` 裡那兩條走真 UDP 的用的是 quiche 自己的用戶端，
//! **兩邊一起理解錯時它們會一起綠**。
//!
//! ⚠ ⚠ ★ **一句不再成立的警告比沒有警告更糟** —— 「批 J 不提供換代零中斷」曾在
//! [`gen_id`] 與 [`listener`] 裡各寫了一遍，批 K 接上轉交之後那兩處是**一起**改的。

pub mod gen_id;
pub mod h3_conn;
pub mod h3_session;
pub mod listener;
pub mod relay;
pub mod retry;
