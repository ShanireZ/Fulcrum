//! 诊断：源码位置、稳定错误码、rustc 风格的渲染。
//!
//! 规格是 **G51**（`PLAN.md` §10）：**一次报全 + 稳定错误码**，
//! 每条至少带 **行列号 + 源码片段 + caret**。样例见
//! [DSL 指令集参考](../../../docs/architecture/dsl-reference.md) §九。
//!
//! ★ ★ **错误码一旦发出就不能改含义**（G51 明写，与 D9 相邻）。所以它们集中登记在
//! [`DiagCode`] 的常量表里，并由 `tests/` 里的一条测试钉住「编号不重复、且与文档里
//! 那条样例用的 `FUL-DSL-0007` 对得上」——文档里印出去的那个号码就是契约的一部分。

use std::fmt::Write as _;

/// 源码里的一段字节区间。半开区间 `[start, end)`。
///
/// ★ 存**字节**偏移而不是行列：行列是渲染时才算的派生量。词法器每读一个 token 都去
/// 维护行列，既慢又容易在多字节字符上算错，而错了不会有任何症状——诊断只是指偏一格。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// 把两段合成一段（取外包络）。
    pub fn join(self, other: Span) -> Span {
        Span::new(self.start.min(other.start), self.end.max(other.end))
    }
}

/// 一份源码文件：路径 + 全文 + 行首偏移索引。
#[derive(Debug, Clone)]
pub struct Source {
    path: String,
    text: String,
    /// 每一行的起始字节偏移。`line_starts[0] == 0`。
    line_starts: Vec<usize>,
}

/// 一个字节偏移对应的人读位置。行、列都从 **1** 开始。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    pub line: usize,
    /// ★ 列按**字符**数，不是字节数。用户看到的是字符。
    pub col: usize,
}

