---
type: 验证记录
title: musl + BoringSSL 静态链接（G103/G104 的未验前置）
description: 通过 —— 静态二进制在 FROM scratch 里跑完了一次 QUIC 握手；★★★ 而卡住这件事的从来不是 musl，是构建宿主：Alpine 上 build script 默认静态，静态二进制不能 dlopen，bindgen 就死在这里。
resource: ../../tests/musl/probe.sh
tags: [验证, 已通过, 依赖, 必读, 易错]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-25T00:00:00Z
sources:
  - id: plan-10-59
    resource: /references/plan.md
    title: `PLAN.md` §10 G103–G105（D11 结案：取 quiche，TLS 栈换 BoringSSL）
  - id: plan-51
    resource: /references/plan.md
    title: PLAN.md §5.1 第 1 条（原「锁死 rustls」，已由 G104 推翻）、§5 分发口径 = musl 单静态二进制（G13）
  - id: plan-9
    resource: /references/plan.md
    title: PLAN.md §9 风险表「BoringSSL 是 C 依赖，而分发口径是 musl 单静态二进制」
  - id: http3-libs
    resource: /platform/http3-libraries.md
    title: HTTP/3 库选型事实表（D11）
---

★ **本页记的是实际跑出来的东西，属历史事实。** 结论可以被后续实验推翻，但「那几次跑出了什么」不会变。

> **结论：通过。G103 站得住，不需要重议。**
>
> 一个**完全静态**的 musl 二进制（`INTERP=0`、`NEEDED=0`）被塞进一个
> `FROM scratch` 镜像 —— 那里面除了它自己什么都没有 —— 在 **x86_64 与 aarch64 两个架构上**
> 各跑完一次真的 QUIC 握手、一次 1-RTT 应用数据往返，
> 并且 `set_select_certificate_callback` **真的被调用了**，回调里读得到 ClientHello 的 SNI。
>
> ⚠ ⚠ 而**卡住这件事的从来不是 musl，也不是 BoringSSL** —— 见下面第 2 节。
> 这个区别对下一批是决定性的：它决定了要改的是**构建宿主**，不是选型。

跑法：

```bash
bash tests/musl/probe.sh              # 两个架构 + 反证
ARCHES="amd64" bash tests/musl/probe.sh
```

---

## 1. 四条判据，为什么是四条

| | 判据 | 它单独存在的理由 |
|---|---|---|
| **A** | `boring-sys` 能在 musl 上把 BoringSSL 编出来 | D11 取 quiche 的前提 |
| **B** | 产物**完全静态**（无 `INTERP` 段、无 `NEEDED` 条目）| §5 的分发口径 = 单静态二进制（G13）|
| **C** | 在 musl 上**真的跑完**一次 QUIC 握手 **且** 一次 1-RTT 应用数据往返 | ★ 分成 C1/C2 两条：握手证密钥交换与证书链，应用数据证 1-RTT AEAD —— BoringSSL 里是两段代码 |
| **D** | `set_select_certificate_callback` **真的被调用过** | ★ ★ ★ **G104 整条决策就压在这一个回调上** |

★ ★ ★ **A 与 C 必须分开，因为「链接成功」与「跑得起来」在 `cargo build` 那一层长得一样，
而前者是绿的。** musl 上最爱被引用的那一类失效正是这个形状：**线程栈比 glibc 小得多**，
链接、启动、`main` 全都好好的，直到握手真的在工作线程上跑起来。
⇒ 探针的握手**故意跑在 `std::thread::spawn` 起的线程上**，不在主线程 ——
主线程那一份是进程栈（8 MB 量级），测它等于测了一个产品不会走的路径，
而产品里的握手同样发生在 tokio 的工作线程上。

> ⚠ ⚠ **但「musl 默认线程栈 128 KB」这句话对 Rust 程序并不成立** ——
> `std::thread` 不用 libc 的默认值，它自己指定栈大小。
> ⇒ 探针**把这条线程真实的栈大小量出来印在日志里**（`pthread_getattr_np`）：
> 实测 glibc **2,097,152** 字节、musl **2,099,880 / 2,099,864** 字节（见第 4 节）——**都是 2 MiB 量级**。
> ★ ★ 「跑在工作线程上而不是主线程」这条判据**本身没错**，错的是当初给它配的那个理由。
> ⚠ **一条正确的判据配一个错误的理由，比一条错误的判据更难被发现**：
> 判据是绿的，没有人会回头去核它为什么在那里。

