//! 全域塊那兩個 TLS 設定的**唯一套用點**（G168）。
//!
//! h1/h2 每個 HTTPS 連接埠的 acceptor 與 h3 的 context 都在建好之後各叫一次 [`TlsPolicy::apply_to`] ——
//! 與 [`crate::SniResolver::install_into`] 同一個形狀（G104）：兩個入口的 TLS 政策**只有一份實作**，
//! 「h1/h2 釘了組而 h3 沒釘」在結構上做不到。
//!
//! ★ ★ 兩條取捨是 owner 2026-10-23 拍的，⛔ 別在這裡改：
//! - `groups` 是**允許的集合**，握手照**用戶端**的順序在其中挑 ⇒ ⛔ 不設
//!   `SSL_OP_CIPHER_SERVER_PREFERENCE`（它會連帶把 TLS 1.2 的密碼套件也改成照伺服器端順序，
//!   還可能為了伺服器端偏好多出一次 HelloRetryRequest）。
//! - `session_resumption == false` = **一張 ticket 都不發** + 伺服器端 session cache 關。
//!   BoringSSL 帶 `NO_TICKET` 時 TLS 1.3 也不發（`ssl/tls13_server.cc` 的 `add_new_session_tickets`）——
//!   ⚠ OpenSSL 不是這樣，那是對拍裡 nginx / HAProxy 的事，見 `bench/README.md`。
//!
//! ★ 預設政策（兩個鍵都沒寫）**對 BoringSSL 一個呼叫都不做** ⇒ 不寫的人握手逐位元組照舊。

use pingora_boringssl::error::ErrorStack;
use pingora_boringssl::ssl::{SslContextBuilder, SslOptions, SslSessionCacheMode};

/// 兩個全域 TLS 設定，已經過 `fulcrum_runtime::Runtime::build` 驗證（組名都是 BoringSSL 的正式名）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsPolicy {
    groups: Vec<String>,
    session_resumption: bool,
}

impl Default for TlsPolicy {
    fn default() -> Self {
        TlsPolicy {
            groups: Vec::new(),
            session_resumption: true,
        }
    }
}

impl TlsPolicy {
    /// `groups` 空 = BoringSSL 預設；`session_resumption == true` = 照舊恢復。
    pub fn new(groups: Vec<String>, session_resumption: bool) -> TlsPolicy {
        TlsPolicy {
            groups,
            session_resumption,
        }
    }

    /// 兩個鍵都沒寫。
    pub fn is_default(&self) -> bool {
        self.groups.is_empty() && self.session_resumption
    }

    /// 把政策套到一個（還沒 `build` 的）context 上。預設政策什麼都不做。
    ///
    /// ⚠ 失敗只可能是 BoringSSL 不認某個組名 —— 執行環境裝載已經用同一張表判過一次，
    ///   走到這裡還失敗，說明那張表與這個版本的 BoringSSL 對不上了（換 boring 大版本時要重核）。
    pub fn apply_to(&self, builder: &mut SslContextBuilder) -> Result<(), ErrorStack> {
        if !self.groups.is_empty() {
            builder.set_curves_list(&self.groups.join(":"))?;
        }
        if !self.session_resumption {
            builder.set_options(SslOptions::NO_TICKET);
            builder.set_session_cache_mode(SslSessionCacheMode::OFF);
        }
        Ok(())
    }