impl Source {
    pub fn new(path: impl Into<String>, text: impl Into<String>) -> Self {
        let text: String = text.into();
        let mut line_starts = vec![0usize];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self {
            path: path.into(),
            text,
            line_starts,
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// 字节偏移 → 行列。偏移越界时钳到文件末尾（诊断宁可指得偏，也不该 panic）。
    pub fn location(&self, byte: usize) -> Location {
        let byte = byte.min(self.text.len());
        // 最后一个 <= byte 的行首。行数不多，线性/二分都行，这里用二分。
        let line_idx = match self.line_starts.binary_search(&byte) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let line_start = self.line_starts[line_idx];
        let col = self.text[line_start..byte].chars().count() + 1;
        Location {
            line: line_idx + 1,
            col,
        }
    }

    /// 取某一行的正文（不含行尾换行；`\r` 也一并去掉）。
    ///
    /// ⚠ `\r` 必须去：配置文件在 Windows 上被编辑过就会带 CRLF，留着它渲染出来的
    /// 那一行会把后面的 caret 行覆盖掉——终端上看是「错误提示少了一行」。
    pub fn line_text(&self, line: usize) -> &str {
        if line == 0 || line > self.line_starts.len() {
            return "";
        }
        let start = self.line_starts[line - 1];
        let end = self
            .line_starts
            .get(line)
            .map(|e| e - 1)
            .unwrap_or(self.text.len());
        self.text[start..end].trim_end_matches('\r')
    }
}

/// 稳定的诊断编号，渲染成 `FUL-DSL-0007`。
///
/// ★ ★ **只增不改**：一个编号一旦出现在某个版本的输出里，它的含义就固定了。
/// 要换含义就发新号，旧号留着（哪怕不再产生）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DiagCode(pub u16);

impl DiagCode {
    // ── 词法 ────────────────────────────────────────────────────────────────
    /// 文件在块还开着的时候结束了。
    pub const UNCLOSED_BLOCK: DiagCode = DiagCode(1);
    /// 多出来的 `}`。
    pub const UNEXPECTED_CLOSE: DiagCode = DiagCode(2);
    /// 引号没闭合。
    pub const UNCLOSED_QUOTE: DiagCode = DiagCode(3);

    // ── 结构 ────────────────────────────────────────────────────────────────
    /// 全局选项块必须在最前，且只能有一个。
    pub const GLOBAL_BLOCK_POSITION: DiagCode = DiagCode(4);
    /// 站点地址写法不合法。
    pub const BAD_SITE_ADDRESS: DiagCode = DiagCode(5);
    /// 站点块缺少 `{`。
    pub const SITE_BLOCK_EXPECTED: DiagCode = DiagCode(6);

    // ── 指令 ────────────────────────────────────────────────────────────────
    /// 未知指令。★ 这个号码印在 DSL 参考文档 §九的样例里，**不能改**。
    pub const UNKNOWN_DIRECTIVE: DiagCode = DiagCode(7);
    /// 指令参数个数不对。
    pub const BAD_ARITY: DiagCode = DiagCode(8);
    /// 未知的子指令。
    pub const UNKNOWN_SUBDIRECTIVE: DiagCode = DiagCode(9);
    /// 指令用在了不允许它出现的位置。
    pub const BAD_PLACEMENT: DiagCode = DiagCode(10);
    /// 需要子块却没给，或给了不该给的子块。
    pub const BLOCK_MISMATCH: DiagCode = DiagCode(11);

    // ── 匹配器（G50）────────────────────────────────────────────────────────
    /// 引用了没定义过的 `@name`。
    pub const UNKNOWN_MATCHER: DiagCode = DiagCode(12);
    /// 行内匹配器只能是路径。
    pub const INLINE_MATCHER_NOT_PATH: DiagCode = DiagCode(13);
    /// `@name` 重复定义。
    pub const DUPLICATE_MATCHER: DiagCode = DiagCode(14);
    /// 未知的匹配条件。
    pub const UNKNOWN_MATCHER_CONDITION: DiagCode = DiagCode(15);

    // ── 占位符（G61）────────────────────────────────────────────────────────
    /// 未知占位符。
    pub const UNKNOWN_PLACEHOLDER: DiagCode = DiagCode(16);
    /// 占位符用在了它不可用的位置。★ 这里必须是错误，不能解析成空串。
    pub const PLACEHOLDER_NOT_AVAILABLE: DiagCode = DiagCode(17);

    // ── 字面量类型（DSL 参考 §七）───────────────────────────────────────────
    /// 时长写法不对（★ 裸数字是错误，不做默认单位）。
    pub const BAD_DURATION: DiagCode = DiagCode(18);
    /// 大小写法不对。
    pub const BAD_SIZE: DiagCode = DiagCode(19);
    /// 布尔只认 `true` / `false`。
    pub const BAD_BOOL: DiagCode = DiagCode(20);
    /// 枚举取值不在允许集合里。
    pub const BAD_ENUM: DiagCode = DiagCode(21);
    /// 状态码不合法。
    pub const BAD_STATUS: DiagCode = DiagCode(22);

    // ── 站点级 ──────────────────────────────────────────────────────────────
    /// 同一站点里重复出现了只能有一次的指令。
    pub const DUPLICATE_DIRECTIVE: DiagCode = DiagCode(23);
    /// 未知的全局选项。
    pub const UNKNOWN_GLOBAL_OPTION: DiagCode = DiagCode(24);
    /// 站点地址在多个块里重复。
    pub const DUPLICATE_SITE_ADDRESS: DiagCode = DiagCode(25);
    /// M1 不做 `import`（G62）。
    pub const IMPORT_NOT_SUPPORTED: DiagCode = DiagCode(26);
    /// 一个站点块里一条指令都没有。
    pub const EMPTY_SITE: DiagCode = DiagCode(27);
    /// ★ ★ 这一条**专属于 G49**：内建顺序表把书写顺序与执行顺序拆开之后，
    /// 「我写在后面的兜底怎么先跑了」是必然会出现的意外。G49 配套第 4 条要求
    /// **诊断必须说出它实际跑在第几步**，否则用户只能去背那张表。
    pub const UNREACHABLE_STEP: DiagCode = DiagCode(28);
    /// `resolvers` 里的一条地址写得不对（形状层面：端口、主机名字符、IPv6 字面量……）。
    ///
    /// ★ ★ 新增。在此之前这一类错误长在**装载期**，处置是
    /// 「打一行 error，本站点的 DNS-01 不启用」—— 而 `validate` 退出码仍是 0、
    /// 站点照常起来，于是一份写错的配置**在每一处都显得正常**，
    /// 直到那张证书永远签不下来。⇒ 形状判据搬到编译期，它不需要网络。
    /// ⚠ 「主机名解析得出来吗」**不在**这条里：那要网络，留到签发那一刻。
    pub const BAD_RESOLVER: DiagCode = DiagCode(29);
    /// 自动 HTTP 重定向合成的站点，在 :80 上盖过了用户自己写的端口兜底站点。
    ///
    /// ★ warning 而不是 error：这是**有意的行为**（G12 承诺的自动跳转），
    /// 但它改变了那几个主机名在 :80 上的去向 —— 而沉默地改掉别人配置的行为，
    /// 正是本仓库反复点名的「现场看不出问题」。
    pub const AUTO_REDIRECT_SHADOWS: DiagCode = DiagCode(30);
    /// 凭据的来源前缀写错了（`fil:` / `ENV:` 这类）。
    ///
    /// ★ ★ 此后「不写前缀就是值本身」，于是**一个打错的前缀会被当成凭据**，
    /// 带着一个根本不是凭据的字符串去打对端 —— 现场是「凭据不对」，
    /// 而真正的原因是打错了几个字母。这条错误就是为它准备的。
    pub const BAD_CREDENTIAL_SOURCE: DiagCode = DiagCode(31);
    /// 一条**必填**的子指令没写（M2 批 F 起：`file_server { root … }`）。
    ///
    /// ★ 它与 `BAD_ARITY` 的差别在于「谁缺席」：BAD_ARITY 是子指令**写了**但参数个数不对，
    /// 这一条是子指令**整个没出现**——报错要指着那条链上指令本身，而不是指着一个不存在的 span。
    pub const MISSING_REQUIRED_SUB: DiagCode = DiagCode(32);
    /// 路径必须是绝对路径（G91）。
    ///
    /// ★ ★ 相对路径依赖进程 cwd，而 systemd 下 cwd 是 `/`、开发机上是项目目录
    /// ⇒ **同一份配置在两处指向两个地方**。那正是「能装载、行为不同」那一类事故，
    /// 现场看到的只是 404，没有任何东西说「你的 root 解析到别处去了」。
    pub const PATH_NOT_ABSOLUTE: DiagCode = DiagCode(33);
    /// 这条全局选项**曾经存在，现在删掉了**（`fallback_*`，G98）。
    ///
    /// ★ ★ 它与 `UNKNOWN_GLOBAL_OPTION` 有意分开：后者说的是「没这个东西」，
    /// 而这一条说的是「有过，没了，去哪了」。⚠ 合成一条的话，
    /// 一个照着旧文档写的配置会得到「你是不是想写 XXX」这种毫无帮助的建议。
    pub const REMOVED_GLOBAL_OPTION: DiagCode = DiagCode(34);
    /// 多个 `cache` 块对一件**进程级**的事给出了互相矛盾的值（M2 批 H 起：`disk`）。
    ///
    /// ★ ★ 它必须是 error 而不是「取第一个」：缓存后端整个进程只有一个，
    /// 而两个不同的 `disk` 目录里，**必有一个是用户以为生效、其实没有的**。
    /// ⚠ 与它相邻的 `capacity` 走的是另一条路（取最大值 + 装载日志说出生效值），
    /// 那条能成立是因为「容量取大一点」不会让任何一个站点拿不到它要的东西；
    /// 而**目录取一个**会让另一个站点的缓存整个落在别处，现场是
    /// 「我的缓存怎么没落到我写的那个盘上」，配置里一个字都看不出问题。
    pub const CACHE_BACKEND_CONFLICT: DiagCode = DiagCode(35);
    /// `log { headers … }` / `resp_headers` 的白名单里写了一个**敏感头**
    /// （**M2 批 L 第 ③ 步**）。
    ///
    /// ★ ★ ★ **取「编译期拒绝」而不是「运行时脱敏」，理由是两者的失效形态不同**：
    /// 脱敏表要跟得上每一个新的敏感头名，而**漏一个就是一次静默泄漏**
    /// —— 日志里多出一行 `req_hdr_authorization`，没有任何东西会说；
    /// 而编译期拒绝的失效形态是「这份配置装不上」，当场可见。
    ///
    /// ⚠ 理由要连着 **G114** 一起读：owner 推翻的是「记不记头」这条推荐项，
    /// **没有**推翻它背后那半条 ——「私钥、ACME 凭据、上游认证信息不得进普通日志」
    /// （安全基线，也是观测那一页「最容易在哪做错」第 2 条）。
    /// ★ **一个被推翻的推荐项，它的理由通常只有一半被推翻**，那半条要换一个落点继续成立。
    pub const SENSITIVE_HEADER_LOGGED: DiagCode = DiagCode(36);
    /// `metrics` 这一步**没有任何限制得了来源的匹配器**（**M2 批 M**，G116）。
    ///
    /// ★ ★ G116 把指标端点做成了普通站点块里的终结指令，代价写在它自己那一条里：
    /// **指标与业务共用监听器，matcher 写错就会把指标暴露出去**，而
    /// 「这一条只能靠**文档与诊断**兜，架构兜不住」。这个编号就是那个诊断。
    ///
    /// # ★ ★ ★ 判据是「限制得了来源」，不是「有没有匹配器」
    ///
    /// 一条匹配器算不算数，问的是：**它能不能把两个发同样请求的客户端分开？**
    ///
    /// | 条件 | 算不算 | 为什么 |
    /// |---|---|---|
    /// | `remote_ip` | ✅ | socket 对端，在这一层伪造不了 |
    /// | `header` | ✅ | 一个共享密钥可以放在这里 |
    /// | `path` · `path_regexp` · `host` · `method` · `query` | ❌ | 全都是请求行与请求头里的东西，**任何客户端都能照着发一份** |
    ///
    /// ⚠ ⚠ **「路径匹配器不算」是这条诊断现在的全部价值所在。**
    /// 从 nginx / Caddy 迁过来的人第一反应就是写一条 `handle /metrics { metrics }`，
    /// 于是「最可能出现的那个裸奔配置」恰恰是最先撞上来的那一个 ——
    /// 若把它算成保护，这条诊断就在它唯一要抓的东西上沉默，
    /// 而**一个只见过绿的门与一个不存在的门无法区分**。
    ///
    /// ⚠ 仍然判不动的那一半：**匹配器写得对不对**（网段圈没圈对、这台机器摆在谁后面、
    /// `header User-Agent Prometheus` 其实谁都挡不住）。编译期没有这些信息
    /// ⇒ **宁可漏报也不误报**：一条报「已保护」的假话会让人不再去看那一行，
    /// 而它恰恰是唯一还会有人看的机会。
    ///
    /// ★ warning 而不是 error：把指标开在内网可信段上、或者开在只有自己连得到的
    /// 环回地址上，都是正当配置 —— 一个拒绝装载的门会把它们一起挡掉。
    pub const METRICS_UNGUARDED: DiagCode = DiagCode(37);
    /// `reverse_proxy { weight <地址> … }` 里那个地址**不在这条 `reverse_proxy` 的上游清单里**
    /// （**M2 批 N**，裁决 R1）。
    ///
    /// ★ ★ 比对是**逐字相同**，不做归一化：`normalize_upstream`（`backend` → `backend:80`）
    /// 住在 `fulcrum-runtime`，而这一步在 `fulcrum-config`。在这边再写一份「差不多的」
    /// 就是分家，而分家的现场是「写了 `weight backend 3`，配置照过，权重没生效」。
    /// ⇒ 对不上就在装载期拒绝。
    ///
    /// ⚠ ⚠ **这条诊断必须把那条 `reverse_proxy` 的上游清单原样列出来**：
    /// 只说「找不到」等于让人去猜自己上一行写的到底是什么 —— 而这条错误最常见的成因
    /// 恰恰是两处写法差了一个端口。
    pub const UNKNOWN_WEIGHT_UPSTREAM: DiagCode = DiagCode(38);
    /// 同一个上游写了两条 `weight`（**M2 批 N**）。
    ///
    /// ★ ★ 必须是 error，⛔ **不许「后写的赢」**：那种规则下删掉或挪动其中一行
    /// 会**静默**改掉权重，而它与「两行本来就是一样的」在配置里长得一模一样。
    pub const DUPLICATE_WEIGHT: DiagCode = DiagCode(39);
    /// 权重不在 `[1, 65535]` 里 —— 含 `0`、负数、带单位、不是数字（**M2 批 N**，裁决 R3）。
    ///
    /// ★ ★ **`0` 不合法是有意的**：「这台不参与调度」**只有一种表达方式**，
    /// 就是管理面临时覆盖层的 `disable`（G18）。让 `weight 0` 也表示摘掉，
    /// 就是两条路做同一件事 —— 而两条路迟早分家，且分家那天没有任何东西会说。
    pub const BAD_WEIGHT: DiagCode = DiagCode(40);

    pub fn as_str(&self) -> String {
        format!("FUL-DSL-{:04}", self.0)
    }
}

/// 严重级别。M1 只有 `Error` 与 `Warning` 两档。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// 一条诊断。
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: DiagCode,
    /// 标题行那句话。
    pub message: String,
    pub span: Span,
    /// caret 下面那几个字（指着这一段到底哪里不对）。
    pub label: String,
    /// `= help:` 行，通常是「你是不是想写 X」。
    pub help: Option<String>,
    /// `= note:` 行，通常指向文档。
    pub note: Option<String>,
}

impl Diagnostic {
    pub fn error(code: DiagCode, span: Span, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code,
            message: message.into(),
            span,
            label: String::new(),
            help: None,
            note: None,
        }
    }

