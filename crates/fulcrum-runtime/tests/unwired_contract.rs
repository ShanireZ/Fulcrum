//! [`UNWIRED`] 那份清單是**契約**，由這裡逐字釘住。
//!
//! ★ ★ 為什麼要釘：它記的是「M1 認得、但這一批還沒串接」的能力。
//! 這類清單的兩種爛法**方向相反、而且都不會有任何回報錯誤**：
//!
//! | 爛法 | 後果 |
//! |---|---|
//! | 串接做完了、忘了從清單裡刪 | 裝載時永遠打一條假警告，久了沒人看 |
//! | 從清單裡刪了、但沒串接 | 一條指令靜靜地什麼都不做——**「宣告了卻沒人接」** |
//!
//! 下面那張表兩個方向都擋：**清單必須與它逐字相等**。
//! 串接完成時改這裡是**必須的一步**，不是「記得也可以」。
//! ⚠ 判據取「逐字相等」而不是「有孤兒就紅」——後者在清單本身錯了的時候照樣綠。
//!
//! ★ ★ 那張表管的是「清單本身對不對」，管不到**掃描認不認得設定裡寫的那一條**：
//! 掃描的結果與清單求交之後才出口 ⇒ **清單裡沒有的鍵，掃描寫成什麼樣都看不見**。
//! 於是「按今天的清單挑一條來驗」這種判據會隨著能力串接一條條啞掉，且啞掉時不紅。
//! ⇒ 全域選項那一半由 `全局选项那一半真的被扫到了` 拿一張**合成清單**單獨守著。

use fulcrum_config::compile_str;
use fulcrum_runtime::{Runtime, UNWIRED};

/// M1 這一批**確實**還沒串接的能力。改動這張表 = 改動契約。
const EXPECTED: &[&str] = &[
    "tls_internal",
    "on_demand",
    "tracing",
    // ★ ★ **`weight` 在 M2 批 N 任務 2 銷號**：它在任務 1 裡進來（DSL 與結構化模型），
    //   在任務 2 裡離開（四種 `lb_policy` 全部按累積權重挑）。⚠ 兩頭都由這張表擋著。
    // ★ ★ **能力做完就銷號**（`encode` / `log` / `l4` / `proxy_protocol_from` 都是這麼離開的）。
    //   ⚠ ⚠ 而刪掉一行的**同時**必須真的串接 —— 這道門的全部意義就在這裡：
    //   「實作了但忘了更新清單」與「刪了清單但沒實作」**都過不去**。
];

#[test]
fn 清单与契约逐字相等() {
    let actual: Vec<&str> = UNWIRED.iter().map(|(k, _)| *k).collect();
    assert_eq!(
        actual, EXPECTED,
        "UNWIRED 变了。接线完成就把它从 UNWIRED 里删掉，并同步改这张表；\
         反之，只删表不接线是不行的。"
    );
}

