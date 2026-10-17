//! L4 面的**建置期**判據（M2 批 A / B / C）。
//!
//! ★ 這一層測的是「執行環境圖建得對不對」，端到端那半邊在 `tests/l4/run.sh`：
//! 那邊驗「真位元組真的被搬過去了、換代時連接沒斷」，這邊驗
//! 「監聽位址拆得對、上游進了該進的表、錯的設定在**裝載期**就紅」。
//! ⚠ 兩邊都要有：只有端到端，一條判據紅了只會得到「連不上」，查起來要靠猜；
//! 只有這一層，一個把圖建得完美卻根本沒起監聽器的實作照樣全綠。

use fulcrum_config::compile_str;
use fulcrum_runtime::{Runtime, normalize_l4_upstream, parse_l4_listen};

fn build(src: &str) -> Result<Runtime, String> {
    let o = compile_str("t.Fulcrumfile", src);
    let cfg = o.config.as_ref().ok_or_else(|| o.render_diagnostics())?;
    Runtime::build(cfg).map_err(|errs| {
        errs.iter()
            .map(|e| format!("{e}"))
            .collect::<Vec<_>>()
            .join("\n")
    })
}

#[test]
fn 监听地址的两种写法都认() {
    assert_eq!(parse_l4_listen(":3306"), Ok((None, 3306)));
    assert_eq!(
        parse_l4_listen("127.0.0.1:3306"),
        Ok((Some("127.0.0.1".to_string()), 3306))
    );
}

/// ⚠ **連接埠必須寫**：L4 上沒有「預設連接埠」這回事 —— HTTP 那邊缺連接埠能按 scheme
/// 補 80/443，而一個裸 TCP 監聽器沒有 scheme。
/// ★ 判據釘的是「回報錯誤」，不是「悄悄補一個」：後者會讓 `l4 { tcp :  { … } }` 這種筆誤
/// 變成一個**監聽在別處**的服務，而設定讀起來完全正常。
#[test]
fn 监听地址缺端口或端口非法都要红() {
    for bad in [
        "", "3306", "abc", ":0", ":70000", ":-1", "/tmp/x", "[::1]:1",
    ] {
        assert!(
            parse_l4_listen(bad).is_err(),
            "`{bad}` 应当被拒，却过了：{:?}",
            parse_l4_listen(bad)
        );
    }
}

#[test]
fn l4上游必须写明端口() {
    assert_eq!(
        normalize_l4_upstream("10.0.0.5:3306"),
        Ok("10.0.0.5:3306".to_string())
    );
    assert_eq!(
        normalize_l4_upstream("db.internal:5432"),
        Ok("db.internal:5432".to_string())
    );
    // ★ 與 HTTP 那邊**有意不同**的一處：那邊 `backend` 會補成 `backend:80`。
    assert!(normalize_l4_upstream("db.internal").is_err());
    assert!(normalize_l4_upstream("10.0.0.5:0").is_err());
    assert!(normalize_l4_upstream("10.0.0.5:70000").is_err());
}

#[test]
fn 纯tcp的块建出监听器并且ip字面量当场解析好() {
    let rt = build("l4 {\n  tcp :3306 {\n    proxy 10.0.0.5:3306 10.0.0.6:3306\n  }\n}\n").unwrap();
    assert_eq!(rt.l4_listeners.len(), 1);
    let l = &rt.l4_listeners[0];
    assert_eq!(l.listen_port, 3306);
    assert_eq!(l.listen_host, None);
    // ★ 批 C 起 `target` 是 `Option`：沒有備援是一種**合法設定**（「只服務我認得的名字」）。
    let t = l.target.as_ref().expect("这份配置写了兜底 proxy");
    assert_eq!(t.upstreams.len(), 2);
    // ★ IP 字面值在建置期就填好槽：它不需要 DNS，也永遠不會變。
    //   ⚠ 而網域一律留空 —— `fulcrum validate` 必須離線就能說話。
    assert_eq!(t.upstreams[0].dial_candidates().len(), 1);
}

