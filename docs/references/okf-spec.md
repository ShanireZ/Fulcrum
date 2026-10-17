---
type: 外部規範
title: Open Knowledge Format v0.2
description: 本 bundle 遵循的格式規範——plain markdown + YAML frontmatter 的知識 bundle。
resource: https://github.com/GoogleCloudPlatform/open-knowledge-format
tags: [規範, 元資訊]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: spec
    resource: https://github.com/GoogleCloudPlatform/open-knowledge-format/blob/main/SPEC.md
    title: OKF SPEC.md v0.2
  - id: blog
    resource: https://cloud.google.com/blog/products/data-analytics/how-the-open-knowledge-format-can-improve-data-sharing/
    title: How the Open Knowledge Format can improve data sharing（Google Cloud Blog）
---

**Open Knowledge Format**（OKF）是一個**廠商中立**的開放規範，把「LLM-wiki」這個模式形式化成可移植、可互操作的格式：**plain markdown + YAML frontmatter**，不需要 SDK、沒有建置步驟。

本 bundle 用的是 **v0.2**。

# 本 bundle 用到的部分

## 目錄與保留檔案名

```text
docs/                          ← bundle 根
  index.md                     ← 保留名：目录清单（渐进披露）
  log.md                       ← 保留名：变更历史
  <子目录>/
    index.md
    <概念>.md
```

**概念 ID = bundle 內的檔案路徑去掉 `.md` 後綴。**

## Frontmatter

| 欄位 | 本 bundle 的用法 |
|---|---|
| **`type`** ★ **唯一必填** | `项目治理` / `产品定位` / `架构基线` / `技术基线` / `验证记录` / `外部规范` |
| `title` / `description` | 全部填寫 |
| `resource` | 指向真實檔案（如 `../../PLAN.md`）或外部 URL |
| `tags` | `必读` / `易错` / `承重墙` / `尚未落地` / `已通过` / `过渡期` 等 |
| `status` | 全部 `stable` |
| `generated` | `{ by: claude-code/opus-5, at: ... }` |
| `sources` | ★ **關鍵欄位** —— 指回 [`PLAN.md`](/references/plan.md) 的章節與決策編號 |

★ **`verified` 一律不填。** 本 bundle 的文字由 agent 生成、尚未經 owner 逐篇複核——OKF 的信任分層裡這屬於 `unverified`，**如實留空比虛報可信度好**。底層的決策本身是 owner 拍板的，但那是 `sources` 指向的東西，不是這些文字。

## 連結

- **概念之間**用 **bundle 相對路徑**（以 `/` 開頭，如 `/architecture/tls.md`）—— 文件在子目錄間移動時仍然穩定
- **指向 bundle 外**（`PLAN.md` 等）用**普通相對路徑**，保證在 GitHub 上可點

連結只表示「有關係」，**具體是什麼關係由周圍的文字說明**，格式本身不帶類型。

# 本 bundle 的兩處特殊約定

## 1. 主體不是權威

與成均相同：★ **本 bundle 主體是[指向 `PLAN.md` 的導覽圖](/governance/source-of-truth.md)。** 一份把結論抄一遍的 bundle 會與權威分岔，所以每篇概念只寫「這是什麼、和誰相連、最容易在哪做錯」，**結論本身去讀 `PLAN.md`**。

## 2. ★ 但架構一節是例外，它帶真內容

[架構](/architecture/index.md) 的七篇是原 `docs/architecture.md` 拆分而來的**技術基線**，`PLAN.md` 裡沒有第二份。它與原檔案一樣**服從 `PLAN.md`**。

★ **這與成均的用法不同**，是 Fulcrum 的本機約定。理由與邊界見 [唯一權威與本 bundle 的定位](/governance/source-of-truth.md)。

# 未使用的部分

`verified`、`stale_after`、`usage_window`、`sources[].usage_count`，以及整個 **Attested Computation** 家族（`runtime` / `parameters` / `computation` / `executor` / `attester`）在本 bundle 中未使用。

★ **將來有一處很可能用得上**：[效能驗收標準](/verification/performance-bar.md) 要求「基準指令碼與原始資料全部公開、可被第三方重現」——**那正是 Attested Computation 的用武之地**。M3 做對拍時值得回頭看這一節。
