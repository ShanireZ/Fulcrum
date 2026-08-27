//! 下游响应头**只有一个出口**的结构门（G110）。
//!
//! G110 要求 h1/h2 的**每一条**响应都发 `Alt-Svc: h3=":<端口>"` ——
//! ⚠ 不发它，浏览器永远不会主动尝试 HTTP/3，整个 h3 入口对真实用户等于不存在。
//! 数据面有四处写响应头的地方，而第五处随时会出现 ⇒ 落地不是「四处各加一行」，
//! 而是把 `&mut ServerSession` 换成 `crate::Downstream`：它的**内在方法**
//! `write_response_header` 遮蔽了 `Deref` 过去的同名方法，既有调用点一个字不改，
//! 却从此都经过那一道。
//!
//! ⚠ 而**正是这处同名遮蔽让文本判据看不见区别**：`raw.write_response_header(…)` 与
//! `funnel.write_response_header(…)` 逐字相同，差别只在 `session` 的类型里 ——
//! 编译器知道，`grep` 不知道。⇒ 判据换成一句 `grep` 说得出、而且真的等价的话：
//! **漏斗之外的数据面根本拿不到一个裸 `ServerSession`。**
//!
//! 三道门各答一个不同的问题（★ 不许混着说）：
//!
//! | 门 | 它答的问题 | 它答不了的 |
//! |---|---|---|
//! | 1 | 漏斗**之外**的数据面文件里有没有出现 `ServerSession` | `lib.rs` 自己 |
//! | 2 | `lib.rs` 里裸 `ServerSession` 有没有跑到漏斗那段之外 | 运行时真的发了没有 |
//! | 3 | 那唯一的出口**是不是真的在加** `Alt-Svc` | 同上 |
//!
//! ⚠ 三道都是文本判据，一条都证不了「运行时真的发出去了」——
//! 那一半由 [`tests/h3/run.sh`] 拿真 curl 在六条响应路径上各量一次。
//!
//! 两处豁免：`src/quic/` 是 **h3 自己**（已经在 h3 上的客户端不需要被告知有 h3，
//! 那一层也不经过 `Downstream`）；`src/admin.rs` 听的是 unix socket，
//! 广播一个不存在的端口是**错的**而不是「多余的」。
//! ★ 豁免写成**路径前缀**而不是文件名清单 —— 清单会在有人新加一个文件时安静地少覆盖一格。

/// 仓库根。★ `CARGO_MANIFEST_DIR` 是 `crates/fulcrum`。
fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("找不到仓库根")
}

