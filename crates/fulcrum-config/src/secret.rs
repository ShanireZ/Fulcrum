//! 凭据值（**G59 第 1 条的修订**）。
//!
//! # ★ ★ ★ 口径变了：从「写不下字面量」变成「写得下，但它变成秘密」
//!
//! 在此之前，DSL 里写凭据字面量是**编译期错误**，只认 `env:` / `file:` 两种来源。
//! owner 拍板改成 Caddy 的形状：**一份配置文件就能跑完**。
//!
//! ⚠ 而这句话真正的代价不是「多一种写法」，是**配置文件的性质变了** ——
//! 它从「可以随便看的东西」变成了「秘密」。而枢衡里有四条路会把配置原样吐出来：
//!
//! | 路 | 原本会泄漏什么 |
//! |---|---|
//! | `fulcrum compile` | 整份结构化 JSON（凭据在里面）|
//! | `POST /load` 的载荷 | 同上，还会穿过管理面 |
//! | 诊断 | **出错那一行会被原样打出来**（带 caret）—— 一个语法错误就能把 token 打进 journald |
//! | `Debug` / 日志 | 任何 `{:?}` 一整份配置的地方 |
//!
//! ⇒ 所以本类型的默认行为是**脱敏**，露出真值要显式进 [`reveal`] 作用域。
//! ★ 判据不是「我们记得在这四处脱敏」，而是**默认就不出真值** ——
//! 一条要在 N 个地方记住的规则，迟早会在其中一个地方被忘掉。

use serde::de::{self, Deserialize, Deserializer};
use serde::ser::{Serialize, Serializer};
use std::cell::Cell;
use std::fmt;

/// 脱敏之后印出来的样子。
///
/// ★ 有意选一个**不可能是合法凭据**的串：它出现在哪儿，哪儿就是脱敏过的，
/// 而不是「某个恰好长这样的 token」。
pub const REDACTED: &str = "«已脱敏»";

thread_local! {
    /// 当前线程是否在 [`reveal`] 作用域里。
    ///
    /// ⚠ 用 thread-local 而不是给 `Serialize` 加参数：serde 的 `Serialize` 签名是固定的，
    /// 而**默认脱敏**这件事必须落在 `Serialize` 自己身上 —— 落在调用方就等于
    /// 「每个调用方都要记得」，那正是这里要避免的。
    static REVEAL: Cell<bool> = const { Cell::new(false) };
}

/// 在这个闭包里，[`Secret`] 序列化出**真值**。
///
/// ★ 唯一的用法是 `fulcrum compile --with-secrets` 与管理面的 load 载荷生成 ——
/// 两处都是**用户显式要求**的时刻。
pub fn reveal<T>(f: impl FnOnce() -> T) -> T {
    REVEAL.with(|r| r.set(true));
    let out = f();
    REVEAL.with(|r| r.set(false));
    out
}

fn revealing() -> bool {
    REVEAL.with(|r| r.get())
}

/// 一个凭据「从哪儿来」——可能是指针（`env:` / `file:`），也可能就是值本身。
#[derive(Clone, PartialEq, Eq)]
pub struct Secret {
    raw: String,
    /// 真值就在 `raw` 里（字面量）⇒ 印出去要脱敏。
    sensitive: bool,
    /// ★ 这一份是**从脱敏过的产物读回来的** —— 里面没有真值。
    ///
    /// ⚠ 它必须与「普通字面量」分开：把 `«已脱敏»` 当成凭据发给 CA，
    /// 现场表现是「凭据不对」，而没有任何一处会说「你 load 的是一份脱敏产物」。
    redacted: bool,
}

impl Secret {
    /// 从 DSL / JSON 里那个词造一个。
    ///
    /// | 写法 | 结果 |
    /// |---|---|
    /// | `env:NAME` / `file:/path` | 指针，不脱敏（它本来就不是秘密）|
    /// | `literal:<值>` | 字面量，脱敏 |
    /// | `«已脱敏»` | ★ 标记为「读回来的脱敏产物」，用它去签发会**明确报错** |
    /// | 其它任何值 | 字面量，脱敏（Caddy 形状：不写前缀就是值本身）|
    pub fn parse(raw: &str) -> Secret {
        if raw == REDACTED {
            return Secret {
                raw: raw.to_string(),
                sensitive: true,
                redacted: true,
            };
        }
        let sensitive = !(raw.starts_with("env:") || raw.starts_with("file:"));
        Secret {
            raw: raw.to_string(),
            sensitive,
            redacted: false,
        }
    }

    /// 造一个**明确不是秘密**的值（例如 `dns exec` 的程序路径）。
    ///
    /// # ⚠ ⚠ 为什么需要它 —— 一刀切「无前缀就是凭据」会误伤
    ///
    /// `dns` 的第二个参数**按供应商有两种含义**：
    ///
    /// | 供应商 | 第二个参数是 | 是秘密吗 |
    /// |---|---|---|
    /// | `cloudflare` / `dnspod` | 凭据 | ✅ |
    /// | `exec` | **hook 程序的路径** | ❌ |
    ///
    /// 不分这个岔的话，`dns exec /etc/fulcrum/dns-hook.sh`
    /// 被当成了字面量凭据 —— **门禁当场红**：ACME 那个场景的配置是 0644，
    /// 而它里面根本没有秘密。
    /// ★ ★ 一道会在**没有秘密的配置**上开火的门，会训练人去无脑 chmod、
    /// 或者干脆无视它 —— 那时它挡不住真正该挡的那一次。
    pub fn path(raw: &str) -> Secret {
        Secret {
            raw: raw.to_string(),
            sensitive: false,
            redacted: false,
        }
    }

