//! 供应链的五道结构门。
//!
//! ★ 起因：`instant-acme` 的 **default feature 里就含 `aws-lc-rs`**（`ring` 要显式开），
//! 照默认写一行依赖就会把它悄悄拉回来。「上游 crate 的 `default` 是一条静默的供应链入口」
//! 在本仓库现身过四次，前三次都是**发现之后**才知道自己一直在付代价 ——
//! 所以它要有一道门，不能只写在注释里。
//!
//! # 判据挂在**锁**上，不挂在清单文本上
//!
//! 门 1 查的是「两把 `Cargo.lock` 里有没有 `aws-lc-rs`」，不是「有没有人忘写
//! `default-features = false`」：后者只覆盖一种写法，换个中间依赖或 feature 名就看不见了，
//! 而结果一样。清单文本那两道门是第二层，不是第一层。
//!
//! # 五道门各答一个**不同**的问题（★ 不该混着说）
//!
//! | 门 | 它答的问题 |
//! |---|---|
//! | 1 | 两把**锁**里有没有 `aws-lc-rs` |
//! | 2 | **清单文本**里 `instant-acme` 等有没有关掉 default |
//! | 3 | 产品 crate 依赖 `pingora-core` 时有没有开 `boringssl` |
//! | 4 | **锁里写着**哪几套 TLS 实现 |
//! | 5 | **依赖图里真有**哪几套 TLS 实现（`cargo tree`）|
//!
//! ⚠ ⚠ **门 4 与门 5 不是一回事**：`Cargo.lock` 是依赖图的**超集** —— 关掉
//! `instant-acme` 的 `hyper-rustls` feature 之后，`hyper-rustls` / `rustls` / `schannel`
//! 仍然在锁里，却已经没有任何人链接它们。门 4 因此对那一次改动原理上是瞎的
//! （它在自己的注释里预言会红，而它没红）。详见门 4 那一段。
//!
//! **一个从没红过的门与不存在的门无法区分**，所以每一条的判定函数都由本文件里的固定输入
//! 证明过「能命中也能错过」；每一条还带一个**下界断言** —— 读不到清单、或一个 crate 都没
//! 匹配上时，它会在什么都没查的情况下报绿。

/// 根 workspace 的锁。
const ROOT_LOCK: &str = include_str!("../../../Cargo.lock");
/// ★ fork 的锁。**它才是要紧的那一把**：`[patch.crates-io]` 把 `pingora-core`
/// 指向 vendor，根锁里连一个 rustls 相关的包都没有。
const VENDOR_LOCK: &str = include_str!("../../../vendor/pingora/Cargo.lock");

const ROOT_MANIFEST: &str = include_str!("../../../Cargo.toml");

/// 仓库根。★ `CARGO_MANIFEST_DIR` 是 `crates/fulcrum`。
fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("找不到仓库根")
}

/// **运行时**列出 `crates/*/Cargo.toml`。
///
/// ★ ★ 为什么不用一串 `include_str!`：那是一张**手写清单**，
/// 而新加一个 crate 时没人会想起来往里加一行——于是这道门会安静地少覆盖一个 crate，
/// 看起来照样是绿的。这与 vendor 回归网那张 16 项手写名单、`unclaimed.sh` 的忽略名单
/// 是同一个形状：**不自省的名单**。
/// ⇒ 读目录，没有名单可维护；而下面那条下界断言负责证明它真的读到了东西。
fn crate_manifests() -> Vec<(String, String)> {
    let dir = repo_root().join("crates");
    let mut out = Vec::new();
    for e in std::fs::read_dir(&dir).expect("读不到 crates/") {
        let p = e.expect("目录项").path().join("Cargo.toml");
        if p.is_file() {
            let name = format!(
                "crates/{}/Cargo.toml",
                p.parent()
                    .and_then(|d| d.file_name())
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
            );
            out.push((name, std::fs::read_to_string(&p).expect("读不到清单")));
        }
    }
    out.sort();
    out
}

/// 从 `Cargo.lock` 里抠出全部包名。
fn lock_packages(lock: &str) -> Vec<&str> {
    lock.lines()
        .filter_map(|l| l.strip_prefix("name = \""))
        .filter_map(|l| l.strip_suffix('"'))
        .collect()
}

// ── 门 1：aws-lc-rs 不许回来 ────────────────────────────────────────────────

