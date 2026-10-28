//! **執行順序表是公開契約**（G49 配套第 1 條）——這裡把契約的兩側釘在一起。
//!
//! `docs/architecture/dsl-reference.md` §三那張表與
//! `src/directive.rs` 的 `chain_directives!` 是同一份東西的兩個副本。
//! 副本會分叉，這是本儲存庫反覆吃過虧的形狀（`unclaimed.sh` 的忽略名單、
//! `supply-audit.py` 的 `ACCEPTED`、vendor 迴歸網那張手寫的 16 項清單，都是同一族）。
//!
//! 處置不是「記得兩邊一起改」，是**讓它編不過 / 測不過**。
//!
//! ★ ★ **本檔案自己也是一個掃查器，所以它先自證**：
//! [`table_parser_can_both_hit_and_miss`] 用固定輸入證明比對函式
//! **既能在一致時給綠、也能在四種不一致時各給紅**。
//! 一個自己瞎了的掃查器照樣會列印一份很像結論的報告——這條教訓在
//! `docs/architecture/` 與 `AGENTS.md` 裡各記著一遍。

use fulcrum_config::diag::DiagCode;
use fulcrum_config::directive::{
    CACHE_SUBS, ChainDirective, FILE_SERVER_SUBS, GLOBAL_OPTIONS, LOG_SUBS, REVERSE_PROXY_SUBS,
    SiteDirective, TLS_SUBS,
};
use fulcrum_config::placeholder;

const DOC: &str = include_str!("../../../docs/architecture/dsl-reference.md");

#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    order: u16,
    name: String,
    kind: String,
    owner: String,
}

/// 從 markdown 裡摳出 §三那張表。
fn parse_order_table(md: &str) -> Vec<Row> {
    let start = md
        .find("# 3. Execution order")
        .or_else(|| md.find("# 三、執行順序表"))
        .or_else(|| md.find("# 三、执行顺序表"))
        .expect("文档里找不到 §三 —— 标题被改过？契约测试必须先知道自己在读哪一段");
    let rest = &md[start..];
    // 到下一個一級標題為止。跳過第一個字元，免得撞上自己。
    let end = rest[1..].find("\n# ").map(|i| i + 1).unwrap_or(rest.len());
    rest[..end].lines().filter_map(parse_row).collect()
}

fn parse_row(line: &str) -> Option<Row> {
    let line = line.trim();
    if !line.starts_with('|') {
        return None;
    }
    let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
    if cells.len() < 4 {
        return None;
    }
    Some(Row {
        order: cells[0].parse::<u16>().ok()?,
        name: cells[1].trim_matches('`').to_string(),
        kind: cells[2]
            .replace("Middleware", "中间件")
            .replace("Terminal", "终结")
            .replace("Container", "容器")
            .replace("中介軟體", "中间件")
            .replace("終結", "终结"),
        owner: cells[3]
            .replace("M1 native", "M1 自研")
            .replace("M2 native", "M2 自研")
            .replace("Reserved", "预留")
            .replace("預留", "预留"),
    })
}

fn code_rows() -> Vec<Row> {
    ChainDirective::ALL
        .iter()
        .map(|d| Row {
            order: d.order(),
            name: d.name().to_string(),
            kind: d.kind().doc_label().to_string(),
            owner: d.owner().doc_label().to_string(),
        })
        .collect()
}

/// 逐行比對，回傳全部不一致。空 = 一致。
fn diff(doc: &[Row], code: &[Row]) -> Vec<String> {
    let mut out = Vec::new();
    for c in code {
        match doc.iter().find(|d| d.name == c.name) {
            None => out.push(format!("代码里有 `{}`，文档 §三的表里没有", c.name)),
            Some(d) if d != c => out.push(format!("`{}`：文档 {:?} ≠ 代码 {:?}", c.name, d, c)),
            Some(_) => {}
        }
    }
    for d in doc {
        if !code.iter().any(|c| c.name == d.name) {
            out.push(format!("文档 §三的表里有 `{}`，代码里没有", d.name));
        }
    }
    out
}

