# 枢衡 Fulcrum（fulcrum）— 提示词

> 本文件是当前采用那套视觉资产的提示词与合成规范，`assets/brand/` 里的成品按它生成。
> 换图之前先读第 5 节的自检清单。

## 项目档案

| 项 | 值 |
| --- | --- |
| code / 显示名 | `fulcrum` / 枢衡 Fulcrum |
| 定位 | Rust 自研**单进程** Web 服务器 + 反向代理 + 负载均衡器：简单时像 Caddy，复杂时不必换软件 |
| 最终文字 | 主标「枢衡」· 副标 `Fulcrum` |
| 字体 | 中文 **HarmonyOS Sans SC Bold**（`Round1/ux/assets/fonts/HarmonyOS_Sans_SC_Bold.woff2`）· 拉丁 **Geist Mono**（`Round1/ux/assets/fonts/GeistMonoVF.woff2`） |
| 色板 | 石墨 `#14161A` · 深石墨 `#1C2026` · 赤铜锈橙 `#C2622B` · 冷钢灰 `#8A93A0` · 暖白 `#EDE9E2` |
| 视觉方向 | ★ **器物特写：支点上的天平横梁**（owner 2026-08-19 拍板） |

## 视觉方向

名字自己就是图：**「枢」是门轴与中枢，「衡」是平衡**；英文 `Fulcrum` 是杠杆与天平的支点，
既是转动的中心，也是平衡的基准。所以主体就一件器物——**一根机加工横梁，水平架在一枚三棱支点上**：
梁上有刻度（衡），梁心是带轴环的可转枢轴（枢），**三股流线从左汇入、一股从右送出**——
那正是产品主张「Caddy + HAProxy + Nginx 收进一个进程」。

★ **横梁必须绝对水平。** 倾斜的天平是「失衡」，与「衡」正好反义；这不是审美偏好，是语义错误。
赤铜锈橙既是 Rust 的语义色，也是真实氧化金属的颜色，不是为了「科技感」挑的。

★ **`crab` 必须写进 Avoid**：Rust 的社区吉祥物 Ferris 是一只螃蟹，生成模型看到 "Rust" 很容易
自己塞一只进来。同理要挡住云图标、机柜、齿轮堆和通用科技蓝——那是市面上所有云原生官网的样子，
不是这个产品的样子。

## 1. Logo 原生生成（随后抠图）

```text
Use case: logo-brand
Asset type: transparent platform logo source
Primary request: an original emblem for a Rust web server that is simultaneously a reverse proxy and a load balancer — a precision balance beam resting on a single fulcrum
Subject: a horizontal machined beam seen straight on, resting on a sharp triangular fulcrum; fine engraved graduation ticks run along the beam's upper edge; at the beam's exact center sits a small round pivot hub with a visible axle collar, reading as a door pivot; three thin lines converge into the left end of the beam and one thicker line leaves the right end; the whole mark is symmetric about the fulcrum and the beam is exactly level
Style/medium: premium industrial instrument insignia, machined steel and oxidized copper, crisp engraved detail, slightly dimensional, strong readable silhouette, no photorealism
Composition/framing: one centered horizontally-oriented emblem, 15% padding, beam plus fulcrum plus pivot hub fused into one coherent mark, readable at 208px
Color palette: graphite #14161A, oxidized copper #C2622B, cool steel gray #8A93A0, tiny warm-white specular highlights; no magenta inside the subject
Scene/backdrop: perfectly flat solid #FF00FF chroma-key background for local removal
Constraints: one uniform backdrop with no shadow, gradient, texture, floor or reflection; no text; no letters; no numbers; no watermark; no frame; the beam must be exactly level, never tilted
Avoid: tilted or unbalanced beam, courthouse justice scales with hanging pans, blindfold, gears, cloud icon, server rack, circuit board, generic tech blue, neon, glassmorphism, crab or any animal mascot, rounded app-icon plate behind the mark
```

抠图后入库 `Fulcrum/assets/brand/fulcrum-logo.png`。

## 2. 徽章背景原生生成（3:1）

