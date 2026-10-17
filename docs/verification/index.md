---
type: 驗證記錄
title: 驗證
description: 已經跑過什麼、達標線在哪、還有哪些技術假設沒被程式碼證明。
resource: ../../tests
tags: [驗證, 索引]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan
    resource: /references/plan.md
    title: PLAN.md §7 M0、§8 效能驗收標準、§9 主要風險
---

★ **本節與 bundle 其他部分不同：它記的是實際發生過的測試與觀測到的輸出，那是事實不是轉述。** 結論可以被後續實驗推翻，但「那一次跑出了什麼」不會變。

* [M0 接縫驗證](/verification/m0-seam.md) - ✅ **已通過**（三次獨立執行，三類流量零中斷）；★ 順帶給 D2 與 D11 各留下一條結論
* [M1 spike #1 · systemd 下的零停機升級](/verification/m1-systemd.md) - ✅ **已通過，但推翻了 G31 的一半**：撐住 unit 的是 `ExitType=cgroup`，而「搶過 MainPID」會**悄悄弄丟優雅停機**；★ 順帶撈到一條 M0 不可能覆蓋的新缺陷（移交來的監聽 fd 沒有 `CLOEXEC`）
* [musl + BoringSSL 靜態連結](/verification/musl-boringssl.md) - ✅ **已通過**（G103/G104 的未驗前置，2026-08-25）：完全靜態的 musl 二進位在 `FROM scratch` 裡跑完真的 QUIC 握手，`set_select_certificate_callback` 真的被呼叫；★ ★ ★ **而卡住它的從來不是 musl 也不是 BoringSSL，是建置宿主** —— bindgen 要 `dlopen`，而 Alpine 上 build script 預設是靜態的，回報錯誤文字里三個詞卻全指向 BoringSSL
* [rustls 接縫 · 打開 G6 的那個 feature 會發生什麼](/verification/m1-rustls-seam.md) - ✅ **已通過**（G41/G42）；★ ★ ★ **這條接縫從來沒被編譯、測試或稽核過**——三道門結構上都夠不到它，打開後一次進來 **42 個從未受審的 crate**，其中 `aws-lc-rs` 被無謂編進產物；★ 順帶把迴歸網的已知失敗名單從 3 條**縮到** 2 條（查根因，而不是再登記一條）
* [效能驗收標準](/verification/performance-bar.md) - ★ **尚未完成實作**，是 M3 的內容。逐類設門、不劣於該類最強者 10%
* [尚未驗證的接縫](/verification/open-seams.md) - ★ M0 解除了最大的一條，但**衍生出一條新的**：升級視窗內 QUIC 連接歸屬