#[test]
fn 执行顺序表两侧逐行一致() {
    let doc = parse_order_table(DOC);
    // ★ 先釘住「確實讀到了東西」。比對函式在兩側都是空表時也會給綠，
    //   而那正是這道門最可能的失效方式——標題改了、表格換了寫法。
    assert!(
        doc.len() >= 9,
        "只从文档里读出 {} 行，表格格式多半变了；本次比对不能采信",
        doc.len()
    );
    let code = code_rows();
    assert_eq!(
        doc.len(),
        code.len(),
        "文档 {} 行，代码 {} 行",
        doc.len(),
        code.len()
    );
    let d = diff(&doc, &code);
    assert!(d.is_empty(), "执行顺序表两侧不一致：\n  {}", d.join("\n  "));
}

#[test]
fn 文档里的序号严格递增() {
    let doc = parse_order_table(DOC);
    let mut last = 0u16;
    for r in &doc {
        assert!(
            r.order > last,
            "文档里 `{}` 的序号 {} 没有递增",
            r.name,
            r.order
        );
        last = r.order;
    }
}

/// ★ ★ 掃查器自證：一致時給綠，四種不一致各給紅。
#[test]
fn table_parser_can_both_hit_and_miss() {
    const GOOD: &str = "\
# 3. Execution order (G49)

| 序 | 指令 | 类别 | M1 归属 |
|---|---|---|---|
| 20 | `header` | Middleware | M1 native |
| 80 | `reverse_proxy` | Terminal | M1 native |

# 四、别的
| 90 | `不该被读到` | 终结 | M1 自研 |
";
    let rows = parse_order_table(GOOD);
    assert_eq!(rows.len(), 2, "只应读到 §三那一张表，实际 {rows:?}");
    let code = vec![
        Row {
            order: 20,
            name: "header".into(),
            kind: "中间件".into(),
            owner: "M1 自研".into(),
        },
        Row {
            order: 80,
            name: "reverse_proxy".into(),
            kind: "终结".into(),
            owner: "M1 自研".into(),
        },
    ];
    assert!(diff(&rows, &code).is_empty(), "一致时不该报");

    // ① 序號被改
    let mut changed = code.clone();
    changed[0].order = 21;
    assert_eq!(diff(&rows, &changed).len(), 1, "序号改了必须抓到");

    // ② 類別被改
    let mut changed = code.clone();
    changed[1].kind = "中间件".into();
    assert_eq!(diff(&rows, &changed).len(), 1, "类别改了必须抓到");

    // ③ 程式碼多一條（= 新指令沒寫進文件）
    let mut more = code.clone();
    more.push(Row {
        order: 95,
        name: "新指令".into(),
        kind: "终结".into(),
        owner: "M1 自研".into(),
    });
    assert_eq!(diff(&rows, &more).len(), 1, "代码多一条必须抓到");

    // ④ 程式碼少一條（= 文件裡有個沒人實作的指令）
    let fewer = vec![code[0].clone()];
    assert_eq!(diff(&rows, &fewer).len(), 1, "代码少一条必须抓到");
}

#[test]
fn 每条指令都在文档里出现过() {
    let mut missing = Vec::new();
    for name in ChainDirective::ALL
        .iter()
        .map(|d| d.name())
        .chain(SiteDirective::ALL.iter().map(|d| d.name()))
    {
        if !DOC.contains(&format!("`{name}`")) {
            missing.push(name);
        }
    }
    assert!(
        missing.is_empty(),
        "这些指令代码里有、文档里没有：{missing:?}"
    );
}