/// ★ ★ **L4 那條路的上游權重恆為 1**（M2 批 N 任務 2，計畫 §2 的 S2）。
///
/// `l4` 塊裡**沒有 `lb_policy` 的位置**（dsl-reference §4.5），也就沒有權重的位置。
/// ⇒ 累積權重在這條路上每一段寬度都是 1，`pick_index_by` 退化成 `游标 % n` ——
/// 與批 N 之前**逐字相同**。
/// ⚠ 這條判據是「加權排程沒有溢進 L4」唯一的結構性證明：`pick_index_by` 是**共用的**
/// （HTTP 與 L4 同一份實作，見 `crates/fulcrum-server/src/l4.rs` 的模組註解），
/// 所以「HTTP 那邊改了、L4 這邊沒事」不是自動成立的。
#[test]
fn l4的上游权重恒为一而且轮转序列不变() {
    let rt = build("l4 {\n  tcp :3306 {\n    proxy 10.0.0.5:3306 10.0.0.6:3306\n  }\n}\n").unwrap();
    let ups = rt.all_upstreams();
    assert_eq!(ups.len(), 2, "夹具里应当有两个 L4 上游");
    for u in &ups {
        assert_eq!(
            u.weight(),
            fulcrum_config::model::DEFAULT_UPSTREAM_WEIGHT,
            "L4 上游 {} 的权重不是默认值",
            u.addr
        );
    }
    // 落點逐字不變：遊標從 0 起，兩個上游輪著來。
    let t = rt.l4_listeners[0]
        .target
        .as_ref()
        .expect("这份配置写了兜底 proxy");
    let got: Vec<usize> = (0..6).map(|_| t.pick_index_by(None).unwrap()).collect();
    assert_eq!(got, (0..6).map(|c| c % 2).collect::<Vec<_>>());
}

/// ★ ★ **L4 那條路一個位元組沒變**（M2 批 N 任務 2.8 / 2.9，計畫 §2 的 S6）。
///
/// `l4` 塊裡沒有 `reverse_proxy`，也就沒有 `id` ⇒ L4 的 [`ProxyTarget::id`] 恆為空串，
/// 而覆蓋層那張鍵的清單（`keyed_proxies_of` / `proxy_key_fanout_of`）只收 `SiteRt`
/// —— L4 的上游沒有站台，按 R6 的鍵根本尋址不到它們，也就永遠不進那張清單。
///
/// ⚠ 這條判據必須存在的理由與上面那條權重的一樣：`ProxyTarget` 是 **HTTP 與 L4 共用**
/// 的一個結構體，「HTTP 那邊加了一格、L4 這邊沒事」不是自動成立的。
#[test]
fn l4的_id_恒为空串() {
    let rt = build("l4 {\n  tcp :3306 {\n    proxy 10.0.0.5:3306 10.0.0.6:3306\n  }\n}\n").unwrap();
    let t = rt.l4_listeners[0]
        .target
        .as_ref()
        .expect("这份配置写了兜底 proxy");
    assert_eq!(t.id, "", "L4 的 reverse_proxy id 该是空串");
    // ★ ★ **L4 的上游根本不進覆蓋層那張鍵的清單**（M2 批 N 任務 2.9）：
    //   鍵的第一格是站台名，而這份設定一個站台都沒有 ⇒ 那張清單必須是空的。
    //   ⚠ 少了這一條，一個「把 L4 的上游也收進鍵裡」的實作在本檔案裡毫無症狀 ——
    //   而它的現場是「`disable` 一個站台裡的上游，順手把某個 L4 連接埠也摘了」。
    assert!(
        rt.proxy_key_fanout().is_empty(),
        "只有 `l4` 块的配置不该有覆盖层的键：{:?}",
        rt.proxy_key_fanout()
    );
    // ★ 分流規則裡那一組也一樣（批 C 之後它是第二處 `build_l4_target` 的產物）。
    let rt = build(
        "l4 {\n  tcp :3307 {\n    sni a.example {\n      proxy 10.0.0.7:3306\n    }\n  }\n}\n",
    )
    .unwrap();
    assert_eq!(rt.l4_listeners[0].rules[0].target.id, "");
}

