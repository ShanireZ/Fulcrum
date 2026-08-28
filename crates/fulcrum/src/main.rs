//! 枢衡 Fulcrum 的产品二进制。
//!
//! ```text
//! fulcrum validate <file>          # 只检查，出错走退出码 1
//! fulcrum compile  <file> [-o f]   # DSL → 结构化配置（JSON，G48）
//! fulcrum plan     <file>          # 人读的执行计划：每条指令实际跑在第几步 + 回落
//! fulcrum serve    <file>          # 起数据面（HTTP/HTTPS、ACME、反代、管理面）
//! ```
//!
//! ⚠ **不要在这里写现状描述** —— 一句过期的现状描述比没有描述更糟，
//! 它会让下一个人按一个不存在的世界做判断。能力面以 `PLAN.md` §1 为准。
//!
//! ★ `serve` 由 systemd 托管（G31/G33/G37/G78）：前台运行、`Type=notify`、
//! `SIGUSR2` 触发零停机换代。部署形状见 `docs/platform/deploy.md`。
//! `spikes/` 下那两个仍是验证台，不是产品代码。
//!
//! ★ ★ **`validate` 现在跑两层校验，不是一层**：DSL 前端 + **运行时图构建**。
//! 后者不是重复劳动——结构化配置那一层是**公开入口**（G11：机器直接写它），
//! 而机器写进来的一份根本不经过词法与语法。正则编不过、CIDR 不合法、上游地址写错，
//! 都只有构建运行时图时才看得见。
//!
//! ★ 没有用任何命令行解析库。三条子命令、两个选项，手写二十行比拖一棵依赖树划算，
//! 而 G29 那套「追新 + 24 小时隔离期 + 每周检查」是按依赖条数收费的。

use fulcrum_config::model::{MatcherRef, Step, StepBody, StructuredConfig};
use fulcrum_config::{Outcome, compile_str};
use fulcrum_runtime::Runtime;
use std::io::Write as _;
use std::process::ExitCode;
use std::sync::Arc;

const USAGE: &str = "\
枢衡 Fulcrum

用法：
    fulcrum validate <file>              检查一份 DSL 配置（含运行时图构建）
    fulcrum compile  <file> [-o <out>]   编译成结构化配置（JSON）
                     [--with-secrets]    ★ 输出里露出真凭据（默认脱敏）
    fulcrum plan     <file>              打印执行计划与回落路由
    fulcrum serve    <file> [选项]       起数据面（由 systemd 托管，见 docs/platform/deploy.md）

serve 的选项：
    --bind-host <H>      监听地址的主机部分，默认 0.0.0.0
    --pid-file <P>       默认 /run/fulcrum/fulcrum.pid
    --upgrade-sock <S>   默认 /run/fulcrum/upgrade.sock
    --state-dir <D>      状态目录（证书存在 <D>/certs/ 下），默认 /var/lib/fulcrum
    -u, --upgrade        从正在跑的旧世代接管（零停机换代）

