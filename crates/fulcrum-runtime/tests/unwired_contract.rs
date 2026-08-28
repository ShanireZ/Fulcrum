//! [`UNWIRED`] 那份清单是**契约**，由这里逐字钉住。
//!
//! ★ ★ 为什么要钉：它记的是「M1 认得、但这一批还没接线」的能力。
//! 这类清单的两种烂法**方向相反、而且都不会有任何报错**：
//!
//! | 烂法 | 后果 |
//! |---|---|
//! | 接线做完了、忘了从清单里删 | 装载时永远打一条假警告，久了没人看 |
//! | 从清单里删了、但没接线 | 一条指令静静地什么都不做——**「声明了却没人接」** |
//!
//! 下面那张表两个方向都挡：**清单必须与它逐字相等**。
//! 接线完成时改这里是**必须的一步**，不是「记得也可以」。
//! ⚠ 判据取「逐字相等」而不是「有孤儿就红」——后者在清单本身错了的时候照样绿。
//!
//! ★ ★ 那张表管的是「清单本身对不对」，管不到**扫描认不认得配置里写的那一条**：
//! 扫描的结果与清单求交之后才出口 ⇒ **清单里没有的键，扫描写成什么样都看不见**。
//! 于是「按今天的清单挑一条来验」这种判据会随着能力接线一条条哑掉，且哑掉时不红。
//! ⇒ 全局选项那一半由 `全局选项那一半真的被扫到了` 拿一张**合成清单**单独守着。

use fulcrum_config::compile_str;
use fulcrum_runtime::{Runtime, UNWIRED};

