# 樞衡 Fulcrum（fulcrum）— 提示詞

> 本檔案是當前採用那套視覺資產的提示詞與合成規範，`assets/brand/` 裡的成品按它生成。
> 換圖之前先讀第 5 節的自檢清單。

## 專案檔案

| 項 | 值 |
| --- | --- |
| code / 顯示名 | `fulcrum` / 樞衡 Fulcrum |
| 定位 | Rust 自研**單處理程序** Web 伺服器 + 反向代理 + 負載均衡器：簡單時像 Caddy，複雜時不必換軟體 |
| 最終文字 | 主標「樞衡」· 副標 `Fulcrum` |
| 字體 | 中文 **HarmonyOS Sans SC Bold**（`Round1/ux/assets/fonts/HarmonyOS_Sans_SC_Bold.woff2`）· 拉丁 **Geist Mono**（`Round1/ux/assets/fonts/GeistMonoVF.woff2`） |
| 色板 | 石墨 `#14161A` · 深石墨 `#1C2026` · 赤銅鏽橙 `#C2622B` · 冷鋼灰 `#8A93A0` · 暖白 `#EDE9E2` |
| 視覺方向 | ★ **器物特寫：支點上的天平橫樑**（owner 2026-08-19 拍板） |

## 視覺方向

名字自己就是圖：**「樞」是門軸與中樞，「衡」是平衡**；英文 `Fulcrum` 是槓桿與天平的支點，
既是轉動的中心，也是平衡的基準。所以主體就一件器物——**一根機加工橫樑，水平架在一枚三稜支點上**：
樑上有刻度（衡），梁心是帶軸環的可轉樞軸（樞），**三股流線從左匯入、一股從右送出**——
那正是產品主張「Caddy + HAProxy + Nginx 收進一個處理程序」。

★ **橫樑必須絕對水平。** 傾斜的天平是「失衡」，與「衡」正好反義；這不是審美偏好，是語義錯誤。
赤銅鏽橙既是 Rust 的語義色，也是真實氧化金屬的顏色，不是為了「科技感」挑的。

★ **`crab` 必須寫進 Avoid**：Rust 的社群吉祥物 Ferris 是一隻螃蟹，生成模型看到 "Rust" 很容易
自己塞一隻進來。同理要擋住雲圖標、機櫃、齒輪堆和通用科技藍——那是市面上所有云原生官網的樣子，
不是這個產品的樣子。

## 1. Logo 原生生成（隨後摳圖）

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

摳圖後入庫 `Fulcrum/assets/brand/fulcrum-logo.png`。

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

入庫 `Fulcrum/assets/brand/fulcrum-badge-bg.webp`。

## 3. 豎版背景原生生成（4:5）

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

入庫 `Fulcrum/assets/brand/fulcrum-stage-bg.webp`。

## 4. 後期合成

- **徽章**：左側 1/4 疊 `fulcrum-logo.png`；中間兩行排「樞衡」與 `Fulcrum`；右側只保留橫樑 HERO。
  主標 HarmonyOS Sans SC Bold 約 84px、`#EDE9E2`、字距 0.14em；副標 Geist Mono 約 30px、
  `#C2622B`、字距 0.16em。
- **豎版宣傳圖**：中央疊 Logo，下方排中文與英文，整組垂直居中。
- ★ **「樞衡」是本批唯一的兩字主標**。中段名稱區若照四字主標的字號排，兩個字會在一大片空裡發飄：
  **字號取上限（約 84px）＋ 字距拉到 0.12–0.16em**，才與左側 Logo 和右側 HERO 三者平衡。
  副標 `Fulcrum` 用等寬體，本身就帶字距，再加 0.16em 後正好與主標同寬域收邊。
- ★ Logo 是**橫向器物**（橫樑），與另外三個專案的豎向 Logo 不同：徽章左側 1/4 那格按**高度**
  給它定尺會太小，應按**寬度**佔滿該格的 78–84% 定尺，再與中段文字組按包圍盒中心垂直居中。

## 5. 自檢

- **橫樑必須水平**；任何傾斜即判廢，不做「轉正一點」的挽救。
- **支點、刻度、樞軸三件必須同時成立**；缺一件就退化成「一根棍子架在三角上」，那不是樞衡。
- 三進一出的流線要看得出來——那是「三合一」的產品主張，不是裝飾線條。
- 不得出現螃蟹或任何動物吉祥物、雲圖標、機櫃、線纜、齒輪、通用科技藍。
- 背景裡不得出現任何模型生成的字母、數字或偽 UI。
- 「樞衡」二字必須實心；`Fulcrum` 大小寫逐字正確（F 大寫、其餘小寫），
  不寫成 `FULCRUM` / `fulcrum` / `FulCrum`。
