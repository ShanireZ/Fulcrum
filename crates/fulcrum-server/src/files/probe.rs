//! 一次文件探测的结果。
//!
//! ★ ★ ★ **这个文件单独存在，为的就是那三个字段够不着 —— 它的存在本身就是判据。**
//!
//! ⚠ ⚠ Rust 的私有字段是**模块级**的：[`Probe`] 若和 `serve` / `send_file` 同住
//! `files/mod.rs`，把字段标成私有**一点用都没有** —— 那个模块里照样随便构造，
//! 而「绑成一个结构体」买到的只有「两个值一起走」，**不是**「配错不可表示」。
//! ⇒ 它必须住在子模块里：父模块 `files` 够不到子模块的私有字段。
//! ⛔ **别把它并回 `mod.rs`**，那会让下面两条不变式在一次无害的重构里静默失效。

use std::fs::Metadata;
use std::io::Read;
use std::path::{Path, PathBuf};

/// **这条路径上**的 metadata，以及（小文件时）它的全部字节。
///
/// 两条不变式都由类型守着，⛔ 不靠调用方记得：
///
/// ① **字节必定属于 `meta` 描述的那个文件。** 唯一带字节的构造口是
///    [`Probe::open_blocking`]，它在**同一次** `open` 之后 `fstat` 再 `read`
///    ⇒ 「A 的 metadata 配 B 的字节」**不可表示**。
///    ⚠ 那种误配的后果是**发错内容而 `Content-Length` 与 ETag 全都自洽**
///    ⇒ 不会有任何东西说，所以它值得由类型来挡。
///
/// ② **字节只能按路径取回**（[`Probe::bytes_for`]，路径不符回 `None`）。
///    ⇒ 预压缩旁文件读的是**另一个文件**，它会自动落回分块路径，
///    ⛔ 不再依赖调用方在发送那一行写对「表示是不是原文件」。
pub(super) struct Probe {
    path: PathBuf,
    meta: Metadata,
    bytes: Option<Vec<u8>>,
}

impl Probe {
    /// 一次系统调用序列里做完 `open` + `fstat` +（`len <= inline_limit` 时）整读。
    ///
    /// ⚠ **本函数会阻塞**，调用方负责把它放进 `spawn_blocking`。
    /// ★ 名字里带 `_blocking` 就是为了让「谁负责不阻塞 runtime」在调用点上看得见。
    ///
    /// ⚠ 目录在 Linux 上 `open` 得开而 `read` 不得 ⇒ 只有普通文件才读。
    /// ⚠ 读失败**不算探测失败**：回一个只有 metadata 的 [`Probe`]，
    ///   让调用方那条分块路径去处理（它有完整的报错路径）。
    pub(super) fn open_blocking(path: PathBuf, inline_limit: u64) -> std::io::Result<Self> {
        let mut f = std::fs::File::open(&path)?;
        let meta = f.metadata()?;
        let bytes = if meta.is_file() && meta.len() <= inline_limit {
            let mut buf = Vec::with_capacity(meta.len() as usize);
            match f.read_to_end(&mut buf) {
                Ok(_) => Some(buf),
                Err(_) => None,
            }
        } else {
            None
        };
        Ok(Self { path, meta, bytes })
    }

    /// 只有 metadata 的探测 —— ★ **结构上不可能带字节**。
    ///
    /// 两个调用点：`open` 失败后回落到 `stat()` 的那条路，以及目录索引
    /// （那里的 metadata 来自对**索引文件**的一次 `stat`，与外层那次探测无关）。
    pub(super) fn meta_only(path: PathBuf, meta: Metadata) -> Self {
        Self {
            path,
            meta,
            bytes: None,
        }
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn meta(&self) -> &Metadata {
        &self.meta
    }

    /// 取回 `p` 这个文件的字节；**路径不符就回 `None`**。
    ///
    /// ★ ★ 这是不变式 ② 的落点：发送那一步问的是「要发的**那个表示**的字节在不在
    /// 手上」，⇒ 预压缩旁文件永远问不到，自动走分块路径。
    /// ⛔ 别加一个不带路径的 `bytes()` —— 那等于把这道门重新交回给调用方的记性。
    pub(super) fn bytes_for(&self, p: &Path) -> Option<&[u8]> {
        if p == self.path {
            self.bytes.as_deref()
        } else {
            None
        }
    }
}