/// ★ ★ **L4 的上游必須出現在 `all_upstreams()` 裡**，否則網域形式的 L4 上游
/// 永遠不會被解析、也不會被後台重解析 —— 而現場表現是「那個連接埠連上就斷」，
/// 設定、紀錄、健康檢查全都正常。
/// ⚠ 這條判據存在的理由與批 10 給回落後端補的那條逐字相同：
/// **那個函式是 DNS 解析唯一的輸入，誰不在裡面誰就不存在。**
#[test]
fn l4的域名上游进得了dns那张表() {
    let rt = build("l4 {\n  tcp :3306 {\n    proxy db.internal:5432\n  }\n}\n").unwrap();
    let names: Vec<&str> = rt.all_upstreams().iter().map(|u| u.addr.as_str()).collect();
    assert!(
        names.contains(&"db.internal:5432"),
        "L4 上游没进 all_upstreams：{names:?}"
    );
    // 網域在建置期**不解析**（validate 要離線可用）。
    let l = &rt.l4_listeners[0];
    let t = l.target.as_ref().expect("这份配置写了兜底 proxy");
    assert!(t.upstreams[0].dial_candidates().is_empty());
}

/// `udp` 也建監聽器（**M2 批 B**），而裝載期判據兩種協定一視同仁。
///
/// ⚠ **舊契約（批 A）**：`udp` **不建**監聽器（它還在 `UNWIRED` 裡），
/// 但那時裝載期判據就已經對它照跑 —— 理由是「等 UDP 串接那天，
/// 一份早就寫錯的 `udp` 塊不該在那天才一次性冒出一堆錯誤」。
/// ★ 那個決定在這一批**兌現了**：串接時判據一條都不用補，只刪掉那個 `continue`。
#[test]
fn udp也建监听器而且错的配置照样红() {
    let rt = build("l4 {\n  udp :53 {\n    proxy 10.0.0.6:53\n  }\n}\n").unwrap();
    assert_eq!(rt.l4_listeners.len(), 1, "UDP 现在也要建出监听器");
    assert_eq!(rt.l4_listeners[0].proto.as_str(), "udp");
    assert_eq!(rt.l4_listeners[0].listen_port, 53);
    // 上游寫錯了 ⇒ 裝載期就紅（哪怕它是 udp）。
    let err = build("l4 {\n  udp :53 {\n    proxy 10.0.0.6\n  }\n}\n").unwrap_err();
    assert!(err.contains("缺端口"), "错误信息没说到点子上：{err}");

    // ★ ★ **同一個連接埠號上 tcp 與 udp 可以並存** —— 它們是兩個位址族命名空間，
    //   核心不會衝突。⚠ 一個「只按連接埠號查重」的實作會把這份合法設定判紅，
    //   而 DNS（53/udp + 53/tcp）恰恰是最常見的 L4 用法之一。
    let both = build(
        "l4 {\n  tcp :53 {\n    proxy 10.0.0.6:53\n  }\n  udp :53 {\n    proxy 10.0.0.6:53\n  }\n}\n",
    )
    .expect("同端口的 tcp 与 udp 应当并存");
    assert_eq!(both.l4_listeners.len(), 2);
}

#[test]
fn 两个监听器抢同一个端口装载期就红() {
    let err = build(
        "l4 {\n  tcp :3306 {\n    proxy 10.0.0.5:3306\n  }\n  tcp :3306 {\n    proxy 10.0.0.6:3306\n  }\n}\n",
    )
    .unwrap_err();
    assert!(err.contains("已经被"), "错误信息没说到点子上：{err}");
}

/// ★ L4 連接埠撞上 HTTP 站台連接埠 ⇒ 同一個處理程序裡兩個監聽器搶同一個連接埠，起不來。
/// ⚠ 不在裝載期攔的話，現場是「服務起來了，但那個連接埠時好時壞」——
/// 取決於哪個監聽器先 bind 成功。
#[test]
fn l4端口撞上http站点端口就红() {
    let err = build("http://a.com:8080 {\n  respond 200\n}\nl4 {\n  tcp :8080 {\n    proxy 10.0.0.5:3306\n  }\n}\n")
        .unwrap_err();
    assert!(err.contains("HTTP 站点"), "错误信息没说到点子上：{err}");
}

