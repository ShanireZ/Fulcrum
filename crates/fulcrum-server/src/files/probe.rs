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
use std::os::fd::AsRawFd;
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

    /// **一次跨线程都不做**的探测：`open` + `fstat` +（≤ `inline_limit` 时）
    /// 一次 `preadv2(RWF_NOWAIT)` 整读。
    ///
    /// - `Some(probe)` —— 这一趟已经做完了。两种情形：数据本来就在页缓存里；
    ///   或者根本不需要读（目录 / 超过 `inline_limit` 的大文件）。
    /// - `None` —— **这一次要等 I/O**（或 `open`/`fstat` 就失败了）
    ///   ⇒ 调用方去走 [`Probe::open_blocking`]，那条路才值得付一次
    ///   `spawn_blocking` 的代价。
    ///
    /// ★ ★ ★ **它把「什么算小」这个判断整个交给了内核。** 尺寸是个坏代理：
    /// 冷 NFS 上的 1 KB 一样要等，页缓存里的 10 MB 一点都不用等 —— 而
    /// 「阈值取多少才对」写不出反证。这里的判据是「内核说现在不用等」，
    /// ⇒ 反证得动（见本文件末尾的单测与 `tests/files/run.sh` 的注入对照）。
    ///
    /// ⚠ ⚠ ★ **`open` 自己仍然可能阻塞**（冷 dentry / 冷 inode）。这一条没有解，
    /// 要解得上 io_uring。⛔ 写在明处，别当它不存在。
    pub(super) fn open_nowait(path: PathBuf, inline_limit: u64) -> Option<Self> {
        let f = std::fs::File::open(&path).ok()?;
        let meta = f.metadata().ok()?;
        // 不用读的两种：目录（Linux 上 `read` 本来就不得）、大到该分块发的。
        // ⇒ 这一趟**没有任何会等 I/O 的动作**，直接算做完。
        if !meta.is_file() || meta.len() > inline_limit {
            return Some(Self {
                path,
                meta,
                bytes: None,
            });
        }
        let mut buf = vec![0u8; meta.len() as usize];
        if !read_full_nowait(&f, &mut buf) {
            return None;
        }
        Some(Self {
            path,
            meta,
            bytes: Some(buf),
        })
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

/// `preadv2(2)` 带 `RWF_NOWAIT`：**数据不在页缓存时立刻返回**，⛔ 不等 I/O。
///
/// ⚠ ⚠ ★ **本函数不区分 errno，这是有意的。** 调用方只问一件事 ——
/// 「这一趟有没有把整份读全」，而读不全的每一种原因在处置上完全一样
/// （回落到会阻塞的那条路）：
///
/// - `EAGAIN` —— 数据要等 I/O。**这是本函数存在的理由。**
/// - `EOPNOTSUPP` —— ★ ★ ★ **文件系统不认 `RWF_NOWAIT`**。2026-09-06 在
///   内核 `6.18` 上实测：**overlayfs 与 tmpfs 都回这个**，ext4 正常。
///   ⇒ 容器里把站点根放在镜像层或 tmpfs 上时，本条快路径**恒不生效**，
///   行为与改动前逐字相同。⛔ 别把它读成缺陷，也⛔ 别假装它不存在。
/// - `ENOSYS` / `EINVAL` —— 内核太老（`preadv2` 要 4.6，`RWF_NOWAIT` 要 4.14）
///   ⇒ 整个改动**自动**退化成改动前的行为，⛔ 不需要任何版本判断。
/// - 短读 —— 一部分页在缓存里、一部分不在。
///
/// ⇒ 少了一整类「errno 分类分错了」的缺陷。⛔ **别为了「看清楚发生了什么」
/// 把 errno 分支加回来** —— 那会新增一条只在冷路径上执行、平时永远走不到的判断。
///
/// ⛔ ⛔ **返回 `true` 时 `buf` 一定被写满**（`rc == buf.len()`，⛔ 不是 `rc >= 0`）。
/// 把短读当成成功的后果是**发出截断的正文，而 `Content-Length` 与 ETag 全都自洽**
/// —— 不会有任何东西说。这一行是承重的。
fn read_full_nowait(f: &std::fs::File, buf: &mut [u8]) -> bool {
    let iov = libc::iovec {
        iov_base: buf.as_mut_ptr().cast::<libc::c_void>(),
        iov_len: buf.len(),
    };
    // SAFETY: `iov` 指向 `buf` 那一段活着的、可写的内存，`iov_len` 就是它的长度，
    // 而 `iovcnt = 1` 与之相符；fd 取自上面那个仍然活着的 `File`。
    // `preadv2` 带**显式偏移**，不改 fd 的文件位置，也不动它的任何其它状态。
    let rc = unsafe { libc::preadv2(f.as_raw_fd(), &iov, 1, 0, libc::RWF_NOWAIT) };
    rc >= 0 && rc as usize == buf.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const LIMIT: u64 = 64 * 1024;

    /// ⚠ ⚠ ★ **这一族判据的**断言**与文件系统无关，而它的**覆盖**不是。**
    ///
    /// `RWF_NOWAIT` 是**每个文件系统各自决定认不认**的：2026-09-06 在内核 `6.18`
    /// 上实测 ext4 认、**overlayfs 与 tmpfs 回 `EOPNOTSUPP`**。
    /// ⇒ 一条写成「快路径必须命中」的断言会在一种文件系统上绿、另一种上红，
    /// 而一条写成「快路径没命中就跳过」的判据是**恒真的**，两种都不要。
    /// ⇒ 这里断言的是**契约**：两条路要么给出同一个答案，要么快路径老实说
    /// 「我这一趟做不完」而慢路径把事情做对。★ 两个分支都被断言，⛔ 没有 skip。
    ///
    /// ⚠ ⚠ ★ ★ ★ **但「两个分支都被断言」≠「两个分支这一趟都被走到」** ——
    /// 而这两件事被混成一句话，代价是这族判据落地当天一整天是半瞎的。
    ///
    /// **落地那天**：门禁只跑在容器可写层（overlayfs）上 ⇒ `认` **恒为假**
    /// ⇒ `Some(fast)` 那一支**除空文件那一格外一次都走不到**。
    /// ⚠ 那不是推理，是量出来的：把 [`read_full_nowait`] 改成「非空文件一律回
    /// `false`」（＝快路径对每一个真实文件都失效），整趟 `UNIT_ONLY`
    /// **RC=0、881 条全绿，连这条契约测试自己都是 `ok`**。
    ///
    /// **修完之后**：[`本趟要跑的根`] 再供一个根（门禁挂的匿名卷，实测落在 ext4 上）
    /// ⇒ 同一个注入现在 **RC=1**，红的正是本条、报文逐字点名
    /// `根="/fulcrum-fs-fixture"`。而「那个根真的在场」由
    /// [`每趟必须两个方向都真的走到`] 单独钉着 —— ⛔ 少了它，本条会悄悄退回半瞎。
    ///
    /// 「快路径在 ext4 上真的省掉了那次跨线程」是一条**测量**，不是这里的门 ——
    /// 它由诊断台上的每请求上下文切换数给出。⛔ 别把这两件事混成一句话。
    struct Tmp(PathBuf);

    impl Tmp {
        fn new(tag: &str) -> Self {
            Self::under(&std::env::temp_dir(), tag)
        }

        /// ⚠ `root` 逐字用作父目录 —— 调用方给哪个根就落在哪个根上，
        /// ⛔ 这里不做任何回落到 `temp_dir()` 的补救：一次静默的回落会让
        /// 「在 ext4 上跑过了」与「其实又跑在 overlayfs 上」长得一模一样。
        fn under(root: &Path, tag: &str) -> Self {
            let d = root.join(format!("fulcrum-probe-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }

        fn file(&self, name: &str, len: usize) -> (PathBuf, Vec<u8>) {
            let p = self.0.join(name);
            let body: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            std::fs::File::create(&p).unwrap().write_all(&body).unwrap();
            (p, body)
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 独立于被测代码的能力探测：**自己**发一次 `preadv2(RWF_NOWAIT)`。
    ///
    /// ★ ★ ★ **它有意不复用 [`read_full_nowait`]。** 复用会让判据变成自证的 ——
    /// `read_full_nowait` 若被改成恒 `false`，「这个文件系统不认」与
    /// 「`open_nowait` 老实回落」会**一起**成立，测试照常绿。
    /// ⇒ 这里自己发系统调用，被测的是**它之上那层封装**。
    ///
    /// ⚠ 探测文件有意**非空**：实测（内核 6.18）零长读在检查 `FMODE_NOWAIT`
    /// 之前就被短路，overlayfs 上 `len=0` 回 `rc=0` 而 `len=1` 回 `ENOTSUP`
    /// ⇒ 拿空文件探能力会把「不认」误判成「认」。
    fn 文件系统认不认_nowait(dir: &Path) -> bool {
        let p = dir.join("cap-probe.bin");
        std::fs::write(&p, b"cap").unwrap();
        let f = std::fs::File::open(&p).unwrap();
        let mut buf = [0u8; 3];
        let iov = libc::iovec {
            iov_base: buf.as_mut_ptr().cast::<libc::c_void>(),
            iov_len: buf.len(),
        };
        // SAFETY: 与 `read_full_nowait` 同形 —— `iov` 指向本函数栈上那个活着的
        // `buf`，长度相符，`iovcnt = 1`；fd 取自上面那个仍然活着的 `File`。
        let rc = unsafe { libc::preadv2(f.as_raw_fd(), &iov, 1, 0, libc::RWF_NOWAIT) };
        let _ = std::fs::remove_file(&p);
        rc == 3 && &buf == b"cap"
    }

    /// 本趟要在**哪几个目录**上各跑一遍这族判据。
    ///
    /// `std::env::temp_dir()` 恒在；`FULCRUM_TEST_FS_ROOTS`（`:` 分隔）由门禁供，
    /// [`tests/m0/docker-run.sh`] 给容器挂一个 **匿名卷**（`-v /fulcrum-fs-fixture`，
    /// 没有源）—— 实测它落在 **ext4** 上，而容器可写层是 **overlayfs**
    /// ⇒ 两种文件系统这一趟都在。
    ///
    /// ⚠ ⚠ ★ **这里有意不写死「哪个根该是 ext4」。** 承重的性质是
    /// 「本趟见过一个认的、也见过一个不认的」，⛔ 不是「某个具体路径是某个具体
    /// 文件系统」—— 后者会在换一台宿主（xfs 一样认 `RWF_NOWAIT`）时红得毫无道理。
    /// 那条性质由 [`每趟必须两个方向都真的走到`] 断言。
    ///
    /// ⚠ 取不到环境变量时**只回 `temp_dir()`，⛔ 不报错** —— 报错的地方只有一处，
    /// 就是那条覆盖判据；两处都报会让一次红说不清是「夹具没挂」还是「契约破了」。
    fn 本趟要跑的根() -> Vec<PathBuf> {
        let mut v = vec![std::env::temp_dir()];
        if let Ok(s) = std::env::var("FULCRUM_TEST_FS_ROOTS") {
            v.extend(s.split(':').filter(|p| !p.is_empty()).map(PathBuf::from));
        }
        v
    }

    #[test]
    fn 认_nowait_就必须命中快路径_不认就必须老实回落() {
        for root in 本趟要跑的根() {
            let t = Tmp::under(&root, "cap");
            let 认 = 文件系统认不认_nowait(&t.0);
            // 0 / 1 / 常见 / **正好压在上限上**（边界与其余判据同一个约定：等于算「装得下」）
            for len in [0usize, 1, 4096, LIMIT as usize] {
                let (p, body) = t.file(&format!("f{len}.bin"), len);
                let slow = Probe::open_blocking(p.clone(), LIMIT).unwrap();
                assert_eq!(
                    slow.bytes_for(&p),
                    Some(&body[..]),
                    "慢路径本来就该带全字节（len={len}，根={root:?}）"
                );
                // ⚠ 空文件**不需要等任何 I/O** ⇒ 它在每种文件系统上都该走快路径。
                //   这不是猜的：内核把零长读短路在 `FMODE_NOWAIT` 检查之前（见上）。
                let 该命中 = 认 || len == 0;
                match Probe::open_nowait(p.clone(), LIMIT) {
                    Some(fast) => {
                        assert!(
                            该命中,
                            "文件系统不认 RWF_NOWAIT，快路径却声称做完了（len={len}，根={root:?}）"
                        );
                        assert_eq!(
                            fast.meta().len(),
                            slow.meta().len(),
                            "len={len}，根={root:?}"
                        );
                        assert_eq!(
                            fast.bytes_for(&p),
                            Some(&body[..]),
                            "len={len}，根={root:?}"
                        );
                    }
                    None => assert!(
                        !该命中,
                        "文件系统认 RWF_NOWAIT，快路径却回落了（len={len}，根={root:?}）—— \
                         ⛔ 这条回落是纯亏损，它正是本次改动要去掉的那一次跨线程"
                    ),
                }
            }
        }
    }

    /// ★ ★ ★ **它与上面那条是一对，而少了哪一条都会留下一个安静的洞** ——
    /// 形状照 `tests/bench/gate.sh` 的 C12/C13（`G145`）。
    ///
    /// - 只有上面那条契约判据：门禁只跑在 overlayfs 上时 `认` 恒假 ⇒
    ///   `Some(fast)` 那一支除空文件外**一次都走不到**，而它照常绿。
    ///   ⚠ ⚠ **这不是推理，是实测**：把 [`read_full_nowait`] 改成非空一律回 `false`，
    ///   2026-09-06 那趟 `UNIT_ONLY` **RC=0、881 条全绿、契约测试自己也是 `ok`**。
    /// - 只有本条覆盖判据：两个方向都走得到，而快路径可以在 ext4 上发错的字节，
    ///   本条一声不吭 —— 那件事只有上面那条判得动。
    ///
    /// ⇒ 本条**只**回答一句话：「这一趟，认与不认两种文件系统是不是都真的在场」。
    /// ⛔ 它不判任何产品行为。
    ///
    /// ⚠ 失效方向是**噪音不是沉默**：夹具没挂、挂错地方、或哪天 `temp_dir()`
    /// 自己变成了 ext4（两个根一起认）都会红，而红的那一刻它逐字说出该去看哪里。
    #[test]
    fn 每趟必须两个方向都真的走到() {
        let mut 认的 = Vec::new();
        let mut 不认的 = Vec::new();
        for root in 本趟要跑的根() {
            let t = Tmp::under(&root, "cover");
            if 文件系统认不认_nowait(&t.0) {
                认的.push(root);
            } else {
                不认的.push(root);
            }
        }
        assert!(
            !认的.is_empty(),
            "本趟没有任何一个根认 RWF_NOWAIT ⇒ 「认 ⇒ 必须命中快路径」那一支\
             （空文件那一格除外）这一趟一次都没走到，而契约判据会照常绿。\n\
             \x20  查这里：`tests/m0/docker-run.sh` 里那行 `-v /fulcrum-fs-fixture`\
             （匿名卷，落在 ext4 上）与随它一起传的 `FULCRUM_TEST_FS_ROOTS`。\n\
             \x20  本趟的根：认的 {认的:?}；不认的 {不认的:?}"
        );
        assert!(
            !不认的.is_empty(),
            "本趟每一个根都认 RWF_NOWAIT ⇒ 回落那一支一次都没走到。\n\
             \x20  它平时由容器可写层（overlayfs）供 —— 若 `temp_dir()` 被挪到了\
             一个认 RWF_NOWAIT 的文件系统上，得另外给一个不认的根。\n\
             \x20  本趟的根：认的 {认的:?}；不认的 {不认的:?}"
        );
    }

    #[test]
    fn 超过上限的大文件不读_只带_metadata() {
        // ★ 这一格与文件系统无关：它根本不发那次 `preadv2`。
        let t = Tmp::new("big");
        let (p, _) = t.file("big.bin", LIMIT as usize + 1);
        let probe = Probe::open_nowait(p.clone(), LIMIT).expect("大文件不需要等 I/O");
        assert_eq!(probe.meta().len(), LIMIT + 1);
        assert!(
            probe.bytes_for(&p).is_none(),
            "超过上限就该留给分块路径，⛔ 不许把整份读进内存"
        );
    }

    #[test]
    fn 目录探测得到_而结构上带不了字节() {
        let t = Tmp::new("dir");
        let probe = Probe::open_nowait(t.0.clone(), LIMIT).expect("目录 open 得开");
        assert!(probe.meta().is_dir());
        assert!(probe.bytes_for(&t.0).is_none());
    }

    #[test]
    fn 不存在的路径回_none_而不是恐慌() {
        let t = Tmp::new("missing");
        assert!(Probe::open_nowait(t.0.join("没有这个文件"), LIMIT).is_none());
    }

    #[test]
    fn 字节只能按路径取回_不变式二() {
        let t = Tmp::new("bykey");
        let (p, _) = t.file("a.bin", 128);
        let (other, _) = t.file("b.bin", 128);
        // 快路径可能因文件系统而回 None ⇒ 这一条钉在两条路各自的产物上。
        for probe in [
            Probe::open_nowait(p.clone(), LIMIT),
            Probe::open_blocking(p.clone(), LIMIT).ok(),
        ]
        .into_iter()
        .flatten()
        {
            assert!(
                probe.bytes_for(&other).is_none(),
                "路径不符就必须回 None，⛔ 否则预压缩旁文件会拿到原文件的字节"
            );
        }
    }
}
