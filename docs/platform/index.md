---
type: 技術基線
title: 技術與維運
description: 選了什麼、依賴怎麼管、供應鏈現狀如何、怎麼建置、安全底線在哪。
resource: ../../Cargo.toml
tags: [技術, 索引]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan
    resource: /references/plan.md
    title: PLAN.md §5 技術底盤、§10 G26 與 G29
---

* [技術棧](/platform/tech-stack.md) - Rust + Pingora 0.8.1 + **quiche** + **BoringSSL**（G103/G104）；★ 兩處決策與清單之間的差距
* [依賴策略](/platform/dependency-policy.md) - G29；★ **絕不裸跑 `cargo update`**；★ dep-check 看不見被上游上界卡住的東西
* [供應鏈現狀](/platform/supply-chain.md) - ★ **必讀**：脫字號需求自帶上界、pingora 的十條天花板、4 條公告與可達性分析
* [HTTP/3 庫選型事實表](/platform/http3-libraries.md) - D11 拍板前擺出來的事實，**不給推薦項**；★ quiche 用 BoringSSL 不是 rustls、s2n-quic 根本不帶 HTTP/3、`h3` 是「沒發版」不是「沒人開發」
* [向上遊提 PR 的流程清單](/platform/upstream-pr.md) - G32；★ **必須先開 issue**；★ MSRV 1.85.0 正好卡住 `lru 0.18.2`；★ 上游改面比樞衡 fork 大得多
* [建置與驗證](/platform/build-and-test.md) - 一切在 Docker 裡跑；★ 三個 Windows 宿主機上的坑
* [宿主機陷阱、門禁紀律、場景開關與連接埠表](/platform/host-and-gate-traps.md) - 從 `AGENTS.md` 拆出來的細則，**仍然逐條是硬規矩**；★ 綠了也可能什麼都不說明的五種形態；★ 加場景要同時改三處；★ 連接埠分配表
* [部署](/platform/deploy.md) - G31/G33/G37/G78 的完成實作形狀；★ **`TimeoutStopSec` 小於停機預算＝硬殺連接**；★ 換二進位必須 rename
* [安全基線](/platform/security-baseline.md) - 七條；★ 第 5 條（不手寫協定棧）是整個安全論證的支點