/// 一個上游都沒有的 `tcp` 塊 ⇒ 紅。
/// ⚠ DSL 層已經會報一次（`至少要有一条 proxy`），這裡守的是**結構化入口**那一半：
/// 機器直接寫結構化設定時不經過 DSL 前端（G11），而一個沒有上游的監聽器
/// 會接受連接然後立刻關掉 —— 現場表現與「上游全掛」一模一樣。
#[test]
fn 没有上游的tcp块在结构化层也要红() {
    // ★ 從一份合法設定出發，**在結構化那一層**把上游抹掉 ——
    //   這正是機器直接寫結構化設定時可能交出來的東西，而它不經過 DSL 前端。
    let o = compile_str(
        "t.Fulcrumfile",
        "l4 {\n  tcp :3306 {\n    proxy 10.0.0.5:3306\n  }\n}\n",
    );
    let mut cfg = o.config.expect("这份 DSL 应当编得过");
    cfg.l4
        .as_mut()
        .expect("应当有 l4")
        .listeners
        .get_mut(0)
        .expect("应当有一个监听器")
        .upstreams
        .clear();
    let err = Runtime::build(&cfg).unwrap_err();
    assert!(
        err.iter()
            .any(|e| format!("{e}").contains("一个可用的上游都没有")),
        "错误信息没说到点子上：{err:?}"
    );
}
// ══════════════════════════════════════════════════════════════════════════
// SNI / ALPN 分流（M2 批 C）
// ══════════════════════════════════════════════════════════════════════════

const SNI_DSL: &str = "l4 {\n  tcp :443 {\n    sni api.example.com {\n      proxy 10.0.0.1:8443\n    }\n    sni *.internal.example.com {\n      proxy 10.0.0.2:8443\n    }\n    alpn h2 {\n      proxy 10.0.0.3:8443\n    }\n    proxy 10.0.0.9:8443\n  }\n}\n";

#[test]
fn 规则建得出来而且顺序就是书写顺序() {
    let rt = build(SNI_DSL).unwrap();
    let l = &rt.l4_listeners[0];
    assert_eq!(l.rules.len(), 3);
    // ★ 順序即語義：DSL 裡怎麼寫，這裡就怎麼排 —— 資料面按這個順序取第一個命中。
    assert_eq!(l.rules[0].values, vec!["api.example.com"]);
    assert_eq!(l.rules[1].values, vec!["*.internal.example.com"]);
    assert_eq!(l.rules[2].values, vec!["h2"]);
    assert!(l.target.is_some(), "写了裸 proxy 就该有兜底");
}

/// SNI 的匹配語義：精確、大小寫不敏感、通配**只吃一層**。
///
/// ★ ★ 最後一條是 **G66 / D18** 那條紀律的第三個落點：站台索引、證書解析、
/// 現在是 L4 分流 —— 三處**共用同一份實作**（`wildcard_covers`）。
/// ⚠ 另寫一份「差不多的」匹配，就是把 D18 那個洞在第三個地方再挖一次。
#[test]
fn sni_精确与通配的语义与站点索引一致() {
    let rt = build(SNI_DSL).unwrap();
    let l = &rt.l4_listeners[0];
    let exact = &l.rules[0];
    let wild = &l.rules[1];

    assert!(exact.matches(Some("api.example.com"), &[]));
    // ⚠ DNS 名不區分大小寫 —— 一個只會 `==` 的實作在 `API.example.com` 上會漏。
    assert!(exact.matches(Some("API.Example.COM"), &[]));
    assert!(!exact.matches(Some("other.example.com"), &[]));
    assert!(!exact.matches(None, &[]), "没带 SNI 不该命中任何 sni 规则");

    assert!(wild.matches(Some("a.internal.example.com"), &[]));
    assert!(
        !wild.matches(Some("a.b.internal.example.com"), &[]),
        "通配只吃一层（G66）"
    );
    assert!(
        !wild.matches(Some("internal.example.com"), &[]),
        "裸域不该被 `*.` 覆盖"
    );
}

