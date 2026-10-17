---
type: 驗證記錄
title: musl + BoringSSL 靜態連結（G103/G104 的未驗前置）
description: 通過 —— 靜態二進位在 FROM scratch 裡跑完了一次 QUIC 握手；★★★ 而卡住這件事的從來不是 musl，是建置宿主：Alpine 上 build script 預設靜態，靜態二進位不能 dlopen，bindgen 就死在這裡。
resource: ../../tests/musl/probe.sh
tags: [驗證, 已通過, 相依, 必讀, 易錯]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-25T00:00:00Z
sources:
  - id: plan-10-59
    resource: /references/plan.md
    title: `PLAN.md` §10 G103–G105（D11 結案：取 quiche，TLS 棧換 BoringSSL）
  - id: plan-51
    resource: /references/plan.md
    title: PLAN.md §5.1 第 1 條（原「鎖死 rustls」，已由 G104 推翻）、§5 散佈口徑 = musl 單靜態二進位（G13）
  - id: plan-9
    resource: /references/plan.md
    title: PLAN.md §9 風險表「BoringSSL 是 C 相依，而散佈口徑是 musl 單靜態二進位」
  - id: http3-libs
    resource: /platform/http3-libraries.md
    title: HTTP/3 庫選型事實表（D11）
---

★ **本頁記的是實際跑出來的東西，屬歷史事實。** 結論可以被後續實驗推翻，但「那幾次跑出了什麼」不會變。

> **結論：通過。G103 站得住，不需要重議。**
>
> 一個**完全靜態**的 musl 二進位（`INTERP=0`、`NEEDED=0`）被塞進一個
> `FROM scratch` 鏡像 —— 那裡面除了它自己什麼都沒有 —— 在 **x86_64 與 aarch64 兩個架構上**
> 各跑完一次真的 QUIC 握手、一次 1-RTT 應用資料往返，
> 並且 `set_select_certificate_callback` **真的被呼叫了**，回呼裡讀得到 ClientHello 的 SNI。
>
> ⚠ ⚠ 而**卡住這件事的從來不是 musl，也不是 BoringSSL** —— 見下面第 2 節。
> 這個區別對下一批是決定性的：它決定了要改的是**建置宿主**，不是選型。

跑法：

```bash
bash tests/musl/probe.sh              # 兩個架構 + 反證
ARCHES="amd64" bash tests/musl/probe.sh
```

---

## 1. 四條判據，為什麼是四條

| | 判據 | 它單獨存在的理由 |
|---|---|---|
| **A** | `boring-sys` 能在 musl 上把 BoringSSL 編出來 | D11 取 quiche 的前提 |
| **B** | 產物**完全靜態**（無 `INTERP` 段、無 `NEEDED` 條目）| §5 的散佈口徑 = 單靜態二進位（G13）|
| **C** | 在 musl 上**真的跑完**一次 QUIC 握手 **且** 一次 1-RTT 應用資料往返 | ★ 分成 C1/C2 兩條：握手證金鑰交換與證書鏈，應用資料證 1-RTT AEAD —— BoringSSL 裡是兩段程式碼 |
| **D** | `set_select_certificate_callback` **真的被呼叫過** | ★ ★ ★ **G104 整條決策就壓在這一個回呼上** |

★ ★ ★ **A 與 C 必須分開，因為「連結成功」與「跑得起來」在 `cargo build` 那一層長得一樣，
而前者是綠的。** musl 上最愛被引用的那一類失效正是這個形狀：**執行緒棧比 glibc 小得多**，
連結、啟動、`main` 全都好好的，直到握手真的在工作執行緒上跑起來。
⇒ 探針的握手**故意跑在 `std::thread::spawn` 起的執行緒上**，不在主執行緒 ——
主執行緒那一份是處理程序棧（8 MB 量級），測它等於測了一個產品不會走的路徑，
而產品裡的握手同樣發生在 tokio 的工作執行緒上。