    /// 給啟動紀錄的一句話。
    pub fn describe(&self) -> String {
        if self.is_default() {
            return "TLS 组与会话恢复都是 BoringSSL 默认".to_string();
        }
        let groups = if self.groups.is_empty() {
            "BoringSSL 默认".to_string()
        } else {
            self.groups.join(" ")
        };
        let resumption = if self.session_resumption {
            "照旧"
        } else {
            "关（不发 ticket、不留 session）"
        };
        format!("TLS 组 {groups}（照客户端顺序挑）· 会话恢复 {resumption}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pingora_boringssl::ssl::{
        Ssl, SslAcceptor, SslContext, SslMethod, SslSession, SslVerifyMode, SslVersion,
    };
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    /// 伺服器端：與 h1/h2 入口同一個 acceptor（`mozilla_intermediate_v5`），裝一張現造的 P-256 自簽證書，再套政策。
    /// `policy == None` = 對照組：完全不叫 `apply_to`。
    fn server(policy: Option<&TlsPolicy>) -> SslContext {
        let key = rcgen::KeyPair::generate().expect("测试密钥"); // rcgen 預設 ECDSA P-256
        let cert = rcgen::CertificateParams::new(vec!["tls-policy.test".to_string()])
            .expect("测试参数")
            .self_signed(&key)
            .expect("自签");
        let ck =
            crate::cert_key_from_der(cert.der().to_vec(), key.serialize_der()).expect("造 CertKey");
        let mut b = SslAcceptor::mozilla_intermediate_v5(SslMethod::tls()).expect("acceptor");
        b.set_certificate(&ck.chain[0]).expect("装证书");
        b.set_private_key(&ck.key).expect("装私钥");
        if let Some(p) = policy {
            p.apply_to(&mut b).expect("套政策");
        }
        b.build().into_context()
    }

    /// 一次握手的結果。（`SslSession` 沒有 `Debug`，所以這裡不 derive。）
    struct Hs {
        group: Option<&'static str>,
        reused: bool,
        version: &'static str,
        /// 這次握手拿到的 session（TLS 1.3 的 ticket 走 new-session 回呼才拿得到）。
        session: Option<SslSession>,
    }

    /// 用戶端只提供 `client_groups`（冒號分隔、照這個順序偏好），可限最高版本、可帶上一次的 session。
    fn handshake(
        server: &SslContext,
        client_groups: &str,
        max: Option<SslVersion>,
        resume: Option<&SslSession>,
    ) -> Result<Hs, String> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let ctx = server.clone();
        let srv = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            if let Ok(mut s) = Ssl::new(&ctx).unwrap().accept(tcp) {
                let _ = s.write_all(b"x");
                let _ = s.shutdown();
            }
        });

        let mut cb = SslContext::builder(SslMethod::tls()).unwrap();
        cb.set_verify(SslVerifyMode::NONE);
        cb.set_curves_list(client_groups).unwrap();
        cb.set_max_proto_version(max).unwrap();
        cb.set_session_cache_mode(SslSessionCacheMode::CLIENT);
        let got: Arc<Mutex<Option<SslSession>>> = Arc::default();
        let sink = got.clone();
        cb.set_new_session_callback(move |_, s| *sink.lock().unwrap() = Some(s));
        let cctx = cb.build();
        let mut ssl = Ssl::new(&cctx).unwrap();
        ssl.set_hostname("tls-policy.test").unwrap();
        if let Some(s) = resume {
            // SAFETY: `s` 來自用同一個用戶端設定（同一種 SslMethod）建的連接。
            unsafe { ssl.set_session(s).unwrap() };
        }
        let out = match ssl.connect(TcpStream::connect(addr).unwrap()) {
            Ok(mut s) => {
                // 讀一個位元組：TLS 1.3 的 NewSessionTicket 在它之前到，讀了才會觸發回呼。
                let mut one = [0u8; 1];
                let _ = s.read(&mut one);
                Ok(Hs {
                    group: s.ssl().curve_name(),
                    reused: s.ssl().session_reused(),
                    version: s.ssl().version_str(),
                    session: got
                        .lock()
                        .unwrap()
                        .take()
                        .or_else(|| s.ssl().session().map(|r| r.to_owned())),
                })
            }
            Err(e) => Err(format!("{e}")),
        };
        srv.join().unwrap();
        out
    }

    const BOTH: &str = "X25519MLKEM768:X25519";

    fn pinned_mlkem_no_resumption() -> TlsPolicy {
        TlsPolicy::new(vec!["X25519MLKEM768".into()], false)
    }

    #[test]
    fn 钉了_x25519mlkem768_只给_x25519_的客户端握不上() {
        let s = server(Some(&pinned_mlkem_no_resumption()));
        assert!(
            handshake(&s, "X25519", None, None).is_err(),
            "组被钉死了，只给 X25519 的客户端不该握上"
        );
        // 對照：不套政策時同一個用戶端握得上 —— 證明紅的是政策，不是用戶端本身。
        assert!(handshake(&server(None), "X25519", None, None).is_ok());
    }

    #[test]
    fn 两者都给时协商出_x25519mlkem768() {
        let hs = handshake(
            &server(Some(&pinned_mlkem_no_resumption())),
            BOTH,
            None,
            None,
        )
        .unwrap();
        assert_eq!(hs.group, Some("X25519MLKEM768"));
        assert_eq!(hs.version, "TLSv1.3");
    }

    /// owner 2026-10-23：清單是集合、照**用戶端**順序挑 —— 伺服器端寫的順序不起作用。
    #[test]
    fn 清单是集合_照客户端顺序挑() {
        let p = TlsPolicy::new(vec!["X25519".into(), "X25519MLKEM768".into()], true);
        let s = server(Some(&p));
        assert_eq!(
            handshake(&s, BOTH, None, None).unwrap().group,
            Some("X25519MLKEM768")
        );
        assert_eq!(
            handshake(&s, "X25519:X25519MLKEM768", None, None)
                .unwrap()
                .group,
            Some("X25519")
        );
    }

    #[test]
    fn 关恢复时第二次带_session_的握手是新握手() {
        let s = server(Some(&pinned_mlkem_no_resumption()));
        let first = handshake(&s, BOTH, None, None).unwrap();
        // 0 張 ticket ⇒ TLS 1.3 用戶端根本拿不到可恢復的 session。
        let second = handshake(&s, BOTH, None, first.session.as_ref()).unwrap();
        assert!(!second.reused, "关了会话恢复，第二次却恢复了");
    }

    /// 反方向：不寫時第二次**是**恢復的 —— 否則上面那條在一個「從來不恢復」的實作下照樣綠。
    #[test]
    fn 不写时第二次是恢复的() {
        let s = server(Some(&TlsPolicy::default()));
        let first = handshake(&s, BOTH, None, None).unwrap();
        assert!(
            first.session.is_some(),
            "TLS 1.3 下没拿到 ticket —— 这条判据就失去了意义"
        );
        let second = handshake(&s, BOTH, None, first.session.as_ref()).unwrap();
        assert!(second.reused, "缺省该照旧恢复");
    }

    /// cache OFF 那一半：TLS 1.2 用戶端靠 session ID / 1.2 的 ticket 恢復，也必須是新握手。
    ///
    /// ⚠ 用戶端的組要含 `P-256`：TLS 1.2 下 ECDSA P-256 證書要求用戶端的 supported_groups 裡有它
    ///   （只給 `X25519MLKEM768:X25519` 時伺服器端直接 `HANDSHAKE_FAILURE_ON_CLIENT_HELLO`，10-23 實測）；
    ///   後量子組在 1.2 裡不存在 ⇒ `tls_groups` 只釘 `X25519MLKEM768` 時 TLS 1.2 用戶端一律握不上。
    #[test]
    fn 关恢复时_tls1_2_也不恢复() {
        const TLS12: &str = "X25519:P-256";
        let off = server(Some(&TlsPolicy::new(Vec::new(), false)));
        let first = handshake(&off, TLS12, Some(SslVersion::TLS1_2), None).unwrap();
        assert_eq!(first.version, "TLSv1.2");
        let second = handshake(
            &off,
            TLS12,
            Some(SslVersion::TLS1_2),
            first.session.as_ref(),
        )
        .unwrap();
        assert!(!second.reused, "TLS 1.2 下关了恢复却恢复了");

        let on = server(Some(&TlsPolicy::default()));
        let first = handshake(&on, TLS12, Some(SslVersion::TLS1_2), None).unwrap();
        let second =
            handshake(&on, TLS12, Some(SslVersion::TLS1_2), first.session.as_ref()).unwrap();
        assert!(second.reused, "反方向：缺省时 TLS 1.2 该恢复");
    }

    /// 預設政策與「完全不叫 `apply_to`」行為相同：協商出的組、恢復與否都一樣。
    #[test]
    fn 不写时不改_context() {
        assert!(TlsPolicy::default().is_default());
        for s in [server(None), server(Some(&TlsPolicy::default()))] {
            let first = handshake(&s, BOTH, None, None).unwrap();
            assert_eq!(first.group, Some("X25519MLKEM768"));
            let second = handshake(&s, BOTH, None, first.session.as_ref()).unwrap();
            assert!(second.reused);
        }
    }

    #[test]
    fn 启动日志说得出生效的政策() {
        assert!(TlsPolicy::default().describe().contains("BoringSSL 默认"));
        let d = pinned_mlkem_no_resumption().describe();
        assert!(d.contains("X25519MLKEM768") && d.contains("关"), "{d}");
    }
}