/// ALPN 是**逐位元組相等**，不是前綴。
///
/// ⚠ `h2` 與 `h2c` 是兩個不同的協定標識；前綴匹配會把明文 h2c 的連接
/// 送到只會 h2-over-TLS 的後端去，而現場表現是「偶爾握手失敗」。
#[test]
fn alpn_逐字节相等而不是前缀() {
    let rt = build(SNI_DSL).unwrap();
    let alpn_rule = &rt.l4_listeners[0].rules[2];
    assert!(alpn_rule.matches(None, &[b"h2".to_vec()]));
    assert!(
        alpn_rule.matches(None, &[b"http/1.1".to_vec(), b"h2".to_vec()]),
        "清单里有就算命中"
    );
    assert!(!alpn_rule.matches(None, &[b"h2c".to_vec()]), "h2c 不是 h2");
    assert!(!alpn_rule.matches(None, &[b"http/1.1".to_vec()]));
    assert!(
        !alpn_rule.matches(Some("api.example.com"), &[]),
        "alpn 规则不看 SNI"
    );
}

/// 分流規則裡的上游**也要進 DNS 那張表**。
///
/// ⚠ 少了這一格，一個網域形式的 `sni` 上游永遠解析不出位址 ——
/// 而現場是「那個名字連上就斷」，備援那條卻一切正常，看起來像 SNI 匹配錯了。
#[test]
fn 规则里的域名上游也进得了dns那张表() {
    let rt = build(
        "l4 {\n  tcp :443 {\n    sni a.com {\n      proxy inner.svc:8443\n    }\n    proxy 10.0.0.9:8443\n  }\n}\n",
    )
    .unwrap();
    let names: Vec<&str> = rt.all_upstreams().iter().map(|u| u.addr.as_str()).collect();
    assert!(
        names.contains(&"inner.svc:8443"),
        "规则里的上游没进 all_upstreams：{names:?}"
    );
}

/// **只有規則、沒有備援**是一種合法設定（「只服務我認得的那幾個名字」）。
/// ⚠ 而**兩者都沒有**不是：那樣的監聽器接受連接之後只能立刻關掉。
#[test]
fn 没有兜底合法但两者都没有不合法() {
    let ok_rt =
        build("l4 {\n  tcp :443 {\n    sni a.com {\n      proxy 10.0.0.1:8443\n    }\n  }\n}\n")
            .expect("只有规则、没有兜底 —— 合法");
    assert!(ok_rt.l4_listeners[0].target.is_none());
    assert_eq!(ok_rt.l4_listeners[0].rules.len(), 1);

    // 結構化入口那一半（機器直接寫這一層，不經過 DSL 前端）。
    let o = compile_str(
        "t.Fulcrumfile",
        "l4 {\n  tcp :443 {\n    proxy 10.0.0.1:8443\n  }\n}\n",
    );
    let mut cfg = o.config.expect("这份 DSL 应当编得过");
    let l = cfg.l4.as_mut().unwrap().listeners.get_mut(0).unwrap();
    l.upstreams.clear();
    l.rules.clear();
    let err = Runtime::build(&cfg).unwrap_err();
    assert!(
        err.iter()
            .any(|e| format!("{e}").contains("一个可用的上游都没有")),
        "错误信息没说到点子上：{err:?}"
    );
}

/// `udp` 上不能按 SNI/ALPN 分流 —— 而這**不是「以後再做」，是做不到**。
///
/// ★ 判據釘在**結構化層**：DSL 那一側已經攔過一次，而結構化設定是公開入口（G11）。
#[test]
fn udp上不能分流并且理由要说出来() {
    // DSL 那一半
    let o = compile_str(
        "t.Fulcrumfile",
        "l4 {\n  udp :53 {\n    sni a.com {\n      proxy 10.0.0.1:53\n    }\n    proxy 10.0.0.2:53\n  }\n}\n",
    );
    assert!(o.diagnostics.has_errors(), "udp 里写 sni 应当在 DSL 层就红");
    let text = o.render_diagnostics();
    assert!(
        text.contains("ClientHello") || text.contains("加密"),
        "诊断要说出**为什么**做不到，而不只是「不支持」：{text}"
    );

    // 結構化那一半
    let o2 = compile_str(
        "t.Fulcrumfile",
        "l4 {\n  tcp :443 {\n    sni a.com {\n      proxy 10.0.0.1:8443\n    }\n  }\n}\n",
    );
    let mut cfg = o2.config.expect("这份 DSL 应当编得过");
    cfg.l4.as_mut().unwrap().listeners[0].proto = "udp".to_string();
    let err = Runtime::build(&cfg).unwrap_err();
    assert!(
        err.iter().any(|e| format!("{e}").contains("ClientHello")),
        "结构化层也要拦，且说出理由：{err:?}"
    );
}