★ ★ **D 必须单独判，因为握手可以在回调从未被调用的情况下完全成功**
（BoringSSL 会直接用 context 上挂着的那张证书）。
⇒ **「握手通过」证不了 G104**，只有「回调真的进来过」才证得了。
而回调里**读一下 SNI 再放行**，不是空跑一个 `Ok(())`：
按 SNI 动态挑证书要的正是「拿得到 ClientHello 的内容」这一条。

### B 怎么量的：不看 `file(1)`，把它丢进一个空镜像里跑

产物被 `COPY` 进 `FROM scratch`，`ENTRYPOINT` 就是它自己。
动态链接的东西在那里会死在「找不到解释器」上。

> ★ **这同时是 §6.2 那句话的字面检验**：首版明确不做官方容器镜像，
> **「文档给一份 `FROM scratch` Dockerfile 代替」** —— 这份探针跑的就是那份 Dockerfile。

★ **反证走同一份 Dockerfile**，只把 `+crt-static` 翻成 `-crt-static`。实测那一趟：

```
CRT_STATIC=-crt-static      ELFTYPE=DYN (Position-Independent Executable file)
INTERP=1   NEEDED=2   BYTES=3,048,080
FILE=… dynamically linked, interpreter /lib/ld-musl-x86_64.so.1 …
在 FROM scratch 里：exec /probe: no such file or directory
```

⇒ 这把尺子在好坏两种情况下读数不同，它才是一把尺子。

> ⚠ ⚠ **一条我自己编出来又删掉的理由。** `Dockerfile.musl-probe` 的第一版注释里写着
> 「`file(1)` 说的 `statically linked` 是一句可以被相信错的话」—— **那是编的，不是量的**。
> 上面这份反证证据里 `file` 的输出**分得一清二楚**。
> ★ 真正的理由不是「`file` 不可信」，是**它答的不是同一个问题**：
> `file` 描述的是 ELF 头里写着什么，而我们要发的是「除了这个文件之外什么都不需要」——
> 前者是后者的必要条件，不是它本身。
> ★ ★ **一条为了让判据显得更有必要而编出来的理由，比没有理由更坏**：
> 它会让下一个人以为 `file` 这条路走不通，从而不去用一件其实好用的工具。

---

## 2. ★★★ 本次最贵的一条：卡住的不是 musl，是构建宿主

第一次尝试是在 Alpine 上原生编（Alpine 的 libc 本来就是 musl，`build-base` 直接给
原生的 gcc/g++ 与 musl 版 libstdc++ —— **不是交叉编译**）。BoringSSL 的 `crypto` 与 `ssl`
两个 target **都编出来了**，然后死在这里：

```
thread 'main' panicked at bindgen-0.72.1/lib.rs:616:27:
Unable to find libclang: "the `libclang` shared library at /usr/lib/llvm22/lib/libclang.so.22.1.3
could not be opened: Dynamic loading not supported"
```

根因链是三步，每一步单独看都很平常：

1. `boring-sys` 用 **bindgen** 生成 FFI 绑定，bindgen 默认走 `clang-sys` 的 runtime 模式，
   也就是 **`dlopen("libclang.so")`**；
2. **静态链接的二进制不能 `dlopen`**；
3. ⚠ ⚠ **Alpine 上 rustc 的 host 目标默认就是 `+crt-static`** —— 实测
   `rustc --print cfg` 里有 `target_feature="crt-static"`，不给任何 `RUSTFLAGS`
   编出来的 hello world 也没有 `INTERP` 段。⇒ **build script 自己是静态的。**

> ★ ★ ★ **这条失败与「BoringSSL 能不能和 musl 静态链接」一个字的关系都没有。**
> 它卡的是**构建宿主**（谁在编），不是**目标**（编给谁）。
> ⚠ 而报错文本里只有 `libclang`、`Dynamic loading` 与一个 `boring-sys` 的栈帧 ——
> 三个词都指向「BoringSSL 那边有问题」。**把它记成「musl 不行」会直接推翻一条正确的选型。**

### 走通的顺序：两趟，而这不是优化

```
第一趟  RUSTFLAGS="-C target-feature=-crt-static" cargo build --release --locked
        ⇒ 全图动态，build script 能 dlopen，BoringSSL 编出来、绑定生成好（实测 41–54 秒）

第二趟  cargo rustc --release --locked --bin <bin> -- -C target-feature=+crt-static
        ⇒ `cargo rustc --` 后面那串参数**只作用于正在编的这一个 crate**，
          依赖一个都不重编（实测 2.8–2.9 秒），最终产物 static-pie
```

