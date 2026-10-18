//! 樞衡 Fulcrum 的產品二進位。
//!
//! ```text
//! fulcrum validate <file>          # 只檢查，出錯走退出碼 1
//! fulcrum compile  <file> [-o f]   # DSL → 結構化配置（JSON，G48）
//! fulcrum plan     <file>          # 人讀的執行計劃：每條指令實際跑在第幾步 + 回落
//! fulcrum serve    <file>          # 起資料面（HTTP/HTTPS、ACME、反代、管理面）
//! ```
//!
//! ⚠ **不要在這裡寫現狀描述** —— 一句過期的現狀描述比沒有描述更糟，
//! 它會讓下一個人按一個不存在的世界做判斷。能力面以 `PLAN.md` §1 為準。
//!
//! ★ `serve` 由 systemd 託管（G31/G33/G37/G78）：前台執行、`Type=notify`、
//! `SIGUSR2` 觸發零停機換代。部署形狀見 `docs/platform/deploy.md`。
//! `spikes/` 下那兩個仍是驗證臺，不是產品程式碼。
//!
//! ★ ★ **`validate` 現在跑兩層檢核，不是一層**：DSL 前端 + **執行環境圖建置**。
//! 後者不是重複勞動——結構化設定那一層是**公開入口**（G11：機器直接寫它），
//! 而機器寫進來的一份根本不經過詞法與語法。正則編不過、CIDR 不合法、上游位址寫錯，
//! 都只有建置執行環境圖時才看得見。
//!
//! ★ 沒有用任何命令行解析庫。三條子命令、兩個選項，手寫二十行比拖一棵相依樹劃算，
//! 而 G29 那套「追新 + 24 小時隔離期 + 每週檢查」是按相依條數收費的。

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

    // 狀態目錄：證書儲存的根。★ 檢核 TLS 也要用它，所以在分支之前先定下來。
    let state_dir = flags
        .state_dir
        .clone()
        .unwrap_or_else(|| fulcrum_server::ServeOptions::default().state_dir);

    // ★ 四層檢核一次跑完：診斷 → 結構化設定 → 執行環境圖 → TLS 裝載。
    let Some((cfg, rt)) = prepare(&outcome, &state_dir) else {
        return Ok(ExitCode::from(1));
    };

    // ── ★ ★ ★ 權限門（批 22）─────────────────────────────────────
    //
    //   憑證可以內聯進 Fulcrumfile 之後，**這份檔案的性質變了**：它是秘密了。
    //   ⇒ 只要設定裡出現字面值憑證，就檢查一次「別人讀不讀得到」。
    //
    // ★ 它掛在**所有**子命令上，不只是 serve：`validate` 的全部意義就是
    //   「上線之前先問一遍」，而一份 0644 帶著憑證的設定正是它該攔住的東西。
    // ⚠ 沒有字面值憑證時**一個字都不說** —— 不去管別人怎麼放一份沒有秘密的設定。
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
            // ★ ★ **預設脫敏。** 這份 JSON 有兩個去處：給人看，和 `POST /load` 的載荷。
            //   前者不該看見憑證，後者需要 —— 而**預設必須服從前者**：
            //   一個預設吐真值的命令，只要有人把輸出貼進 issue 一次就完了，
            //   而那一次不會有任何提示。
            // ⚠ 需要真值時顯式 `--with-secrets`，並且**在 stderr 上說一聲**：
            //   它是一次有後果的操作，不該悄無聲息。
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
            // ★ 不回傳：Pingora 的 `run_forever()` 接管處理程序。
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

/// 紀錄：`serve` 才需要。★ 格式裡帶 pid，因為零停機換代時**兩代會同時在跑**，
/// 而分不清哪一行是誰打的，升級視窗裡的紀錄就沒法讀。
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