// ── M2 批 D：PROXY protocol 的建置期判據 ────────────────────────────────────

#[test]
fn 收那侧的信任清单进了运行时图() {
    let rt = build(
        "l4 {\n  tcp :5432 {\n    proxy_protocol_from 10.0.0.0/8 192.168.1.7\n    proxy 127.0.0.1:15432\n  }\n}\n",
    )
    .unwrap();
    let l = &rt.l4_listeners[0];
    assert_eq!(l.proxy_protocol_from.len(), 2, "两条都要在");
    // ★ 判據落在**行為**（信不信這個 IP）上，不落在「清單裡有幾條」上：
    //   後者一個把網段解析成 /32 的實作也能過。
    assert!(l.trusts_proxy_protocol("10.9.9.9".parse().unwrap()));
    assert!(l.trusts_proxy_protocol("192.168.1.7".parse().unwrap()));
    assert!(!l.trusts_proxy_protocol("192.168.1.8".parse().unwrap()));
    assert!(!l.trusts_proxy_protocol("203.0.113.1".parse().unwrap()));
}

/// ★ ★ **這一條是整批最要緊的判據**：一份**沒寫** `proxy_protocol_from` 的設定
/// 必須誰都不信 —— 而不是「沒配 ⇒ 不檢查 ⇒ 都信」。
///
/// ⚠ 後者的現場表現是：**任何人都能自稱自己是任何 IP**，而站台照常服務、
/// 紀錄照常有行、每一道功能門都是綠的。
#[test]
fn 没写清单就是谁都不信而不是谁都信() {
    let rt = build("l4 {\n  tcp :5432 {\n    proxy 127.0.0.1:15432\n  }\n}\n").unwrap();
    let l = &rt.l4_listeners[0];
    assert!(l.proxy_protocol_from.is_empty());
    for ip in ["127.0.0.1", "10.0.0.1", "::1", "203.0.113.1"] {
        assert!(
            !l.trusts_proxy_protocol(ip.parse().unwrap()),
            "空清单却信了 {ip}"
        );
    }
}

#[test]
fn http_面那份清单也是全局的而且默认谁都不信() {
    let rt = build("{\n  proxy_protocol_from 10.0.0.0/8\n}\nhttp://a.com {\n  respond 200\n}\n")
        .unwrap();
    assert!(rt.trusts_proxy_protocol("10.1.2.3".parse().unwrap()));
    assert!(!rt.trusts_proxy_protocol("11.1.2.3".parse().unwrap()));

    let bare = build("http://a.com {\n  respond 200\n}\n").unwrap();
    assert!(!bare.trusts_proxy_protocol("10.1.2.3".parse().unwrap()));
    assert!(!bare.trusts_proxy_protocol("127.0.0.1".parse().unwrap()));
}

/// ⚠ **v4 與 v6 不互通**（與 `remote_ip` 同一份 `Cidr`）：
/// 一條 `10.0.0.0/8` 不許命中任何 v6 用戶端。
/// ★ 把 v4 映射成 `::ffff:a.b.c.d` 再比，會讓這條清單在**雙棧監聽器**上
/// 對著 v6 用戶端全線放行 —— 而 `--bind-host [::]` 正是本專案生產機上的寫法。
#[test]
fn 信任清单的_v4_与_v6_不互通() {
    let rt = build(
        "l4 {\n  tcp :5432 {\n    proxy_protocol_from 10.0.0.0/8\n    proxy 127.0.0.1:15432\n  }\n}\n",
    )
    .unwrap();
    let l = &rt.l4_listeners[0];
    assert!(!l.trusts_proxy_protocol("::ffff:10.0.0.1".parse().unwrap()));
    assert!(!l.trusts_proxy_protocol("::1".parse().unwrap()));
}