> ⚠ ⚠ **但「musl 預設執行緒棧 128 KB」這句話對 Rust 處理程序並不成立** ——
> `std::thread` 不用 libc 的預設值，它自己指定棧大小。
> ⇒ 探針**把這條執行緒真實的棧大小量出來印在紀錄裡**（`pthread_getattr_np`）：
> 實測 glibc **2,097,152** 位元組、musl **2,099,880 / 2,099,864** 位元組（見第 4 節）——**都是 2 MiB 量級**。
> ★ ★ 「跑在工作執行緒上而不是主執行緒」這條判據**本身沒錯**，錯的是當初給它配的那個理由。
> ⚠ **一條正確的判據配一個錯誤的理由，比一條錯誤的判據更難被發現**：
> 判據是綠的，沒有人會回頭去核它為什麼在那裡。

★ ★ **D 必須單獨判，因為握手可以在回呼從未被呼叫的情況下完全成功**
（BoringSSL 會直接用 context 上掛著的那張證書）。
⇒ **「握手通過」證不了 G104**，只有「回呼真的進來過」才證得了。
而回呼裡**讀一下 SNI 再放行**，不是空跑一個 `Ok(())`：
按 SNI 動態挑證書要的正是「拿得到 ClientHello 的內容」這一條。

### B 怎麼量的：不看 `file(1)`，把它丟進一個空鏡像裡跑

產物被 `COPY` 進 `FROM scratch`，`ENTRYPOINT` 就是它自己。
動態連結的東西在那裡會死在「找不到解釋器」上。

> ★ **這同時是 §6.2 那句話的字面檢驗**：首版明確不做官方容器鏡像，
> **「文件給一份 `FROM scratch` Dockerfile 代替」** —— 這份探針跑的就是那份 Dockerfile。

★ **反證走同一份 Dockerfile**，只把 `+crt-static` 翻成 `-crt-static`。實測那一趟：

```
CRT_STATIC=-crt-static      ELFTYPE=DYN (Position-Independent Executable file)
INTERP=1   NEEDED=2   BYTES=3,048,080
FILE=… dynamically linked, interpreter /lib/ld-musl-x86_64.so.1 …
在 FROM scratch 里：exec /probe: no such file or directory
```

⇒ 這把尺子在好壞兩種情況下讀數不同，它才是一把尺子。

> ⚠ ⚠ **一條我自己編出來又刪掉的理由。** `Dockerfile.musl-probe` 的第一版註解裡寫著
> 「`file(1)` 說的 `statically linked` 是一句可以被相信錯的話」—— **那是編的，不是量的**。
> 上面這份反證證據裡 `file` 的輸出**分得一清二楚**。
> ★ 真正的理由不是「`file` 不可信」，是**它答的不是同一個問題**：
> `file` 描述的是 ELF 頭裡寫著什麼，而我們要發的是「除了這個檔案之外什麼都不需要」——
> 前者是後者的必要條件，不是它本身。
> ★ ★ **一條為了讓判據顯得更有必要而編出來的理由，比沒有理由更壞**：
> 它會讓下一個人以為 `file` 這條路走不通，從而不去用一件其實好用的工具。

---

## 2. ★★★ 本次最貴的一條：卡住的不是 musl，是建置宿主

第一次嘗試是在 Alpine 上原生編（Alpine 的 libc 本來就是 musl，`build-base` 直接給
原生的 gcc/g++ 與 musl 版 libstdc++ —— **不是交叉編譯**）。BoringSSL 的 `crypto` 與 `ssl`
兩個 target **都編出來了**，然後死在這裡：

```
thread 'main' panicked at bindgen-0.72.1/lib.rs:616:27:
Unable to find libclang: "the `libclang` shared library at /usr/lib/llvm22/lib/libclang.so.22.1.3
could not be opened: Dynamic loading not supported"
```

根因鏈是三步，每一步單獨看都很平常：

1. `boring-sys` 用 **bindgen** 生成 FFI 綁定，bindgen 預設走 `clang-sys` 的 runtime 模式，
   也就是 **`dlopen("libclang.so")`**；
2. **靜態連結的二進位不能 `dlopen`**；
3. ⚠ ⚠ **Alpine 上 rustc 的 host 目標預設就是 `+crt-static`** —— 實測
   `rustc --print cfg` 裡有 `target_feature="crt-static"`，不給任何 `RUSTFLAGS`
   編出來的 hello world 也沒有 `INTERP` 段。⇒ **build script 自己是靜態的。**