/// 递归列出一个目录下所有的 `.rs`，返回**相对仓库根**的路径。
///
/// ★ 读目录而不是写一张清单，理由与 `supply_gates::crate_manifests` 逐字相同。
fn rust_sources(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<String>) {
    for e in std::fs::read_dir(dir).expect("读不到目录") {
        let p = e.expect("目录项").path();
        if p.is_dir() {
            rust_sources(&p, root, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            let rel = p
                .strip_prefix(root)
                .expect("路径不在仓库根下")
                .to_string_lossy()
                .replace('\\', "/");
            out.push(rel);
        }
    }
}

/// 一个源文件里**产品代码**那一段（`#[cfg(test)]` 之后整段不算）。
///
/// ⚠ 判据自己造的桩（`quic::listener` 的 `EchoHandler` 等）写的是它们自己那条响应，
/// 与产品的响应路径无关。★ 切法取「第一处 `#[cfg(test)]` 之后全部丢掉」——
/// 本仓库的惯例是测试模块放在文件末尾，而**一个更聪明的切法（数花括号）
/// 会在它数错时静默地少看一整块产品代码**，那比切多了危险得多。
fn product_part(src: &str) -> &str {
    match src.find("#[cfg(test)]") {
        Some(i) => &src[..i],
        None => src,
    }
}

/// 丢掉行注释（`//` 到行尾，含 `///` 与 `//!`），只留代码。
///
/// # ★ ★ 为什么必须有这一步
///
/// 直接在原文上数 `mut ServerSession` 会多数出一处 —— 它在 `Downstream` 的
/// **文档注释**里，而那段话解释的正是「把 `&mut ServerSession` 换成这个类型」。
///
/// > ★ ★ **把不变量解释清楚，会踩响守这条不变量的门。**
/// > ⇒ 判据要看的是**代码**，不是**关于代码的话**。
///
/// ⚠ **它的边界写在明处**：字符串字面量里的 `//`（如 `"http://…"`）也会被当成注释起点，
/// 于是那一行的后半截被丢掉。⇒ 理论上会**漏判**「同一行里 `"http://…"` 之后
/// 才出现 `mut ServerSession`」这种写法。★ 那不是一个真实的签名写法，
/// 而换来的是不用在判据里塞一个 Rust 词法分析器 —— 代价与收益都写在这里，供下一个人推翻。
fn code_only(src: &str) -> String {
    src.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 数据面文件清单（已去掉两处豁免与 `lib.rs`）。
fn data_plane_files_outside_lib(root: &std::path::Path) -> Vec<String> {
    const EXEMPT_PREFIXES: &[&str] = &[
        "crates/fulcrum-server/src/quic/",
        "crates/fulcrum-server/src/admin.rs",
        // ⚠ `lib.rs` 是漏斗**自己住的地方**，由门 2 单独判 —— 它当然会出现 `ServerSession`。
        "crates/fulcrum-server/src/lib.rs",
    ];
    let mut files = Vec::new();
    rust_sources(&root.join("crates/fulcrum-server/src"), root, &mut files);
    files.sort();
    // ★ 下界断言：这道门必须真的读到了东西。
    //   ⚠ 少了它，一次路径写错会让它变成一道**永远绿的空门**。
    assert!(
        files.len() >= 10,
        "只扫到 {} 个源文件，路径大概写错了：{files:?}",
        files.len()
    );
    files
        .into_iter()
        .filter(|rel| !EXEMPT_PREFIXES.iter().any(|p| rel.starts_with(p)))
        .collect()
}

/// **门 1**：漏斗之外的数据面**根本拿不到**裸 `ServerSession`。
///
/// ★ ★ 这一句比「只许写一次响应头」更强，也更好判：拿不到那个类型，
/// `x.write_response_header(…)` 就**只可能**解析到 `Downstream` 那个内在方法。
/// ⇒ 「静态文件那条路会不会漏掉 `Alt-Svc`」从「靠人记着」变成了**类型系统的事**。
#[test]
fn 门1_漏斗之外的数据面拿不到裸_serversession() {
    let root = repo_root();
    let mut hits: Vec<(String, usize)> = Vec::new();
    for rel in data_plane_files_outside_lib(&root) {
        let src = std::fs::read_to_string(root.join(&rel)).expect("读不到源文件");
        let n = code_only(product_part(&src))
            .matches("ServerSession")
            .count();
        if n > 0 {
            hits.push((rel, n));
        }
    }
    assert!(
        hits.is_empty(),
        "★ 漏斗之外的数据面又出现裸 `ServerSession` 了：{hits:?}\n\
         G110 要求 h1/h2 的**每一条**响应都带 `Alt-Svc`，而拿着裸 session 写出去的那一条不会带 ——\n\
         ⚠ 它坏起来是无声的：响应完全正常，只是浏览器从此不知道这台机器会 h3。\n\
         ⇒ 把那个参数换成 `&mut crate::Downstream<'_>`；**调用点一个字都不用改**\n\
         （`Downstream` 对 `ServerSession` 有 `Deref`/`DerefMut`，而写响应头那一个是内在方法）。"
    );
}

/// **门 2**：`lib.rs` 里的裸 `ServerSession` **只活在漏斗那一段里**。
///
/// # ⚠ 为什么这一段要单独判
///
/// 门 1 把 `lib.rs` 排除在外了 —— 漏斗自己住在那里，它当然要碰裸类型。
/// 而 `lib.rs` 恰恰是数据面**最可能**新加一条响应路径的地方。
/// ⇒ 这里把范围收成一句可判的话：`mut ServerSession` 这个写法（`&mut` / `&'a mut` 都算）
/// **只许出现在 `Downstream` 那一段的定义里**。
///
/// ★ 用「段的起止标记」而不是一个数字：一个 `assert_eq!(…, 4)` 会在
/// 给漏斗加一个方法时变红，而那次红**说不出该改哪边**。
///
/// ⚠ 它判不动 UFCS 绕过（`ServerSession::write_response_header(&mut *out, …)`），
/// 所以那一条单独再判一次 —— 两句话，不是一句。
#[test]
fn 门2_lib里的裸_serversession_只活在漏斗那一段() {
    let root = repo_root();
    let src = std::fs::read_to_string(root.join("crates/fulcrum-server/src/lib.rs"))
        .expect("读不到 lib.rs");
    // ⚠ 先丢注释再找标记：注释里出现的 `mut ServerSession` 说的是**这件事本身**，
    //   而不是一条新的响应路径。理由见 `code_only` 的文档。
    let head = code_only(product_part(&src));
    let head = head.as_str();

    const SPAN_BEGIN: &str = "pub(crate) struct Downstream<'a> {";
    // ★ 段尾取 `DerefMut` 那个 impl 的最后一行 —— 漏斗的全部管道都在它之前。
    const SPAN_END: &str = "fn deref_mut(&mut self) -> &mut ServerSession {";

    let begin = head.find(SPAN_BEGIN).unwrap_or_else(|| {
        panic!("lib.rs 里找不到漏斗的起点标记 `{SPAN_BEGIN}` —— 它被改名或删掉了？")
    });
    let end_marker = head
        .find(SPAN_END)
        .unwrap_or_else(|| panic!("lib.rs 里找不到漏斗的终点标记 `{SPAN_END}`"));
    assert!(begin < end_marker, "漏斗那一段的起止标记顺序反了");
    // 段尾再往后吃到那一行结束（`deref_mut` 的函数体只有一行 `self.session`）。
    let end = head[end_marker..]
        .find("\n    }\n")
        .map(|i| end_marker + i + "\n    }\n".len())
        .expect("`deref_mut` 的函数体收不了尾");

    let outside = format!("{}{}", &head[..begin], &head[end..]);
    let n = outside.matches("mut ServerSession").count();
    assert_eq!(
        n, 0,
        "★ `lib.rs` 的漏斗之外出现了 {n} 处裸 `mut ServerSession`。\n\
         ⚠ 两种情况长得一样，而处置相反：\n\
           · 你在给 `Downstream` **本身**加方法 ⇒ 把它挪进那一段里（起止标记之间）；\n\
           · 你在给数据面**新写一条响应路径** ⇒ 那正是本门要拦的：换成 `&mut Downstream<'_>`，\n\
             否则这条路上的响应不会带 `Alt-Svc`，而它坏起来完全无声。"
    );

    // ⚠ 另一条绕过：`Deref` 让 `&mut *out` 就是一个裸 session，UFCS 调过去谁都拦不住。
    //   ★ 它与上面那条**不是同一句话** —— 上面那条判「有没有人拿到裸类型」，
    //     这条判「有没有人绕过内在方法」。
    assert!(
        !head.contains("ServerSession::write_response_header"),
        "★ 有人用 UFCS 绕过了漏斗（`ServerSession::write_response_header(…)`）。\n\
         那条响应不会带 `Alt-Svc`。⇒ 直接调 `Downstream::write_response_header`。"
    );
}

/// **门 3**：那唯一的出口**真的在加** `Alt-Svc`。
///
/// ⚠ ⚠ ★ 门 1 与门 2 只判「只有一个出口」，**判不动那个出口做了什么** ——
/// 把 `insert_header("Alt-Svc", …)` 整行删掉，它们照样全绿。
/// ★ 这正是本仓库第 67/68 轮连着记两次的那条：
/// **一道门的名字比它的断言宽，与它根本不存在的差别，只在出事那天才看得出来。**
#[test]
fn 门3_那唯一的出口真的在加_alt_svc() {
    let src = std::fs::read_to_string(repo_root().join("crates/fulcrum-server/src/lib.rs"))
        .expect("读不到 lib.rs");
    // ⚠ 同样只看代码：把那一行**注释掉**与把它删掉，对 G110 是同一件事。
    assert!(
        code_only(product_part(&src)).contains(r#"insert_header("Alt-Svc""#),
        "★ 那唯一的出口里没有在写 `Alt-Svc` 了。\n\
         ⚠ 出口还在、前两道门还是绿的，而 G110 那一半已经没了 —— \n\
         「只有一个出口」与「那个出口做对了事」是两句话。"
    );
}