退出码：
    0  没有 error 级诊断
    1  有 error 级诊断（含运行时图构建失败）
    2  命令行用法错误 / 读不到文件
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("fulcrum: {msg}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run(args: &[String]) -> Result<ExitCode, String> {
    let Some(cmd) = args.first() else {
        return Err("缺少子命令".to_string());
    };
    if cmd == "-h" || cmd == "--help" || cmd == "help" {
        print!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }

    let (positional, flags) = split_args(&args[1..])?;
    let path = positional.first().ok_or("缺少配置文件路径")?;
    if positional.len() > 1 {
        return Err(format!(
            "一次只能给一份配置文件（多给了 {}）",
            positional[1]
        ));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("读不到 {path}：{e}"))?;
    let outcome = compile_str(path, &text);

    // 状态目录：证书存储的根。★ 校验 TLS 也要用它，所以在分支之前先定下来。
    let state_dir = flags
        .state_dir
        .clone()
        .unwrap_or_else(|| fulcrum_server::ServeOptions::default().state_dir);

    // ★ 四层校验一次跑完：诊断 → 结构化配置 → 运行时图 → TLS 装载。
    let Some((cfg, rt)) = prepare(&outcome, &state_dir) else {
        return Ok(ExitCode::from(1));
    };

    // ── ★ ★ ★ 权限门（批 22）─────────────────────────────────────
    //
    //   凭据可以内联进 Fulcrumfile 之后，**这份文件的性质变了**：它是秘密了。
    //   ⇒ 只要配置里出现字面量凭据，就检查一次「别人读不读得到」。
    //
    // ★ 它挂在**所有**子命令上，不只是 serve：`validate` 的全部意义就是
    //   「上线之前先问一遍」，而一份 0644 带着凭据的配置正是它该拦住的东西。
    // ⚠ 没有字面量凭据时**一个字都不说** —— 不去管别人怎么放一份没有秘密的配置。
    match fulcrum_config::secret_guard::check(path, cfg) {
        Ok(None) => {}
        Ok(Some(note)) => eprintln!("· {note}"),
        Err(e) => {
            eprintln!("fulcrum: {e}");
            return Ok(ExitCode::from(1));
        }
    }

    match cmd.as_str() {
        "validate" => {}
        "compile" => {
            // ★ ★ **默认脱敏。** 这份 JSON 有两个去处：给人看，和 `POST /load` 的载荷。
            //   前者不该看见凭据，后者需要 —— 而**默认必须服从前者**：
            //   一个默认吐真值的命令，只要有人把输出贴进 issue 一次就完了，
            //   而那一次不会有任何提示。
            // ⚠ 需要真值时显式 `--with-secrets`，并且**在 stderr 上说一声**：
            //   它是一次有后果的操作，不该悄无声息。
            let json = if flags.with_secrets {
                if fulcrum_config::secret_guard::has_inline_secret(cfg) {
                    eprintln!(
                        "⚠ --with-secrets：输出里含**真凭据**。★ 它的去处应该只有 \
                         `POST /load` 的载荷；别贴进 issue、别进版本控制。"
                    );
                }
                fulcrum_config::secret::reveal(|| serde_json_to_string(cfg))
            } else {
                serde_json_to_string(cfg)
            };
            match json {
                Ok(json) => write_out(flags.out.as_deref(), &json),
                Err(e) => eprintln!("fulcrum: 序列化失败：{e}"),
            }
        }
        "plan" => {
            print!("{}", render_plan(cfg));
            let unwired = rt.unwired_in_use(cfg);
            if !unwired.is_empty() {
                println!("\n⏳ 这一批还没接线（配置里用到了，但运行时不生效）：");
                for (k, why) in unwired {
                    println!("    {k}：{why}");
                }
            }
        }
        "serve" => {
            init_logging();
            let mut opts = fulcrum_server::ServeOptions::default();
            if let Some(h) = flags.bind_host {
                opts.bind_host = h;
            }
            if let Some(p) = flags.pid_file {
                opts.pid_file = p;
            }
            if let Some(s) = flags.upgrade_sock {
                opts.upgrade_sock = s;
            }
            opts.state_dir = state_dir;
            opts.upgrade = flags.upgrade;
            // ★ 不返回：Pingora 的 `run_forever()` 接管进程。
            fulcrum_server::serve(cfg, rt, opts);
        }
        other => return Err(format!("未知子命令 `{other}`")),
    }

    let warnings = outcome
        .diagnostics
        .items()
        .iter()
        .filter(|d| d.severity == fulcrum_config::Severity::Warning)
        .count();
    if warnings > 0 {
        eprintln!("{warnings} 条警告。");
    }
    Ok(ExitCode::SUCCESS)
}

/// 日志：`serve` 才需要。★ 格式里带 pid，因为零停机换代时**两代会同时在跑**，
/// 而分不清哪一行是谁打的，升级窗口里的日志就没法读。
fn init_logging() {
    use std::io::Write as _;
    let mut b = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    b.format(|buf, record| {
        writeln!(
            buf,
            "[{} {:<5} pid={} {}] {}",
            buf.timestamp(),
            record.level(),
            std::process::id(),
            record.target(),
            record.args()
        )
    })
    .init();
}