> ★ ★ ★ **這條失敗與「BoringSSL 能不能和 musl 靜態連結」一個字的關係都沒有。**
> 它卡的是**建置宿主**（誰在編），不是**目標**（編給誰）。
> ⚠ 而回報錯誤文字裡只有 `libclang`、`Dynamic loading` 與一個 `boring-sys` 的棧影格 ——
> 三個詞都指向「BoringSSL 那邊有問題」。**把它記成「musl 不行」會直接推翻一條正確的選型。**

### 走通的順序：兩趟，而這不是優化

```
第一趟  RUSTFLAGS="-C target-feature=-crt-static" cargo build --release --locked
        ⇒ 全图动态，build script 能 dlopen，BoringSSL 编出来、绑定生成好（实测 41–54 秒）

第二趟  cargo rustc --release --locked --bin <bin> -- -C target-feature=+crt-static
        ⇒ `cargo rustc --` 后面那串参数**只作用于正在编的这一个 crate**，
          依赖一个都不重编（实测 2.8–2.9 秒），最终产物 static-pie
```

★ **為什麼不能用 `RUSTFLAGS` 一把梭**：它作用於全圖，第二趟會連 build script 一起重編成靜態，
於是又回到 `dlopen` 那一步。**stable 上 per-crate 的口子只有 `cargo rustc --` 這一個。**

---

## 3. 沿路查實的事實（都是這次真撞到的，不是印象）

1. **`boring-sys 4.22.0` 的建置相依有四樣，而只有一樣被文件提到。**
   quiche 的 README 只說 **cmake**；實測還要 **C/C++ 編譯器**、**`git`**、**libclang**。
   ⚠ 缺 `git` 時的回報錯誤是 `Os { code: 2, kind: NotFound, message: "No such file or directory" }`
   —— **它不說自己找不到的是 git**（原始碼在 `build/main.rs:673`，
   `ensure_patches_applied` 會 `git init` + `git apply` 打自己的補丁）。
   ★ 在儲存庫那張 Debian 鏡像上撞不到這一條，因為 `rust:*-trixie` 自帶 git；`rust:*-alpine` 不帶。
   ⇒ **一條只在換基礎鏡像時才現形的建置相依。**
   ✅ 2026-09-24 隨 rebase 升到 `boring-sys 5.2.0`：同一張 Alpine 建置鏡像**一個包沒增刪**就編過了 ⇒ 這四樣仍然夠。

2. ⚠ ⚠ **`boring` 不能按 G29「追新」寫。** 實測 crates.io 上最新是 **5.2.0**，
   而 `quiche 0.29.3` 解出來的是 **4.22.0**（`cargo add` 當場提示
   `Adding boring v4.22.0 (available: v5.2.0)`）。照追新寫一行 `boring = "5"`，
   cargo 會**同時**留下兩份 boring，`SslContextBuilder` 同名卻是兩個類型 ——
   ★ 與根 `Cargo.toml` 裡 `pingora-http` 那條 `[patch]` 註解記的是同一個形狀。
   ✅ 2026-09-24：`quiche 0.30.0` 把 boring 的區間放寬到 `>=4.19, <6`，探針隨產品一起取 **5.2.0**
   ⇒ 圖裡仍然只有一份 boring（`cargo tree -i boring` 實測；與產品那把鎖的五個相關包版本逐個相同）。
   ⚠ 規矩本身不變：寫到 quiche 的區間**之外**，照樣同時留下兩份。

3. **Debian trixie 沒有 musl 的 C++ 編譯器。** 實測 `musl-tools` 只裝出
   `/usr/bin/musl-gcc` 與 `/usr/bin/musl-ldd`（C），**沒有 `musl-g++`**；
   `apt-cache search` 裡也沒有任何 musl 交叉包。而 BoringSSL 的 `ssl/` 是 C++，
   `boring-sys` 還會 `cargo:rustc-link-lib=stdc++`。
   ⇒ **儲存庫現有的那張建置鏡像（`docker/Dockerfile.build`）按原樣編不出 musl 產物。**

4. `quiche 0.29.3` 的 default feature **就是** `boringssl-boring-crate`（0.30.0 仍是，讀的是它的 `Cargo.toml` `[features]`），
   `Config::with_boring_ssl_ctx_builder` 走的正是它 —— G104 假定的那條路是通的，
   **而且現在是被編譯器與一次真握手一起證過的**，不再只是讀文件。