#[test]
fn 发那侧的版本进了运行时图而且省略参数就是_v2() {
    use fulcrum_runtime::proxyproto::Version;
    let rt = build(
        "l4 {\n  tcp :5432 {\n    proxy_protocol\n    proxy 127.0.0.1:15432\n  }\n  tcp :5433 {\n    proxy_protocol v1\n    proxy 127.0.0.1:15433\n  }\n  tcp :5434 {\n    proxy 127.0.0.1:15434\n  }\n}\n",
    )
    .unwrap();
    assert_eq!(rt.l4_listeners[0].proxy_protocol, Some(Version::V2));
    assert_eq!(rt.l4_listeners[1].proxy_protocol, Some(Version::V1));
    // ★ 沒寫就是**不發**，而不是「發一個預設的」。
    assert_eq!(rt.l4_listeners[2].proxy_protocol, None);
}

/// ⚠ ⚠ **寫錯的網段必須在裝載期紅，不能留到請求路徑上。**
///
/// ★ 這一條的線上事故形態尤其壞：信任清單沒生效 ⇒ **用戶端 IP 全都取錯**，
/// 而站台照常服務、訪問紀錄照常有行 —— 沒有任何東西會說出來。
#[test]
fn 写错的网段在装载期就红() {
    for bad in ["10.0.0.0/33", "not-an-ip", "10.0.0.0/", "10.0.0.256"] {
        let src = format!(
            "l4 {{\n  tcp :5432 {{\n    proxy_protocol_from {bad}\n    proxy 127.0.0.1:15432\n  }}\n}}\n"
        );
        let e = build(&src).expect_err(&format!("`{bad}` 应当在装载期被拒"));
        assert!(
            e.contains("proxy_protocol_from"),
            "{bad} 的报错没指到位：{e}"
        );
    }
    // 全域那一份走同一個 `Cidr::parse`，也要紅。
    let e = build("{\n  proxy_protocol_from 10.0.0.0/33\n}\nhttp://a.com {\n  respond 200\n}\n")
        .expect_err("全局那份也要红");
    assert!(e.contains("proxy_protocol_from"), "{e}");
}

/// ★ ★ **判據在兩層都有**（與 `sni` / `alpn` 在 udp 上那條逐字同一條理由）：
/// DSL 前端攔一次，**執行環境圖再攔一次** —— 因為結構化設定是**公開入口**（G11），
/// 機器可以繞過 DSL 直接寫它。
#[test]
fn udp_上的_proxy_protocol_两层都拦() {
    // 第一層：DSL 前端。
    for line in ["proxy_protocol_from 10.0.0.0/8", "proxy_protocol v2"] {
        let src = format!("l4 {{\n  udp :53 {{\n    {line}\n    proxy 127.0.0.1:15353\n  }}\n}}\n");
        let e = build(&src).expect_err(&format!("udp 上的 `{line}` 应当被拒"));
        assert!(e.contains("udp"), "{line} 的报错没说是 udp 的问题：{e}");
    }

    // 第二層：直接喂結構化設定，繞過 DSL。
    let json = r#"{
      "schema_version": 1,
      "global": {"acme_email":null,"acme_ca":null,"admin":null,"default_sni":null,
                 "grace_period_ms":null,"fallback_nginx":null,"fallback_caddy":null,
                 "auto_http_redirect":true,"proxy_protocol_from":[]},
      "defaults": {"no_site_match":421,"no_route_match":404,"all_upstreams_down":502},
      "sites": [],
      "l4": {"listeners":[{"proto":"udp","listen":":53","upstreams":["127.0.0.1:15353"],
             "rules":[],"proxy_protocol_from":["10.0.0.0/8"],"proxy_protocol":null}]}
    }"#;
    let cfg: fulcrum_config::model::StructuredConfig = serde_json::from_str(json).unwrap();
    let errs = Runtime::build(&cfg).expect_err("结构化那条路也必须拦住");
    let msg = errs
        .iter()
        .map(|e| format!("{e}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(msg.contains("udp"), "{msg}");
}