#[test]
fn 两把锁里都不许出现_aws_lc_rs() {
    for (which, lock) in [("根", ROOT_LOCK), ("vendor/pingora", VENDOR_LOCK)] {
        let pkgs = lock_packages(lock);
        // ★ 先钉住「确实读到了东西」。抠不出包名时下面的 `any` 恒为假，
        //   这道门会在**什么都没查**的情况下报绿。
        assert!(
            pkgs.len() >= 100,
            "{which}锁里只抠出 {} 个包名，格式多半变了；本次检查不能采信",
            pkgs.len()
        );
        assert!(
            !pkgs.iter().any(|p| p.starts_with("aws-lc")),
            "{which}锁里出现了 aws-lc —— G41/G45 把它赶出依赖图的成果被推翻了。\n\
             最可能的原因：新加的依赖照默认写了一行（`instant-acme` 的 default feature 就含它）。\n\
             修法：那条依赖写 `default-features = false`，再显式开 `ring`。"
        );
    }
}

#[test]
fn 锁扫描器自证能命中也能错过() {
    const WITH: &str = "[[package]]\nname = \"aws-lc-rs\"\nversion = \"1.0.0\"\n";
    const WITHOUT: &str = "[[package]]\nname = \"ring\"\nversion = \"0.17.14\"\n";
    assert_eq!(lock_packages(WITH), vec!["aws-lc-rs"]);
    assert_eq!(lock_packages(WITHOUT), vec!["ring"]);
    assert!(lock_packages(WITH).iter().any(|p| p.starts_with("aws-lc")));
    assert!(
        !lock_packages(WITHOUT)
            .iter()
            .any(|p| p.starts_with("aws-lc"))
    );
    // 空输入不能被当成「查过了、没问题」——这正是上面那条下界断言存在的理由。
    assert!(lock_packages("").is_empty());
}

// ── 门 2：已知会拖进 crypto provider 的依赖必须关掉 default ─────────────────

/// 这些 crate 的 default feature 会自己挑一个 crypto provider。
/// ★ 名单会长，但它**不是**判据的全部——判据是门 1 那把锁。这里只是让人早一步看见。
const MUST_DISABLE_DEFAULT: &[&str] = &[
    "instant-acme",
    "rustls",
    "tokio-rustls",
    "quinn",
    "rcgen",
    // ⚠ 批 8 加：它的 default 是
    //   `["native-tokio","http1","tls12","logging","aws-lc-rs"]` —— **含 aws-lc-rs**。
    "hyper-rustls",
    // ⚠ ⚠ 批 J 加（G103）：**它进这张名单的理由与上面几条都不一样，要说清。**
    //   上面几条是「default 会把一个我们不要的 provider 拖进来」；
    //   `quiche` 的 default（`["boringssl-boring-crate"]`）**正好就是我们要的那一个**。
    //   它进来是因为**另一条岔路**：`boringssl-vendored` 会让 quiche 自己从源码编一份
    //   BoringSSL ⇒ 产物里两套，而「统一 BoringSSL」（G104）按字面读就成了假话。
    //   ⇒ 本条守的是**上游哪天改了 default**，仅此而已。
    //
    // ⚠ ⚠ **门 5 守不住 `boringssl-vendored` 那一半**：门 4 与门 5 都对包名做了
    //   `dedup()`，判的是「出现了哪几个**名字**」，判不动「同一个名字有几个**版本**」；
    //   而 `boringssl-vendored` 根本不新增任何包名（BoringSSL 在 quiche 自己的 build
    //   script 里编）⇒ **两种情形门 5 都是绿的**。
    //   ★ ★ **真正守住这条约束的是编译器**：`SniResolver::install_into()` 收的是
    //   `pingora_boringssl::ssl::SslContextBuilder`，而它要被交给
    //   `quiche::Config::with_boring_ssl_ctx_builder()` —— 两边不是同一个 `boring`
    //   就是两个同名类型，**当场编不过**；feature 翻到 vendored 那一侧时，
    //   我们调的那个 API 干脆不存在，同样编不过。
    //   ⇒ 这正是 D18/G66 那条「**让分家在结构上做不到**」，只是这次执行者是类型系统。
    //   ⚠ **仍然有一条它盖不住的缝**：feature 是**并集**的 —— 若将来有别的 crate
    //     也依赖 quiche 并开了 `boringssl-vendored`，两个 feature 会同时打开，
    //     我们调的 API 还在、编得过，而产物里可能真有两份 BoringSSL。
    //     今天全 workspace 只有 `fulcrum-server` 一个消费者，所以这条缝是空的；
    //     ★ **第二个消费者出现的那一天，这里要重新想一遍**（登记在 PLAN.md §7 批 J）。
    "quiche",
];