    pub fn warning(code: DiagCode, span: Span, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            ..Self::error(code, span, message)
        }
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// 渲染成 rustc 风格的一段文本。格式与 DSL 参考 §九的样例逐字符对齐。
    pub fn render(&self, src: &Source) -> String {
        let loc = src.location(self.span.start);
        let end = src.location(self.span.end.max(self.span.start));
        let line_no = loc.line.to_string();
        let gutter = " ".repeat(line_no.len());
        let line_text = src.line_text(loc.line);

        // caret 的起点与长度按**显示宽度**算，不是字符数——中文一个字占两列。
        // 用字符数会让含中文的那一行 caret 整体左偏，而偏移量恰好等于中文字数。
        let before: String = line_text.chars().take(loc.col - 1).collect();
        let pad = display_width(&before);
        let span_chars = if end.line == loc.line {
            end.col.saturating_sub(loc.col)
        } else {
            line_text.chars().count().saturating_sub(loc.col - 1)
        };
        let highlighted: String = line_text
            .chars()
            .skip(loc.col - 1)
            .take(span_chars.max(1))
            .collect();
        let caret_len = display_width(&highlighted).max(1);

        let mut out = String::new();
        let _ = writeln!(
            out,
            "{}[{}]: {}",
            self.severity.label(),
            self.code.as_str(),
            self.message
        );
        let _ = writeln!(out, "{gutter}--> {}:{}:{}", src.path(), loc.line, loc.col);
        let _ = writeln!(out, "{gutter} |");
        let _ = writeln!(out, "{line_no} | {line_text}");
        let label_sep = if self.label.is_empty() { "" } else { " " };
        let _ = writeln!(
            out,
            "{gutter} | {}{}{label_sep}{}",
            " ".repeat(pad),
            "^".repeat(caret_len),
            self.label
        );
        if self.help.is_some() || self.note.is_some() {
            let _ = writeln!(out, "{gutter} |");
        }
        if let Some(h) = &self.help {
            let _ = writeln!(out, "{gutter} = help: {h}");
        }
        if let Some(n) = &self.note {
            let _ = writeln!(out, "{gutter} = note: {n}");
        }
        out
    }
}