/// 診斷 → 結構化設定 → 執行環境圖。三層都過了才回傳 `Some`。
///
/// ★ 診斷永遠打到 **stderr**，產物永遠打到 **stdout**。分開不是潔癖：
/// `fulcrum compile x.Fulcrumfile > cfg.json` 必須能用，
/// 而只要有一條診斷混進 stdout，那份 JSON 就壞了——**而且壞得很安靜**。
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

    // ⚠ 這裡原本按 G52 逐條列出走回落的路由。回落層（M2 批 G）
    //   整塊刪除（G98）⇒ **沒有回落路由可列了**。
    //   ★ G52 那條紀律本身沒作廢，它換了物件：現在由快取與靜態檔案那兩處
    //   「把生效的預設值打出來」承擔（`log_cache_summary` / hide 清單）。

    // ★ ★ 第三層：執行環境圖。結構化設定是公開入口（G11），所以這一層也檢核。
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

    // ★ ★ 第四層：TLS 裝載。**證書路徑寫錯、私鑰與證書對不上，要在這裡紅**，
    //   而不是等第一個用戶端連上來變成一次握手失敗——後者在紀錄裡只是一行
    //   「TLS error」，看不出是設定寫錯了。
    let cert_root = std::path::Path::new(state_dir).join("certs");
    // ★ 簽發者目錄名跟著 `acme_ca` 走 —— `validate` 要看的是**這份設定**會用哪個目錄，
    //   而不是某個寫死的預設值。寫死它的後果是：換了 CA 之後 validate 一切正常，
    //   而 serve 起來發現一張證書都沒有。
    let issuer = fulcrum_acme::issuer_slug(
        outcome
            .config
            .as_ref()
            .and_then(|c| c.global.acme_ca.as_deref())
            .unwrap_or(fulcrum_acme::LETSENCRYPT_PRODUCTION),
    );
    // ★ `default_sni` 與 `issuer` 一樣從**這份設定**的全域塊取：`validate` 要回答的是
    //   「這份設定起來之後會怎樣」，而不是某個寫死的預設值。
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