    /// 真值（含 `env:` / `file:` 前缀那种指针写法）。
    ///
    /// ⚠ 拿到它就有责任不把它打出去。★ 名字里带 `expose` 是有意的：
    /// grep 一下就知道全仓有哪几处碰过真值。
    pub fn expose(&self) -> &str {
        &self.raw
    }

    /// 是不是字面量（真值就在配置文件里）。★ 装载期的权限门看这一条。
    pub fn is_sensitive(&self) -> bool {
        self.sensitive
    }

    /// 是不是从脱敏产物读回来的。
    pub fn is_redacted(&self) -> bool {
        self.redacted
    }

    /// 印出来给人看的样子：字面量一律脱敏，指针原样（它不是秘密，而且它是**线索**）。
    pub fn display(&self) -> &str {
        if self.sensitive { REDACTED } else { &self.raw }
    }
}

/// ⚠ `Debug` 也脱敏：一个 `{:?}` 一整份配置的日志语句，是最容易被忘掉的那条路。
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret({})", self.display())
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display())
    }
}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.sensitive && !revealing() {
            return s.serialize_str(REDACTED);
        }
        // ★ 露真值时给字面量补上 `literal:` 前缀：结构化层是**公开入口**（G11），
        //   而一个没有前缀的裸值在那里是有歧义的（它是值？还是某种没写对的来源？）。
        if self.sensitive && !self.raw.starts_with("literal:") {
            return s.serialize_str(&format!("literal:{}", self.raw));
        }
        s.serialize_str(&self.raw)
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Secret, D::Error> {
        let raw = String::deserialize(d)?;
        if let Some(v) = raw.strip_prefix("literal:") {
            if v.is_empty() {
                return Err(de::Error::custom("`literal:` 后面是空的"));
            }
            return Ok(Secret {
                raw: v.to_string(),
                sensitive: true,
                redacted: false,
            });
        }
        Ok(Secret::parse(&raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 字面量默认脱敏_而指针原样() {
        let lit = Secret::parse("cfat_realtoken12345");
        assert!(lit.is_sensitive());
        assert_eq!(lit.display(), REDACTED);
        assert_eq!(format!("{lit}"), REDACTED);
        assert_eq!(format!("{lit:?}"), format!("Secret({REDACTED})"));
        assert_eq!(
            serde_json::to_string(&lit).unwrap(),
            format!("\"{REDACTED}\"")
        );

        // ★ 指针不是秘密，而且它是**线索**：出错时「从哪个变量/文件读的」必须说得出来。
        let ptr = Secret::parse("env:CF_API_TOKEN");
        assert!(!ptr.is_sensitive());
        assert_eq!(ptr.display(), "env:CF_API_TOKEN");
        assert_eq!(serde_json::to_string(&ptr).unwrap(), "\"env:CF_API_TOKEN\"");
    }

    #[test]
    fn 只有显式进入_reveal_才吐真值() {
        let lit = Secret::parse("cfat_realtoken12345");
        let hidden = serde_json::to_string(&lit).unwrap();
        let shown = reveal(|| serde_json::to_string(&lit).unwrap());
        assert_eq!(hidden, format!("\"{REDACTED}\""));
        assert_eq!(shown, "\"literal:cfat_realtoken12345\"");
        // ⚠ 出了作用域必须立刻恢复 —— 否则一次 `--with-secrets` 会把整个进程后续
        //   所有序列化都变成明文。
        assert_eq!(
            serde_json::to_string(&lit).unwrap(),
            format!("\"{REDACTED}\"")
        );
    }

    #[test]
    fn 脱敏产物读回来要认得出是脱敏的() {
        // ★ ★ 这一条挡的是最坏的那种失败：把 `«已脱敏»` 当成凭据发给 CA，
        //   现场表现是「凭据不对」，而没有任何一处会说「你 load 的是一份脱敏产物」。
        let back: Secret = serde_json::from_str(&format!("\"{REDACTED}\"")).unwrap();
        assert!(back.is_redacted());
        assert!(back.is_sensitive());
    }

    #[test]
    fn 带_literal_前缀的写回来还是同一个值() {
        let lit = Secret::parse("literal:abc123");
        // `literal:` 前缀在 parse 时不剥离（DSL 侧由 compile 负责），
        // 但 JSON 往返必须**稳定**：写出去带前缀，读回来剥掉，值不变。
        let json = reveal(|| serde_json::to_string(&lit).unwrap());
        let back: Secret = serde_json::from_str(&json).unwrap();
        assert_eq!(back.expose(), "abc123");
        assert!(back.is_sensitive());
        assert!(!back.is_redacted());
    }
}
