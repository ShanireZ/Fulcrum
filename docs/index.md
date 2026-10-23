---
okf_version: "0.2"
---

# 樞衡 Fulcrum · 知識 bundle

按 [Open Knowledge Format v0.2](/references/okf-spec.md) 組織的知識 bundle。

★ **[`PLAN.md`](../PLAN.md) 是唯一權威。** 本 bundle 的絕大部分是指向它的**導覽圖**——每篇概念用 `sources` 宣告出處（章節號 + 決策編號），正文只做定位與串聯，不復述結論。與 `PLAN.md` 衝突時一律以 `PLAN.md` 為準，並且要把這裡改掉。

★ **一處例外必須說清**：[架構](/architecture/index.md) 一節**帶真內容**，它是原 `docs/architecture.md` 拆分而來的**技術基線**，不是 `PLAN.md` 的複述。它與原檔案一樣**服從 `PLAN.md`**。詳見 [唯一權威與本 bundle 的定位](/governance/source-of-truth.md)。

## [治理](/governance/index.md)

* [唯一權威與本 bundle 的定位](/governance/source-of-truth.md) - 誰說了算、架構一節為什麼是例外、發現衝突怎麼辦
* [決策紀錄](/governance/decision-log.md) - §10 的導覽：哪條管哪件事、★ owner 推翻 AI 建議的那些決定
* [待定清單](/governance/open-questions.md) - 仍然開著的那些。★ **號碼有意不抄在這裡**：那張表由門釘成與 `PLAN.md` 待定清單逐字相等，而**抄出來的第二份沒有任何門守著** —— 這一行此前就同時錯了兩頭（D26 已由 G118 結案卻還列著、D29 開著卻沒列），而它自洽地躺了很多輪
* [實施路線](/governance/roadmap.md) - M0–M4 五個里程碑與各自的退出條件；判據只有「能不能真用」
* [工作方式](/governance/working-agreement.md) - 不可回頭的硬約束、不得繞過的三條、效能宣告的紀律

## [產品](/product/index.md)

* [定位](/product/positioning.md) - 一個處理程序覆蓋三件事；三家的長處與短處；★ 競爭位置必須誠實記錄
* [首版範圍](/product/scope.md) - 自研四塊、明確不做七項、回落層兩家
* [設計原則](/product/design-principles.md) - 七條；★ 第七條「回落是腳手架」的成功標誌是被刪除

## [架構（技術基線）](/architecture/index.md)

* [處理程序與元件邊界](/architecture/process-model.md) - 樞衡是**一個處理程序**，既是控制面也是資料面
* [設定分層](/architecture/configuration.md) - DSL → 結構化設定 → 執行環境物件圖；★ 不做反向生成
* [DSL 指令集參考](/architecture/dsl-reference.md) - ★ M1 的完整指令清單；執行順序表是**公開契約**
* [管理面](/architecture/control-plane.md) - 全量 load + Runtime 增量雙通道；★ 基線 ⊕ 臨時覆蓋層
* [TLS 與自動 HTTPS](/architecture/tls.md) - ⚠ ★ **此後後端是 BoringSSL 不是 rustls**（G104 推翻 §5.1 第 1 條）；不留雙路徑、On-Demand 強制存取資格
* [資料路徑](/architecture/data-path.md) - 三個入口一個路由層；靜態與快取；上游發現與健康
* [回落層](/architecture/fallback.md) - 過渡期頂班，1.0 時歸零；四條防止它長成第二個產品的約束
* [觀測](/architecture/observability.md) - 指標 + 結構化紀錄 + Runtime 即時 stats

## [技術與維運](/platform/index.md)

* [技術棧](/platform/tech-stack.md) - Rust + Pingora 0.8.1 + **quiche** + **BoringSSL**（★ 由 G103/G104 換掉 quinn/h3 與 rustls）；單靜態二進位
* [相依策略](/platform/dependency-policy.md) - G29：追新 + 不設上界 + `Cargo.lock` 入庫 + **24 小時安全懷疑期**
* [供應鏈現狀](/platform/supply-chain.md) - ★ **必讀**：脫字號需求自帶上界、pingora 的十條天花板、4 條公告與可達性分析
* [HTTP/3 庫選型事實表](/platform/http3-libraries.md) - D11 拍板前的事實，**不給推薦項**；★ quiche 用 BoringSSL 不是 rustls、s2n-quic 根本不帶 HTTP/3
* [建置與驗證](/platform/build-and-test.md) - 一切在 Docker 裡跑（G26）；宿主機只需 Docker
* [部署](/platform/deploy.md) - systemd unit 照抄形狀；★ `TimeoutStopSec` 怎麼算；變更 / 回復 / 換二進位三條路
* [安全基線](/platform/security-baseline.md) - 管理面預設不出機器；不自己實作 TLS/HTTP2/HPACK/QUIC

## [驗證](/verification/index.md)

* [M0 接縫驗證](/verification/m0-seam.md) - ✅ **已通過**：自建 TCP/UDP 監聽器參與了 Pingora 的 socket 移交
* [效能驗收標準](/verification/performance-bar.md) - G19：三家全對拍、逐類設門、指令碼與原始資料全公開
* [musl + BoringSSL 靜態連結](/verification/musl-boringssl.md) - ✅ **已通過**（G103/G104 的未驗前置）；★ ★ ★ 而卡住它的是**建置宿主**，不是 musl 也不是 BoringSSL
* [尚未驗證的接縫](/verification/open-seams.md) - 還剩哪些沒被程式碼證明；★ 升級視窗內 UDP 資料報分流

## [外部材料](/references/index.md)

* [OKF 規範](/references/okf-spec.md) - 本 bundle 遵循的格式
* [PLAN.md](/references/plan.md) - 唯一權威的入口與章節地圖
* [AGENTS.md](/references/agents.md) - agent 的專案級約束
* [上游技術入口](/references/upstream.md) - Pingora / quiche / BoringSSL 與三家的官方文件

## Agent 約定

`agents/` 存的是**工具怎麼用這個儲存庫**（issue 落在哪、triage 標籤怎麼對應、領域文件去哪找），不是產品知識；工作區準則與 OKF 維護合同只在工作區根各存一份，由索引指過去。專案級 agent 約束的權威始終是 [`AGENTS.md`](../AGENTS.md)，本目錄只補它引到的那幾份細則。

* [Agent 設定索引](/agents/index.md) - 四份細則的入口：issue tracker 與 Wayfinder 約定、triage 標籤映射、領域文件佈局（`GLOSSARY.md` / ADR，不存在就靜默跳過）、在地化時引號裡的 owner 原話只轉字形，外加工作區準則與本 bundle 遵循的 OKF v0.2 維護合同的指針。
