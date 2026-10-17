//! 供應鏈的五道結構門。
//!
//! ★ 起因：`instant-acme` 的 **default feature 裡就含 `aws-lc-rs`**（`ring` 要顯式開），
//! 照預設寫一行依賴就會把它悄悄拉回來。「上游 crate 的 `default` 是一條靜默的供應鏈入口」
//! 在本儲存庫現身過四次，前三次都是**發現之後**才知道自己一直在付代價 ——
//! 所以它要有一道門，不能只寫在註解裡。
//!
//! # 判據掛在**鎖**上，不掛在清單文字上
//!
//! 門 1 查的是「兩把 `Cargo.lock` 裡有沒有 `aws-lc-rs`」，不是「有沒有人忘寫
//! `default-features = false`」：後者只覆蓋一種寫法，換個中間依賴或 feature 名就看不見了，
//! 而結果一樣。清單文字那兩道門是第二層，不是第一層。
//!
//! # 五道門各答一個**不同**的問題（★ 不該混著說）
//!
//! | 門 | 它答的問題 |
//! |---|---|
//! | 1 | 兩把**鎖**裡有沒有 `aws-lc-rs` |
//! | 2 | **清單文字**裡 `instant-acme` 等有沒有關掉 default |
//! | 3 | 產品 crate 依賴 `pingora-core` 時有沒有開 `boringssl` |
//! | 4 | **鎖裡寫著**哪幾套 TLS 實作 |
//! | 5 | **依賴圖裡真有**哪幾套 TLS 實作（`cargo tree`）|
//!
//! ⚠ ⚠ **門 4 與門 5 不是一回事**：`Cargo.lock` 是依賴圖的**超集** —— 關掉
//! `instant-acme` 的 `hyper-rustls` feature 之後，`hyper-rustls` / `rustls` / `schannel`
//! 仍然在鎖裡，卻已經沒有任何人連結它們。門 4 因此對那一次改動原理上是瞎的
//! （它在自己的註解裡預言會紅，而它沒紅）。詳見門 4 那一段。
//!
//! **一個從沒紅過的門與不存在的門無法區分**，所以每一條的判定函式都由本檔案裡的固定輸入
//! 證明過「能命中也能錯過」；每一條還帶一個**下界斷言** —— 讀不到清單、或一個 crate 都沒
//! 匹配上時，它會在什麼都沒查的情況下報綠。

/// 根 workspace 的鎖。
const ROOT_LOCK: &str = include_str!("../../../Cargo.lock");
/// ★ fork 的鎖。**它才是要緊的那一把**：`[patch.crates-io]` 把 `pingora-core`
/// 指向 vendor，根鎖裡連一個 rustls 相關的包都沒有。
const VENDOR_LOCK: &str = include_str!("../../../vendor/pingora/Cargo.lock");

const ROOT_MANIFEST: &str = include_str!("../../../Cargo.toml");

/// 儲存庫根。★ `CARGO_MANIFEST_DIR` 是 `crates/fulcrum`。
fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("找不到仓库根")
}

/// **執行環境**列出 `crates/*/Cargo.toml`。
///
/// ★ ★ 為什麼不用一串 `include_str!`：那是一張**手寫清單**，
/// 而新加一個 crate 時沒人會想起來往裡加一行——於是這道門會安靜地少覆蓋一個 crate，
/// 看起來照樣是綠的。這與 vendor 迴歸網那張 16 項手寫名單、`unclaimed.sh` 的忽略名單
/// 是同一個形狀：**不自省的名單**。
/// ⇒ 讀目錄，沒有名單可維護；而下面那條下界斷言負責證明它真的讀到了東西。
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

/// 從 `Cargo.lock` 裡摳出全部包名。
fn lock_packages(lock: &str) -> Vec<&str> {
    lock.lines()
        .filter_map(|l| l.strip_prefix("name = \""))
        .filter_map(|l| l.strip_suffix('"'))
        .collect()
}

// ── 門 1：aws-lc-rs 不許回來 ────────────────────────────────────────────────