/// 诊断 → 结构化配置 → 运行时图。三层都过了才返回 `Some`。
///
/// ★ 诊断永远打到 **stderr**，产物永远打到 **stdout**。分开不是洁癖：
/// `fulcrum compile x.Fulcrumfile > cfg.json` 必须能用，
/// 而只要有一条诊断混进 stdout，那份 JSON 就坏了——**而且坏得很安静**。
fn prepare<'a>(
    outcome: &'a Outcome,
    state_dir: &str,
) -> Option<(&'a StructuredConfig, Arc<Runtime>)> {
    if !outcome.diagnostics.is_empty() {
        eprint!("{}", outcome.render_diagnostics());
    }
    let cfg = match &outcome.config {
        Some(c) => c,
        None => {
            eprintln!("{} 个错误。", outcome.diagnostics.error_count());
            return None;
        }
    };

    // ⚠ 这里原本按 G52 逐条列出走回落的路由。回落层（M2 批 G）
    //   整块删除（G98）⇒ **没有回落路由可列了**。
    //   ★ G52 那条纪律本身没作废，它换了对象：现在由缓存与静态文件那两处
    //   「把生效的默认值打出来」承担（`log_cache_summary` / hide 清单）。

    // ★ ★ 第三层：运行时图。结构化配置是公开入口（G11），所以这一层也校验。
    let rt = match Runtime::build(cfg) {
        Ok(rt) => Arc::new(rt),
        Err(errors) => {
            eprintln!("运行时图建不起来，{} 处：", errors.len());
            for e in &errors {
                eprintln!("    {e}");
            }
            return None;
        }
    };

    // ★ ★ 第四层：TLS 装载。**证书路径写错、私钥与证书对不上，要在这里红**，
    //   而不是等第一个客户端连上来变成一次握手失败——后者在日志里只是一行
    //   「TLS error」，看不出是配置写错了。
    let cert_root = std::path::Path::new(state_dir).join("certs");
    // ★ 签发者目录名跟着 `acme_ca` 走 —— `validate` 要看的是**这份配置**会用哪个目录，
    //   而不是某个写死的默认值。写死它的后果是：换了 CA 之后 validate 一切正常，
    //   而 serve 起来发现一张证书都没有。
    let issuer = fulcrum_acme::issuer_slug(
        outcome
            .config
            .as_ref()
            .and_then(|c| c.global.acme_ca.as_deref())
            .unwrap_or(fulcrum_acme::LETSENCRYPT_PRODUCTION),
    );
    // ★ `default_sni` 与 `issuer` 一样从**这份配置**的全局块取：`validate` 要回答的是
    //   「这份配置起来之后会怎样」，而不是某个写死的默认值。
    match fulcrum_server::tls::plan_tls(&rt, &cert_root, &issuer, cfg.global.default_sni.as_deref())
    {
        Ok(plan) => {
            for n in &plan.notes {
                eprintln!("⏳ {n}");
            }
            let known = plan.resolver.known();
            if !known.is_empty() {
                eprintln!("已装载 {} 个 SNI 的证书：{known:?}", known.len());
            }
        }
        Err(errors) => {
            eprintln!("TLS 装载失败，{} 处：", errors.len());
            for e in &errors {
                eprintln!("    {e}");
            }
            return None;
        }
    }

    Some((cfg, rt))
}