---

## 4. 數字

一次跑（`bash tests/musl/probe.sh`，2026-08-25，**9 分 17 秒**，兩個架構 + 反證全綠）：

| | x86_64 | aarch64 |
|---|---|---|
| 目標三元組 | `x86_64-unknown-linux-musl` | `aarch64-unknown-linux-musl` |
| ELF Machine | `Advanced Micro Devices X86-64` | `AArch64` |
| `INTERP` / `NEEDED` | **0 / 0** | **0 / 0** |
| `file(1)` | `static-pie linked` | `statically linked` |
| 二進位大小 | **3,191,224** 位元組 | **2,951,168** 位元組 |
| 握手執行緒的棧 | 2,099,880 位元組 | 2,099,864 位元組 |
| 建置方式 | Alpine 原生 | Alpine 原生 + **qemu 模擬**（本機無 aarch64 硬體）|

✅ **2026-09-24 重跑**（隨 rebase 升到 `quiche 0.30.0` + `boring 5.2.0`；`ARCHES=amd64`，**2 分 57 秒**，含反證）：
x86_64 `INTERP=0 / NEEDED=0`、`static-pie linked`，二進位 **3,734,824** 位元組（比上表 +543,600，約 +17%），
握手執行緒的棧 2,099,864 位元組；C1 / C2 / D 全過，反證照樣在 scratch 裡跑不起來。
⏳ aarch64 那一格沒重跑 —— 要等本機裝好 binfmt（qemu）之後補。

★ **兩邊 `file(1)` 的措辭不一樣（`static-pie` vs `statically`），而它們是同一件事**：
`INTERP` 與 `NEEDED` 兩個數都是 0。差別只是 x86_64 那份是位置無關的靜態執行檔。
⚠ 記在這裡是因為**只按 `file` 的字面去比會以為 aarch64 那份「不夠靜態」**，反過來也一樣。

★ 表裡那兩個 musl 數只差 16 位元組，與 glibc 那個（2,097,152）也只差幾千位元組 —— **三個都是 2 MiB 量級**。
參見第 1 節：「musl 預設執行緒棧 128 KB」這句話對 Rust 處理程序不成立。

⚠ **aarch64 那一格是在 qemu 上編的、也是在 qemu 上跑的。**
★ 它證的是「這份程式碼能編成 aarch64 musl 靜態產物並在 aarch64 上跑」，
**證不了**「在真的 arm64 機器上也一樣」—— qemu 與真機的差別不在編譯結果裡，在別處。

---

## 5. 這份探針**證不了**什麼

★ 按 AGENTS.md「a spike proves the road exists; it does not prove the product is on it」逐條寫死：

1. **它不是產品。** 跑這份探針時產品還一行都沒相依 BoringSSL。
   ⇒ 「探針編得出來」證不了「樞衡編得出來」—— 真正的產品圖裡還有 `pingora-core`、
   `instant-acme` 等一大票，**其中任何一個的 `*-sys` 相依都可能在 musl 上另有說法**。
   ✅ 那一問後來由 [`tests/musl/product.sh`](../../tests/musl/product.sh) 用**產品本體**答掉了
   （G108）。
2. **它沒證明「產物裡只有一套 TLS」。** 探針裡 rustls 與 BoringSSL 並不共存，
   而產品遷移期一定會有一段兩者都在圖裡。⚠ 而 `crates/fulcrum/tests/supply_gates.rs`
   的三道門**對 boring 結構上說不出話**（門 1 只認 `aws-lc-rs`）——
   ⇒ 那三道門要跟著 G104 一起重想。
3. **探針自己那份 `Cargo.lock` 沒有任何門看著。** 它在根 workspace 之外
   （`spikes/musl-boringssl/Cargo.toml` 裡有一個空的 `[workspace]` 表），
   所以 `supply_gates.rs` 掃的那兩把鎖不包括它。⇒ **登記在此，不要以為它被掃過。**