/// ★ ★ ★ **文件裡那句「仍有幾條只是解析得過」也要跟著這張表走。**
///
/// ⚠ 它補的是一處真的腐爛過的地方：`dsl-reference.md` 開頭那句清單裡曾一直掛著
/// **`自动签发`（ACME）**，而 ACME 早已全部串接、`UNWIRED` 裡一條不剩。
/// 上面那道 `清单与契约逐字相等` 只釘 Rust 那一份，**文件那一份沒有任何東西在看**。
///
/// ★ 判據是**雙向**的：文件裡出現的每一格都要還在 `UNWIRED` 裡（防過期的警告），
/// `UNWIRED` 裡的每一格也都要在文件那句裡說得出來（防漏報）。
/// ⚠ 只做前一半的話，新加一條未串接能力而忘了寫進文件，沒有任何東西會紅。
///
/// ★ 匹配用**文件裡那幾個反引號詞**，而不是整句逐字相等：後者一改標點就紅，
/// 而那種恆紅的門最後一定會被人加 `#[ignore]`。
#[test]
fn 文档里那句未接线清单与_unwired_对得上() {
    const DOC: &str = include_str!("../../../docs/architecture/dsl-reference.md");
    // 那句話在文件開頭的引言塊裡，以「仍有幾條只是」開頭，到行末的破折號為止。
    let start = DOC
        .find("仍有幾條只是")
        .or_else(|| DOC.find("仍有几条只是"))
        .expect("dsl-reference.md 里找不到那句「仍有几条只是「解析得过」」——它被改写了？");
    // ⚠ 不要按固定位元組數開窗：中文是多位元組，切在字元中間會 panic。
    //   `find` 給回的一定是字元邊界。
    let seg = &DOC[start..];
    let seg = match seg.find("——") {
        Some(i) => &seg[..i],
        None => panic!("那句清单后面应当有一个「——」把它收住"),
    };
    // ★ 順帶釘一下它沒有跑飛：那句話是一行半，幾百位元組；
    //   萬一「——」被刪掉，上面會 panic；萬一它離得很遠，這裡會紅。
    assert!(
        seg.len() < 500,
        "从「仍有几条只是」到「——」有 {} 字节，太长了 —— 那句清单被改写过？",
        seg.len()
    );

    // 文件裡寫的是人話（`health_*` / `passive_*`），而 UNWIRED 裡是具體的鍵名。
    // ★ 這張對照表本身就是契約的一部分：改名時兩邊都得動。
    let doc_word_for = |k: &str| -> &'static str {
        match k {
            "tls_internal" => "`tls internal`",
            "on_demand" => "`on_demand`",
            "encode" => "`encode`",
            "log" => "`log`",
            "tracing" => "`tracing`",
            // ⚠ ⚠ 對照詞必須正好是**一個反引號詞**：下面第 ② 半按反引號詞掃文件，
            //   寫成「全域的 `proxy_protocol_from`」的話，② 會從文件裡摳出裸的那個、
            //   在 `expected` 裡找不到，於是**紅在一個與事實相反的理由上**。
            "proxy_protocol_from" => "`proxy_protocol_from`",
            other => panic!(
                "`{other}` 在 UNWIRED 里，而这条测试不知道它在文档那句里该写成什么。\
                 先在这里补上对照，再去文档里加 —— 顺序反过来的话，漏写没人看得见。"
            ),
        }
    };

    let mut problems: Vec<String> = Vec::new();
    // ① UNWIRED → 文件：每一條都要說得出來。
    for (k, _) in UNWIRED {
        let w = doc_word_for(k);
        if !seg.contains(w) {
            problems.push(format!("`{k}` 还没接线，而文档那句清单里没有 {w}"));
        }
    }
    // ② 文件 → UNWIRED：文件裡出現的反引號詞都得還在表裡。
    //    ⚠ 這一半才是這次真出問題的那一半。
    let expected: std::collections::BTreeSet<&str> =
        UNWIRED.iter().map(|(k, _)| doc_word_for(k)).collect();
    for chunk in seg.split('`').skip(1).step_by(2) {
        let word = format!("`{chunk}`");
        if !expected.contains(word.as_str()) {
            problems.push(format!(
                "文档那句清单里还写着 {word}，但它已经不在 UNWIRED 里了 —— \
                 接线做完就要把它从文档那句里删掉。过期的警告比过期的状态更危险。"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

const PLAN_MD: &str = include_str!("../../../PLAN.md");

/// `PLAN.md` §1「能力面」那句 `⏳ **未落地** ——` 裡，**反引號鍵名**的那一部分。
///
/// ★ ★ 判據只截到**第一個句號**：句號之後是給人看的註解，而註解裡必然會提到
/// 那些鍵名（講的就是它們）。⛔ 把註解也讀進來 = 一個把正確寫法判紅的門，
/// 而那種門最後會被人繞過去，不會被人滿足（同 `tools/plan-refs.py` 記的第 4 條）。
fn plan_unlanded_segment(md: &str) -> &str {
    let start = md
        .find("⏳ **未完成實作** ——")
        .or_else(|| md.find("⏳ **未落地** ——"))
        .expect("PLAN.md §1 里找不到那句「⏳ **未落地** ——」——它被改写了？");
    let seg = &md[start..];
    match seg.find('。') {
        Some(i) => &seg[..i],
        None => panic!("「未落地」那一行后面应当有一个句号把它收住"),
    }
}

/// 那一段裡出現、而**已經不在** `unwired` 裡的反引號鍵名。
///
/// ★ 寫成純函式是承重的：今天那一段裡**一個反引號詞都沒有**（`passive_fail` 已由
/// G136 銷號並從這行刪掉，剩下的 PROXY protocol「發」那半邊是散文）⇒ 拿真資料跑
/// 只能得到一個空集，而**一個恆回傳空集的判據，與一個壞掉的判據輸出完全相同**。
/// ⇒ 判別力由下面那幾條**合成輸入**提供。
fn stale_unlanded_keys(segment: &str, unwired: &[&str]) -> Vec<String> {
    segment
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|w| !unwired.contains(w))
        .map(|w| w.to_string())
        .collect()
}

/// ★ ★ ★ **這一行講的是「還沒串接」，而它沒有任何門守著 —— 直到這一條。**
///
/// 2026-09-06 實測：`passive_fail` 由 `G136` 串接並從 [`UNWIRED`] 銷號之後，
/// `PLAN.md` §1 那一行**仍然把它列在「未完成實作」裡**，多掛了一天。
/// ⚠ ⚠ 它躲過了本儲存庫當時**每一道**門：
///
/// * 上面那條 `文档里那句未接线清单与_unwired_对得上` 只釘 `dsl-reference.md`；
/// * `tools/plan-refs.py` 判據 ② 只認**同一行裡帶 `§11` 字樣**的句子，
///   而這一句不帶 —— 那道門的檔案頭自己把這一類列為第 1 條漏網；
/// * `docs-check.py` 只判可達性，不讀句意。
///
/// ⇒ 一句**自洽的**假話，本儲存庫內**原理上**沒有任何東西看得見它。這一條補的就是它。
///
/// ⚠ **只判一個方向**（這一行 → UNWIRED），⛔ 不判反向：這一行是**摘要不是全集**
/// （`tls_internal` / `on_demand` / `tracing` 都在 UNWIRED 裡而不在這一行），
/// 而且 PROXY protocol「發」那半邊**根本進不了 UNWIRED** —— 它連 DSL 面都沒有，
/// 而 UNWIRED 記的是「DSL 認得、執行環境不做」。⇒ 反向判據會把一句正確的話判紅。
#[test]
fn plan_第1节未落地那一行不许列已接线的能力() {
    let keys: Vec<&str> = UNWIRED.iter().map(|(k, _)| *k).collect();
    let segment = plan_unlanded_segment(PLAN_MD);

    // ① 真資料：那一段裡不許出現已經銷號的鍵名。
    let stale = stale_unlanded_keys(segment, &keys);
    assert!(
        stale.is_empty(),
        "PLAN.md §1「未落地」那一行还写着 {stale:?}，而它已经不在 UNWIRED 里了 —— \
         接线做完就要把它从这一行删掉。过期的状态比没有状态更危险。\n\
         那一段：{segment}"
    );

    // ② ★ ★ ★ **合成輸入：證明它真的判得動。** 沒有這幾條，上面那個 `is_empty()`
    //   在「函式恆回傳空 vec」時**同樣是綠的** —— 而那正是今天最可能的壞法，
    //   因為真資料這一側本來就是空集。
    let synthetic = "⏳ **未落地** —— PROXY protocol 的「发」那半边 · `passive_fail`";
    assert_eq!(
        stale_unlanded_keys(plan_unlanded_segment(&format!("{synthetic}。")), &keys),
        vec!["passive_fail".to_string()],
        "一个已经销号的键名写在这一行里，判据没有把它逮出来"
    );
    // 還在 UNWIRED 裡的鍵名**不許**被判紅（否則這道門恆紅，早晚被加 #[ignore]）。
    let ok_line = "⏳ **未落地** —— `tracing` 与 `on_demand`";
    assert!(
        stale_unlanded_keys(plan_unlanded_segment(&format!("{ok_line}。")), &keys).is_empty(),
        "还没接线的键名被判成了过期"
    );
    // 純散文（今天的真實形狀）不該報任何東西。
    let prose = "⏳ **未落地** —— PROXY protocol 的「发」那半边";
    assert!(
        stale_unlanded_keys(plan_unlanded_segment(&format!("{prose}。")), &keys).is_empty(),
        "一行散文被判出了键名"
    );
    // ⚠ 截斷那一步也要釘：句號**之後**的註解裡提到銷號鍵名時，⛔ 不許被算進來。
    let with_note = "⏳ **未落地** —— PROXY protocol 的「发」那半边。\n\
                     ⚠ 注解：`passive_fail` 已由 G136 销号。";
    assert!(
        stale_unlanded_keys(plan_unlanded_segment(with_note), &keys).is_empty(),
        "句号之后的注解被读进了判据范围 —— 这道门会把正确的写法判红"
    );
}

const DSL_DOC: &str = include_str!("../../../docs/architecture/dsl-reference.md");

/// `dsl-reference.md` 裡一張**子指令表**的一行。
#[derive(Debug, PartialEq, Eq)]
struct SubRow {
    /// 第一格里的那些反引號名字。⚠ 一格里**可以有多個**
    /// （`` `passive_fail` / `passive_window` `` 就是一行）。
    names: Vec<String>,
    /// 這一行的行文裡提到 `UNWIRED` 了嗎。
    mentions_unwired: bool,
}

/// 摳出全部**子指令表**的資料行。
///
/// ★ 表的集合是**推導**出來的（表頭第一格正好是「子指令」），⛔ 不是一張手寫清單：
/// 手寫清單會在下一次新加一張子指令表時靜靜漏掉那一張，而漏掉的那一刻不紅。
fn sub_directive_rows(md: &str) -> Vec<SubRow> {
    let mut out = Vec::new();
    let mut in_table = false;
    for line in md.lines() {
        let t = line.trim();
        if !t.starts_with('|') {
            // 表以第一條非表格行為界。⚠ 不這樣的話，緊跟其後的另一張表
            //   （表頭不是「子指令」的那種）會被當成同一張表接著讀下去。
            in_table = false;
            continue;
        }
        let cells: Vec<&str> = t.trim_matches('|').split('|').map(str::trim).collect();
        if cells.first().copied() == Some("子指令") {
            in_table = true;
            continue;
        }
        if !in_table {
            continue;
        }
        // `|---|---|` 那一行。
        if cells
            .iter()
            .all(|c| !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':'))
        {
            continue;
        }
        let first = cells.first().copied().unwrap_or("");
        out.push(SubRow {
            names: first
                .split('`')
                .skip(1)
                .step_by(2)
                .map(|s| s.to_string())
                .collect(),
            mentions_unwired: t.contains("UNWIRED"),
        });
    }
    out
}

/// ★ ★ ★ **子指令表那一行也要與 [`UNWIRED`] 對得上，兩個方向都釘住。**
///
/// # 這道門補的是一個**實測過的洞**
///
/// 批 N 任務 1 給 `dsl-reference.md` 的 `reverse_proxy` 子指令表加了 `weight` 一行，
/// 行尾寫著「現在只到設定層為止……排程還不認」。任務 1 實測：**把那一整行刪掉，
/// 沒有任何門會紅** —— `doc_contract.rs` 判的是「`` `weight` `` 在**整份文件裡**
/// 出現過沒有」，而它當時同時出現在開頭那句未串接清單裡。
/// ⇒ 那句收尾話在排程接上之後就是一句**假話**，而沒有任何東西會提醒人改它。
///
/// # 判據
///
/// - ① **文件 → 清單**：子指令表裡凡是行文提到 `UNWIRED` 的那一行，
///   它第一格里的名字**必須**還在 [`UNWIRED`] 裡 ⇒ 擋「串接了忘了改文件」。
/// - ② **清單 → 文件**：[`UNWIRED`] 裡凡是在子指令表裡有行的那個名字，
///   那一行**必須**提到 `UNWIRED` ⇒ 擋「文件說得像已經能用了、其實執行環境不做」。
///
/// ⚠ ⚠ **②那一半才是這次漏掉的方向**（AGENTS.md 門禁紀律第二條：
/// 「一條錨在**缺失**上的判據，每次那東西發佈時都要重新錨定」）——
/// ①只在文件裡**還留著**那句話時才有活幹，而它恰好會隨著串接一條條啞掉。
/// ⇒ 下面另有一條斷言釘住「今天至少有一行真的提到 `UNWIRED`」，
/// 否則①會在某一天變成一條空轉的門，而空轉的那一刻不紅。
#[test]
fn 子指令表里的_unwired_字样与清单两个方向都对得上() {
    let rows = sub_directive_rows(DSL_DOC);
    // 先釘住「確實讀到了東西」——比對在兩側都是空的時候也會給綠。
    assert!(
        rows.len() >= 25,
        "只从子指令表里读出 {} 行，表格写法多半变了；本次比对不能采信",
        rows.len()
    );
    // 而且讀到的是**幾張**表，不是隻讀了第一張。
    for must in ["lb_policy", "zones", "output"] {
        assert!(
            rows.iter().any(|r| r.names.iter().any(|n| n == must)),
            "子指令表里没读到 `{must}` —— 扫查器多半只读到了其中一张表"
        );
    }
    let unwired: std::collections::BTreeSet<&str> = UNWIRED.iter().map(|(k, _)| *k).collect();

    let mut problems: Vec<String> = Vec::new();
    // ① 文件 → 清單
    for r in &rows {
        if r.mentions_unwired && !r.names.iter().any(|n| unwired.contains(n.as_str())) {
            problems.push(format!(
                "子指令表里 {:?} 那一行说自己在 UNWIRED 里，而 UNWIRED 里已经没有它了 —— \
                 接线做完就要把那句收尾话改掉。过期的警告比过期的状态更危险。",
                r.names
            ));
        }
    }
    // ② 清單 → 文件
    for (k, _) in UNWIRED {
        for r in &rows {
            if r.names.iter().any(|n| n == k) && !r.mentions_unwired {
                problems.push(format!(
                    "`{k}` 还没接线，而子指令表里它那一行一个字都没说 —— \
                     照着文档写配置的人会以为它能用。"
                ));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));

    // ★ ★ 最後釘住這道門**今天真的有活可幹**：①那一半隻在文件裡還留著那句話時才生效。
    //   ⚠ 沒有這一條的話，某天最後一條未串接能力接完、文件裡那幾句一起刪掉，
    //   ①就變成一條空轉的門 —— 而它會一直是綠的。
    assert!(
        rows.iter().any(|r| r.mentions_unwired),
        "子指令表里现在一行都没提到 UNWIRED ⇒ 上面①那一半是空转的。\
         若 UNWIRED 真的清空了，把这道门连同那张表一起销号；\
         若没有，那就是文档漏了。"
    );
}

/// ★ ★ 掃查器自證：一致時給綠，四種不一致各給紅。
///
/// ⚠ 一個自己瞎了的掃查器照樣會列印一份很像結論的報告 ——
/// 這條教訓在 `AGENTS.md` 與 `doc_contract.rs` 裡各記著一遍。
#[test]
fn 子指令表扫查器能命中也能落空() {
    const FIXTURE: &str = "\
### `x` 的子块

| 子指令 | 说明 |
|---|---|
| `a` | 平平无奇的一条 |
| `b` / `b2` | 一格里两个名字，且这一行提到 UNWIRED |

一段正文，表到此为止。

| 指令 | 形式 |
|---|---|
| `c` | ⚠ 这不是子指令表，`c` 不该被读到（哪怕它提到 UNWIRED）|

### `y` 的子块

| 子指令 | 说明 |
|---|---|
| `d` | 第二张表也要读到 |
";
    let rows = sub_directive_rows(FIXTURE);
    // ① 唯讀子指令表，且讀全了兩張。
    let names: Vec<Vec<String>> = rows.iter().map(|r| r.names.clone()).collect();
    assert_eq!(
        names,
        vec![
            vec!["a".to_string()],
            vec!["b".to_string(), "b2".to_string()],
            vec!["d".to_string()],
        ],
        "读出来的行不对：{rows:?}"
    );
    // ② `UNWIRED` 字樣按行認，不按整份文件認。
    assert_eq!(
        rows.iter().map(|r| r.mentions_unwired).collect::<Vec<_>>(),
        vec![false, true, false]
    );
    // ③ 一格里有多個名字時**每一個**都算數（`passive_fail` / `passive_window` 就是這形狀）。
    assert!(rows[1].names.iter().any(|n| n == "b2"));
    // ④ 表頭與分隔行不當成資料行（否則「子指令」會被當成一條子指令）。
    assert!(
        !rows.iter().any(|r| r.names.is_empty()),
        "分隔行或表头被当成数据行了：{rows:?}"
    );
}

#[test]
fn 每一条都有理由并且不重复() {
    let mut seen = std::collections::BTreeSet::new();
    for (k, why) in UNWIRED {
        assert!(seen.insert(*k), "{k} 在清单里出现了两次");
        assert!(
            why.len() >= 8,
            "{k} 的理由太短了：一条「还没做」的登记必须说清楚它在等什么"
        );
    }
}

fn unwired_for(dsl: &str) -> Vec<&'static str> {
    unwired_for_table(UNWIRED, dsl)
}

/// 同上，但清單由呼叫方給 —— 見下面的 `全局选项那一半真的被扫到了`。
fn unwired_for_table(table: &[(&'static str, &'static str)], dsl: &str) -> Vec<&'static str> {
    let o = compile_str("t.Fulcrumfile", dsl);
    assert!(!o.diagnostics.has_errors(), "{}", o.render_diagnostics());
    let cfg = o.config.unwrap();
    let rt = Runtime::build(&cfg).expect("应当能建起来");
    rt.unwired_in_use_of(table, &cfg)
        .iter()
        .map(|(k, _)| *k)
        .collect()
}

/// 全域選項那一半**真的被掃到了** —— 判據不掛在「今天恰好哪幾條沒串接」上。
///
/// # ★ ★ 為什麼這一條非得拿合成表來驗
///
/// `unwired_in_use` 收尾要與 [`UNWIRED`] 求交，而那張表裡**現在一條全域選項都沒有**
/// ⇒ 拿真表怎麼寫都驗不到全域那半邊：斷言只能是空，而把整段全域掃描刪掉，它照樣是空。
/// ⚠ 這就是這條測試來之前的狀態 —— `只报本次配置真的用到的那几条` 裡那行 `admin`
/// 旁邊曾寫著「這一行守著全域那一半」，一句在 `admin`（批 9）串接的同一刻就過期、
/// 且不會有任何東西說出來的話。**一個只見過綠的門與一個不存在的門無法區分。**
///
/// ★ 合成表把兩件事拆開：**清單裡有沒有這個鍵**（假的，只在本測試裡存在）
/// 與**掃描認不認得它**（真的，掃的就是產品那一格）。於是串接進度再往前走，
/// 這條判據也不會到期 —— 那正是上一版沒做到的事。
/// ⚠ 兩個方向都要：只驗「報得出來」的話，一個把 `used.insert` 寫成無條件的實作照樣綠。
#[test]
fn 全局选项那一半真的被扫到了() {
    // 形狀與 UNWIRED 一樣，內容與它無關。
    // ⚠ 鍵必須取**真的**（`admin`）：掃的是產品裡那一格，假鍵只會驗到
    //   「表裡有、掃描不認得」，而那是另一件事。
    let 合成表: &[(&str, &str)] = &[("admin", "合成的登记词")];
    let 写了 = "{\n  admin unix//tmp/x.sock\n}\nhttp://a.com {\n  respond 200\n}\n";
    let 没写 = "http://a.com {\n  respond 200\n}\n";

    assert_eq!(
        unwired_for_table(合成表, 写了),
        vec!["admin"],
        "全局块里写着 `admin`，而扫描没认出来 —— 全局那一半没在工作"
    );
    // 反向半：⚠ 少了它，一個把 `used.insert` 寫成無條件的實作照樣綠。
    assert_eq!(
        unwired_for_table(合成表, 没写),
        Vec::<&str>::new(),
        "配置里没有 `admin`，却报了出来"
    );
}

#[test]
fn 只报本次配置真的用到的那几条() {
    // ★ 把八條全打出來會變成噪音，而噪音會把真的那幾條一起埋掉。
    // 純 HTTP + respond：一條都不該報。
    assert_eq!(
        unwired_for("http://a.com {\n  respond 200\n}\n"),
        Vec::<&str>::new()
    );

    // https 位址 + 預設自動 HTTPS，而設定裡**沒有 80 連接埠** → 只能靠 TLS-ALPN-01，
    // ★ 而它已經串接，所以**一條缺口都不該報**。
    //   ⚠ 留著舊期望的話，這道門會把一件已經做完的事繼續報成沒做 ——
    //   **過期的警告比過期的狀態更危險**：它會主動阻止下一個人去用這條路。
    assert_eq!(
        unwired_for("a.com {\n  respond 200\n}\n"),
        Vec::<&str>::new()
    );

    // ★ ★ 同一份設定補上一個 80 連接埠的明文站台，同樣一條都不該報。
    //   ⚠ 這是反向判據：只驗「報得出來」而不驗「該沒有時真的沒有」，
    //     一個恆報的實作照樣綠。
    assert_eq!(
        unwired_for("a.com {\n  respond 200\n}\nhttp://a.com:80 {\n  respond 200\n}\n"),
        Vec::<&str>::new()
    );

    // ★ DNS-01 與 TLS-ALPN-01 都接上了 ⇒ 一個沒配 `dns` 的通配符站台一條都不報。
    //   ⚠ 它當然仍然籤不出證書（通配符只能走 DNS-01），但那由 `plan_tls` 的警告
    //   與 `Target::actionable()` 的「推遲」負責說，**不是這張未串接表的職責**。
    assert_eq!(
        unwired_for("*.a.com {\n  respond 200\n}\n"),
        Vec::<&str>::new()
    );

    // ★ 配了 `dns exec` 的站台：DNS-01 零連接埠依賴（G54），
    //   所以**「沒有 80 連接埠」那一條對它不成立** —— 一條都不該報。
    //   ⚠ 不分這個岔的話，一個正確配了 DNS-01 的站台會被報成「缺 TLS-ALPN-01」。
    assert_eq!(
        unwired_for(
            "*.a.com {\n  tls {\n    dns exec /bin/true\n    resolvers 127.0.0.1:8053\n  }\n  respond 200\n}\n"
        ),
        Vec::<&str>::new()
    );

    // ★ 原生供應商（G57）接上了，所以寫了 `dns cloudflare` 的站台**一條缺口都不該報**。
    //   ⚠ 注意這份設定比之前多了 `zones` —— 少了它現在是**編譯期錯誤**（G59 第 3 條），
    //   而 `unwired_for` 裡那句 `assert!(!has_errors)` 會先炸。
    //   ★ 也就是說：這一行既在驗「不報缺口」，也順帶釘住了「G59 那道編譯期門存在」。
    assert_eq!(
        unwired_for(
            "*.a.com {\n  tls {\n    dns cloudflare env:CF_TOKEN\n    zones a.com\n    resolvers 127.0.0.1:8053\n  }\n  respond 200\n}\n"
        ),
        Vec::<&str>::new()
    );

    // ★ 給了 PEM 路徑的站台**不該**被報成未串接
    assert_eq!(
        unwired_for("a.com {\n  tls /c.pem /k.pem\n  respond 200\n}\n"),
        Vec::<&str>::new()
    );

    // `tls internal` 與 on_demand 各自單獨報
    assert_eq!(
        unwired_for("a.com {\n  tls internal\n  respond 200\n}\n"),
        vec!["tls_internal"]
    );
    assert_eq!(
        unwired_for(
            "a.com {\n  tls {\n    on_demand\n    ask https://x/y\n  }\n  respond 200\n}\n"
        ),
        vec!["on_demand"]
    );

    // ★ ★ M2 批 I 把這一條**翻了個方向**（與 `dns_refresh` 那次同款）：
    //   `encode` 串接了，一條寫了 `encode gzip` 的設定**一條缺口都不該報**。
    //   ⚠ 留著舊期望的話，這道門會把一件已經做完的事繼續報成沒做 ——
    //   而假警告會訓練人忽略整張表。
    assert_eq!(
        unwired_for("http://a.com {\n  encode gzip\n  respond 200\n}\n"),
        Vec::<&str>::new()
    );

    // ★ ★ 批 10 把這一條**翻了個方向**：`dns_refresh` 串接了，
    //   一條光寫 `reverse_proxy` 的設定**一條缺口都不該報**。
    //   ⚠ 留著舊期望的話，這道門會把一件已經做完的事繼續報成沒做——
    //   而假警告會訓練人忽略整張表。
    assert_eq!(
        unwired_for("http://a.com {\n  reverse_proxy x:1\n}\n"),
        Vec::<&str>::new()
    );

    // ★ ★ ★ **G136 把這一條也翻了方向**（第五次，與 `dns_refresh` / `encode` /
    //   `health_uri` / `weight` 同款）：批 11 之後這裡斷言「只剩 `passive_fail` 一條」，
    //   而 G136 串接之後 —— **`reverse_proxy` 塊裡已經沒有任何未串接的子指令了**。
    //   ⚠ 留著舊期望的話，這道門會把一件已經做完的事繼續報成沒做。
    //   ★ 這一條有意**保留成「一條都不報」**而不是換個樣本：它守的正是
    //   「`reverse_proxy` 那一整塊已經全部串接」這個事實，而那是一句會過期的話 ——
    //   哪天再加一條未串接的子指令，它必須當場紅。
    assert_eq!(
        unwired_for(
            "http://a.com {\n  reverse_proxy x:1 {\n    health_uri /h\n    passive_fail 3\n    passive_window 5s\n  }\n}\n"
        ),
        Vec::<&str>::new()
    );

    // ★ ★ **批 N 任務 2 把這一條翻了個方向**（與 `dns_refresh` / `encode` / `health_uri`
    //   那三次同款）：任務 1 裡「寫了 `weight` ⇒ 報未串接」，任務 2 串接之後
    //   **寫了權重的設定一條都不該報**。
    //   ⚠ 留著舊期望的話，這道門會把一件已經做完的事繼續報成沒做 ——
    //   而假警告會訓練人忽略整張表。
    assert_eq!(
        unwired_for("http://a.com {\n  reverse_proxy x:1 x:2 {\n    weight x:1 3\n  }\n}\n"),
        Vec::<&str>::new()
    );
    // 沒寫 `weight` 的那條同樣一條都不報。
    assert_eq!(
        unwired_for("http://a.com {\n  reverse_proxy x:1 x:2\n}\n"),
        Vec::<&str>::new()
    );

    // l4 —— ★ ★ 這一格在一天裡翻了**兩次**方向：
    //   批 A 之前「有 `l4` 塊就報」；批 A（TCP 串接）之後改成「有 `udp` 才報」；
    //   批 B（UDP 串接）之後**兩種協定都不報**。
    //   ⚠ 每一次不跟著改都會留下一條**假警報**：一個正在轉發的連接埠被紀錄說成「還沒串接」。
    //   ★ 三份夾具都留著（純 tcp / 純 udp / 混寫），它們分別守著那三種寫法。
    for dsl in [
        "l4 {
  tcp :3306 {
    proxy 10.0.0.5:3306
  }
}
",
        "l4 {
  udp :53 {
    proxy 10.0.0.6:53
  }
}
",
        "l4 {
  tcp :3306 {
    proxy 10.0.0.5:3306
  }
  udp :53 {
    proxy 10.0.0.6:53
  }
}
",
    ] {
        assert_eq!(
            unwired_for(dsl),
            Vec::<&str>::new(),
            "L4 两种协议都接线了，不该再报：{dsl}"
        );
    }

    // ★ 全域選項 `admin` 在批 9 已經串接（管理面 + 全量原子 load + 強制續期），
    //   所以**一條都不該報**。
    //   ⚠ ⚠ 這一行**只**驗到這裡為止：`admin` 不在 `UNWIRED` 裡，求交之後本來就是空的
    //   ⇒ 把整段全域掃描刪掉，它照樣綠。它守的是「已串接的能力不許被報成沒串接」，
    //   **不是**「全域那一半在工作」—— 後者由 `全局选项那一半真的被扫到了` 拿合成表守。
    //   ★ 兩條各守一件事，別把其中一條當成另一條：這一行的註解上一版正是這麼寫錯的。
    assert_eq!(
        unwired_for("{\n  admin unix//tmp/x.sock\n}\nhttp://a.com {\n  respond 200\n}\n"),
        Vec::<&str>::new()
    );

    // tracing
    assert_eq!(
        unwired_for("http://a.com {\n  tracing\n  respond 200\n}\n"),
        vec!["tracing"]
    );

    // ⚠ ⚠ 這裡原本用的是 `log`，而 **M2 批 L 第 ② 步把它串接了** ——
    //   於是這條夾具在當天紅了，**而它紅得對**：
    //   一條「寫了它、而執行環境什麼都不做」的判據，在那件事不再成立時必須紅。
    //   ★ 換成一條**今天仍然沒串接**的，而不是把斷言改成空 ——
    //   改成空的話，這條判據就再也不檢查「站台內的未串接能力會被報出來」了。
    //   ⚠ ⚠ **G136 之後這裡換了第二次**：上一版用的是 `passive_fail`，
    //   而它在 G136 串接了 —— 這條夾具當天紅了，**且紅得對**。
    //   ⇒ 現在用 `tls internal`（仍在 `UNWIRED` 裡），嵌套還更深一層（站台 > tls > internal）。
    assert_eq!(
        unwired_for("a.com {\n  tls internal\n  reverse_proxy 10.0.0.1:80\n}\n"),
        vec!["tls_internal"]
    );
}

