//! 下游回應頭**只有一個出口**的結構門（G110）。
//!
//! G110 要求 h1/h2 的**每一條**回應都發 `Alt-Svc: h3=":<端口>"` ——
//! ⚠ 不發它，瀏覽器永遠不會主動嘗試 HTTP/3，整個 h3 入口對真實使用者等於不存在。
//! 資料面有四處寫回應頭的地方，而第五處隨時會出現 ⇒ 完成實作不是「四處各加一行」，
//! 而是把 `&mut ServerSession` 換成 `crate::Downstream`：它的**內在方法**
//! `write_response_header` 遮蔽了 `Deref` 過去的同名方法，既有呼叫點一個字不改，
//! 卻從此都經過那一道。
//!
//! ⚠ 而**正是這處同名遮蔽讓文字判據看不見區別**：`raw.write_response_header(…)` 與
//! `funnel.write_response_header(…)` 逐字相同，差別只在 `session` 的類型裡 ——
//! 編譯器知道，`grep` 不知道。⇒ 判據換成一句 `grep` 說得出、而且真的等價的話：
//! **漏斗之外的資料面根本拿不到一個裸 `ServerSession`。**
//!
//! 三道門各答一個不同的問題（★ 不許混著說）：
//!
//! | 門 | 它答的問題 | 它答不了的 |
//! |---|---|---|
//! | 1 | 漏斗**之外**的資料面檔案裡有沒有出現 `ServerSession` | `lib.rs` 自己 |
//! | 2 | `lib.rs` 裡裸 `ServerSession` 有沒有跑到漏斗那段之外 | 執行環境真的發了沒有 |
//! | 3 | 那唯一的出口**是不是真的在加** `Alt-Svc` | 同上 |
//!
//! ⚠ 三道都是文字判據，一條都證不了「執行環境真的發出去了」——
//! 那一半由 [`tests/h3/run.sh`] 拿真 curl 在六條回應路徑上各量一次。
//!
//! 兩處豁免：`src/quic/` 是 **h3 自己**（已經在 h3 上的用戶端不需要被告知有 h3，
//! 那一層也不經過 `Downstream`）；`src/admin.rs` 聽的是 unix socket，
//! 廣播一個不存在的連接埠是**錯的**而不是「多餘的」。
//! ★ 豁免寫成**路徑前綴**而不是檔案名清單 —— 清單會在有人新加一個檔案時安靜地少覆蓋一格。

/// 儲存庫根。★ `CARGO_MANIFEST_DIR` 是 `crates/fulcrum`。
fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("找不到仓库根")
}

/// 遞歸列出一個目錄下所有的 `.rs`，回傳**相對儲存庫根**的路徑。
///
/// ★ 讀目錄而不是寫一張清單，理由與 `supply_gates::crate_manifests` 逐字相同。
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

/// 一個原始檔裡**產品程式碼**那一段（`#[cfg(test)]` 之後整段不算）。
///
/// ⚠ 判據自己造的樁（`quic::listener` 的 `EchoHandler` 等）寫的是它們自己那條回應，
/// 與產品的回應路徑無關。★ 切法取「第一處 `#[cfg(test)]` 之後全部丟掉」——
/// 本儲存庫的慣例是測試模組放在檔案末尾，而**一個更聰明的切法（數花括號）
/// 會在它數錯時靜默地少看一整塊產品程式碼**，那比切多了危險得多。
fn product_part(src: &str) -> &str {
    match src.find("#[cfg(test)]") {
        Some(i) => &src[..i],
        None => src,
    }
}