4. **探針的 Rust 程式碼沒有 clippy 看著。** 同樣因為它在根 workspace 之外，
   lint 那一格的 `cargo clippy --workspace` 夠不到它。
   ⚠ **`cargo fmt` 是特意補了一條 `--manifest-path` 進 lint 門的**（零建置成本），
   **clippy 沒補** —— 補了意味著每一次 lint 都要先編一遍 BoringSSL。
   ★ 這是權衡，不是遺漏；寫在這裡是為了讓它是**已知**的。
5. **它沒驗 qemu 之外的 aarch64。**
6. **它沒有回答「發佈流水線怎麼搭」** —— 那是另一件事，見下節。

---

## 6. 由它長出來的兩條（掛號 **D21** / **D22**）—— ✅ 兩條都已結案（G131 / G108）

1. ~~**D21 · 建置宿主的口徑。**~~ ✅ **2026-09-04 由 G131 結案：取（a）。**
   ⛔ 決定性理由只有一條 —— （b）的全部代價落在**供應鏈**上；⚠ 代價認下：aarch64 那一趟
   在 qemu 上慢一個量級。✅ **那個耗時後來量到了**（曾是 D24 的輸入）：2026-09-05
   `ShanireHomePC` 冷建 **18m07s**、2026-09-06 `ShanirePCX` 冷建 **34m05s**
   （⚠ 近兩倍，**機器本機**，⛔ 別互相照抄）。D24 已由 **G144** 結案 = 候選 ②。
   下面這段是當初擺在面前的兩條，⛔ 有意不刪 —— 「當初為什麼那樣定」本身仍是有效資訊：
   現在這份探針用的是**第二張被釘死的基礎鏡像**（Alpine），
   而儲存庫此前只有 `rust:1.97.1-trixie` 一張。擺在面前的至少兩條：
   （a）**Alpine 原生 + qemu 跑 aarch64** —— 就是探針現在這條，不新增任何工具鏈，
   代價是 aarch64 那一趟慢一個量級；
   （b）**glibc 宿主 + musl 交叉工具鏈**（zig 或 musl-cross-make）—— 兩個架構都快、
   不用模擬，代價是給供應鏈新增一整套 C/C++ 交叉編譯器。
   ★ 這條**不急**：發佈流水線本身還不存在（打包是 M4 的事），
   而 HTTP/3 的實施批次不需要先答它。
2. ~~**D22 · 這份探針什麼時候必須變成一道常設的門。**~~ ✅ **2026-08-26 結案，
   而結論不是這條登記寫的那一條。** 原文保留在下面，因為它記錄了當初為什麼不掛。
   > ~~現在它**不在門禁裡**，理由是產品今天一行都沒相依 BoringSSL —— 把一個 qemu 上
   > 幾十分鐘的建置掛進每一次門禁，等於給一個還不存在的相依付全額保費。
   > ⚠ **到期條件寫死**：等 G104 真的落進產品（`fulcrum-tls` 換到 BoringSSL）的那一批，
   > 「產物是不是單靜態二進位」必須變成一道常設的門。~~

   ⚠ ⚠ **那個到期條件滿足之後又過了四輪才被發現，而每一輪都讀過 §11 那張表。**
   ★ **一條寫著「等 X 發生就到期」的登記，沒有任何東西會在 X 發生時通知它。**

   ⇒ **owner 2026-08-26 改的是判據本身**：D22 想守的是 **G13 的散佈口徑 ——「產物是不是
   單靜態二進位」**，而**這份探針答不了它**（正是上面第 5 節第 1 條自己寫下的那句話：
   「『探針編得出來』證不了『樞衡編得出來』」）。
   ⇒ 新的場景 [`tests/musl/product.sh`](../../tests/musl/product.sh) 拿**產品本體**去證；
   **本頁這份探針留在門外當歷史記錄。**

   ✅ **產品那一格第一次跑的結果（2026-08-26，x86_64）**：整個產品圖在 musl 上編出來了，
   產物 **16,191,800 位元組**、`INTERP=0 / NEEDED=0`、`static-pie linked`，
   在 `FROM scratch` 裡跑完了 `fulcrum validate` 四層。
   ★ 兩個反向都做了：動態版在 scratch 裡跑不起來；一份該被拒的設定給出了**那條專門的**診斷。
   ⏳ 它**只覆蓋 x86_64** —— 而 G13 承諾兩個架構，aarch64 那一半仍然沒有門。
