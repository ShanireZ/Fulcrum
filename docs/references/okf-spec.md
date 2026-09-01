---
type: 外部规范
title: Open Knowledge Format v0.2
description: 本 bundle 遵循的格式规范——plain markdown + YAML frontmatter 的知识 bundle。
resource: https://github.com/GoogleCloudPlatform/open-knowledge-format
tags: [规范, 元信息]
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

**Open Knowledge Format**（OKF）是一个**厂商中立**的开放规范，把「LLM-wiki」这个模式形式化成可移植、可互操作的格式：**plain markdown + YAML frontmatter**，不需要 SDK、没有构建步骤。

本 bundle 用的是 **v0.2**。

# 本 bundle 用到的部分

## 目录与保留文件名

```text
docs/                          ← bundle 根
  index.md                     ← 保留名：目录清单（渐进披露）
  log.md                       ← 保留名：变更历史
  <子目录>/
    index.md
    <概念>.md
```

**概念 ID = bundle 内的文件路径去掉 `.md` 后缀。**

## Frontmatter

| 字段 | 本 bundle 的用法 |
|---|---|
| **`type`** ★ **唯一必填** | `项目治理` / `产品定位` / `架构基线` / `技术基线` / `验证记录` / `外部规范` |
| `title` / `description` | 全部填写 |
| `resource` | 指向真实文件（如 `../../PLAN.md`）或外部 URL |
| `tags` | `必读` / `易错` / `承重墙` / `尚未落地` / `已通过` / `过渡期` 等 |
| `status` | 全部 `stable` |
| `generated` | `{ by: claude-code/opus-5, at: ... }` |
| `sources` | ★ **关键字段** —— 指回 [`PLAN.md`](/references/plan.md) 的章节与决策编号 |

★ **`verified` 一律不填。** 本 bundle 的文本由 agent 生成、尚未经 owner 逐篇复核——OKF 的信任分层里这属于 `unverified`，**如实留空比虚报可信度好**。底层的决策本身是 owner 拍板的，但那是 `sources` 指向的东西，不是这些文本。

## 链接

- **概念之间**用 **bundle 相对路径**（以 `/` 开头，如 `/architecture/tls.md`）—— 文档在子目录间移动时仍然稳定
- **指向 bundle 外**（`PLAN.md` 等）用**普通相对路径**，保证在 GitHub 上可点

链接只表示「有关系」，**具体是什么关系由周围的文字说明**，格式本身不带类型。

# 本 bundle 的两处特殊约定

## 1. 主体不是权威

与成均相同：★ **本 bundle 主体是[指向 `PLAN.md` 的导航图](/governance/source-of-truth.md)。** 一份把结论抄一遍的 bundle 会与权威分岔，所以每篇概念只写「这是什么、和谁相连、最容易在哪做错」，**结论本身去读 `PLAN.md`**。

## 2. ★ 但架构一节是例外，它带真内容

[架构](/architecture/index.md) 的七篇是原 `docs/architecture.md` 拆分而来的**技术基线**，`PLAN.md` 里没有第二份。它与原文件一样**服从 `PLAN.md`**。

★ **这与成均的用法不同**，是 Fulcrum 的本地约定。理由与边界见 [唯一权威与本 bundle 的定位](/governance/source-of-truth.md)。

# 未使用的部分

`verified`、`stale_after`、`usage_window`、`sources[].usage_count`，以及整个 **Attested Computation** 家族（`runtime` / `parameters` / `computation` / `executor` / `attester`）在本 bundle 中未使用。

★ **将来有一处很可能用得上**：[性能验收标准](/verification/performance-bar.md) 要求「基准脚本与原始数据全部公开、可被第三方复现」——**那正是 Attested Computation 的用武之地**。M3 做对拍时值得回头看这一节。