/// 终端上一段文本占几列。
///
/// ★ 只是够用的近似：把常见的东亚全角区间算作 2 列，其余算 1 列。
/// 它服务的是「caret 对不对得齐」这一件事，不追求 Unicode 宽度表的完备性。
/// ⚠ 组合字符（如声调符号）会被多算——本项目的配置里不会出现，出现了也只是偏一格。
pub fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

fn char_width(c: char) -> usize {
    let cp = c as u32;
    let wide = matches!(cp,
        0x1100..=0x115F        // 韩文字母
        | 0x2E80..=0x303E      // CJK 部首、假名标点
        | 0x3041..=0x33FF      // 平假名、片假名、注音、CJK 兼容
        | 0x3400..=0x4DBF      // CJK 扩展 A
        | 0x4E00..=0x9FFF      // CJK 统一表意
        | 0xA000..=0xA4CF      // 彝文
        | 0xAC00..=0xD7A3      // 韩文音节
        | 0xF900..=0xFAFF      // CJK 兼容表意
        | 0xFE30..=0xFE6F      // CJK 兼容形式
        | 0xFF00..=0xFF60      // 全角 ASCII
        | 0xFFE0..=0xFFE6      // 全角符号
        | 0x1F300..=0x1F9FF    // emoji
        | 0x20000..=0x3FFFD    // CJK 扩展 B 及以后
    );
    if wide { 2 } else { 1 }
}