/// M1 这一批**确实**还没接线的能力。改动这张表 = 改动契约。
const EXPECTED: &[&str] = &[
    "tls_internal",
    "on_demand",
    "tracing",
    "passive_fail",
    // ★ ★ **`weight` 在 M2 批 N 任务 2 销号**：它在任务 1 里进来（DSL 与结构化模型），
    //   在任务 2 里离开（四种 `lb_policy` 全部按累积权重挑）。⚠ 两头都由这张表挡着。
    // ★ ★ **能力做完就销号**（`encode` / `log` / `l4` / `proxy_protocol_from` 都是这么离开的）。
    //   ⚠ ⚠ 而删掉一行的**同时**必须真的接线 —— 这道门的全部意义就在这里：
    //   「实现了但忘了更新清单」与「删了清单但没实现」**都过不去**。
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

/// ★ ★ ★ **文档里那句「仍有几条只是解析得过」也要跟着这张表走。**
///
/// ⚠ 它补的是一处真的腐烂过的地方：`dsl-reference.md` 开头那句清单里曾一直挂着
/// **`自动签发`（ACME）**，而 ACME 早已全部接线、`UNWIRED` 里一条不剩。
/// 上面那道 `清单与契约逐字相等` 只钉 Rust 那一份，**文档那一份没有任何东西在看**。
///
/// ★ 判据是**双向**的：文档里出现的每一格都要还在 `UNWIRED` 里（防过期的警告），
/// `UNWIRED` 里的每一格也都要在文档那句里说得出来（防漏报）。
/// ⚠ 只做前一半的话，新加一条未接线能力而忘了写进文档，没有任何东西会红。
///
/// ★ 匹配用**文档里那几个反引号词**，而不是整句逐字相等：后者一改标点就红，
/// 而那种恒红的门最后一定会被人加 `#[ignore]`。
#[test]
fn 文档里那句未接线清单与_unwired_对得上() {
    const DOC: &str = include_str!("../../../docs/architecture/dsl-reference.md");
    // 那句话在文档开头的引言块里，以「仍有几条只是」开头，到行末的破折号为止。
    let start = DOC
        .find("仍有几条只是")
        .expect("dsl-reference.md 里找不到那句「仍有几条只是「解析得过」」——它被改写了？");
    // ⚠ 不要按固定字节数开窗：中文是多字节，切在字符中间会 panic。
    //   `find` 给回的一定是字符边界。
    let seg = &DOC[start..];
    let seg = match seg.find("——") {
        Some(i) => &seg[..i],
        None => panic!("那句清单后面应当有一个「——」把它收住"),
    };
    // ★ 顺带钉一下它没有跑飞：那句话是一行半，几百字节；
    //   万一「——」被删掉，上面会 panic；万一它离得很远，这里会红。
    assert!(
        seg.len() < 500,
        "从「仍有几条只是」到「——」有 {} 字节，太长了 —— 那句清单被改写过？",
        seg.len()
    );

    // 文档里写的是人话（`health_*` / `passive_*`），而 UNWIRED 里是具体的键名。
    // ★ 这张对照表本身就是契约的一部分：改名时两边都得动。
    let doc_word_for = |k: &str| -> &'static str {
        match k {
            "tls_internal" => "`tls internal`",
            "on_demand" => "`on_demand`",
            "encode" => "`encode`",
            "log" => "`log`",
            "tracing" => "`tracing`",
            "passive_fail" => "`passive_*`",
            // ⚠ ⚠ 对照词必须正好是**一个反引号词**：下面第 ② 半按反引号词扫文档，
            //   写成「全局的 `proxy_protocol_from`」的话，② 会从文档里抠出裸的那个、
            //   在 `expected` 里找不到，于是**红在一个与事实相反的理由上**。
            "proxy_protocol_from" => "`proxy_protocol_from`",
            other => panic!(
                "`{other}` 在 UNWIRED 里，而这条测试不知道它在文档那句里该写成什么。\
                 先在这里补上对照，再去文档里加 —— 顺序反过来的话，漏写没人看得见。"
            ),
        }
    };

    let mut problems: Vec<String> = Vec::new();
    // ① UNWIRED → 文档：每一条都要说得出来。
    for (k, _) in UNWIRED {
        let w = doc_word_for(k);
        if !seg.contains(w) {
            problems.push(format!("`{k}` 还没接线，而文档那句清单里没有 {w}"));
        }
    }
    // ② 文档 → UNWIRED：文档里出现的反引号词都得还在表里。
    //    ⚠ 这一半才是这次真出问题的那一半。
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

const DSL_DOC: &str = include_str!("../../../docs/architecture/dsl-reference.md");

/// `dsl-reference.md` 里一张**子指令表**的一行。
#[derive(Debug, PartialEq, Eq)]
struct SubRow {
    /// 第一格里的那些反引号名字。⚠ 一格里**可以有多个**
    /// （`` `passive_fail` / `passive_window` `` 就是一行）。
    names: Vec<String>,
    /// 这一行的行文里提到 `UNWIRED` 了吗。
    mentions_unwired: bool,
}

/// 抠出全部**子指令表**的数据行。
///
/// ★ 表的集合是**推导**出来的（表头第一格正好是「子指令」），⛔ 不是一张手写清单：
/// 手写清单会在下一次新加一张子指令表时静静漏掉那一张，而漏掉的那一刻不红。
fn sub_directive_rows(md: &str) -> Vec<SubRow> {
    let mut out = Vec::new();
    let mut in_table = false;
    for line in md.lines() {
        let t = line.trim();
        if !t.starts_with('|') {
            // 表以第一条非表格行为界。⚠ 不这样的话，紧跟其后的另一张表
            //   （表头不是「子指令」的那种）会被当成同一张表接着读下去。
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

/// ★ ★ ★ **子指令表那一行也要与 [`UNWIRED`] 对得上，两个方向都钉住。**
///
/// # 这道门补的是一个**实测过的洞**
///
/// 批 N 任务 1 给 `dsl-reference.md` 的 `reverse_proxy` 子指令表加了 `weight` 一行，
/// 行尾写着「现在只到配置层为止……调度还不认」。任务 1 实测：**把那一整行删掉，
/// 没有任何门会红** —— `doc_contract.rs` 判的是「`` `weight` `` 在**整份文档里**
/// 出现过没有」，而它当时同时出现在开头那句未接线清单里。
/// ⇒ 那句收尾话在调度接上之后就是一句**假话**，而没有任何东西会提醒人改它。
///
/// # 判据
///
/// - ① **文档 → 清单**：子指令表里凡是行文提到 `UNWIRED` 的那一行，
///   它第一格里的名字**必须**还在 [`UNWIRED`] 里 ⇒ 挡「接线了忘了改文档」。
/// - ② **清单 → 文档**：[`UNWIRED`] 里凡是在子指令表里有行的那个名字，
///   那一行**必须**提到 `UNWIRED` ⇒ 挡「文档说得像已经能用了、其实运行时不做」。
///
/// ⚠ ⚠ **②那一半才是这次漏掉的方向**（AGENTS.md 门禁纪律第二条：
/// 「一条锚在**缺失**上的判据，每次那东西发布时都要重新锚定」）——
/// ①只在文档里**还留着**那句话时才有活干，而它恰好会随着接线一条条哑掉。
/// ⇒ 下面另有一条断言钉住「今天至少有一行真的提到 `UNWIRED`」，
/// 否则①会在某一天变成一条空转的门，而空转的那一刻不红。
#[test]
fn 子指令表里的_unwired_字样与清单两个方向都对得上() {
    let rows = sub_directive_rows(DSL_DOC);
    // 先钉住「确实读到了东西」——比对在两侧都是空的时候也会给绿。
    assert!(
        rows.len() >= 25,
        "只从子指令表里读出 {} 行，表格写法多半变了；本次比对不能采信",
        rows.len()
    );
    // 而且读到的是**几张**表，不是只读了第一张。
    for must in ["lb_policy", "zones", "output"] {
        assert!(
            rows.iter().any(|r| r.names.iter().any(|n| n == must)),
            "子指令表里没读到 `{must}` —— 扫查器多半只读到了其中一张表"
        );
    }
    let unwired: std::collections::BTreeSet<&str> = UNWIRED.iter().map(|(k, _)| *k).collect();

    let mut problems: Vec<String> = Vec::new();
    // ① 文档 → 清单
    for r in &rows {
        if r.mentions_unwired && !r.names.iter().any(|n| unwired.contains(n.as_str())) {
            problems.push(format!(
                "子指令表里 {:?} 那一行说自己在 UNWIRED 里，而 UNWIRED 里已经没有它了 —— \
                 接线做完就要把那句收尾话改掉。过期的警告比过期的状态更危险。",
                r.names
            ));
        }
    }
    // ② 清单 → 文档
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

    // ★ ★ 最后钉住这道门**今天真的有活可干**：①那一半只在文档里还留着那句话时才生效。
    //   ⚠ 没有这一条的话，某天最后一条未接线能力接完、文档里那几句一起删掉，
    //   ①就变成一条空转的门 —— 而它会一直是绿的。
    assert!(
        rows.iter().any(|r| r.mentions_unwired),
        "子指令表里现在一行都没提到 UNWIRED ⇒ 上面①那一半是空转的。\
         若 UNWIRED 真的清空了，把这道门连同那张表一起销号；\
         若没有，那就是文档漏了。"
    );
}

/// ★ ★ 扫查器自证：一致时给绿，四种不一致各给红。
///
/// ⚠ 一个自己瞎了的扫查器照样会打印一份很像结论的报告 ——
/// 这条教训在 `AGENTS.md` 与 `doc_contract.rs` 里各记着一遍。
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
    // ① 只读子指令表，且读全了两张。
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
    // ② `UNWIRED` 字样按行认，不按整份文档认。
    assert_eq!(
        rows.iter().map(|r| r.mentions_unwired).collect::<Vec<_>>(),
        vec![false, true, false]
    );
    // ③ 一格里有多个名字时**每一个**都算数（`passive_fail` / `passive_window` 就是这形状）。
    assert!(rows[1].names.iter().any(|n| n == "b2"));
    // ④ 表头与分隔行不当成数据行（否则「子指令」会被当成一条子指令）。
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

/// 同上，但清单由调用方给 —— 见下面的 `全局选项那一半真的被扫到了`。
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

/// 全局选项那一半**真的被扫到了** —— 判据不挂在「今天恰好哪几条没接线」上。
///
/// # ★ ★ 为什么这一条非得拿合成表来验
///
/// `unwired_in_use` 收尾要与 [`UNWIRED`] 求交，而那张表里**现在一条全局选项都没有**
/// ⇒ 拿真表怎么写都验不到全局那半边：断言只能是空，而把整段全局扫描删掉，它照样是空。
/// ⚠ 这就是这条测试来之前的状态 —— `只报本次配置真的用到的那几条` 里那行 `admin`
/// 旁边曾写着「这一行守着全局那一半」，一句在 `admin`（批 9）接线的同一刻就过期、
/// 且不会有任何东西说出来的话。**一个只见过绿的门与一个不存在的门无法区分。**
///
/// ★ 合成表把两件事拆开：**清单里有没有这个键**（假的，只在本测试里存在）
/// 与**扫描认不认得它**（真的，扫的就是产品那一格）。于是接线进度再往前走，
/// 这条判据也不会到期 —— 那正是上一版没做到的事。
/// ⚠ 两个方向都要：只验「报得出来」的话，一个把 `used.insert` 写成无条件的实现照样绿。
#[test]
fn 全局选项那一半真的被扫到了() {
    // 形状与 UNWIRED 一样，内容与它无关。
    // ⚠ 键必须取**真的**（`admin`）：扫的是产品里那一格，假键只会验到
    //   「表里有、扫描不认得」，而那是另一件事。
    let 合成表: &[(&str, &str)] = &[("admin", "合成的登记词")];
    let 写了 = "{\n  admin unix//tmp/x.sock\n}\nhttp://a.com {\n  respond 200\n}\n";
    let 没写 = "http://a.com {\n  respond 200\n}\n";

    assert_eq!(
        unwired_for_table(合成表, 写了),
        vec!["admin"],
        "全局块里写着 `admin`，而扫描没认出来 —— 全局那一半没在工作"
    );
    // 反向半：⚠ 少了它，一个把 `used.insert` 写成无条件的实现照样绿。
    assert_eq!(
        unwired_for_table(合成表, 没写),
        Vec::<&str>::new(),
        "配置里没有 `admin`，却报了出来"
    );
}

#[test]
fn 只报本次配置真的用到的那几条() {
    // ★ 把八条全打出来会变成噪音，而噪音会把真的那几条一起埋掉。
    // 纯 HTTP + respond：一条都不该报。
    assert_eq!(
        unwired_for("http://a.com {\n  respond 200\n}\n"),
        Vec::<&str>::new()
    );

    // https 地址 + 默认自动 HTTPS，而配置里**没有 80 端口** → 只能靠 TLS-ALPN-01，
    // ★ 而它已经接线，所以**一条缺口都不该报**。
    //   ⚠ 留着旧期望的话，这道门会把一件已经做完的事继续报成没做 ——
    //   **过期的警告比过期的状态更危险**：它会主动阻止下一个人去用这条路。
    assert_eq!(
        unwired_for("a.com {\n  respond 200\n}\n"),
        Vec::<&str>::new()
    );

    // ★ ★ 同一份配置补上一个 80 端口的明文站点，同样一条都不该报。
    //   ⚠ 这是反向判据：只验「报得出来」而不验「该没有时真的没有」，
    //     一个恒报的实现照样绿。
    assert_eq!(
        unwired_for("a.com {\n  respond 200\n}\nhttp://a.com:80 {\n  respond 200\n}\n"),
        Vec::<&str>::new()
    );

    // ★ DNS-01 与 TLS-ALPN-01 都接上了 ⇒ 一个没配 `dns` 的通配符站点一条都不报。
    //   ⚠ 它当然仍然签不出证书（通配符只能走 DNS-01），但那由 `plan_tls` 的警告
    //   与 `Target::actionable()` 的「推迟」负责说，**不是这张未接线表的职责**。
    assert_eq!(
        unwired_for("*.a.com {\n  respond 200\n}\n"),
        Vec::<&str>::new()
    );

    // ★ 配了 `dns exec` 的站点：DNS-01 零端口依赖（G54），
    //   所以**「没有 80 端口」那一条对它不成立** —— 一条都不该报。
    //   ⚠ 不分这个岔的话，一个正确配了 DNS-01 的站点会被报成「缺 TLS-ALPN-01」。
    assert_eq!(
        unwired_for(
            "*.a.com {\n  tls {\n    dns exec /bin/true\n    resolvers 127.0.0.1:8053\n  }\n  respond 200\n}\n"
        ),
        Vec::<&str>::new()
    );

    // ★ 原生供应商（G57）接上了，所以写了 `dns cloudflare` 的站点**一条缺口都不该报**。
    //   ⚠ 注意这份配置比之前多了 `zones` —— 少了它现在是**编译期错误**（G59 第 3 条），
    //   而 `unwired_for` 里那句 `assert!(!has_errors)` 会先炸。
    //   ★ 也就是说：这一行既在验「不报缺口」，也顺带钉住了「G59 那道编译期门存在」。
    assert_eq!(
        unwired_for(
            "*.a.com {\n  tls {\n    dns cloudflare env:CF_TOKEN\n    zones a.com\n    resolvers 127.0.0.1:8053\n  }\n  respond 200\n}\n"
        ),
        Vec::<&str>::new()
    );

    // ★ 给了 PEM 路径的站点**不该**被报成未接线
    assert_eq!(
        unwired_for("a.com {\n  tls /c.pem /k.pem\n  respond 200\n}\n"),
        Vec::<&str>::new()
    );

    // `tls internal` 与 on_demand 各自单独报
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

    // ★ ★ M2 批 I 把这一条**翻了个方向**（与 `dns_refresh` 那次同款）：
    //   `encode` 接线了，一条写了 `encode gzip` 的配置**一条缺口都不该报**。
    //   ⚠ 留着旧期望的话，这道门会把一件已经做完的事继续报成没做 ——
    //   而假警告会训练人忽略整张表。
    assert_eq!(
        unwired_for("http://a.com {\n  encode gzip\n  respond 200\n}\n"),
        Vec::<&str>::new()
    );

    // ★ ★ 批 10 把这一条**翻了个方向**：`dns_refresh` 接线了，
    //   一条光写 `reverse_proxy` 的配置**一条缺口都不该报**。
    //   ⚠ 留着旧期望的话，这道门会把一件已经做完的事继续报成没做——
    //   而假警告会训练人忽略整张表。
    assert_eq!(
        unwired_for("http://a.com {\n  reverse_proxy x:1\n}\n"),
        Vec::<&str>::new()
    );

    // ★ ★ 批 11：`health_uri` 也接线了，只剩被动熔断这一条。
    //   ⚠ 这两条以前是绑在一起报的（`UNWIRED` 里 `passive_fail` 的理由甚至写着
    //   「随健康检查一起做」）—— 而它们其实是两件事：主动检查打的是一个
    //   专门的探测路径，被动熔断看的是**真实流量**的失败率。
    let got = unwired_for(
        "http://a.com {\n  reverse_proxy x:1 {\n    health_uri /h\n    passive_fail 3\n  }\n}\n",
    );
    assert_eq!(got, vec!["passive_fail"]);

    // ★ 反向那一半：只写健康检查、不写被动熔断 ⇒ **一条都不该报**。
    //   ⚠ 少了它，一个「见到任何 health_* 就报 passive_fail」的实现照样绿。
    assert_eq!(
        unwired_for("http://a.com {\n  reverse_proxy x:1 {\n    health_uri /h\n  }\n}\n"),
        Vec::<&str>::new()
    );

    // ★ ★ **批 N 任务 2 把这一条翻了个方向**（与 `dns_refresh` / `encode` / `health_uri`
    //   那三次同款）：任务 1 里「写了 `weight` ⇒ 报未接线」，任务 2 接线之后
    //   **写了权重的配置一条都不该报**。
    //   ⚠ 留着旧期望的话，这道门会把一件已经做完的事继续报成没做 ——
    //   而假警告会训练人忽略整张表。
    assert_eq!(
        unwired_for("http://a.com {\n  reverse_proxy x:1 x:2 {\n    weight x:1 3\n  }\n}\n"),
        Vec::<&str>::new()
    );
    // 没写 `weight` 的那条同样一条都不报。
    assert_eq!(
        unwired_for("http://a.com {\n  reverse_proxy x:1 x:2\n}\n"),
        Vec::<&str>::new()
    );

    // l4 —— ★ ★ 这一格在一天里翻了**两次**方向：
    //   批 A 之前「有 `l4` 块就报」；批 A（TCP 接线）之后改成「有 `udp` 才报」；
    //   批 B（UDP 接线）之后**两种协议都不报**。
    //   ⚠ 每一次不跟着改都会留下一条**假警报**：一个正在转发的端口被日志说成「还没接线」。
    //   ★ 三份夹具都留着（纯 tcp / 纯 udp / 混写），它们分别守着那三种写法。
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

    // ★ 全局选项 `admin` 在批 9 已经接线（管理面 + 全量原子 load + 强制续期），
    //   所以**一条都不该报**。
    //   ⚠ ⚠ 这一行**只**验到这里为止：`admin` 不在 `UNWIRED` 里，求交之后本来就是空的
    //   ⇒ 把整段全局扫描删掉，它照样绿。它守的是「已接线的能力不许被报成没接线」，
    //   **不是**「全局那一半在工作」—— 后者由 `全局选项那一半真的被扫到了` 拿合成表守。
    //   ★ 两条各守一件事，别把其中一条当成另一条：这一行的注释上一版正是这么写错的。
    assert_eq!(
        unwired_for("{\n  admin unix//tmp/x.sock\n}\nhttp://a.com {\n  respond 200\n}\n"),
        Vec::<&str>::new()
    );

    // tracing
    assert_eq!(
        unwired_for("http://a.com {\n  tracing\n  respond 200\n}\n"),
        vec!["tracing"]
    );

    // ⚠ ⚠ 这里原本用的是 `log`，而 **M2 批 L 第 ② 步把它接线了** ——
    //   于是这条夹具在当天红了，**而它红得对**：
    //   一条「写了它、而运行时什么都不做」的判据，在那件事不再成立时必须红。
    //   ★ 换成一条**今天仍然没接线**的（`passive_fail`），而不是把断言改成空 ——
    //   改成空的话，这条判据就再也不检查「站点内的未接线能力会被报出来」了。
    assert_eq!(
        unwired_for("http://a.com {\n  reverse_proxy 10.0.0.1:80 {\n    passive_fail 3\n  }\n}\n"),
        vec!["passive_fail"]
    );
}

#[test]
fn 容器里面的也要被看见() {
    // ⚠ 反向判据：只扫站点顶层的话，`route { … }` 里那条会被漏掉，
    //   而漏掉的表现是「一条都不报」—— 看起来像「没有未接线的东西」。
    // ★ 夹具里那条能力接线之后，**换夹具而不是删掉这条**：它验的是**扫不扫容器里面**。
    //   > 一条被删掉的判据，与一个从来没有过判据的功能，事后看起来一模一样。
    assert_eq!(
        unwired_for("http://a.com {\n  route {\n    tracing\n    respond 200\n  }\n}\n"),
        vec!["tracing"]
    );
    assert_eq!(
        unwired_for("http://a.com {\n  handle {\n    tracing\n    respond 200\n  }\n}\n"),
        vec!["tracing"]
    );
    // handle_errors 里的也算
    assert_eq!(
        unwired_for("http://a.com {\n  respond 200\n  handle_errors {\n    respond 500\n  }\n}\n"),
        Vec::<&str>::new()
    );
}

#[test]
fn 顺序跟着_unwired_的声明顺序走而不是随机() {
    // 报告的顺序要稳定，否则装载日志每次都不一样，diff 起来全是噪音。
    let got = unwired_for(
        "a.com {\n  tracing\n  encode gzip\n  log {\n    level info\n  }\n  reverse_proxy x:1 {\n    passive_fail 3\n  }\n}\n",
    );
    let order: Vec<usize> = got
        .iter()
        .map(|k| UNWIRED.iter().position(|(u, _)| u == k).unwrap())
        .collect();
    let mut sorted = order.clone();
    sorted.sort_unstable();
    assert_eq!(order, sorted, "输出顺序必须与 UNWIRED 的声明顺序一致");
}