```text
Use case: stylized-concept
Asset type: platform badge background, 3:1 full-bleed
Primary request: a wide precision-instrument workbench plate for a single-binary web server, reverse proxy and load balancer
Scene/backdrop: a dark graphite machined surface with a fine engineering grid engraved into it, long shallow channels running left to right like guide rails, faint measurement scales etched along the lower edge, restrained brushed-metal grain and honest wear
Subject: right 40% holds the hero — a heavy machined beam resting exactly level on a triangular fulcrum, its central pivot hub catching a copper rim light, engraved graduation ticks along its top edge; three copper-lit channels converge from the left into the fulcrum and one wider channel continues out to the right edge; left 55% stays dark, flat and low-detail for later typography
Style/medium: premium industrial product illustration with a studio-photography sense of material, machined steel and oxidized copper, precise, restrained, engineering-grade
Composition/framing: wide 3:1, full bleed, right focal point, clean left text-safe zone
Lighting/mood: low raking light from the right, one warm copper edge highlight, disciplined and quiet, no drama
Color palette: graphite #14161A and #1C2026, oxidized copper #C2622B, cool steel gray #8A93A0, sparse warm-white specular
Constraints: no text, no letters, no numbers, no logo, no watermark, no frame, no user interface, no server racks, no cables
Avoid: data center photo, cloud icons, glowing blue network globe, circuit board, neon cyberpunk, gears, crab or animal mascot, hanging scale pans, tilted beam, cluttered left side
```

入库 `Fulcrum/assets/brand/fulcrum-badge-bg.webp`。

## 3. 竖版背景原生生成（4:5）

```text
Use case: stylized-concept
Asset type: full-height vertical key-visual background, 4:5
Primary request: a vertical composition looking down a graphite instrument column toward a level balance beam at its heart
Scene/backdrop: a tall dark graphite structure of stacked machined plates and shallow guide channels; fine engraved engineering scales run down both sides; three copper-lit channels descend from the top edge and converge toward the middle; below the convergence one wider channel continues down to the bottom edge
Subject: in the upper-middle sits the hero — a machined beam resting exactly level on a triangular fulcrum, its central pivot hub catching a copper rim light; the exact center and lower-middle stay calm and dark enough for a logo and two lines of typography to be added later
Style/medium: premium industrial illustration, machined steel and oxidized copper, precise engineering geometry, restrained, tactile
Composition/framing: 4:5 portrait, full bleed; hero inside the central 64% safe area; top and bottom 12% expendable to responsive cropping; strong vertical symmetry
Lighting/mood: cool low key with a single warm copper accent, disciplined, quiet, no drama
Color palette: #14161A, #1C2026, copper #C2622B, steel gray #8A93A0, sparse warm-white specular
Constraints: no text, no letters, no numbers, no logo, no watermark, no frame, no user interface, no server racks, no cables, no people
Avoid: data center photo, cloud icons, glowing blue globe, circuit board, neon, gears, crab or animal mascot, hanging scale pans, tilted beam, a small logo floating in an empty box
```

入库 `Fulcrum/assets/brand/fulcrum-stage-bg.webp`。

## 4. 后期合成

- **徽章**：左侧 1/4 叠 `fulcrum-logo.png`；中间两行排「枢衡」与 `Fulcrum`；右侧只保留横梁 HERO。
  主标 HarmonyOS Sans SC Bold 约 84px、`#EDE9E2`、字距 0.14em；副标 Geist Mono 约 30px、
  `#C2622B`、字距 0.16em。
- **竖版宣传图**：中央叠 Logo，下方排中文与英文，整组垂直居中。
- ★ **「枢衡」是本批唯一的两字主标**。中段名称区若照四字主标的字号排，两个字会在一大片空里发飘：
  **字号取上限（约 84px）＋ 字距拉到 0.12–0.16em**，才与左侧 Logo 和右侧 HERO 三者平衡。
  副标 `Fulcrum` 用等宽体，本身就带字距，再加 0.16em 后正好与主标同宽域收边。
- ★ Logo 是**横向器物**（横梁），与另外三个项目的竖向 Logo 不同：徽章左侧 1/4 那格按**高度**
  给它定尺会太小，应按**宽度**占满该格的 78–84% 定尺，再与中段文字组按包围盒中心垂直居中。

## 5. 自检

- **横梁必须水平**；任何倾斜即判废，不做「转正一点」的挽救。
- **支点、刻度、枢轴三件必须同时成立**；缺一件就退化成「一根棍子架在三角上」，那不是枢衡。
- 三进一出的流线要看得出来——那是「三合一」的产品主张，不是装饰线条。
- 不得出现螃蟹或任何动物吉祥物、云图标、机柜、线缆、齿轮、通用科技蓝。
- 背景里不得出现任何模型生成的字母、数字或伪 UI。
- 「枢衡」二字必须实心；`Fulcrum` 大小写逐字正确（F 大写、其余小写），
  不写成 `FULCRUM` / `fulcrum` / `FulCrum`。