#[test]
fn 容器里面的也要被看见() {
    // ⚠ 反向判據：只掃站台頂層的話，`route { … }` 裡那條會被漏掉，
    //   而漏掉的表現是「一條都不報」—— 看起來像「沒有未串接的東西」。
    // ★ 夾具裡那條能力串接之後，**換夾具而不是刪掉這條**：它驗的是**掃不掃容器裡面**。
    //   > 一條被刪掉的判據，與一個從來沒有過判據的功能，事後看起來一模一樣。
    assert_eq!(
        unwired_for("http://a.com {\n  route {\n    tracing\n    respond 200\n  }\n}\n"),
        vec!["tracing"]
    );
    assert_eq!(
        unwired_for("http://a.com {\n  handle {\n    tracing\n    respond 200\n  }\n}\n"),
        vec!["tracing"]
    );
    // handle_errors 裡的也算
    assert_eq!(
        unwired_for("http://a.com {\n  respond 200\n  handle_errors {\n    respond 500\n  }\n}\n"),
        Vec::<&str>::new()
    );
}

#[test]
fn 顺序跟着_unwired_的声明顺序走而不是随机() {
    // 報告的順序要穩定，否則裝載紀錄每次都不一樣，diff 起來全是噪音。
    // ⚠ 這份夾具要**同時命中兩條以上**未串接的，否則「順序」這件事無從談起。
    //   ★ G136 之後 `passive_fail` 不再是候選 ⇒ 換成 `tls internal` + `tracing`，
    //   它們在 `UNWIRED` 裡的宣告順序是「tls_internal 在前、tracing 在後」，
    //   而這份設定**有意把 `tracing` 寫在前面** —— 判據要的是輸出跟著宣告順序走，
    //   ⛔ 不是跟著書寫順序走。
    let got =
        unwired_for("a.com {\n  tracing\n  tls internal\n  encode gzip\n  reverse_proxy x:1\n}\n");
    let order: Vec<usize> = got
        .iter()
        .map(|k| UNWIRED.iter().position(|(u, _)| u == k).unwrap())
        .collect();
    let mut sorted = order.clone();
    sorted.sort_unstable();
    assert_eq!(order, sorted, "输出顺序必须与 UNWIRED 的声明顺序一致");
}
