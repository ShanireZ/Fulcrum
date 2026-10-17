//! 一次檔案探測的結果。
//!
//! ★ ★ ★ **這個檔案單獨存在，為的就是那三個欄位夠不著 —— 它的存在本身就是判據。**
//!
//! ⚠ ⚠ Rust 的私有欄位是**模組級**的：[`Probe`] 若和 `serve` / `send_file` 同住
//! `files/mod.rs`，把欄位標成私有**一點用都沒有** —— 那個模組裡照樣隨便建構，
//! 而「綁成一個結構體」買到的只有「兩個值一起走」，**不是**「配錯不可表示」。
//! ⇒ 它必須住在子模組裡：父模組 `files` 夠不到子模組的私有欄位。
//! ⛔ **別把它並回 `mod.rs`**，那會讓下面兩條不變式在一次無害的重構裡靜默失效。

use bytes::Bytes;
use std::fs::Metadata;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

/// **這條路徑上**的 metadata，以及（小檔案時）它的全部位元組。
///
/// 兩條不變式都由類型守著，⛔ 不靠呼叫方記得：
///
/// ① **位元組必定屬於 `meta` 描述的那個檔案。** 唯一帶位元組的建構口是
///    [`Probe::open_blocking`]，它在**同一次** `open` 之後 `fstat` 再 `read`
///    ⇒ 「A 的 metadata 配 B 的位元組」**不可表示**。
///    ⚠ 那種誤配的後果是**發錯內容而 `Content-Length` 與 ETag 全都自洽**
///    ⇒ 不會有任何東西說，所以它值得由類型來擋。
///
/// ② **位元組只能按路徑取回**（[`Probe::bytes_for`] / [`Probe::check_bytes_for`]，路徑不符回 `None`）。
///    ⇒ 預壓縮旁檔案讀的是**另一個檔案**，它會自動落回分塊路徑，
///    ⛔ 不再依賴呼叫方在傳送那一行寫對「表示是不是原檔案」。
pub(super) struct Probe {
    path: PathBuf,
    meta: Metadata,
    /// ★ `Bytes` 而不是 `Vec<u8>`：傳送時按區間 `slice` 出去只增引用計數，
    ///   ⛔ 不再把讀進來的位元組整份再拷一遍（HTTP 層診斷 10-05 · 正文那次拷貝）。
    bytes: Option<Bytes>,
}

impl Probe {
    /// 一次系統呼叫序列裡做完 `open` + `fstat` +（`len <= inline_limit` 時）整讀。
    ///
    /// ⚠ **本函式會阻塞**，呼叫方負責把它放進 `spawn_blocking`。
    /// ★ 名字裡帶 `_blocking` 就是為了讓「誰負責不阻塞 runtime」在呼叫點上看得見。
    ///
    /// ⚠ 目錄在 Linux 上 `open` 得開而 `read` 不得 ⇒ 只有普通檔案才讀。
    /// ⚠ 讀失敗**不算探測失敗**：回一個只有 metadata 的 [`Probe`]，
    ///   讓呼叫方那條分塊路徑去處理（它有完整的回報錯誤路徑）。
    pub(super) fn open_blocking(path: PathBuf, inline_limit: u64) -> std::io::Result<Self> {
        let mut f = std::fs::File::open(&path)?;
        let meta = f.metadata()?;
        let bytes = if meta.is_file() && meta.len() <= inline_limit {
            let mut buf = Vec::with_capacity(meta.len() as usize);
            match f.read_to_end(&mut buf) {
                Ok(_) => Some(Bytes::from(buf)),
                Err(_) => None,
            }
        } else {
            None
        };
        Ok(Self { path, meta, bytes })
    }