#[test]
fn 每条子指令与全局选项都在文档里出现过() {
    let tables: [(&str, &[fulcrum_config::directive::SubSpec]); 6] = [
        ("reverse_proxy", REVERSE_PROXY_SUBS),
        ("tls", TLS_SUBS),
        ("log", LOG_SUBS),
        ("cache", CACHE_SUBS),
        ("file_server", FILE_SERVER_SUBS),
        ("全局选项", GLOBAL_OPTIONS),
    ];
    let mut missing = Vec::new();
    for (parent, table) in tables {
        // 空表會讓這個循環一次都不跑而依然給綠——先釘住下界。
        assert!(!table.is_empty(), "{parent} 的子指令表是空的");
        for s in table {
            if !DOC.contains(&format!("`{}`", s.name)) {
                missing.push(format!("{parent}.{}", s.name));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "这些子指令代码里有、文档里没有：{missing:?}"
    );
}

#[test]
fn 占位符全表都在文档里出现过() {
    let tokens = placeholder::doc_tokens();
    assert!(
        tokens.len() >= 10,
        "占位符表只有 {} 项，多半是读错了",
        tokens.len()
    );
    let missing: Vec<&String> = tokens
        .iter()
        .filter(|t| !DOC.contains(t.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "这些占位符代码里有、文档 §六里没有：{missing:?}"
    );
}

#[test]
fn 文档样例里的那个错误码就是代码里的那个() {
    // ★ DSL 參考 §九的範例印著 `FUL-DSL-0007: unknown directive`。
    //   印出去的號碼就是契約的一部分——G51 明寫「一旦發出就不能改含義」。
    assert_eq!(DiagCode::UNKNOWN_DIRECTIVE.as_str(), "FUL-DSL-0007");
    assert!(
        DOC.contains("error[FUL-DSL-0007]: unknown directive"),
        "文档 §九的样例变了，或者错误码变了——两者必须一起动"
    );
}

/// 抽出某個標題之後的第一個 ```text 程式碼塊。
fn code_block_after(md: &str, heading: &str) -> String {
    let start = md
        .find(heading)
        .unwrap_or_else(|| panic!("文档里找不到标题 {heading}"));
    let rest = &md[start..];
    let open = rest.find("```text").expect("这一节里没有 text 代码块");
    let after = &rest[open + "```text".len()..];
    let end = after.find("```").expect("代码块没闭合");
    after[..end].trim_start_matches('\n').to_string()
}

#[test]
fn 文档里印出来的示例必须真能编译() {
    // ★ ★ 這一條是**契約測試裡最直接的一個**：文件裡那份設定是給人抄的，
    //   抄下來編不過就是文件在騙人。而它同時還是 G49 那條容器排位的活判據——
    //   §一的範例只有在 `handle` 跑在 `respond` 之前時才成立。
    // ★ `### \`metrics\``（批 M）：那一份範例正是 G116 教人怎麼把指標圈起來的那一份，
    //   ⚠ 而它同時是**唯一**印出「`handle` 必須罩住 `metrics`」的地方 ——
    //   抄下來編不過，等於把唯一那份正確寫法印錯了。
    for heading in [
        "# 1. Overview",
        "## 4.5 Top-level L4 blocks",
        "### `metrics`",
    ] {
        let src = code_block_after(DOC, heading);
        assert!(src.len() > 40, "{heading} 抽出来的代码块太短：{src:?}");
        let o = fulcrum_config::compile_str("docs/architecture/dsl-reference.md", &src);
        assert!(
            !o.diagnostics.has_errors(),
            "文档 {heading} 的示例编译不过：\n{}",
            o.render_diagnostics()
        );
    }
}

#[test]
fn 总览示例里的兜底_respond_确实排在_handle_之后() {
    // ⚠ 只斷言「編譯得過」不夠：把 handle 挪到 respond 後面它照樣編譯得過，
    //   只是那個「備援 403」會變成恆勝，而**沒有任何一行輸出會說出來**。
    let src = code_block_after(DOC, "# 1. Overview");
    let cfg = fulcrum_config::compile_str("t", &src)
        .config
        .expect("示例应当能编译");
    let api = cfg
        .sites
        .iter()
        .find(|s| s.addresses.iter().any(|a| a.host == "api.example.com"))
        .expect("示例里应当有 api.example.com");
    let names: Vec<&str> = api.chain.iter().map(|s| s.body.directive_name()).collect();
    assert_eq!(names, vec!["handle", "respond"]);
}

#[test]
fn 默认响应三条与文档一致() {
    let d = fulcrum_config::model::Defaults::default();
    assert_eq!(d.no_site_match, 421);
    assert!(DOC.contains("421 Misdirected Request"));
    assert!(DOC.contains("| No route within site | 404 |"));
    assert!(DOC.contains("| All upstreams unhealthy | 502 |"));
}