/// 一份清单里，某个依赖有没有关掉 default features。
///
/// 返回 `None` 表示这份清单根本没声明它。
///
/// ★ ★ **`workspace = true` 算作「已经关掉了」**，因为 feature 的口径整个继承自根清单，
/// 而根清单本身也在被扫的名单里——**同一个事实只在一个地方判**。
///
/// ⚠ 这一条是被这道门自己的**假警报**逼出来的：`fulcrum-acme` 里写的是
/// `instant-acme = { workspace = true }`，根清单里写的是
/// `instant-acme = { version = "…", default-features = false, features = [...] }`——
/// 依赖图里**没有** aws-lc-rs（门 1 那把锁作证），而门 2 照样判红。
/// ★ 那正是本仓库反复撞见的那个形状：**判据只认得一种写法**。
/// 而假警报比没有警报更糟——它会训练人忽略这道门，连带把真的那次一起埋掉。
fn declares_without_default_off(manifest: &str, dep: &str) -> Option<bool> {
    let mut found = None;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue; // ★ 注释掉的一行不算声明
        }
        let Some(rest) = line.strip_prefix(dep) else {
            continue;
        };
        let rest = rest.trim_start();
        if !rest.starts_with('=') {
            continue; // `instant-acme-foo = …` 之类，不是它
        }
        let delegated = rest.contains("workspace = true");
        found = Some(
            found.unwrap_or(false) || !(delegated || rest.contains("default-features = false")),
        );
    }
    found
}