    /// **一次跨執行緒都不做**的探測：`open` + `fstat` +（≤ `inline_limit` 時）
    /// 一次 `preadv2(RWF_NOWAIT)` 整讀。
    ///
    /// - `Ok(probe)` —— 這一趟已經做完了。兩種情形：資料本來就在頁快取裡；
    ///   或者根本不需要讀（目錄 / 超過 `inline_limit` 的大檔案）。
    /// - `Err(path)` —— **這一次要等 I/O**（或 `open`/`fstat` 就失敗了），路徑原樣交回
    ///   ⇒ 呼叫方去走 [`Probe::open_blocking`]，那條路才值得付一次
    ///   `spawn_blocking` 的代價。
    ///
    /// ★ ★ ★ **它把「什麼算小」這個判斷整個交給了核心。** 尺寸是個壞代理：
    /// 冷 NFS 上的 1 KB 一樣要等，頁快取裡的 10 MB 一點都不用等 —— 而
    /// 「閾值取多少才對」寫不出反證。這裡的判據是「核心說現在不用等」，
    /// ⇒ 反證得動（見本檔案末尾的單測與 `tests/files/run.sh` 的注入對照）。
    ///
    /// ⚠ ⚠ ★ **`open` 自己仍然可能阻塞**（冷 dentry / 冷 inode）。這一條沒有解，
    /// 要解得上 io_uring。⛔ 寫在明處，別當它不存在。
    ///
    /// ★ 路徑**按值收**、成功時就存進探測結果，失敗時原樣交回（`Err`）給呼叫方去走回落 ——
    ///   ⛔ 不為「存一份」而另拷一份（10-14 · 檔案那條路剩下的分配）。
    pub(super) fn open_nowait(path: PathBuf, inline_limit: u64) -> Result<Self, PathBuf> {
        let Ok(f) = std::fs::File::open(&path) else {
            return Err(path);
        };
        let Ok(meta) = f.metadata() else {
            return Err(path);
        };
        // 不用讀的兩種：目錄（Linux 上 `read` 本來就不得）、大到該分塊發的。
        // ⇒ 這一趟**沒有任何會等 I/O 的動作**，直接算做完。
        if !meta.is_file() || meta.len() > inline_limit {
            return Ok(Self {
                path,
                meta,
                bytes: None,
            });
        }
        let mut buf = vec![0u8; meta.len() as usize];
        if !read_full_nowait(&f, &mut buf) {
            return Err(path);
        }
        Ok(Self {
            path,
            meta,
            bytes: Some(Bytes::from(buf)),
        })
    }

    /// 只有 metadata 的探測 —— ★ **結構上不可能帶位元組**。
    ///
    /// 兩個呼叫點：`open` 失敗後回落到 `stat()` 的那條路，以及目錄索引
    /// （那裡的 metadata 來自對**索引檔案**的一次 `stat`，與外層那次探測無關）。
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

    /// 取回 `p` 這個檔案的位元組；**路徑不符就回 `None`**。
    ///
    /// ★ ★ 這是不變式 ② 的落點：傳送那一步問的是「要發的**那個表示**的位元組在不在
    /// 手上」，⇒ 預壓縮旁檔案永遠問不到，自動走分塊路徑。
    /// ⛔ 別加一個不帶路徑的 `bytes()` —— 那等於把這道門重新交回給呼叫方的記性。
    pub(super) fn bytes_for(&self, p: &Path) -> Option<&Bytes> {
        if p == self.path {
            self.bytes.as_ref()
        } else {
            None
        }
    }

    /// 同 [`Self::bytes_for`] 的那道門，但只回一張憑證：`p` 這個檔案的位元組在手上才給。
    /// 位元組要用憑證去 [`Self::take_bytes`] 換 —— 整段交出去，⛔ 不 `clone`。
    ///
    /// ★ 為什麼拆成兩步（10-14 · 檔案那條路剩下的分配）：從 `Vec` 來的 `Bytes` 第一次被 `clone` /
    ///   `slice` 要升級成共享的那一塊（一次分配），整段發時不必付 ⇒ 要把位元組**拿走**；而呼叫方手裡那個
    ///   `p` 往往就藉著本結構體的路徑，藉著它就拿不到 `&mut self`。憑證不借任何東西，
    ///   且只有這裡造得出來 ⇒ 「位元組只能按路徑取回」（不變式 ②）仍由類型守著。
    pub(super) fn check_bytes_for(&self, p: &Path) -> Option<BytesFor> {
        self.bytes_for(p).is_some().then_some(BytesFor(()))
    }

