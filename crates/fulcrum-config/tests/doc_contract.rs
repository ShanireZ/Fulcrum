//! **执行顺序表是公开契约**（G49 配套第 1 条）——这里把契约的两侧钉在一起。
//!
//! `docs/architecture/dsl-reference.md` §三那张表与
//! `src/directive.rs` 的 `chain_directives!` 是同一份东西的两个副本。
//! 副本会分叉，这是本仓库反复吃过亏的形状（`unclaimed.sh` 的忽略名单、
//! `supply-audit.py` 的 `ACCEPTED`、vendor 回归网那张手写的 16 项清单，都是同一族）。
//!
//! 处置不是「记得两边一起改」，是**让它编不过 / 测不过**。
//!
//! ★ ★ **本文件自己也是一个扫查器，所以它先自证**：
//! [`table_parser_can_both_hit_and_miss`] 用固定输入证明比对函数
//! **既能在一致时给绿、也能在四种不一致时各给红**。
//! 一个自己瞎了的扫查器照样会打印一份很像结论的报告——这条教训在
//! `docs/architecture/` 与 `AGENTS.md` 里各记着一遍。

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

/// 从 markdown 里抠出 §三那张表。
fn parse_order_table(md: &str) -> Vec<Row> {
    let start = md
        .find("# 三、执行顺序表")
        .expect("文档里找不到 §三 —— 标题被改过？契约测试必须先知道自己在读哪一段");
    let rest = &md[start..];
    // 到下一个一级标题为止。跳过第一个字符，免得撞上自己。
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
        kind: cells[2].to_string(),
        owner: cells[3].to_string(),
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

/// 逐行比对，返回全部不一致。空 = 一致。
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
    // ★ 先钉住「确实读到了东西」。比对函数在两侧都是空表时也会给绿，
    //   而那正是这道门最可能的失效方式——标题改了、表格换了写法。
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

/// ★ ★ 扫查器自证：一致时给绿，四种不一致各给红。
#[test]
fn table_parser_can_both_hit_and_miss() {
    const GOOD: &str = "\
# 三、执行顺序表（G49）

| 序 | 指令 | 类别 | M1 归属 |
|---|---|---|---|
| 20 | `header` | 中间件 | M1 自研 |
| 80 | `reverse_proxy` | 终结 | M1 自研 |

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

    // ① 序号被改
    let mut changed = code.clone();
    changed[0].order = 21;
    assert_eq!(diff(&rows, &changed).len(), 1, "序号改了必须抓到");

    // ② 类别被改
    let mut changed = code.clone();
    changed[1].kind = "中间件".into();
    assert_eq!(diff(&rows, &changed).len(), 1, "类别改了必须抓到");

    // ③ 代码多一条（= 新指令没写进文档）
    let mut more = code.clone();
    more.push(Row {
        order: 95,
        name: "新指令".into(),
        kind: "终结".into(),
        owner: "M1 自研".into(),
    });
    assert_eq!(diff(&rows, &more).len(), 1, "代码多一条必须抓到");

    // ④ 代码少一条（= 文档里有个没人实现的指令）
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
        // 空表会让这个循环一次都不跑而依然给绿——先钉住下界。
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
    // ★ DSL 参考 §九的样例印着 `FUL-DSL-0007: unknown directive`。
    //   印出去的号码就是契约的一部分——G51 明写「一旦发出就不能改含义」。
    assert_eq!(DiagCode::UNKNOWN_DIRECTIVE.as_str(), "FUL-DSL-0007");
    assert!(
        DOC.contains("error[FUL-DSL-0007]: unknown directive"),
        "文档 §九的样例变了，或者错误码变了——两者必须一起动"
    );
}

/// 抽出某个标题之后的第一个 ```text 代码块。
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
    // ★ ★ 这一条是**契约测试里最直接的一个**：文档里那份配置是给人抄的，
    //   抄下来编不过就是文档在骗人。而它同时还是 G49 那条容器排位的活判据——
    //   §一的示例只有在 `handle` 跑在 `respond` 之前时才成立。
    for heading in ["# 一、总览", "## 4.5 顶层非站点块"] {
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
    // ⚠ 只断言「编译得过」不够：把 handle 挪到 respond 后面它照样编译得过，
    //   只是那个「兜底 403」会变成恒胜，而**没有任何一行输出会说出来**。
    let src = code_block_after(DOC, "# 一、总览");
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
    assert!(DOC.contains("| 站点内无路由匹配 | 404 |"));
    assert!(DOC.contains("| 上游全部不健康 | 502 |"));
}