/// 把子命令之后的参数拆成「位置参数」与 `-o` 的取值。
///
/// ★ **选项与位置参数的顺序是用户自由的，代码不该假定它。**
/// 固定取 `args[1]` 的话，`fulcrum compile -o out.json in.Fulcrumfile`
/// 会去读一个名叫 `-o` 的文件。
/// ⚠ 未知选项**当场报错**而不是当成路径——否则拼错一个选项会变成一句
/// 「读不到 --ouput」，方向指错。
fn split_args(rest: &[String]) -> Result<(Vec<String>, Flags), String> {
    let mut positional = Vec::new();
    let mut f = Flags::default();
    let mut i = 0;
    while i < rest.len() {
        let need = |i: usize, name: &str| -> Result<String, String> {
            rest.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("`{name}` 后面要跟一个值"))
        };
        // ★ ★ ★ **每个臂说出自己吃了几个参数，由循环统一推进。**
        //
        //   在此之前是「每个臂自己 `i += n`」，而那个约定有一个**不会报错的失效方式**：
        //   新加一个臂时忘了推进 —— 编译过、clippy 不响、测试跑不到，
        //   而运行时**死循环**。⚠ 加 `--with-secrets` 时当场踩了一次：
        //   `fulcrum compile … --with-secrets` 直接挂住，什么都不打印。
        //
        //   ⇒ 改成 `let consumed = match … { … => n }`：**忘了给数字就是类型错误**。
        //   这与本仓库那条「G49 的序号写在每行最前面，忘了给位置这个选项不存在」是同一招：
        //   **让错误变成写不出来的东西，而不是靠记得。**
        let consumed: usize = match rest[i].as_str() {
            // ★ 只有长写法，没有 `-s`：这是一次有后果的操作，
            //   而单字母开关最容易在别人的脚本里被顺手加上。
            "--with-secrets" => {
                f.with_secrets = true;
                1
            }
            "-o" | "--output" => {
                f.out = Some(need(i, "-o")?);
                2
            }
            "--bind-host" => {
                f.bind_host = Some(need(i, "--bind-host")?);
                2
            }
            "--pid-file" => {
                f.pid_file = Some(need(i, "--pid-file")?);
                2
            }
            "--upgrade-sock" => {
                f.upgrade_sock = Some(need(i, "--upgrade-sock")?);
                2
            }
            "--state-dir" => {
                f.state_dir = Some(need(i, "--state-dir")?);
                2
            }
            "-u" | "--upgrade" => {
                f.upgrade = true;
                1
            }
            other if other.len() > 1 && other.starts_with('-') => {
                return Err(format!("未知选项 `{other}`"));
            }
            other => {
                positional.push(other.to_string());
                1
            }
        };
        // ⚠ 再加一道：`consumed` 恒 ≥ 1 由上面每个臂保证，但**别让它靠保证**。
        //   一个返回 0 的臂会把这里变回死循环，而这一行让它变成一句能看懂的错。
        if consumed == 0 {
            return Err(format!(
                "内部缺陷：解析 `{}` 时没有推进参数索引 —— 请连同命令行一起报告",
                rest[i]
            ));
        }
        i += consumed;
    }
    Ok((positional, f))
}

#[cfg(test)]
mod arg_tests {
    use super::*;

    fn split(args: &[&str]) -> Result<(Vec<String>, Flags), String> {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        split_args(&owned)
    }

    #[test]
    fn 每一个开关都会推进索引() {
        // ★ ★ 这条测试的价值不在断言的内容，而在**它会不会返回** ——
        //   一个忘了推进的臂会让它挂死，而挂死是测得出来的（超时）。
        //   ⚠ 但它只是第二道：第一道是 `consumed: usize` 那个类型。
        for one in ["--with-secrets", "-u", "--upgrade"] {
            let (pos, _) = split(&[one, "cfg"]).expect("应当解析得了");
            assert_eq!(pos, vec!["cfg".to_string()], "开关 {one} 之后位置参数丢了");
        }
        for (two, _v) in [
            ("-o", "out.json"),
            ("--output", "out.json"),
            ("--bind-host", "[::]"),
            ("--pid-file", "/run/x.pid"),
            ("--upgrade-sock", "/run/x.sock"),
            ("--state-dir", "/var/lib/x"),
        ] {
            let (pos, _) = split(&[two, "value", "cfg"]).expect("应当解析得了");
            assert_eq!(
                pos,
                vec!["cfg".to_string()],
                "带值开关 {two} 之后位置参数丢了"
            );
        }
    }

    #[test]
    fn with_secrets_默认是关的() {
        let (_, f) = split(&["cfg"]).unwrap();
        assert!(!f.with_secrets, "默认就露真凭据了");
        let (_, f) = split(&["cfg", "--with-secrets"]).unwrap();
        assert!(f.with_secrets);
    }

    #[test]
    fn 选项与位置参数的顺序是自由的() {
        // ⚠ 钉的是 `compile -o out.json in.Fulcrumfile` 不能去读一个叫 `-o` 的文件。
        let (pos, f) = split(&["-o", "out.json", "in.Fulcrumfile"]).unwrap();
        assert_eq!(pos, vec!["in.Fulcrumfile".to_string()]);
        assert_eq!(f.out.as_deref(), Some("out.json"));
    }
}