/// 把子命令之後的參數拆成「位置參數」與 `-o` 的取值。
///
/// ★ **選項與位置參數的順序是使用者自由的，程式碼不該假定它。**
/// 固定取 `args[1]` 的話，`fulcrum compile -o out.json in.Fulcrumfile`
/// 會去讀一個名叫 `-o` 的檔案。
/// ⚠ 未知選項**當場回報錯誤**而不是當成路徑——否則拼錯一個選項會變成一句
/// 「讀不到 --ouput」，方向指錯。
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
        // ★ ★ ★ **每個臂說出自己吃了幾個參數，由循環統一推進。**
        //
        //   在此之前是「每個臂自己 `i += n`」，而那個約定有一個**不會回報錯誤的失效方式**：
        //   新加一個臂時忘了推進 —— 編譯過、clippy 不響、測試跑不到，
        //   而執行環境**死循環**。⚠ 加 `--with-secrets` 時當場踩了一次：
        //   `fulcrum compile … --with-secrets` 直接掛住，什麼都不列印。
        //
        //   ⇒ 改成 `let consumed = match … { … => n }`：**忘了給數字就是類型錯誤**。
        //   這與本儲存庫那條「G49 的序號寫在每行最前面，忘了給位置這個選項不存在」是同一招：
        //   **讓錯誤變成寫不出來的東西，而不是靠記得。**
        let consumed: usize = match rest[i].as_str() {
            // ★ 只有長寫法，沒有 `-s`：這是一次有後果的操作，
            //   而單字母開關最容易在別人的指令碼裡被順手加上。
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
        // ⚠ 再加一道：`consumed` 恆 ≥ 1 由上面每個臂保證，但**別讓它靠保證**。
        //   一個回傳 0 的臂會把這裡變回死循環，而這一行讓它變成一句能看懂的錯。
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
        // ★ ★ 這條測試的價值不在斷言的內容，而在**它會不會回傳** ——
        //   一個忘了推進的臂會讓它掛死，而掛死是測得出來的（超時）。
        //   ⚠ 但它只是第二道：第一道是 `consumed: usize` 那個類型。
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
        // ⚠ 釘的是 `compile -o out.json in.Fulcrumfile` 不能去讀一個叫 `-o` 的檔案。
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
    /// `compile --with-secrets`：輸出裡露出真憑證。★ 預設 false。
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

// ── 執行計畫 ────────────────────────────────────────────────────────────────

/// 人讀的執行計畫。
///
/// ★ 它存在的理由是 **G49 配套第 4 條**：內建順序表讓書寫順序 ≠ 執行順序，
/// 那就必須有一個地方能回答「我這條到底跑在第幾步」。
/// 讓人去背 `docs/architecture/dsl-reference.md` §三那張表，是把成本轉嫁給使用者。
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
            // ★ ★ M2 批 A/B：逐條說出**這一條到底由誰來跑**。
            //   ⚠ 批 A 時這裡還要分岔（TCP 自己轉發、UDP 走回落），批 B 之後兩種都是自研 ——
            //   而那句「回落 → caddy」也隨之整條刪掉，連同 `L4Config` 裡那個欄位。
            //   ★ `plan` 的全部價值就是回答「我這條到底會怎麼跑」，**它說錯比不說更貴**。
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
        // ⚠ 不寫 `_`：這類「走遍整張圖找某一類步驟」的 match 裡，`_` 還兼著
        //   「這個變體不裝步驟、不用下鑽」這層意思 —— 而新增一個裝步驟的容器
        //   變體會讓那層意思悄悄變假，夾具裡只有當時已有的容器，擋不住它。
        //   ⇒ 規則：新增變體時這一族 match 必須一起改，由編譯期窮舉來逼。
        // ★ 這一族**跨兩層**：執行環境圖 `BodyRt` 與設定圖 `StepBody` 各有一批
        //   同形遍歷。⛔ 名單不抄進註解（寫死的名單沒有門守著，當天就會過期）
        //   —— 要名單就現場 grep 這兩個枚舉名。
        StepBody::Tracing
        | StepBody::Header { .. }
        | StepBody::Rewrite { .. }
        | StepBody::Encode { .. }
        | StepBody::Cache { .. }
        | StepBody::Redir { .. }
        | StepBody::Respond { .. }
        | StepBody::Metrics
        | StepBody::ReverseProxy { .. }
        | StepBody::FileServer { .. } => {}
    }
}

// ★ 回落層已整層刪除（G98）⇒ 計畫輸出裡**再沒有額外跳數可標**。