#[test]
fn 会自带_crypto_provider_的依赖必须写_default_features_false() {
    let mut manifests = vec![("Cargo.toml".to_string(), ROOT_MANIFEST.to_string())];
    manifests.extend(crate_manifests());
    // ★ 先钉住「确实读到了东西」：读不到 crates/ 时下面的循环一次都不跑，
    //   这道门会在**什么都没查**的情况下报绿。
    assert!(
        manifests.len() >= 4,
        "只读到 {} 份清单，crates/ 的布局多半变了；本次检查不能采信",
        manifests.len()
    );
    let mut bad = Vec::new();
    for (path, text) in &manifests {
        for dep in MUST_DISABLE_DEFAULT {
            if declares_without_default_off(text, dep) == Some(true) {
                bad.push(format!("{path} 的 `{dep}`"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "这些依赖没写 `default-features = false`：{bad:?}\n\
         它们的 default feature 会自己挑一个 crypto provider（多半是 aws-lc-rs）。"
    );
}

#[test]
fn 清单扫描器自证能命中也能错过() {
    let off =
        "instant-acme = { version = \">=0.7\", default-features = false, features = [\"ring\"] }";
    let on = "instant-acme = \">=0.7\"";
    let commented = "# instant-acme = \">=0.7\"";
    let other = "instant-acme-helper = \"1\"";
    // ★ workspace 继承：口径在根清单上，而根清单本身也在被扫的名单里。
    //   ⚠ 没有这一条，这道门会对**本仓库当前的真实写法**报假警——实际发生过。
    let inherited = "instant-acme = { workspace = true }";
    assert_eq!(
        declares_without_default_off(off, "instant-acme"),
        Some(false)
    );
    assert_eq!(declares_without_default_off(on, "instant-acme"), Some(true));
    assert_eq!(
        declares_without_default_off(inherited, "instant-acme"),
        Some(false),
        "workspace 继承被判成了「没关 default」—— 那是一条假警报"
    );
    assert_eq!(
        declares_without_default_off(commented, "instant-acme"),
        None
    );
    assert_eq!(declares_without_default_off(other, "instant-acme"), None);
    assert_eq!(declares_without_default_off("", "instant-acme"), None);
}

// ── 门 4：依赖图里到底有几套 TLS **实现** —— 两个方向都严 ────────────────
//
// ★ ★ ★ **（G104）新加的一道门，而它补的是一个此前没有任何东西看着的洞。**
//   §10 的作废清单第 ⑥ 项写着：门 1/2/3 那套口径**整个建在 rustls 上**，
//   而它们**对 boring 结构上说不出话** —— 门 1 只认 `aws-lc-rs` 这个名字。
//   ⇒ 换后端的迁移期里「有几套 TLS」没有任何判据，**而迁移期恰恰是两套都在的时候**。
//
// ⚠ ⚠ **两个方向都严**（照 `tests/vendor/run.sh` 的 EXPECTED_FAILURES 那套口径）：
//   多一个要红（有人悄悄拖进了第三套），**少一个也要红**。
//   ★ 后者不是洁癖：owner 拍的是「三处全换、rustls 彻底出局」，
//     而那件事做完的那一刻**应该有人被迫来改这张表** —— 那正是它值得被记一笔的时候。
//     一道只在「变多」时才红的门，会让「变少」这件好事悄悄发生、没人知道它发生了。

/// 已知的 TLS **实现** crate。
///
/// ⚠ ⚠ **只列实现，不列类型定义、不列适配层。** 这条边界是判据的一部分：
///   · `rustls-pki-types` 是一组 DER newtype，没有 crypto ⇒ **不算**；
///   · `hyper-rustls` / `tokio-rustls` / `pingora-rustls` / `pingora-boringssl`
///     是适配层，它们各自的实现已经在名单里 ⇒ **不算**（否则同一套 TLS 被数好几遍）；
///   · `openssl-probe` 只是去找系统证书文件在哪 ⇒ **不算**（★ 名字最像的一个）；
///   · `ring` 是**算法库**不是 TLS 栈，rustls 用它当 provider ⇒ 不单独算一套。
/// ★ 名单会长，而**长了不要紧**：它只决定「这个名字算不算一套 TLS」，
///   真正的判据是下面那张 `EXPECTED` 与实测结果逐项相同。
const KNOWN_TLS_IMPLS: &[&str] = &[
    "aws-lc-rs",
    "aws-lc-sys",
    "boring",
    "boring-sys",
    "native-tls",
    "openssl",
    "openssl-sys",
    "rustls",
    "s2n-tls",
    "s2n-tls-sys",
    "schannel",
];

/// 今天**允许**出现在根锁里的那些，按字典序。
///
/// ⚠ ⚠ ★ **这道门读的是锁，而锁不是依赖图，更不是产物。**
/// 第一条边界是它第一次跑就抓到的：`schannel`（Windows 的系统 TLS）**一直在根锁里**，
/// 由 `rustls-platform-verifier` 按 target 条件拖进来，而 G13 的分发是 Linux musl
/// ⇒ 它**一行都不会被编译进产物**。
///
/// # ★ ★ 第二条边界，而它推翻了本门自己写下的一句预言
///
/// 这段原文写着「出站 HTTPS 换完之后这张表会只剩 boring 那两条，**而这道门会先红一次**」——
/// **换完了，而它一个字都没红**：`hyper-rustls` / `rustls` / `tokio-rustls` / `schannel`
/// 仍然在锁里。根因是 **`Cargo.lock` 是依赖图的超集** —— `instant-acme` 的 `ring` feature
/// 里那句 `"hyper-rustls?/ring"` 让**包级解析照样把它写进锁**，尽管 feature 解析没打开它。
/// ✅ 隔离实验：一个只依赖 `instant-acme` 的空 crate，**锁里 129 个包而 `cargo tree` 只有 72 个**。
///
/// > ★ ★ **一道门的注释写「本门守 X」不等于它判得动 X** —— 这一次它连自己会不会红
/// > 都预言错了，而那句预言正是「换完之后会有人被迫来改这张表」的全部依据。
///
/// ⇒ 本门的口径就此收窄：**它答的是「锁里写着哪些」**。它仍然有用（唯一一道离线、零成本、
/// 连 fork 那把锁一起看的），但 ⚠ 别拿它回答「产物里有几套」，答依赖图的是**门 5**。
const EXPECTED_TLS_IMPLS: &[&str] = &["boring", "boring-sys", "rustls", "schannel"];

/// 一串包名里出现了哪些 TLS 实现（去重、排序）。
///
/// ★ 门 4（读锁）与门 5（读 `cargo tree`）**共用这一个判定**，
/// 于是「这个名字算不算一套 TLS」只有一份答案 —— 两份名单迟早会分家。
/// ⚠ ⚠ ★ ★ ★ **这里的 `dedup()` 是门 4 与门 5 共同的一处盲区，批 J 查实。**
/// 它按**包名**去重 ⇒ 这两道门答得了「图里/锁里出现了哪几个 TLS 实现」，
/// 答不了「**同一个名字有几个版本**」。而 G103 那条硬约束恰恰是后者：
/// `quiche` 与 `pingora-boringssl` **必须解到同一个 `boring`**，否则 `SslContextBuilder`
/// 同名而是两个类型。
/// ★ **今天守住它的不是这两道门，是编译器**（两边的类型要真的互相传递，
/// 解成两份就当场编不过）—— 见 `MUST_DISABLE_DEFAULT` 里 `quiche` 那一段。
/// ⚠ 写在这里是因为**这两道门读起来很像已经守住了它**：它们的名字里就有「几套 TLS」。
fn tls_impls_among<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = names
        .filter(|p| KNOWN_TLS_IMPLS.contains(p))
        .map(|p| p.to_string())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// 一把锁里出现了哪些 TLS 实现（去重、排序）。
fn tls_impls_in(lock: &str) -> Vec<String> {
    tls_impls_among(lock_packages(lock).into_iter())
}

#[test]
fn 锁里写着的_tls_实现必须与名单逐项相同() {
    // ★ 下界：抠不出包名时下面那个比较会拿两个空表相等，而它看起来是绿的。
    assert!(
        lock_packages(ROOT_LOCK).len() >= 100,
        "根锁里只抠出 {} 个包名，格式多半变了；本次检查不能采信",
        lock_packages(ROOT_LOCK).len()
    );
    let found = tls_impls_in(ROOT_LOCK);
    let expected: Vec<String> = EXPECTED_TLS_IMPLS.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        found, expected,
        "\n锁里写着的 TLS 实现与名单对不上。\n\
         · 多出来的：有人（多半是一条新依赖的 default feature）拖进了另一套 TLS。\n\
         · 少了的：很可能是好事 —— 有一处真的换掉了。\n\
           ⇒ 把 EXPECTED_TLS_IMPLS 改成新的事实，并在 PLAN.md §10 里记一笔。\n\
         ⚠ ⚠ 但**先看门 5**：锁是依赖图的超集，一个包可以留在锁里而没有任何人链接它\n\
           （实测：`instant-acme` 关掉 hyper-rustls 之后，锁里还有它）。\n\
         ⚠ 两种情况都不许靠「把名单改成实测值」了事而不写清楚为什么。"
    );
}

#[test]
fn tls_扫描器自证能命中也能错过() {
    let mk = |names: &[&str]| {
        names
            .iter()
            .map(|n| format!("[[package]]\nname = \"{n}\"\nversion = \"1.0.0\"\n"))
            .collect::<String>()
    };
    // 命中
    assert_eq!(tls_impls_in(&mk(&["rustls"])), vec!["rustls".to_string()]);
    assert_eq!(
        tls_impls_in(&mk(&["boring-sys", "boring"])),
        vec!["boring".to_string(), "boring-sys".to_string()]
    );
    // ★ ★ 错过：这四个**名字很像而不是 TLS 实现**，一个都不许被数进来。
    //   ⚠ `openssl-probe` 是其中最像的一个，而它只是去找系统证书文件在哪。
    assert!(
        tls_impls_in(&mk(&[
            "openssl-probe",
            "rustls-pki-types",
            "hyper-rustls",
            "pingora-boringssl",
            "ring",
        ]))
        .is_empty()
    );
    // 空输入不能被当成「查过了、没问题」——上面那条下界断言正是为此。
    assert!(tls_impls_in("").is_empty());
    // 同名重复只算一次（锁里同一个包可能出现多个版本）。
    assert_eq!(
        tls_impls_in(&mk(&["boring", "boring"])),
        vec!["boring".to_string()]
    );
}

// ── 门 3：产品 crate 依赖 pingora-core 时必须开 boringssl ──────────────
//
// ⚠ ⚠ ★ **（G104）：这道门原本叫「必须开 rustls」。**
//   §5.1 第 1 条把 TLS 后端锁死在 rustls，而那条约束已被 G104 推翻——
//   后端换成 BoringSSL（成因是 G103 取 quiche 做 HTTP/3）。
//   ★ **门要守的那件事一个字没变**：不开任何 TLS feature 的话，产品链接的是另一套代码，
//     而它那一批传递依赖会留在常规审计视野之外（G41 查出来 rustls 那条曾经
//     **两个月没被编译、测试或审计过**）。变的只是「哪个 feature 才算数」。

/// 一份清单里，`pingora-core` 有没有带上 `boringssl` feature。
fn pingora_without_boringssl(manifest: &str) -> Option<bool> {
    let mut found = None;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix("pingora-core") else {
            continue;
        };
        let rest = rest.trim_start();
        if !rest.starts_with('=') {
            continue;
        }
        found = Some(found.unwrap_or(false) || !rest.contains("\"boringssl\""));
    }
    found
}

#[test]
fn 产品_crate_一旦依赖_pingora_core_就必须开_boringssl() {
    // ✅ 这条**已经不是空跑的了**：`crates/fulcrum-server` 从此后真的依赖
    //    pingora-core，并且带着 `features = ["rustls"]`。
    //    ★ 打开它实测多解析出 46 个包（G41 当年量到的是 133 → 175），
    //    而 `aws-lc-rs` **不在其中**——门 1 那把锁负责证明这一点。
    let manifests = crate_manifests();
    assert!(manifests.len() >= 4, "只读到 {} 份清单", manifests.len());
    let mut depends = 0usize;
    for (path, text) in &manifests {
        match pingora_without_boringssl(text) {
            None => {}
            Some(true) => panic!(
                "{path} 依赖了 pingora-core 却没开 `boringssl` feature。\n\
                 G104 把 TLS 后端定为 BoringSSL；不开这个 feature，产品链接的是另一套代码，\n\
                 而它那一批传递依赖会留在常规审计视野之外。\n\
                 ⚠ 若你正在把它改回 `rustls`，请先读 `PLAN.md` §10：那是被推翻过一次的。"
            ),
            Some(false) => depends += 1,
        }
    }
    // ★ 下界：一个都没有的时候上面那个循环什么都没判，而它看起来是绿的。
    assert!(
        depends >= 1,
        "没有任何产品 crate 依赖 pingora-core —— 数据面不该消失。\n\
         若真的有意拆掉，请连这条断言一起改，别让它变成一条空跑的门。"
    );
}

#[test]
fn pingora_扫描器自证能命中也能错过() {
    let with = "pingora-core = { workspace = true, features = [\"boringssl\"] }";
    let without = "pingora-core = { workspace = true }";
    // ★ 旧口径必须被判成「没开」：G104 之后写 rustls 不算数，而**它看起来完全正常**。
    let old_backend = "pingora-core = { workspace = true, features = [\"rustls\"] }";
    let absent = "serde = \"1\"";
    assert_eq!(pingora_without_boringssl(with), Some(false));
    assert_eq!(pingora_without_boringssl(without), Some(true));
    assert_eq!(pingora_without_boringssl(old_backend), Some(true));
    assert_eq!(pingora_without_boringssl(absent), None);
}

// ── 门 5：**依赖图**里到底有几套 TLS —— 门 4 那把尺子够不到的那一半 ────────
//
// ★ ★ ★ **（G104 第 ③ 处）新加，而加它的理由是一次实测反常**：
//   ③ 做完之后，`hyper-rustls` / `rustls` / `tokio-rustls` / `schannel`
//   **全部离开了依赖图**（`cargo tree -e all --target all -i rustls` → nothing to print），
//   **而门 4 一个字都没红** —— 锁里它们还在。
//   ⇒ 那一批的全部内容，门 4 原理上看不见。
//
// ⚠ ⚠ **本门读的是 `cargo tree`，不是锁**：
//   · `-e all` —— normal + build + dev 三种边都算，一条藏在 build-dependency 里的
//     TLS 栈同样会被编译，同样要审计；
//   · `--target all` —— **不是**「只看 Linux」：`schannel` 当年就是靠 Windows 那一侧
//     进来的，只看本机 target 会让它隐形；
//   · `--locked` —— 拿一把过期的锁算出来的图不能采信，宁可红。
//
// ⚠ 本门答的仍然**不是**「产物里有哪些」：那要 `--target x86_64-unknown-linux-musl`
//   再加一步「真的被链接了」（看符号）。⏳ 那一条仍未做，登记在 PLAN.md §11。
//   ★ 但 `--target all` 是它的**超集**：这里为空，musl 那一格必然也为空。

/// 跑一次 `cargo tree`，拿回**真正在依赖图里**的包名（去重、排序）。
///
/// ⚠ ⚠ **拿不到就 panic，不许「跳过」**：本仓库反复抓到的形状正是
/// 「没能检查」被当成「检查通过」。
fn cargo_tree_packages() -> Vec<String> {
    // ★ `CARGO` 由 cargo 自己传给测试进程，比在 PATH 上碰运气可靠。
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let out = std::process::Command::new(&cargo)
        .arg("tree")
        .arg("--manifest-path")
        .arg(repo_root().join("Cargo.toml"))
        .args([
            "-e",
            "all",
            "--workspace",
            "--target",
            "all",
            "--locked",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ])
        .output()
        .unwrap_or_else(|e| panic!("跑不起来 `{cargo} tree`：{e}"));
    assert!(
        out.status.success(),
        "`cargo tree` 失败（退出码 {:?}）。\n\
         ⚠ 常见原因是 `--locked`：Cargo.lock 过期了。先跑一次构建让它更新，再看这道门。\n\
         stderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut names: Vec<String> = text
        .lines()
        // `{p}` 给的是 `name version (来源)`，取第一段。
        .filter_map(|l| l.split_whitespace().next())
        .map(|s| s.to_string())
        .collect();
    names.sort();
    names.dedup();
    names
}

#[test]
fn 依赖图里的_tls_实现必须只剩_boringssl() {
    let names = cargo_tree_packages();

    // ★ 下界一：抠不出包名时下面那个比较会拿两个空表相等，而它看起来是绿的。
    assert!(
        names.len() >= 100,
        "`cargo tree` 只抠出 {} 个包名，输出格式多半变了；本次检查不能采信",
        names.len()
    );
    // ★ 下界二：**光有数量不够**。一个把每行都截成空串的解析器同样能凑够数量，
    //   而它会让下面那条比较恒为「一个 TLS 都没有」——**恰好是我们希望看到的样子**。
    //   ⇒ 钉一个必然在图里、而与 TLS 无关的名字。
    assert!(
        names.iter().any(|n| n == "tokio"),
        "`cargo tree` 的输出里连 tokio 都没有 —— 解析多半是坏的，本次检查不能采信"
    );

    let found = tls_impls_among(names.iter().map(String::as_str));
    assert_eq!(
        found,
        vec!["boring".to_string(), "boring-sys".to_string()],
        "\n依赖图里的 TLS 实现变了。\n\
         · 多出来的：有人拖进了第二套 TLS —— 而 G104 的整条理由就是「让分家在结构上做不到」。\n\
           ⚠ 先查它是不是某条新依赖的 default feature（本仓库已现身五次）。\n\
         · 少了 boring：那是 TLS 后端整个不见了，不可能是好事。\n\
         ⇒ 改这条断言之前先去 PLAN.md §10 记一笔，别把判据改成实测值了事。"
    );
}

#[test]
fn 依赖图扫描器自证它看得见也数得对() {
    // ★ ★ 这一条守的是**上面那条断言的可信度**，不是产品：
    //   `tls_impls_among` 是门 4 与门 5 共用的判定，而门 4 的自证只喂过它假锁文本。
    //   这里再喂一次 `cargo tree` 那一侧的真实形状（裸包名，没有 `[[package]]` 包装）。
    let names = cargo_tree_packages();
    // 命中：boring 确实在图里，而这正是本批的产物。
    assert!(
        names.iter().any(|n| n == "boring"),
        "图里没有 boring —— 那门 5 的绿说明不了任何事"
    );
    // ★ ★ ★ 错过：`rustls-pki-types` **确实在图里**（`instant-acme` 的 ARI 要它），
    //   而它是一组 DER newtype、不是一套 TLS。
    //   ⇒ 这一条同时证明两件事：扫描器看得见真实的图，且**它不把名字像的算进来**。
    assert!(
        names.iter().any(|n| n == "rustls-pki-types"),
        "图里没有 rustls-pki-types —— 下面那条「名字像也不算」就没了对象"
    );
    assert!(
        !tls_impls_among(names.iter().map(String::as_str))
            .iter()
            .any(|n| n == "rustls-pki-types")
    );
}