★ **为什么不能用 `RUSTFLAGS` 一把梭**：它作用于全图，第二趟会连 build script 一起重编成静态，
于是又回到 `dlopen` 那一步。**stable 上 per-crate 的口子只有 `cargo rustc --` 这一个。**

---

## 3. 沿路查实的事实（都是这次真撞到的，不是印象）

1. **`boring-sys 4.22.0` 的构建依赖有四样，而只有一样被文档提到。**
   quiche 的 README 只说 **cmake**；实测还要 **C/C++ 编译器**、**`git`**、**libclang**。
   ⚠ 缺 `git` 时的报错是 `Os { code: 2, kind: NotFound, message: "No such file or directory" }`
   —— **它不说自己找不到的是 git**（源码在 `build/main.rs:673`，
   `ensure_patches_applied` 会 `git init` + `git apply` 打自己的补丁）。
   ★ 在仓库那张 Debian 镜像上撞不到这一条，因为 `rust:*-trixie` 自带 git；`rust:*-alpine` 不带。
   ⇒ **一条只在换基础镜像时才现形的构建依赖。**

2. ⚠ ⚠ **`boring` 不能按 G29「追新」写。** 实测 crates.io 上最新是 **5.2.0**，
   而 `quiche 0.29.3` 解出来的是 **4.22.0**（`cargo add` 当场提示
   `Adding boring v4.22.0 (available: v5.2.0)`）。照追新写一行 `boring = "5"`，
   cargo 会**同时**留下两份 boring，`SslContextBuilder` 同名却是两个类型 ——
   ★ 与根 `Cargo.toml` 里 `pingora-http` 那条 `[patch]` 注释记的是同一个形状。

3. **Debian trixie 没有 musl 的 C++ 编译器。** 实测 `musl-tools` 只装出
   `/usr/bin/musl-gcc` 与 `/usr/bin/musl-ldd`（C），**没有 `musl-g++`**；
   `apt-cache search` 里也没有任何 musl 交叉包。而 BoringSSL 的 `ssl/` 是 C++，
   `boring-sys` 还会 `cargo:rustc-link-lib=stdc++`。
   ⇒ **仓库现有的那张构建镜像（`docker/Dockerfile.build`）按原样编不出 musl 产物。**

4. `quiche 0.29.3` 的 default feature **就是** `boringssl-boring-crate`，
   `Config::with_boring_ssl_ctx_builder` 走的正是它 —— G104 假定的那条路是通的，
   **而且现在是被编译器与一次真握手一起证过的**，不再只是读文档。

---

## 4. 数字

一次跑（`bash tests/musl/probe.sh`，2026-08-25，**9 分 17 秒**，两个架构 + 反证全绿）：

| | x86_64 | aarch64 |
|---|---|---|
| 目标三元组 | `x86_64-unknown-linux-musl` | `aarch64-unknown-linux-musl` |
| ELF Machine | `Advanced Micro Devices X86-64` | `AArch64` |
| `INTERP` / `NEEDED` | **0 / 0** | **0 / 0** |
| `file(1)` | `static-pie linked` | `statically linked` |
| 二进制大小 | **3,191,224** 字节 | **2,951,168** 字节 |
| 握手线程的栈 | 2,099,880 字节 | 2,099,864 字节 |
| 构建方式 | Alpine 原生 | Alpine 原生 + **qemu 模拟**（本机无 aarch64 硬件）|

★ **两边 `file(1)` 的措辞不一样（`static-pie` vs `statically`），而它们是同一件事**：
`INTERP` 与 `NEEDED` 两个数都是 0。差别只是 x86_64 那份是位置无关的静态可执行文件。
⚠ 记在这里是因为**只按 `file` 的字面去比会以为 aarch64 那份「不够静态」**，反过来也一样。

★ 表里那两个 musl 数只差 16 字节，与 glibc 那个（2,097,152）也只差几千字节 —— **三个都是 2 MiB 量级**。
参见第 1 节：「musl 默认线程栈 128 KB」这句话对 Rust 程序不成立。

⚠ **aarch64 那一格是在 qemu 上编的、也是在 qemu 上跑的。**
★ 它证的是「这份代码能编成 aarch64 musl 静态产物并在 aarch64 上跑」，
**证不了**「在真的 arm64 机器上也一样」—— qemu 与真机的差别不在编译结果里，在别处。

---

## 5. 这份探针**证不了**什么

★ 按 AGENTS.md「a spike proves the road exists; it does not prove the product is on it」逐条写死：