/// 丟掉行註解（`//` 到行尾，含 `///` 與 `//!`），只留程式碼。
///
/// # ★ ★ 為什麼必須有這一步
///
/// 直接在原文上數 `mut ServerSession` 會多數出一處 —— 它在 `Downstream` 的
/// **文件註解**裡，而那段話解釋的正是「把 `&mut ServerSession` 換成這個類型」。
///
/// > ★ ★ **把不變數解釋清楚，會踩響守這條不變數的門。**
/// > ⇒ 判據要看的是**程式碼**，不是**關於程式碼的話**。
///
/// ⚠ **它的邊界寫在明處**：字串字面值裡的 `//`（如 `"http://…"`）也會被當成註解起點，
/// 於是那一行的後半截被丟掉。⇒ 理論上會**漏判**「同一行裡 `"http://…"` 之後
/// 才出現 `mut ServerSession`」這種寫法。★ 那不是一個真實的簽名寫法，
/// 而換來的是不用在判據裡塞一個 Rust 詞法分析器 —— 代價與收益都寫在這裡，供下一個人推翻。
fn code_only(src: &str) -> String {
    src.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 資料面檔案清單（已去掉兩處豁免與 `lib.rs`）。
fn data_plane_files_outside_lib(root: &std::path::Path) -> Vec<String> {
    const EXEMPT_PREFIXES: &[&str] = &[
        "crates/fulcrum-server/src/quic/",
        "crates/fulcrum-server/src/admin.rs",
        // ⚠ `lib.rs` 是漏斗**自己住的地方**，由門 2 單獨判 —— 它當然會出現 `ServerSession`。
        "crates/fulcrum-server/src/lib.rs",
    ];
    let mut files = Vec::new();
    rust_sources(&root.join("crates/fulcrum-server/src"), root, &mut files);
    files.sort();
    // ★ 下界斷言：這道門必須真的讀到了東西。
    //   ⚠ 少了它，一次路徑寫錯會讓它變成一道**永遠綠的空門**。
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

/// **門 1**：漏斗之外的資料面**根本拿不到**裸 `ServerSession`。
///
/// ★ ★ 這一句比「只許寫一次回應頭」更強，也更好判：拿不到那個類型，
/// `x.write_response_header(…)` 就**只可能**解析到 `Downstream` 那個內在方法。
/// ⇒ 「靜態檔案那條路會不會漏掉 `Alt-Svc`」從「靠人記著」變成了**類型系統的事**。
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

/// **門 2**：`lib.rs` 裡的裸 `ServerSession` **只活在漏斗那一段裡**。
///
/// # ⚠ 為什麼這一段要單獨判
///
/// 門 1 把 `lib.rs` 排除在外了 —— 漏斗自己住在那裡，它當然要碰裸類型。
/// 而 `lib.rs` 恰恰是資料面**最可能**新加一條回應路徑的地方。
/// ⇒ 這裡把範圍收成一句可判的話：`mut ServerSession` 這個寫法（`&mut` / `&'a mut` 都算）
/// **只許出現在 `Downstream` 那一段的定義裡**。
///
/// ★ 用「段的起止標記」而不是一個數字：一個 `assert_eq!(…, 4)` 會在
/// 給漏斗加一個方法時變紅，而那次紅**說不出該改哪邊**。
///
/// ⚠ 它判不動 UFCS 繞過（`ServerSession::write_response_header(&mut *out, …)`），
/// 所以那一條單獨再判一次 —— 兩句話，不是一句。
#[test]
fn 门2_lib里的裸_serversession_只活在漏斗那一段() {
    let root = repo_root();
    let src = std::fs::read_to_string(root.join("crates/fulcrum-server/src/lib.rs"))
        .expect("读不到 lib.rs");
    // ⚠ 先丟註解再找標記：註解裡出現的 `mut ServerSession` 說的是**這件事本身**，
    //   而不是一條新的回應路徑。理由見 `code_only` 的文件。
    let head = code_only(product_part(&src));
    let head = head.as_str();

    const SPAN_BEGIN: &str = "pub(crate) struct Downstream<'a> {";
    // ★ 段尾取 `DerefMut` 那個 impl 的最後一行 —— 漏斗的全部管道都在它之前。
    const SPAN_END: &str = "fn deref_mut(&mut self) -> &mut ServerSession {";

    let begin = head.find(SPAN_BEGIN).unwrap_or_else(|| {
        panic!("lib.rs 里找不到漏斗的起点标记 `{SPAN_BEGIN}` —— 它被改名或删掉了？")
    });
    let end_marker = head
        .find(SPAN_END)
        .unwrap_or_else(|| panic!("lib.rs 里找不到漏斗的终点标记 `{SPAN_END}`"));
    assert!(begin < end_marker, "漏斗那一段的起止标记顺序反了");
    // 段尾再往後吃到那一行結束（`deref_mut` 的函式體只有一行 `self.session`）。
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

    // ⚠ 另一條繞過：`Deref` 讓 `&mut *out` 就是一個裸 session，UFCS 調過去誰都攔不住。
    //   ★ 它與上面那條**不是同一句話** —— 上面那條判「有沒有人拿到裸類型」，
    //     這條判「有沒有人繞過內在方法」。
    assert!(
        !head.contains("ServerSession::write_response_header"),
        "★ 有人用 UFCS 绕过了漏斗（`ServerSession::write_response_header(…)`）。\n\
         那条响应不会带 `Alt-Svc`。⇒ 直接调 `Downstream::write_response_header`。"
    );
}

/// **門 3**：那唯一的出口**真的在加** `Alt-Svc`。
///
/// ⚠ ⚠ ★ 門 1 與門 2 只判「只有一個出口」，**判不動那個出口做了什麼** ——
/// 把 `insert_header("Alt-Svc", …)` 整行刪掉，它們照樣全綠。
/// ★ 這正是本儲存庫第 67/68 輪連著記兩次的那條：
/// **一道門的名字比它的斷言寬，與它根本不存在的差別，只在出事那天才看得出來。**
#[test]
fn 门3_那唯一的出口真的在加_alt_svc() {
    let src = std::fs::read_to_string(repo_root().join("crates/fulcrum-server/src/lib.rs"))
        .expect("读不到 lib.rs");
    // ⚠ 同樣只看程式碼：把那一行**註解掉**與把它刪掉，對 G110 是同一件事。
    assert!(
        code_only(product_part(&src)).contains(r#"insert_header("Alt-Svc""#),
        "★ 那唯一的出口里没有在写 `Alt-Svc` 了。\n\
         ⚠ 出口还在、前两道门还是绿的，而 G110 那一半已经没了 —— \n\
         「只有一个出口」与「那个出口做对了事」是两句话。"
    );
}