fn detail(body: &StepBody) -> String {
    match body {
        StepBody::ReverseProxy {
            upstreams,
            lb_policy,
            dns_refresh_ms,
            ..
        } => format!(
            "  {} [{lb_policy}, dns_refresh {}s]",
            // ★ ★ **權重印出來**（M2 批 N 任務 2）：`plan` 說的是「實際會怎麼跑」，
            //   而四種 `lb_policy` 現在全部按累積權重挑 ⇒ 權重就是「怎麼跑」的一部分。
            // ⚠ **權重為 1 的不加後綴**：1 是預設值（`DEFAULT_UPSTREAM_WEIGHT`），
            //   給每個上游都綴一個 `*1` 會讓絕大多數設定的 plan 輸出無端變長，
            //   而 `plan` 的讀法是「與設定逐行對照」—— `*3` 只出現在真寫了 `weight` 的地方，
            //   正好指著那一行。★ 於是沒配 `weight` 的設定，`plan` 輸出**一個位元組都不變**。
            upstreams
                .iter()
                .map(|u| {
                    if u.weight == fulcrum_config::model::DEFAULT_UPSTREAM_WEIGHT {
                        u.addr.clone()
                    } else {
                        format!("{}*{}", u.addr, u.weight)
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
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
        // ★ 快取進 plan 輸出（M2 批 G）：`ttl` 是**備援**不是覆蓋（G96），
        //   ⚠ 而這件事必須在 `plan` 裡說出來 —— 從 nginx 遷過來的人預設以為它是覆蓋。
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
            // ★ 後端進 `plan`（M2 批 H）：`plan` 是「不真跑一遍就能看見會發生什麼」
            //   的那個出口，而「快取落在記憶體還是那個盤上」正是要看的東西之一。
            //   ⚠ 它說的是**設定選了哪個**；「目錄用不了於是關掉」那一檔只有
            //   真啟動時才知道，由裝載紀錄說 —— 兩處各說各能說的那一半。
            match disk_dir {
                Some(d) => format!("磁盘 {d}"),
                None => "内存".to_string(),
            },
        ),
        StepBody::Tracing => String::new(),
        // ★ `metrics` 沒有參數可打，但**它有沒有被匹配器圈住**恰恰是要看的那件事，
        //   而那一格由 `render_step` 統一打（每一步的匹配器都印在名字後面）。
        //   ⚠ 編譯期那條 `FUL-DSL-0037` 也會說同一件事 —— 兩處都說，是因為
        //   `plan` 常常是唯一被讀的那一個（診斷只在改設定那一刻出現一次）。
        StepBody::Metrics => String::new(),
    }
}

#[cfg(test)]
mod plan_tests {
    use super::*;

    /// 從一段 DSL 裡取出那條 `reverse_proxy` 的 `plan` 明細行。
    fn detail_of(dsl: &str) -> String {
        let o = compile_str("t.Fulcrumfile", dsl);
        assert!(!o.diagnostics.has_errors(), "{}", o.render_diagnostics());
        let cfg = o.config.expect("夹具应当能编译");
        for s in &cfg.sites {
            for st in &s.chain {
                if matches!(st.body, StepBody::ReverseProxy { .. }) {
                    return detail(&st.body);
                }
            }
        }
        panic!("夹具里应当有一条 reverse_proxy");
    }

    /// ★ `plan` 說的是「實際會怎麼跑」⇒ 權重串接之後**必須印出來**（M2 批 N 任務 2）。
    /// ⚠ 任務 1 有意沒印：那時排程還不認權重，印 `*3` 等於印一件不成立的事。
    #[test]
    fn plan_印出非默认的上游权重() {
        let s = detail_of(
            "a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2 {\n    weight 10.0.0.1:1 3\n  }\n}\n",
        );
        assert_eq!(
            s,
            "  10.0.0.1:1*3 10.0.0.2:2 [round_robin, dns_refresh 30s]"
        );
    }

    /// ★ ★ 反向：一條 `weight` 都沒寫的設定，`plan` 那一行**一個位元組都不變**。
    /// ⚠ 少了這一條，一個給每個上游都綴 `*1` 的實作照樣綠 —— 而那會讓所有人的
    /// `plan` 輸出在這一批裡無端漂移，而漂移本身沒有任何東西會說。
    #[test]
    fn 没写_weight_的配置_plan_那一行逐字不变() {
        let s = detail_of("a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2\n}\n");
        assert_eq!(s, "  10.0.0.1:1 10.0.0.2:2 [round_robin, dns_refresh 30s]");
    }

    /// ★ ★ **`id` 有意不進 `plan` / `validate` 的輸出**（M2 批 N 任務 2.8，計畫 §2 的 S7）。
    ///
    /// `plan` 說的是「實際會怎麼跑」，而 `id` 一個字都不改變怎麼跑 —— 它只改變
    /// **管理面怎麼尋址**。⇒ 那件事的出口是 `/stats`（裁決 R6 ⑤ 明寫：三格鍵原樣列出來，
    /// 維運照抄即可），不是 `plan`。
    /// ⚠ 這條判據同時是 S7 的落點：`fulcrum validate` 一個位元組都不變。
    #[test]
    fn 写了_id_的配置_plan_那一行也逐字不变() {
        let 没写 = detail_of("a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2\n}\n");
        let 写了 = detail_of(
            "a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2 {\n    id pool_web\n  }\n}\n",
        );
        assert_eq!(写了, 没写, "id 不该出现在 plan 的输出里");
    }
}