1. **它不是产品。** 跑这份探针时产品还一行都没依赖 BoringSSL。
   ⇒ 「探针编得出来」证不了「枢衡编得出来」—— 真正的产品图里还有 `pingora-core`、
   `instant-acme` 等一大票，**其中任何一个的 `*-sys` 依赖都可能在 musl 上另有说法**。
   ✅ 那一问后来由 [`tests/musl/product.sh`](../../tests/musl/product.sh) 用**产品本体**答掉了
   （G108）。
2. **它没证明「产物里只有一套 TLS」。** 探针里 rustls 与 BoringSSL 并不共存，
   而产品迁移期一定会有一段两者都在图里。⚠ 而 `crates/fulcrum/tests/supply_gates.rs`
   的三道门**对 boring 结构上说不出话**（门 1 只认 `aws-lc-rs`）——
   ⇒ 那三道门要跟着 G104 一起重想。
3. **探针自己那份 `Cargo.lock` 没有任何门看着。** 它在根 workspace 之外
   （`spikes/musl-boringssl/Cargo.toml` 里有一个空的 `[workspace]` 表），
   所以 `supply_gates.rs` 扫的那两把锁不包括它。⇒ **登记在此，不要以为它被扫过。**
4. **探针的 Rust 代码没有 clippy 看着。** 同样因为它在根 workspace 之外，
   lint 那一格的 `cargo clippy --workspace` 够不到它。
   ⚠ **`cargo fmt` 是特意补了一条 `--manifest-path` 进 lint 门的**（零构建成本），
   **clippy 没补** —— 补了意味着每一次 lint 都要先编一遍 BoringSSL。
   ★ 这是权衡，不是遗漏；写在这里是为了让它是**已知**的。
5. **它没验 qemu 之外的 aarch64。**
6. **它没有回答「发布流水线怎么搭」** —— 那是另一件事，见下节。

---

## 6. ⏳ 由它长出来的两条，都要拍板（已挂号 **D21** / **D22**）

1. **D21 · 构建宿主的口径。** 现在这份探针用的是**第二张被钉死的基础镜像**（Alpine），
   而仓库此前只有 `rust:1.97.1-trixie` 一张。摆在面前的至少两条：
   （a）**Alpine 原生 + qemu 跑 aarch64** —— 就是探针现在这条，不新增任何工具链，
   代价是 aarch64 那一趟慢一个量级；
   （b）**glibc 宿主 + musl 交叉工具链**（zig 或 musl-cross-make）—— 两个架构都快、
   不用模拟，代价是给供应链新增一整套 C/C++ 交叉编译器。
   ★ 这条**不急**：发布流水线本身还不存在（打包是 M4 的事），
   而 HTTP/3 的实施批次不需要先答它。
2. ~~**D22 · 这份探针什么时候必须变成一道常设的门。**~~ ✅ **2026-08-26 结案，
   而结论不是这条登记写的那一条。** 原文保留在下面，因为它记录了当初为什么不挂。
   > ~~现在它**不在门禁里**，理由是产品今天一行都没依赖 BoringSSL —— 把一个 qemu 上
   > 几十分钟的构建挂进每一次门禁，等于给一个还不存在的依赖付全额保费。
   > ⚠ **到期条件写死**：等 G104 真的落进产品（`fulcrum-tls` 换到 BoringSSL）的那一批，
   > 「产物是不是单静态二进制」必须变成一道常设的门。~~

   ⚠ ⚠ **那个到期条件满足之后又过了四轮才被发现，而每一轮都读过 §11 那张表。**
   ★ **一条写着「等 X 发生就到期」的登记，没有任何东西会在 X 发生时通知它。**

   ⇒ **owner 2026-08-26 改的是判据本身**：D22 想守的是 **G13 的分发口径 ——「产物是不是
   单静态二进制」**，而**这份探针答不了它**（正是上面第 5 节第 1 条自己写下的那句话：
   「『探针编得出来』证不了『枢衡编得出来』」）。
   ⇒ 新的场景 [`tests/musl/product.sh`](../../tests/musl/product.sh) 拿**产品本体**去证；
   **本页这份探针留在门外当历史记录。**

   ✅ **产品那一格第一次跑的结果（2026-08-26，x86_64）**：整个产品图在 musl 上编出来了，
   产物 **16,191,800 字节**、`INTERP=0 / NEEDED=0`、`static-pie linked`，
   在 `FROM scratch` 里跑完了 `fulcrum validate` 四层。
   ★ 两个反向都做了：动态版在 scratch 里跑不起来；一份该被拒的配置给出了**那条专门的**诊断。
   ⏳ 它**只覆盖 x86_64** —— 而 G13 承诺两个架构，aarch64 那一半仍然没有门。