/// 一次编译收集到的全部诊断。
///
/// ★ **一次报全**（G51）：调用方拿到的是一整份，不是遇到第一条就抛。
#[derive(Debug, Clone, Default)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, d: Diagnostic) {
        self.items.push(d);
    }

    pub fn items(&self) -> &[Diagnostic] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn has_errors(&self) -> bool {
        self.items.iter().any(|d| d.severity == Severity::Error)
    }

    pub fn error_count(&self) -> usize {
        self.items
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count()
    }

    /// 按源码位置排序后逐条渲染。
    ///
    /// ★ 排序是有意的：解析器与语义检查是分两遍跑的，不排序的话同一个文件的诊断
    /// 会按「先全部语法错、再全部语义错」出现，读的人要在文件里来回跳。
    pub fn render(&self, src: &Source) -> String {
        let mut sorted: Vec<&Diagnostic> = self.items.iter().collect();
        sorted.sort_by_key(|d| (d.span.start, d.code));
        let mut out = String::new();
        for d in sorted {
            out.push_str(&d.render(src));
            out.push('\n');
        }
        out
    }
}

/// 编辑距离，给「你是不是想写 X」用。
///
/// ★ 固定一组指令名 + 编辑距离，正是 G61 选「小而固定的占位符集合」时点名想要的
/// 那个好处：候选集是有限的，才提得出建议。
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// 在候选里找一个「像」的。找不到返回 `None`。
///
/// 阈值按长度放宽：短词允许 1 处不同，长词允许 2 处。全都不像时**不给建议**——
/// 一个乱指的 help 比没有 help 更耽误人。
pub fn suggest<'a>(input: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let limit = if input.chars().count() <= 4 { 1 } else { 2 };
    candidates
        .into_iter()
        .map(|c| (levenshtein(input, c), c))
        .filter(|(d, _)| *d <= limit)
        .min_by_key(|(d, c)| (*d, c.len()))
        .map(|(_, c)| c)
}