    /// 憑 [`Self::check_bytes_for`] 給的憑證把位元組拿走（之後本結構體裡就沒有了）。
    pub(super) fn take_bytes(&mut self, _: BytesFor) -> Option<Bytes> {
        self.bytes.take()
    }
}

/// [`Probe::check_bytes_for`] 給的憑證。欄位私有 ⇒ 本模組之外造不出來。
pub(super) struct BytesFor(());

/// `preadv2(2)` 帶 `RWF_NOWAIT`：**資料不在頁快取時立刻回傳**，⛔ 不等 I/O。
///
/// ⚠ ⚠ ★ **本函式不區分 errno，這是有意的。** 呼叫方只問一件事 ——
/// 「這一趟有沒有把整份讀全」，而讀不全的每一種原因在處置上完全一樣
/// （回落到會阻塞的那條路）：
///
/// - `EAGAIN` —— 資料要等 I/O。**這是本函式存在的理由。**
/// - `EOPNOTSUPP` —— ★ ★ ★ **檔案系統不認 `RWF_NOWAIT`**。2026-09-06 在
///   核心 `6.18` 上實測：**overlayfs 與 tmpfs 都回這個**，ext4 正常。
///   ⇒ 容器裡把站台根放在鏡像層或 tmpfs 上時，本條快路徑**恆不生效**，
///   行為與改動前逐字相同。⛔ 別把它讀成缺陷，也⛔ 別假裝它不存在。
/// - `ENOSYS` / `EINVAL` —— 核心太老（`preadv2` 要 4.6，`RWF_NOWAIT` 要 4.14）
///   ⇒ 整個改動**自動**退化成改動前的行為，⛔ 不需要任何版本判斷。
/// - 短讀 —— 一部分頁在快取裡、一部分不在。
///
/// ⇒ 少了一整類「errno 分類分錯了」的缺陷。⛔ **別為了「看清楚發生了什麼」
/// 把 errno 分支加回來** —— 那會新增一條只在冷路徑上執行、平時永遠走不到的判斷。
///
/// ⛔ ⛔ **回傳 `true` 時 `buf` 一定被寫滿**（`rc == buf.len()`，⛔ 不是 `rc >= 0`）。
/// 把短讀當成成功的後果是**發出截斷的正文，而 `Content-Length` 與 ETag 全都自洽**
/// —— 不會有任何東西說。這一行是承重的。
fn read_full_nowait(f: &std::fs::File, buf: &mut [u8]) -> bool {
    let iov = libc::iovec {
        iov_base: buf.as_mut_ptr().cast::<libc::c_void>(),
        iov_len: buf.len(),
    };
    // SAFETY: `iov` 指向 `buf` 那一段活著的、可寫的記憶體，`iov_len` 就是它的長度，
    // 而 `iovcnt = 1` 與之相符；fd 取自上面那個仍然活著的 `File`。
    // `preadv2` 帶**顯式偏移**，不改 fd 的檔案位置，也不動它的任何其它狀態。
    let rc = unsafe { libc::preadv2(f.as_raw_fd(), &iov, 1, 0, libc::RWF_NOWAIT) };
    rc >= 0 && rc as usize == buf.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const LIMIT: u64 = 64 * 1024;

    /// ⚠ ⚠ ★ **這一族判據的**斷言**與檔案系統無關，而它的**覆蓋**不是。**
    ///
    /// `RWF_NOWAIT` 是**每個檔案系統各自決定認不認**的：2026-09-06 在核心 `6.18`
    /// 上實測 ext4 認、**overlayfs 與 tmpfs 回 `EOPNOTSUPP`**。
    /// ⇒ 一條寫成「快路徑必須命中」的斷言會在一種檔案系統上綠、另一種上紅，
    /// 而一條寫成「快路徑沒命中就跳過」的判據是**恆真的**，兩種都不要。
    /// ⇒ 這裡斷言的是**契約**：兩條路要麼給出同一個答案，要麼快路徑老實說
    /// 「我這一趟做不完」而慢路徑把事情做對。★ 兩個分支都被斷言，⛔ 沒有 skip。
    ///
    /// ⚠ ⚠ ★ ★ ★ **但「兩個分支都被斷言」≠「兩個分支這一趟都被走到」** ——
    /// 而這兩件事被混成一句話，代價是這族判據完成實作當天一整天是半瞎的。
    ///
    /// **完成實作那天**：門禁只跑在容器可寫層（overlayfs）上 ⇒ `认` **恆為假**
    /// ⇒ `Some(fast)` 那一支**除空檔案那一格外一次都走不到**。
    /// ⚠ 那不是推理，是量出來的：把 [`read_full_nowait`] 改成「非空檔案一律回
    /// `false`」（＝快路徑對每一個真實檔案都失效），整趟 `UNIT_ONLY`
    /// **RC=0、881 條全綠，連這條契約測試自己都是 `ok`**。
    ///
    /// **修完之後**：[`本趟要跑的根`] 再供一個根（門禁掛的匿名卷，實測落在 ext4 上）
    /// ⇒ 同一個注入現在 **RC=1**，紅的正是本條、訊息逐字點名
    /// `根="/fulcrum-fs-fixture"`。而「那個根真的在場」由
    /// [`每趟必须两个方向都真的走到`] 單獨釘著 —— ⛔ 少了它，本條會悄悄退回半瞎。
    ///
    /// 「快路徑在 ext4 上真的省掉了那次跨執行緒」是一條**測量**，不是這裡的門 ——
    /// 它由診斷臺上的每請求上下文切換數給出。⛔ 別把這兩件事混成一句話。
    struct Tmp(PathBuf);

    impl Tmp {
        fn new(tag: &str) -> Self {
            Self::under(&std::env::temp_dir(), tag)
        }

        /// ⚠ `root` 逐字用作父目錄 —— 呼叫方給哪個根就落在哪個根上，
        /// ⛔ 這裡不做任何回落到 `temp_dir()` 的補救：一次靜默的回落會讓
        /// 「在 ext4 上跑過了」與「其實又跑在 overlayfs 上」長得一模一樣。
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

    /// 獨立於被測程式碼的能力探測：**自己**發一次 `preadv2(RWF_NOWAIT)`。
    ///
    /// ★ ★ ★ **它有意不復用 [`read_full_nowait`]。** 重複使用會讓判據變成自證的 ——
    /// `read_full_nowait` 若被改成恆 `false`，「這個檔案系統不認」與
    /// 「`open_nowait` 老實回落」會**一起**成立，測試照常綠。
    /// ⇒ 這裡自己發系統呼叫，被測的是**它之上那層封裝**。
    ///
    /// ⚠ 探測檔案有意**非空**：實測（核心 6.18）零長讀在檢查 `FMODE_NOWAIT`
    /// 之前就被短路，overlayfs 上 `len=0` 回 `rc=0` 而 `len=1` 回 `ENOTSUP`
    /// ⇒ 拿空檔案探能力會把「不認」誤判成「認」。
    fn 文件系统认不认_nowait(dir: &Path) -> bool {
        let p = dir.join("cap-probe.bin");
        std::fs::write(&p, b"cap").unwrap();
        let f = std::fs::File::open(&p).unwrap();
        let mut buf = [0u8; 3];
        let iov = libc::iovec {
            iov_base: buf.as_mut_ptr().cast::<libc::c_void>(),
            iov_len: buf.len(),
        };
        // SAFETY: 與 `read_full_nowait` 同形 —— `iov` 指向本函式棧上那個活著的
        // `buf`，長度相符，`iovcnt = 1`；fd 取自上面那個仍然活著的 `File`。
        let rc = unsafe { libc::preadv2(f.as_raw_fd(), &iov, 1, 0, libc::RWF_NOWAIT) };
        let _ = std::fs::remove_file(&p);
        rc == 3 && &buf == b"cap"
    }

    /// 本趟要在**哪幾個目錄**上各跑一遍這族判據。
    ///
    /// `std::env::temp_dir()` 恆在；`FULCRUM_TEST_FS_ROOTS`（`:` 分隔）由門禁供，
    /// [`tests/m0/docker-run.sh`] 給容器掛一個 **匿名卷**（`-v /fulcrum-fs-fixture`，
    /// 沒有源）—— 實測它落在 **ext4** 上，而容器可寫層是 **overlayfs**
    /// ⇒ 兩種檔案系統這一趟都在。
    ///
    /// ⚠ ⚠ ★ **這裡有意不寫死「哪個根該是 ext4」。** 承重的性質是
    /// 「本趟見過一個認的、也見過一個不認的」，⛔ 不是「某個具體路徑是某個具體
    /// 檔案系統」—— 後者會在換一臺宿主（xfs 一樣認 `RWF_NOWAIT`）時紅得毫無道理。
    /// 那條性質由 [`每趟必须两个方向都真的走到`] 斷言。
    ///
    /// ⚠ 取不到環境變數時**只回 `temp_dir()`，⛔ 不回報錯誤** —— 回報錯誤的地方只有一處，
    /// 就是那條覆蓋判據；兩處都報會讓一次紅說不清是「夾具沒掛」還是「契約破了」。
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
            // 0 / 1 / 常見 / **正好壓在上限上**（邊界與其餘判據同一個約定：等於算「裝得下」）
            for len in [0usize, 1, 4096, LIMIT as usize] {
                let (p, body) = t.file(&format!("f{len}.bin"), len);
                let slow = Probe::open_blocking(p.clone(), LIMIT).unwrap();
                assert_eq!(
                    slow.bytes_for(&p).map(|b| &b[..]),
                    Some(&body[..]),
                    "慢路径本来就该带全字节（len={len}，根={root:?}）"
                );
                // ⚠ 空檔案**不需要等任何 I/O** ⇒ 它在每種檔案系統上都該走快路徑。
                //   這不是猜的：核心把零長讀短路在 `FMODE_NOWAIT` 檢查之前（見上）。
                let 该命中 = 认 || len == 0;
                match Probe::open_nowait(p.clone(), LIMIT).ok() {
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
                            fast.bytes_for(&p).map(|b| &b[..]),
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

    /// ★ ★ ★ **它與上面那條是一對，而少了哪一條都會留下一個安靜的洞** ——
    /// 形狀照 `tests/bench/gate.sh` 的 C12/C13（`G145`）。
    ///
    /// - 只有上面那條契約判據：門禁只跑在 overlayfs 上時 `认` 恆假 ⇒
    ///   `Some(fast)` 那一支除空檔案外**一次都走不到**，而它照常綠。
    ///   ⚠ ⚠ **這不是推理，是實測**：把 [`read_full_nowait`] 改成非空一律回 `false`，
    ///   2026-09-06 那趟 `UNIT_ONLY` **RC=0、881 條全綠、契約測試自己也是 `ok`**。
    /// - 只有本條覆蓋判據：兩個方向都走得到，而快路徑可以在 ext4 上發錯的位元組，
    ///   本條一聲不吭 —— 那件事只有上面那條判得動。
    ///
    /// ⇒ 本條**只**回答一句話：「這一趟，認與不認兩種檔案系統是不是都真的在場」。
    /// ⛔ 它不判任何產品行為。
    ///
    /// ⚠ 失效方向是**噪音不是沉默**：夾具沒掛、掛錯地方、或哪天 `temp_dir()`
    /// 自己變成了 ext4（兩個根一起認）都會紅，而紅的那一刻它逐字說出該去看哪裡。
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

    /// ★ ★ ★ **`probe_file` 那條回落，在這一層是「正確性」承重。**
    ///
    /// ⚠ ⚠ 2026-09-06 它被整個記成「**效能**承重」，依據是一次注入反證：
    /// 把 `probe_file` 裡那次 `spawn_blocking(open_blocking)` 刪掉，**整趟門禁
    /// RC=0、一格都沒紅**。⇒ 於是它當天沒有任何門守著。
    ///
    /// ★ 那個判斷在 [`super::super::serve`] 那一層是對的：`serve` 還有第二條回落
    /// （`probe_file` 回 `None` ⇒ `stat()` ⇒ [`Probe::meta_only`] ⇒ 分塊路徑）
    /// ⇒ 少了它是**1 次跨執行緒變 3 次**，而不是發不出檔案。
    ///
    /// ⛔ **但 `probe_file` 自己的契約不是那個。** 它答的是「這個小檔案的位元組
    /// 拿到了沒有」，而在一個**不認 `RWF_NOWAIT`** 的檔案系統上（overlayfs /
    /// tmpfs 實測如此），唯一做得到這件事的就是那條回落。
    /// ⇒ **在這一層它刪掉即紅**，⛔ 不需要任何效能量綱。
    ///
    /// ⚠ ⚠ ★ **誠實的射程**：本條只釘住「回落在、且把事情做對了」，
    /// ⛔ **釘不住「它只花 1 次跨執行緒」** —— 有人把它重寫成正確但要三次跨執行緒的
    /// 實作，本條照常綠。那一半今天仍然**只由診斷臺的讀數守著**，
    /// ⛔ 別把本條讀成「那條回落全都有門守著了」。
    ///
    /// ★ 它不會退化成恆真：「不認」那個根確實在場，由
    /// [`每趟必须两个方向都真的走到`] 保證 —— 少了那一條，本條會在一臺所有根都
    /// 認 `RWF_NOWAIT` 的宿主上**靜默地什麼都不測**，所以下面那句 `assert!` 也留著。
    #[tokio::test]
    async fn 文件系统不认_nowait_时_probe_file_仍然必须带回全部字节() {
        let 不认的: Vec<PathBuf> = 本趟要跑的根()
            .into_iter()
            .filter(|r| !文件系统认不认_nowait(&Tmp::under(r, "回落-探能力").0))
            .collect();
        assert!(
            !不认的.is_empty(),
            "本趟一个「不认 RWF_NOWAIT」的根都没有 ⇒ 快路径处处命中，\
             那条回落一次都走不到，本判据什么都没测到（⛔ 不是「通过了」）"
        );

        for root in 不认的 {
            let t = Tmp::under(&root, "回落");
            // ⚠ 非空且遠小於 `CHUNK`：它必然走「該整讀」那一支，
            //   ⇒ 快路徑回 `None` 之後，位元組只能由回落帶回來。
            let (p, body) = t.file("f.bin", 4096);
            let probe = super::super::probe_file(p.clone()).await.expect(
                "`probe_file` 在不认 RWF_NOWAIT 的文件系统上回了 None —— \
                 那条 spawn_blocking(open_blocking) 回落多半被删了。\
                 ⚠ 它在 serve 那一层只是性能（还有第二条回落兜着），\
                 而在 probe_file 这一层是**正确性**：这个根上没有别的路拿得到字节",
            );
            assert_eq!(
                probe.bytes_for(&p).map(|b| &b[..]),
                Some(&body[..]),
                "回落回来了却没带上字节（根={root:?}）"
            );
        }
    }

    #[test]
    fn 超过上限的大文件不读_只带_metadata() {
        // ★ 這一格與檔案系統無關：它根本不發那次 `preadv2`。
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
        let missing = t.0.join("没有这个文件");
        assert_eq!(
            Probe::open_nowait(missing.clone(), LIMIT).err(),
            Some(missing),
            "打不开就把路径原样交回，让调用方去走回落"
        );
    }

    #[test]
    fn 字节只能按路径取回_不变式二() {
        let t = Tmp::new("bykey");
        let (p, _) = t.file("a.bin", 128);
        let (other, _) = t.file("b.bin", 128);
        // 快路徑可能因檔案系統而回 None ⇒ 這一條釘在兩條路各自的產物上。
        for mut probe in [
            Probe::open_nowait(p.clone(), LIMIT).ok(),
            Probe::open_blocking(p.clone(), LIMIT).ok(),
        ]
        .into_iter()
        .flatten()
        {
            assert!(
                probe.bytes_for(&other).is_none(),
                "路径不符就必须回 None，⛔ 否则预压缩旁文件会拿到原文件的字节"
            );
            assert!(
                probe.check_bytes_for(&other).is_none(),
                "凭证那道门也一样：路径不符不给凭证"
            );
            // 路徑相符：憑證換得到整段位元組，換過一次就沒有了。
            let token = probe.check_bytes_for(&p).expect("自己的路径要给凭证");
            assert_eq!(probe.take_bytes(token).map(|b| b.len()), Some(128));
            assert!(probe.check_bytes_for(&p).is_none(), "拿走之后不该再给凭证");
        }
    }
}