#[derive(Default)]
struct Flags {
    out: Option<String>,
    bind_host: Option<String>,
    pid_file: Option<String>,
    upgrade_sock: Option<String>,
    state_dir: Option<String>,
    upgrade: bool,
    /// `compile --with-secrets`：输出里露出真凭据。★ 默认 false。
    with_secrets: bool,
}

fn write_out(path: Option<&str>, text: &str) {
    match path {
        None => {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(text.as_bytes());
            let _ = out.write_all(b"\n");
        }
        Some(p) => {
            if let Err(e) = std::fs::write(p, format!("{text}\n")) {
                eprintln!("fulcrum: 写不进 {p}：{e}");
            }
        }
    }
}

fn serde_json_to_string(cfg: &StructuredConfig) -> Result<String, String> {
    fulcrum_config::model::to_pretty_json(cfg).map_err(|e| e.to_string())
}

// ── 执行计划 ────────────────────────────────────────────────────────────────

/// 人读的执行计划。
///
/// ★ 它存在的理由是 **G49 配套第 4 条**：内建顺序表让书写顺序 ≠ 执行顺序，
/// 那就必须有一个地方能回答「我这条到底跑在第几步」。
/// 让人去背 `docs/architecture/dsl-reference.md` §三那张表，是把成本转嫁给用户。
fn render_plan(cfg: &StructuredConfig) -> String {
    let mut out = String::new();
    out.push_str("执行计划（★ 站点块内按内建顺序表执行，不按书写顺序 —— G49）\n");
    for site in &cfg.sites {
        let names: Vec<String> = site.addresses.iter().map(|a| a.raw.clone()).collect();
        out.push_str(&format!("\n站点 {}\n", names.join(", ")));
        for a in &site.addresses {
            out.push_str(&format!(
                "    {} :{}{}{}\n",
                a.scheme,
                a.port,
                if a.wildcard {
                    "  通配符（需要 DNS-01）"
                } else {
                    ""
                },
                if a.auto_https { "  自动 HTTPS" } else { "" }
            ));
        }
        if site.chain.is_empty() {
            out.push_str("    （空：所有请求得到 404）\n");
        }
        for step in &site.chain {
            render_step(&mut out, step, 1);
        }
        if !site.error_handler.is_empty() {
            out.push_str("    handle_errors：\n");
            for step in &site.error_handler {
                render_step(&mut out, step, 2);
            }
        }
    }
    if let Some(l4) = &cfg.l4 {
        out.push_str("\nL4 面\n");
        for l in &l4.listeners {
            // ★ ★ M2 批 A/B：逐条说出**这一条到底由谁来跑**。
            //   ⚠ 批 A 时这里还要分岔（TCP 自己转发、UDP 走回落），批 B 之后两种都是自研 ——
            //   而那句「回落 → caddy」也随之整条删掉，连同 `L4Config` 里那个字段。
            //   ★ `plan` 的全部价值就是回答「我这条到底会怎么跑」，**它说错比不说更贵**。
            let who = match l.proto.as_str() {
                "udp" => "枢衡自己转发（按客户端地址分会话，空闲 60s 回收）",
                _ => "枢衡自己转发（轮询；建连阶段会换下一个上游）",
            };
            out.push_str(&format!(
                "    {} {} → {}　{}\n",
                l.proto,
                l.listen,
                l.upstreams.join(", "),
                who
            ));
        }
    }
    out.push_str(&format!(
        "\n默认响应（G63）：无站点匹配 {} · 无路由匹配 {} · 上游全挂 {}\n",
        cfg.defaults.no_site_match, cfg.defaults.no_route_match, cfg.defaults.all_upstreams_down
    ));
    out
}