#[test]
fn 两把锁里都不许出现_aws_lc_rs() {
    for (which, lock) in [("根", ROOT_LOCK), ("vendor/pingora", VENDOR_LOCK)] {
        let pkgs = lock_packages(lock);
        // ★ 先釘住「確實讀到了東西」。摳不出包名時下面的 `any` 恆為假，
        //   這道門會在**什麼都沒查**的情況下報綠。
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
    // 空輸入不能被當成「查過了、沒問題」——這正是上面那條下界斷言存在的理由。
    assert!(lock_packages("").is_empty());
}

// ── 門 2：已知會拖進 crypto provider 的依賴必須關掉 default ─────────────────

/// 這些 crate 的 default feature 會自己挑一個 crypto provider。
/// ★ 名單會長，但它**不是**判據的全部——判據是門 1 那把鎖。這裡只是讓人早一步看見。
const MUST_DISABLE_DEFAULT: &[&str] = &[
    "instant-acme",
    "rustls",
    "tokio-rustls",
    "quinn",
    "rcgen",
    // ⚠ 批 8 加：它的 default 是
    //   `["native-tokio","http1","tls12","logging","aws-lc-rs"]` —— **含 aws-lc-rs**。
    "hyper-rustls",
    // ⚠ ⚠ 批 J 加（G103）：**它進這張名單的理由與上面幾條都不一樣，要說清。**
    //   上面幾條是「default 會把一個我們不要的 provider 拖進來」；
    //   `quiche` 的 default（`["boringssl-boring-crate"]`）**正好就是我們要的那一個**。
    //   它進來是因為**另一條岔路**：`boringssl-vendored` 會讓 quiche 自己從原始碼編一份
    //   BoringSSL ⇒ 產物裡兩套，而「統一 BoringSSL」（G104）按字面讀就成了假話。
    //   ⇒ 本條守的是**上游哪天改了 default**，僅此而已。
    //   ⚠ 2026-09-24 更正：quiche 0.29.3 / 0.30.0 的特性表裡**沒有** `boringssl-vendored`
    //     （查 crates.io）⇒ 下文把它當成現有特性來寫的地方，一律按「假如上游哪天加了這樣一個特性」讀。
    //
    // ⚠ ⚠ **門 5 守不住 `boringssl-vendored` 那一半**：門 4 與門 5 都對包名做了
    //   `dedup()`，判的是「出現了哪幾個**名字**」，判不動「同一個名字有幾個**版本**」；
    //   而 `boringssl-vendored` 根本不新增任何包名（BoringSSL 在 quiche 自己的 build
    //   script 裡編）⇒ **兩種情形門 5 都是綠的**。
    //   ★ ★ **真正守住這條約束的是編譯器**：`SniResolver::install_into()` 收的是
    //   `pingora_boringssl::ssl::SslContextBuilder`，而它要被交給
    //   `quiche::Config::with_boring_ssl_ctx_builder()` —— 兩邊不是同一個 `boring`
    //   就是兩個同名類型，**當場編不過**；feature 翻到 vendored 那一側時，
    //   我們調的那個 API 乾脆不存在，同樣編不過。
    //   ⇒ 這正是 D18/G66 那條「**讓分家在結構上做不到**」，只是這次執行者是類型系統。
    //   ⚠ **仍然有一條它蓋不住的縫**：feature 是**並集**的 —— 若將來有別的 crate
    //     也依賴 quiche 並開了 `boringssl-vendored`，兩個 feature 會同時打開，
    //     我們調的 API 還在、編得過，而產物裡可能真有兩份 BoringSSL。
    //     今天全 workspace 只有 `fulcrum-server` 一個消費者，所以這條縫是空的；
    //     ★ **第二個消費者出現的那一天，這裡要重新想一遍**（登記在 PLAN.md §7 批 J）。
    "quiche",
];

/// 一份清單裡，某個依賴有沒有關掉 default features。
///
/// 回傳 `None` 表示這份清單根本沒宣告它。
///
/// ★ ★ **`workspace = true` 算作「已經關掉了」**，因為 feature 的口徑整個繼承自根清單，
/// 而根清單本身也在被掃的名單裡——**同一個事實只在一個地方判**。
///
/// ⚠ 這一條是被這道門自己的**假警報**逼出來的：`fulcrum-acme` 裡寫的是
/// `instant-acme = { workspace = true }`，根清單裡寫的是
/// `instant-acme = { version = "…", default-features = false, features = [...] }`——
/// 依賴圖裡**沒有** aws-lc-rs（門 1 那把鎖作證），而門 2 照樣判紅。
/// ★ 那正是本儲存庫反覆撞見的那個形狀：**判據只認得一種寫法**。
/// 而假警報比沒有警報更糟——它會訓練人忽略這道門，連帶把真的那次一起埋掉。
fn declares_without_default_off(manifest: &str, dep: &str) -> Option<bool> {
    let mut found = None;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue; // ★ 註解掉的一行不算宣告
        }
        let Some(rest) = line.strip_prefix(dep) else {
            continue;
        };
        let rest = rest.trim_start();
        if !rest.starts_with('=') {
            continue; // `instant-acme-foo = …` 之類，不是它
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
    // ★ 先釘住「確實讀到了東西」：讀不到 crates/ 時下面的循環一次都不跑，
    //   這道門會在**什麼都沒查**的情況下報綠。
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
    // ★ workspace 繼承：口徑在根清單上，而根清單本身也在被掃的名單裡。
    //   ⚠ 沒有這一條，這道門會對**本儲存庫當前的真實寫法**報假警——實際發生過。
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

// ── 門 4：依賴圖裡到底有幾套 TLS **實作** —— 兩個方向都嚴 ────────────────
//
// ★ ★ ★ **（G104）新加的一道門，而它補的是一個此前沒有任何東西看著的洞。**
//   §10 的作廢清單第 ⑥ 項寫著：門 1/2/3 那套口徑**整個建在 rustls 上**，
//   而它們**對 boring 結構上說不出話** —— 門 1 只認 `aws-lc-rs` 這個名字。
//   ⇒ 換後端的遷移期裡「有幾套 TLS」沒有任何判據，**而遷移期恰恰是兩套都在的時候**。
//
// ⚠ ⚠ **兩個方向都嚴**（照 `tests/vendor/run.sh` 的 EXPECTED_FAILURES 那套口徑）：
//   多一個要紅（有人悄悄拖進了第三套），**少一個也要紅**。
//   ★ 後者不是潔癖：owner 拍的是「三處全換、rustls 徹底出局」，
//     而那件事做完的那一刻**應該有人被迫來改這張表** —— 那正是它值得被記一筆的時候。
//     一道只在「變多」時才紅的門，會讓「變少」這件好事悄悄發生、沒人知道它發生了。

/// 已知的 TLS **實作** crate。
///
/// ⚠ ⚠ **只列實作，不列類型定義、不列適配層。** 這條邊界是判據的一部分：
///   · `rustls-pki-types` 是一組 DER newtype，沒有 crypto ⇒ **不算**；
///   · `hyper-rustls` / `tokio-rustls` / `pingora-rustls` / `pingora-boringssl`
///     是適配層，它們各自的實作已經在名單裡 ⇒ **不算**（否則同一套 TLS 被數好幾遍）；
///   · `openssl-probe` 只是去找系統證書檔案在哪 ⇒ **不算**（★ 名字最像的一個）；
///   · `ring` 是**演算法庫**不是 TLS 棧，rustls 用它當 provider ⇒ 不單獨算一套。
/// ★ 名單會長，而**長了不要緊**：它只決定「這個名字算不算一套 TLS」，
///   真正的判據是下面那張 `EXPECTED` 與實測結果逐項相同。
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

/// 今天**允許**出現在根鎖裡的那些，按字典序。
///
/// ⚠ ⚠ ★ **這道門讀的是鎖，而鎖不是依賴圖，更不是產物。**
/// 第一條邊界是它第一次跑就抓到的：`schannel`（Windows 的系統 TLS）**一直在根鎖裡**，
/// 由 `rustls-platform-verifier` 按 target 條件拖進來，而 G13 的散布是 Linux musl
/// ⇒ 它**一行都不會被編譯進產物**。
///
/// # ★ ★ 第二條邊界，而它推翻了本門自己寫下的一句預言
///
/// 這段原文寫著「出站 HTTPS 換完之後這張表會只剩 boring 那兩條，**而這道門會先紅一次**」——
/// **換完了，而它一個字都沒紅**：`hyper-rustls` / `rustls` / `tokio-rustls` / `schannel`
/// 仍然在鎖裡。根因是 **`Cargo.lock` 是依賴圖的超集** —— `instant-acme` 的 `ring` feature
/// 裡那句 `"hyper-rustls?/ring"` 讓**包級解析照樣把它寫進鎖**，儘管 feature 解析沒打開它。
/// ✅ 隔離實驗：一個只依賴 `instant-acme` 的空 crate，**鎖裡 129 個包而 `cargo tree` 只有 72 個**。
///
/// > ★ ★ **一道門的註解寫「本門守 X」不等於它判得動 X** —— 這一次它連自己會不會紅
/// > 都預言錯了，而那句預言正是「換完之後會有人被迫來改這張表」的全部依據。
///
/// ⇒ 本門的口徑就此收窄：**它答的是「鎖裡寫著哪些」**。它仍然有用（唯一一道離線、零成本、
/// 連 fork 那把鎖一起看的），但 ⚠ 別拿它回答「產物裡有幾套」，答依賴圖的是**門 5**。
const EXPECTED_TLS_IMPLS: &[&str] = &["boring", "boring-sys", "rustls", "schannel"];

/// 一串包名裡出現了哪些 TLS 實作（去重、排序）。
///
/// ★ 門 4（讀鎖）與門 5（讀 `cargo tree`）**共用這一個判定**，
/// 於是「這個名字算不算一套 TLS」只有一份答案 —— 兩份名單遲早會分家。
/// ⚠ ⚠ ★ ★ ★ **這裡的 `dedup()` 是門 4 與門 5 共同的一處盲區，批 J 查實。**
/// 它按**包名**去重 ⇒ 這兩道門答得了「圖裡/鎖裡出現了哪幾個 TLS 實作」，
/// 答不了「**同一個名字有幾個版本**」。而 G103 那條硬約束恰恰是後者：
/// `quiche` 與 `pingora-boringssl` **必須解到同一個 `boring`**，否則 `SslContextBuilder`
/// 同名而是兩個類型。
/// ★ **今天守住它的不是這兩道門，是編譯器**（兩邊的類型要真的互相傳遞，
/// 解成兩份就當場編不過）—— 見 `MUST_DISABLE_DEFAULT` 裡 `quiche` 那一段。
/// ⚠ 寫在這裡是因為**這兩道門讀起來很像已經守住了它**：它們的名字裡就有「幾套 TLS」。
fn tls_impls_among<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = names
        .filter(|p| KNOWN_TLS_IMPLS.contains(p))
        .map(|p| p.to_string())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// 一把鎖裡出現了哪些 TLS 實作（去重、排序）。
fn tls_impls_in(lock: &str) -> Vec<String> {
    tls_impls_among(lock_packages(lock).into_iter())
}

#[test]
fn 锁里写着的_tls_实现必须与名单逐项相同() {
    // ★ 下界：摳不出包名時下面那個比較會拿兩個空表相等，而它看起來是綠的。
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
    // ★ ★ 錯過：這四個**名字很像而不是 TLS 實作**，一個都不許被數進來。
    //   ⚠ `openssl-probe` 是其中最像的一個，而它只是去找系統證書檔案在哪。
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
    // 空輸入不能被當成「查過了、沒問題」——上面那條下界斷言正是為此。
    assert!(tls_impls_in("").is_empty());
    // 同名重複只算一次（鎖裡同一個包可能出現多個版本）。
    assert_eq!(
        tls_impls_in(&mk(&["boring", "boring"])),
        vec!["boring".to_string()]
    );
}

// ── 門 3：產品 crate 依賴 pingora-core 時必須開 boringssl ──────────────
//
// ⚠ ⚠ ★ **（G104）：這道門原本叫「必須開 rustls」。**
//   §5.1 第 1 條把 TLS 後端鎖死在 rustls，而那條約束已被 G104 推翻——
//   後端換成 BoringSSL（成因是 G103 取 quiche 做 HTTP/3）。
//   ★ **門要守的那件事一個字沒變**：不開任何 TLS feature 的話，產品連結的是另一套程式碼，
//     而它那一批傳遞依賴會留在常規稽核視野之外（G41 查出來 rustls 那條曾經
//     **兩個月沒被編譯、測試或稽核過**）。變的只是「哪個 feature 才算數」。

/// 一份清單裡，`pingora-core` 有沒有帶上 `boringssl` feature。
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
    // ✅ 這條**已經不是空跑的了**：`crates/fulcrum-server` 從此後真的依賴
    //    pingora-core，並且帶著 `features = ["rustls"]`。
    //    ★ 打開它實測多解析出 46 個包（G41 當年量到的是 133 → 175），
    //    而 `aws-lc-rs` **不在其中**——門 1 那把鎖負責證明這一點。
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
    // ★ 下界：一個都沒有的時候上面那個循環什麼都沒判，而它看起來是綠的。
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
    // ★ 舊口徑必須被判成「沒開」：G104 之後寫 rustls 不算數，而**它看起來完全正常**。
    let old_backend = "pingora-core = { workspace = true, features = [\"rustls\"] }";
    let absent = "serde = \"1\"";
    assert_eq!(pingora_without_boringssl(with), Some(false));
    assert_eq!(pingora_without_boringssl(without), Some(true));
    assert_eq!(pingora_without_boringssl(old_backend), Some(true));
    assert_eq!(pingora_without_boringssl(absent), None);
}

// ── 門 5：**依賴圖**裡到底有幾套 TLS —— 門 4 那把尺子夠不到的那一半 ────────
//
// ★ ★ ★ **（G104 第 ③ 處）新加，而加它的理由是一次實測反常**：
//   ③ 做完之後，`hyper-rustls` / `rustls` / `tokio-rustls` / `schannel`
//   **全部離開了依賴圖**（`cargo tree -e all --target all -i rustls` → nothing to print），
//   **而門 4 一個字都沒紅** —— 鎖裡它們還在。
//   ⇒ 那一批的全部內容，門 4 原理上看不見。
//
// ⚠ ⚠ **本門讀的是 `cargo tree`，不是鎖**：
//   · `-e all` —— normal + build + dev 三種邊都算，一條藏在 build-dependency 裡的
//     TLS 棧同樣會被編譯，同樣要稽核；
//   · `--target all` —— **不是**「只看 Linux」：`schannel` 當年就是靠 Windows 那一側
//     進來的，只看本機 target 會讓它隱形；
//   · `--locked` —— 拿一把過期的鎖算出來的圖不能採信，寧可紅。
//
// ⚠ 本門答的仍然**不是**「產物裡有哪些」：那要 `--target x86_64-unknown-linux-musl`
//   再加一步「真的被連結了」（看符號）。⏳ 那一條仍未做，登記在 PLAN.md §11。
//   ★ 但 `--target all` 是它的**超集**：這裡為空，musl 那一格必然也為空。

/// 跑一次 `cargo tree`，拿回**真正在依賴圖裡**的包名（去重、排序）。
///
/// ⚠ ⚠ **拿不到就 panic，不許「跳過」**：本儲存庫反覆抓到的形狀正是
/// 「沒能檢查」被當成「檢查通過」。
fn cargo_tree_packages() -> Vec<String> {
    // ★ `CARGO` 由 cargo 自己傳給測試處理程序，比在 PATH 上碰運氣可靠。
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
        // `{p}` 給的是 `name version (来源)`，取第一段。
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

    // ★ 下界一：摳不出包名時下面那個比較會拿兩個空表相等，而它看起來是綠的。
    assert!(
        names.len() >= 100,
        "`cargo tree` 只抠出 {} 个包名，输出格式多半变了；本次检查不能采信",
        names.len()
    );
    // ★ 下界二：**光有數量不夠**。一個把每行都截成空串的解析器同樣能湊夠數量，
    //   而它會讓下面那條比較恆為「一個 TLS 都沒有」——**恰好是我們希望看到的樣子**。
    //   ⇒ 釘一個必然在圖裡、而與 TLS 無關的名字。
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
    // ★ ★ 這一條守的是**上面那條斷言的可信度**，不是產品：
    //   `tls_impls_among` 是門 4 與門 5 共用的判定，而門 4 的自證只餵過它假鎖文字。
    //   這裡再喂一次 `cargo tree` 那一側的真實形狀（裸包名，沒有 `[[package]]` 包裝）。
    let names = cargo_tree_packages();
    // 命中：boring 確實在圖裡，而這正是本批的產物。
    assert!(
        names.iter().any(|n| n == "boring"),
        "图里没有 boring —— 那门 5 的绿说明不了任何事"
    );
    // ★ ★ ★ 錯過：`rustls-pki-types` **確實在圖裡**（`instant-acme` 的 ARI 要它），
    //   而它是一組 DER newtype、不是一套 TLS。
    //   ⇒ 這一條同時證明兩件事：掃描器看得見真實的圖，且**它不把名字像的算進來**。
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