fn render_step(out: &mut String, step: &Step, depth: usize) {
    let indent = "    ".repeat(depth);
    let matcher = match &step.matcher {
        None => String::new(),
        Some(MatcherRef::Named(n)) => format!("  @{n}"),
        Some(MatcherRef::Path(p)) => format!("  {p}"),
    };
    out.push_str(&format!(
        "{indent}[{:>2}] {}{}{}\n",
        step.order,
        step.body.directive_name(),
        matcher,
        detail(&step.body)
    ));
    match &step.body {
        StepBody::Route { steps } => {
            out.push_str(&format!("{indent}     （块内按书写顺序 —— 保序逃生口）\n"));
            for s in steps {
                render_step(out, s, depth + 1);
            }
        }
        StepBody::Handle { arms } => {
            for (i, arm) in arms.iter().enumerate() {
                let m = match &arm.matcher {
                    None => "（兜底）".to_string(),
                    Some(MatcherRef::Named(n)) => format!("@{n}"),
                    Some(MatcherRef::Path(p)) => p.clone(),
                };
                out.push_str(&format!("{indent}     分支 {} {m}\n", i + 1));
                for s in &arm.steps {
                    render_step(out, s, depth + 2);
                }
            }
        }
        _ => {}
    }
}

// ★ 回落层已整层删除（G98）⇒ 计划输出里**再没有额外跳数可标**。

fn detail(body: &StepBody) -> String {
    match body {
        StepBody::ReverseProxy {
            upstreams,
            lb_policy,
            dns_refresh_ms,
            ..
        } => format!(
            "  {} [{lb_policy}, dns_refresh {}s]",
            upstreams.join(" "),
            dns_refresh_ms / 1000
        ),
        StepBody::Respond { status, .. } => format!("  {status}"),
        StepBody::Redir { to, code } => format!("  {to} ({code})"),
        StepBody::Rewrite { to } => format!("  {to}"),
        StepBody::Encode { encodings } => format!("  {}", encodings.join(" ")),
        StepBody::Header { ops } => format!(
            "  {}",
            ops.iter()
                .map(|o| format!("{} {}", o.op, o.name))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        StepBody::Handle { arms } => format!("  {} 个分支", arms.len()),
        StepBody::Route { steps } => format!("  {} 步", steps.len()),
        StepBody::FileServer { root, .. } => match root {
            Some(r) => format!("  root {r}"),
            None => String::new(),
        },
        // ★ 缓存进 plan 输出（M2 批 G）：`ttl` 是**兜底**不是覆盖（G96），
        //   ⚠ 而这件事必须在 `plan` 里说出来 —— 从 nginx 迁过来的人默认以为它是覆盖。
        StepBody::Cache {
            ttl_ms,
            max_size_bytes,
            capacity_bytes,
            disk_dir,
        } => format!(
            "  ttl={} max_size={} capacity={} 后端={}",
            match ttl_ms {
                Some(ms) => format!("{}s（兜底：上游给了新鲜度就听上游的）", ms / 1000),
                None => "（未配 ⇒ 上游没给新鲜度就不缓存）".to_string(),
            },
            max_size_bytes
                .map(|b| b.to_string())
                .unwrap_or_else(|| "默认".into()),
            capacity_bytes
                .map(|b| b.to_string())
                .unwrap_or_else(|| "默认".into()),
            // ★ 后端进 `plan`（M2 批 H）：`plan` 是「不真跑一遍就能看见会发生什么」
            //   的那个出口，而「缓存落在内存还是那个盘上」正是要看的东西之一。
            //   ⚠ 它说的是**配置选了哪个**；「目录用不了于是关掉」那一档只有
            //   真启动时才知道，由装载日志说 —— 两处各说各能说的那一半。
            match disk_dir {
                Some(d) => format!("磁盘 {d}"),
                None => "内存".to_string(),
            },
        ),
        StepBody::Tracing => String::new(),
        // ★ `metrics` 没有参数可打，但**它有没有被匹配器圈住**恰恰是要看的那件事，
        //   而那一格由 `render_step` 统一打（每一步的匹配器都印在名字后面）。
        //   ⚠ 编译期那条 `FUL-DSL-0037` 也会说同一件事 —— 两处都说，是因为
        //   `plan` 常常是唯一被读的那一个（诊断只在改配置那一刻出现一次）。
        StepBody::Metrics => String::new(),
    }
}
