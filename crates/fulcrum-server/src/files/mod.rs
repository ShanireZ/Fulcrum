//! 自研靜態檔案服務（**M2 批 F**，G87–G91）。
//!
//! 在這一批之前，`file_server` 編譯成一個帶回落標記的終結類步驟、由 nginx 頂班。
//! 現在它自己發檔案。
//!
//! ## 做到哪（G89）
//!
//! 發檔案 · 目錄索引 · 目錄不帶尾斜槓 **301** 到帶斜槓 · `browse` 目錄列表 ·
//! `ETag` / `Last-Modified` / `If-None-Match` / `If-Modified-Since` → **304** ·
//! **單段** `Range` 與 `If-Range` · 方法只認 `GET`/`HEAD`，其餘 **405** 帶 `Allow`。
//!
//! **明確不做**（G89，代價已認下）：多段 `multipart/byteranges`（回 200 全量）、
//! 預壓縮旁檔案 `.gz`/`.br`（`encode` 本身還在 `UNWIRED` 裡，
//! 先做旁檔案會讓「壓縮」有兩個互不知道的實作）。
//!
//! ## 請求路徑的次序 —— ★ ★ 順序本身就是判據
//!
//! 1. 方法 → 2. 解碼 → 3. 歸一 → 4. hide → 5. 符號連結 → 6. metadata
//!    → 7. 目錄（301 / index / browse）→ 8. 檔案頭 → 9. 條件請求 → 10. Range → 11. 傳送
//!
//! 前四步全在 [`path`] 裡，是純函式、有自己的單測。
//!
//! ## ⚠ 阻塞 IO 走 `spawn_blocking`，但**只在真的要等 I/O 時**
//!
//! 直接在 async 裡做檔案 IO 會**佔住一個工作執行緒** —— 頁快取命中時只是幾微秒，
//! 而一次冷讀（或一塊壞盤）能把它佔到毫秒級，那時整個 worker 上的其它連接一起卡住。
//! ⇒ 會等 I/O 的活必須進 [`tokio::task::spawn_blocking`]。
//! ⚠ ⚠ ★ **但反過來不成立**：把**不會等**的活也塞進去是純虧損，而這正是
//! 2026-09-06 之前的寫法。
//!
//! ★ ★ ★ **批 F 當初寫的是「不為吞吐做優化，效能是 M3 的事」——
//! M3 發生了，而它給出的數字推翻了那時的實作。** 第一趟真實對拍：
//! 樞衡靜態吞吐 **7638 rps**，nginx / HAProxy **54000+**，⇒ `FAIL`。
//!
//! 根因量了兩輪，⛔ 一次都不是猜的（同處理程序、同負載、同二進位，只換終結指令）：
//!
//! **第一輪**只數了上下文切換次數，據此把三次 `spawn_blocking`
//! （`stat` / `open` / `read`）合成一次 —— 切換/請求 5.87 → 1.71，**預測中了**，
//! ⚠ ⚠ **而 rps 只從 20623 到 24432（+18%）**。
//!
//! **第二輪**（`perf`，並行 200，2 個核）問的是「那些 cycle 到底花在哪」：
//!
//! | 每請求 | `file_server` | `respond`（純記憶體） |
//! |---|---:|---:|
//! | cycles | 250,041 | 96,098 |
//! | instructions | 91,107 | 69,178 |
//! | **IPC** | **0.36** | **0.72** |
//! | **cache-miss** | **7,385** | **1,578** |
//! | 上下文切換 | 1.46 | 0.007 |
//! | CPU 利用 | 1.69 核 | 1.99 核 |
//!
//! 按族合計：排程 / 喚醒 / 系統呼叫入口 **20.94%**（`respond` 9.83%）、
//! **檔案系統本身只有 3.16%**、**本模組自己的業務邏輯只有 2.14%**。
//!
//! ⇒ ★ ★ ★ **成本不在「做檔案那件事」，在「把每個請求在工作執行緒與阻塞執行緒
//! 之間彈一次」。** 而它**不表現為切換次數** —— 表現為 cache-miss 4.68 倍、
//! IPC 掉一半、兩個核還空著 15%。**次數砍掉三分之二，每一次的破壞力沒變**，
//! 所以只換來 +18%。⛔ 這是本模組最容易重犯的一次誤判：量綱選錯了，
//! 而選錯的那個量綱**看上去完全像個正經判據**。
//!
//! ⇒ 現在 [`probe_file`] **先在本執行緒上試一次**
//! （`open` + `fstat` + `preadv2(RWF_NOWAIT)`），頁快取命中就整趟零跨執行緒；
//! 只有核心回「這一次要等」時才付那一次 `spawn_blocking`。
//! 而 [`send_bytes`] 從記憶體發。
//!
//! ⚠ ⚠ ★ **快路徑認不認由檔案系統說了算**：實測 ext4 認，
//! **overlayfs 與 tmpfs 回 `EOPNOTSUPP`** ⇒ 站台根落在鏡像層或 tmpfs 上時
//! 本條**恆不生效**，行為與改動前逐字相同。⛔ 引用本節的收益時不許丟掉這一句。
//!
//! ### 改完之後量到了什麼（診斷臺，⛔ 不是效能讀數）
//!
//! | 並行 200 | 改前 | 改後（ext4）| 改後（overlayfs）|
//! |---|---:|---:|---:|
//! | rps | 28452–29591 | **41023–44361** | 28246–29034 |
//! | 自願切換/請求 | 1.70 | **0.006** | 1.66–1.73 |
//! | 阻塞執行緒 | 74–83 | **6** | 56–63 |
//! | 排程族佔比 | 20.94% | **9.69%** | — |
//! | CPU 利用 | 1.69 核 | **1.99 核** | — |
//!
//! ⇒ **那次 handoff 的代價整個消失了**：切換數、執行緒數、排程佔比與 CPU 利用
//! 四項全部與 `respond` 持平。並行 1 那一格 5492 → 10065（+83%），
//! p50 由 0.173 ms 到 0.095 ms。
//!
//! ⚠ ⚠ ★ ★ ★ **而它仍然只有 `respond` 的一半**（44k 對 91k），⛔ 別讀成
//! 「那個 `FAIL` 修好了」—— 合格宿主上的門檻是 **49078**。
//! 寫這段時立過一條可證偽預測「改完落在 70–90k」，**實測 41–44k，預測錯了**。
//!
//! 剩下那 2.1 倍**不再是排程，是檔案那幾個系統呼叫本身**（每請求實測）：
//!
//! | | `file_server` | `respond` |
//! |---|---:|---:|
//! | 系統呼叫總數 | **6.81** | **3.08** |
//! | 其中 `openat` | 0.943 | 0 |
//! | 其中 `recvfrom` | 0.943 | 0.965 |
//! | 指令數 | 103,148 | 67,678 |
//!
//! ⇒ 網路那一側**每請求逐條相同**，多出來的 **3.73 次**全是
//! `open` / `fstat` / `preadv2` / `close`；而指令數正好多 **35,470**
//! ⇒ 按每次系統呼叫 ~9k 指令（seccomp + spectre 緩解 + `openat` 的整條路徑解析）
//! **它們把差額解釋滿了**。★ 最貴的是 `openat`：每請求重走一遍路徑解析與 dentry 查找。
//!
//! ⏳ ⇒ **再往下只有一條路：別再做這些系統呼叫**（把打開的檔案或它的位元組快取起來，
//! nginx `open_file_cache` 那一路）。⛔ 那是一次獨立的、要 owner 拍板的設計
//! （失效語義、記憶體上限、一整類「發陳舊內容」的缺陷），**本輪有意不做**。
//!
//! ⚠ ⛔ **大檔案、預壓縮旁檔案、目錄索引仍然走原來的分塊路徑** ——
//! `spawn_blocking` 存在的理由在那些場景仍然成立。
//! ⚠ 另有一處**沒有**被本次改動碰到的每請求 `spawn_blocking`：
//! [`within_root`]（只在站台寫了 `follow_symlinks false` 時走）。已登記，⛔ 本次不動。

pub mod httpdate;
pub mod mime;
pub mod path;
mod probe;
pub mod range;

use probe::Probe;

use crate::Downstream;
use bytes::Bytes;
use fulcrum_runtime::FileServerRt;
use http::{HeaderValue, Method};
use log::{debug, warn};
use pingora_http::{RequestHeader, ResponseHeader};
use std::fs::Metadata;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 一次讀多少。★ 64 KiB 是個**沒有實測依據**的起點，寫在這裡是為了讓它可被質疑。
const CHUNK: usize = 64 * 1024;

/// `file_server` 從請求裡要的全部東西：方法 + 5 個頭（**HTTP 層診斷發現 2 · 請求頭不整份克隆**，2026-09-26）。
///
/// ★ 在寫回應**之前**從借來的請求頭裡取出來 ⇒ 資料面不必為 `files` 克隆一整份 `RequestHeader`。
///   h1 上 `HeaderValue` 是從共享讀緩衝切出來的 `Bytes`，clone 只增引用計數、不分配；
///   ⚠ h2（HPACK 解出的字面值）與 h3（`append_header` 收的是 `Vec`）的頭值是可提升的 `Bytes`，
///   第一次 clone 會分配一個小 `Shared` —— 仍比整份克隆少。標準方法的 `Method` clone 不分配。
/// ⚠ 名單**只有這幾樣**：`files` 要讀第 7 個頭就得在這裡加一格 ——
///   這個模組裡不再有 `&RequestHeader` 可查，編譯器會逼著加。
pub(crate) struct FileReq {
    method: Method,
    accept_encoding: Option<HeaderValue>,
    if_none_match: Option<HeaderValue>,
    if_modified_since: Option<HeaderValue>,
    if_range: Option<HeaderValue>,
    range: Option<HeaderValue>,
}

impl FileReq {
    pub(crate) fn from_header(req: &RequestHeader) -> FileReq {
        // ⚠ `HeaderMap::get` 取的是**第一個**同名頭：同名頭出現兩次時後一個不看（單測釘住）。
        let first = |name: &str| req.headers.get(name).cloned();
        FileReq {
            method: req.method.clone(),
            accept_encoding: first("accept-encoding"),
            if_none_match: first("if-none-match"),
            if_modified_since: first("if-modified-since"),
            if_range: first("if-range"),
            range: first("range"),
        }
    }

    fn method(&self) -> &str {
        self.method.as_str()
    }

    fn accept_encoding(&self) -> Option<&str> {
        text(&self.accept_encoding)
    }

    fn if_none_match(&self) -> Option<&str> {
        text(&self.if_none_match)
    }

    fn if_modified_since(&self) -> Option<&str> {
        text(&self.if_modified_since)
    }

    fn if_range(&self) -> Option<&str> {
        text(&self.if_range)
    }

    fn range(&self) -> Option<&str> {
        text(&self.range)
    }
}

/// 頭值當文字讀：不是可見 ASCII（`to_str()` 失敗）的當作沒有這個頭（單測釘住）。
fn text(v: &Option<HeaderValue>) -> Option<&str> {
    v.as_ref().and_then(|v| v.to_str().ok())
}

/// 發檔案。
///
/// `Ok(())` = 已經把回應寫完了。
/// `Err(status)` = 請呼叫方按**站台的 `handle_errors`** 繪製這個狀態碼
/// （與 `Outcome::NoRouteMatch` 一致）。
///
/// ⚠ **405 不走這條 `Err` 路**：RFC 9110 要求 405 必須帶 `Allow`，
/// 而錯誤處理器那條路沒有地方放額外的頭。⇒ 405 在這裡直接寫掉。
/// ★ 代價寫在明處：站台的 `handle_errors` 客製不到 405 那一頁。
//
// ⚠ **`pub(crate)` 而不是 `pub`**（批 J 第六步）：它收的是
// `crate::Downstream`，而那個類型是 `pub(crate)` 的 —— 一個 `pub` 函式收
// 一個夠不到的類型，crate 外面拿它無處可用，`private_interfaces` 會判警告。
// ★ 收窄的是**沒人在用的那一半可見性**：本 crate 之外一處呼叫都沒有。
pub(crate) async fn serve(
    session: &mut Downstream<'_>,
    req: &FileReq,
    fs: &FileServerRt,
    url_path: &str,
    query: &str,
    encoder: Option<crate::encode::Encoder>,
) -> Result<(), u16> {
    // ── 1. 方法 ────────────────────────────────────────────────────────────
    let method = req.method();
    let head_only = match method {
        "GET" => false,
        "HEAD" => true,
        _ => {
            debug!("file_server：不认识的方法 {method} → 405");
            write_head(session, 405, |head| {
                head.push(("Allow", HeaderValue::from_static("GET, HEAD")));
                head.push(("Content-Length", HeaderValue::from_static("0")));
            })
            .await;
            return Ok(());
        }
    };

    // ── 2–4. 解碼 → 歸一 → hide ────────────────────────────────────────────
    let Some(norm) = path::decode_and_normalize(url_path) else {
        debug!("file_server：路径解不开或越界 → 400（{url_path}）");
        return Err(400);
    };
    if path::is_hidden(&norm, &fs.hide) {
        // ★ ★ 回 **404 不是 403**（G88）：403 等於確認「這個檔案在」。
        debug!("file_server：命中 hide 清单 → 404（{url_path}）");
        return Err(404);
    }

    // ── 5. join 到 root，必要時按 canonicalize 檢核 ─────────────────────────
    let full = path::join_under(&fs.root, &norm);
    if !fs.follow_symlinks && !within_root(&fs.root, &full).await {
        // ★ 這裡回 403 而不是 404，與 hide 那條**有意不同**：
        //   hide 擋的是「訪客不該知道它在」，而這裡是「部署方自己放的一條連結
        //   指到了 root 之外」—— 那是一條要讓維運看見的設定事實，藏起來只會讓人查半天。
        warn!("file_server：`{}` 解析到 root 之外 → 403", full.display());
        return Err(403);
    }

    // ── 6. metadata（+ 小檔案順手整讀；頁快取命中時零跨執行緒）──────────────
    //
    // ★ ★ 為什麼是「能不等就別跨執行緒」：見本模組頭部那張 `perf` 表。
    // ⚠ ⚠ `open` 失敗**不是** 404 —— 它的失敗面比 `stat` 寬（檔案本身要可讀）
    //   ⇒ 回落到 `stat()`，讓「存在但打不開」照舊走原來那條路。
    //   ⛔ 這一步是純效能改動，不許改掉任何一個可觀察行為。
    let mut probed = match probe_file(full).await {
        Ok(p) => p,
        // ⚠ ⚠ `open` 失敗**不是** 404 —— 它的失敗面比 `stat` 寬（檔案本身要可讀）
        //   ⇒ 回落到 `stat()`，讓「存在但打不開」照舊走原來那條路。
        //   ★ `meta_only` **結構上不可能帶位元組** ⇒ 這條回落路徑不會誤帶任何資料。
        Err(full) => match stat(&full).await {
            Some(m) => Probe::meta_only(full, m),
            None => {
                debug!("file_server：不存在 → 404（{}）", full.display());
                return Err(404);
            }
        },
    };
    // ★ 路徑從此借探測結果裡那一份（10-14：⛔ 不再為探測另拷一份）。
    let full = probed.path();
    let meta = probed.meta();

    // ── 7. 目錄 ───────────────────────────────────────────────────────────
    if meta.is_dir() {
        // 不帶尾斜槓 → 301 到帶斜槓。★ **查詢串要保留**。
        if !norm.trailing_slash {
            let mut to = String::from(url_path);
            to.push('/');
            if !query.is_empty() {
                to.push('?');
                to.push_str(query);
            }
            debug!("file_server：目录缺尾斜杠 → 301 {to}");
            write_head(session, 301, |head| {
                push_dynamic(head, "Location", to);
                head.push(("Content-Length", HeaderValue::from_static("0")));
            })
            .await;
            return Ok(());
        }
        // 按 index 順序找。
        for name in &fs.index {
            let cand = full.join(name);
            if let Some(m) = stat(&cand).await
                && m.is_file()
            {
                // ★ 索引檔案的 metadata 來自對**它**的一次 `stat` ⇒ `meta_only`，
                //   而那個建構口結構上帶不了位元組。
                let mut probed = Probe::meta_only(cand, m);
                return send_file(session, req, fs, &mut probed, head_only, encoder).await;
            }
        }
        if fs.browse {
            return browse(session, full, url_path, head_only, encoder).await;
        }
        debug!(
            "file_server：目录没有索引且没开 browse → 404（{}）",
            full.display()
        );
        return Err(404);
    }

    if !meta.is_file() {
        // 設備檔案、socket、FIFO —— 發不了，也不該假裝它不在之外的任何事。
        debug!("file_server：不是普通文件 → 404（{}）", full.display());
        return Err(404);
    }

    // ── 8–11 ──────────────────────────────────────────────────────────────
    send_file(session, req, fs, &mut probed, head_only, encoder).await
}

/// `follow_symlinks false` 時的檢核：canonicalize 之後必須仍在 root 之內。
///
/// ⚠ ⚠ 用的是 [`Path::starts_with`]（**按路徑段**比），不是字串前綴比。
/// 字串前綴會讓 root=`/srv/www` 放過 `/srv/wwwevil` —— 而兩者都以那 8 個字元開頭。
async fn within_root(root: &str, full: &Path) -> bool {
    let root = root.to_string();
    let full = full.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let Ok(root_c) = std::fs::canonicalize(&root) else {
            return false;
        };
        match std::fs::canonicalize(&full) {
            Ok(c) => c.starts_with(&root_c),
            // 不存在 ⇒ 交給下一步的 metadata 去回 404。
            // ★ 這裡回 true 不是「放行」，是「這一關不作判斷」——
            //   回 false 會把一次 404 變成一次 403，兩件事在現場必須分得開。
            Err(_) => true,
        }
    })
    .await
    .unwrap_or(false)
}

async fn stat(p: &Path) -> Option<Metadata> {
    let p = p.to_path_buf();
    tokio::task::spawn_blocking(move || std::fs::metadata(&p).ok())
        .await
        .ok()
        .flatten()
}

/// 探一次檔案：**能不等就在本執行緒做完，要等才跨執行緒**。
///
/// ★ ★ ★ 它取代的是**每請求一次無條件的跨執行緒 handoff**（更早還是三次）。
/// 那次 handoff 的真實代價見本模組頭部那張 `perf` 表 —— ⛔ 別再用
/// 「上下文切換次數」當量綱，2026-09-06 已經用那個量綱誤判過一次。
///
/// 判據由核心給：[`Probe::open_nowait`] 走 `preadv2(RWF_NOWAIT)`，
/// 「小」的定義因此是**「核心說這一次不用等」**，⛔ 不是一個寫不出反證的位元組閾值。
///
/// ⚠ 需要讀多少的界取 [`CHUNK`]，⛔ **不引入新常數**：那是本模組已有的分塊粒度，
/// ⇒ 「一個塊裝得下的」與「本來就不必分塊的」是同一件事。
///
/// ⚠ ★ 順帶修掉一個 TOCTOU：原來 `stat(path)` 與後面的 `open(path)` 是**兩次獨立
/// 的路徑解析**，中間檔案可以被換掉（`stream_body` 裡那句「檔案在兩步之間被換掉了」
/// 說的就是它）。現在 metadata 取自**已經打開的那個 fd** ⇒ 兩者必然是同一個檔案。
///
/// ⚠ ⚠ ⛔ **`open` 失敗時回傳 `None`，由呼叫方回落到 `stat()`。** 兩者的失敗面
/// **不一樣**：`stat` 只要父目錄可搜尋就成，而 `open` 還要檔案本身可讀
/// ⇒ 一個 mode 000 的檔案在舊路徑上會走到「發完頭再斷」，直接改成 404 是**行為變更**。
/// 本次是純效能改動，⛔ 不許順手改掉任何一個可觀察行為。
///
/// ★ 路徑**按值收**（10-14 · 檔案那條路剩下的分配）：快路徑成功就存進探測結果，⛔ 不另拷一份；
///   兩條路都失敗時原樣交回（`Err`），呼叫方拿它去 `stat()`。
async fn probe_file(p: PathBuf) -> Result<Probe, PathBuf> {
    // ── ① 先在**本執行緒**上試一次，一次跨執行緒都不做 ─────────────────────────
    //
    // ★ ★ ★ 2026-09-06 剖面量出來的：做檔案那件事只值 **3.16%** 的 CPU，
    //   本模組自己的業務邏輯只值 **2.14%** —— 而把每個請求在工作執行緒與阻塞執行緒
    //   之間彈一次的代價佔了餘下幾乎全部。⚠ 它**不表現為切換次數**
    //   （每請求只剩 1.46 次），表現為每請求 cache-miss **4.68 倍**、
    //   IPC 由 0.72 掉到 0.36、兩個核還空著 15%。
    //   ⇒ 上一輪把切換次數砍掉三分之二只換來 +18%：次數減少了，
    //   **每一次的破壞力沒變**。
    let p = match Probe::open_nowait(p, CHUNK as u64) {
        Ok(probe) => return Ok(probe),
        Err(p) => p,
    };

    // ── ② 到這裡才是真的要等 I/O ──────────────────────────────────────────
    //
    // ★ 這正是 `spawn_blocking` 該出場的場景（別讓一次冷讀佔住工作執行緒），
    //   ⇒ 它沒有被刪掉，只是**不再為每一個頁快取命中付一次**。
    // ⚠ ⚠ 這條路也覆蓋「檔案系統不認 `RWF_NOWAIT`」（overlayfs / tmpfs 實測如此）
    //   ⇒ 那種部署上的行為與本次改動前**逐字相同**，代價是每請求多一次
    //   失敗的 `preadv2`（半微秒量級）。⛔ 不快取這個判斷：一個處理程序可以有
    //   多個站台根落在不同的檔案系統上，而一份記錯了的全域狀態沒有任何東西會說。
    // ⚠ `open` 失敗也走這裡 —— 多一次註定失敗的 `open`，只發生在 404 那條路上。
    //
    // ⚠ ⚠ ★ ★ **它在 [`serve`] 那一層是「效能」承重，在本函式這一層是「正確性」
    //   承重 —— 而這兩件事 2026-09-06 被混成一句話，代價是它整整一天沒有門守著。**
    //
    //   · `serve` 那一層：刪掉它照樣發得出檔案（本函式回 `None` ⇒ `stat()` ⇒
    //     `Probe::meta_only` ⇒ [`stream_body`] 分塊路徑）⇒ 少了它是
    //     **1 次跨執行緒變 3 次**，而不是發不出檔案。**實測**（本機 ShanireHomePC）：
    //     把它整個刪掉，`FILES_ONLY` **RC=0 全綠**，
    //     且 365 條單測裡**只有下面那一條**紅。
    //   · 本函式這一層：契約是「小檔案帶回全部位元組」，而在一個**不認 `RWF_NOWAIT`**
    //     的檔案系統上（overlayfs / tmpfs 實測如此），唯一做得到的就是這條回落。
    //     ⇒ 由 `probe.rs` 的
    //     `文件系统不认_nowait_时_probe_file_仍然必须带回全部字节` 守著，**刪掉即紅**。
    //
    // ⚠ ⛔ **仍然沒有門的那一半**：「它只花 1 次跨執行緒」。把它重寫成正確、
    //   卻要三次跨執行緒的實作，上面那條測試照常綠 —— 那一半今天只由診斷臺的讀數守著。
    // ⚠ 這條慢路徑照舊要一份自有的路徑交給阻塞執行緒；失敗時交回的是呼叫方那一份。
    let q = p.clone();
    match tokio::task::spawn_blocking(move || Probe::open_blocking(q, CHUNK as u64).ok())
        .await
        .ok()
        .flatten()
    {
        Some(probe) => Ok(probe),
        None => Err(p),
    }
}

/// 從**已經在記憶體裡**的位元組發（[`probe_file`] 預取到的那一份）。⛔ 零 `spawn_blocking`。
///
/// ★ 邏輯與 [`stream_body`] 的循環體逐條對應，只是資料源換了 —— ⚠ 壓縮那兩條
/// 陷阱（空塊不能帶 `last=false`、壓縮時最後一塊也不能標 `last`）在這裡**一字不改**
/// 地成立，⛔ 別因為「只有一塊」就把它們省掉。
/// 預取那一份裡的 `[start, start+count)`；區間覆蓋不到回 `None`。
///
/// ★ `slice` 只增引用計數，交出去的仍是 [`Probe`] 讀進來的那塊記憶體 ——
///   ⛔ 別換回 `Bytes::copy_from_slice`：內容一樣，只是每個請求白拷一遍（判據見同名單測）。
/// ★ 整段就原樣交出、⛔ 不 `slice`（10-14）：從 `Vec` 來的 `Bytes` 第一次被 `slice` 要升級成共享的那一塊，
///   那是一次分配；整段發（最常見的 200）不必付。判據：`tests/files_alloc.rs`。
pub fn body_range(bytes: Bytes, start: u64, count: u64) -> Option<Bytes> {
    let s = usize::try_from(start).ok()?;
    let e = s.checked_add(usize::try_from(count).ok()?)?;
    if e > bytes.len() {
        return None;
    }
    if s == 0 && e == bytes.len() {
        return Some(bytes);
    }
    Some(bytes.slice(s..e))
}

async fn send_bytes(
    session: &mut Downstream<'_>,
    bytes: Bytes,
    start: u64,
    count: u64,
    encoder: &mut Option<crate::encode::Encoder>,
) {
    // ⚠ 防禦：預取的那一份必須真的覆蓋這個區間。⛔ 覆蓋不到就斷掉，
    //   與 `stream_body` 裡「檔案比 Content-Length 說的短」同一個處置
    //   （憑證換不到位元組時 `bytes` 是空的，也落在這裡）。
    let Some(raw) = body_range(bytes, start, count) else {
        warn!("file_server：预取的字节比要发的区间短（头已发出）");
        let _ = session.write_response_body(Bytes::new(), true).await;
        return;
    };
    let out = match encoder.as_mut() {
        Some(en) => en.body_filter(Some(&raw), false),
        None => None,
    };
    let chunk = out.unwrap_or(raw);
    let mark_last = encoder.is_none();
    if (!chunk.is_empty() || mark_last)
        && let Err(err) = session.write_response_body(chunk, mark_last).await
    {
        debug!("file_server：写响应体失败：{err}");
        return;
    }
    if let Some(en) = encoder.as_mut() {
        if let Some(tail) = en.body_filter(None, true)
            && !tail.is_empty()
            && let Err(err) = session.write_response_body(tail, false).await
        {
            debug!("file_server：写压缩收尾失败：{err}");
            return;
        }
        let _ = session.write_response_body(Bytes::new(), true).await;
    }
}

/// 預壓縮旁檔案的後綴（**M2 批 I**，G99）。
///
/// ★ 與 Caddy 的 `precompressed` 同名同後綴 —— 寫文件的人不必再記一套。
const SIDECAR_EXT: &[(&str, &str)] = &[("gzip", "gz"), ("br", "br"), ("zstd", "zst")];

/// 這一次要發的**表示**（representation）。
///
/// ★ ★ 預壓縮旁檔案在這裡只是「換一個要讀的檔案」—— 於是 ETag、`Content-Length`、
/// 304、Range、分塊傳送**全部自動落在旁檔案上**，而那正是對的：
/// 被選中的表示就是旁檔案那幾個位元組。
/// ⇒ ★ **預壓縮比現壓嚴格更好**：現壓會掉強 ETag、掉 `Content-Length`、
/// 掉 `Accept-Ranges`，而旁檔案一樣都不掉。
struct Repr<'a> {
    /// 真正要讀的那個檔案（預壓縮時是旁檔案）。
    path: &'a Path,
    meta: &'a Metadata,
    /// `Content-Type` 按**原始**檔案名算。
    ///
    /// ⚠ ★ 旁檔案叫 `x.css.br`，按它算會得到 `application/octet-stream` ——
    /// 瀏覽器就不把它當 CSS 用了，而頁面只是「樣式沒生效」，不報任何錯。
    ctype: &'static str,
    /// 非 `None` = 這是預壓縮旁檔案，要宣佈這個 `Content-Encoding`。
    encoding: Option<&'static str>,
}

/// 按用戶端的 `Accept-Encoding` 順序挑一個**真的存在且不陳**的旁檔案。
///
/// # ★ ★ 為什麼這裡比 nginx / Caddy 多一道 mtime 檢查
///
/// 那兩家預設**不比**旁檔案與原檔案的時間。⚠ 於是一個改了 `x.css`
/// 卻忘了重新生成 `x.css.br` 的部署，會讓**只有支援 br 的那部分使用者**
/// 拿到舊內容 —— 而它不報任何錯、紀錄裡一行都沒有、開發者自己的瀏覽器
/// 還很可能拿到新的。★ 那正是本儲存庫反覆點名的那一類。
/// ⇒ 旁檔案**舊於**原檔案就當它不存在。代價是一次 `stat`，
/// 而那次 `stat` 本來就要做（要拿它的大小與 mtime）。
async fn pick_sidecar(
    fs: &FileServerRt,
    req: &FileReq,
    full: &Path,
    meta: &Metadata,
) -> Option<(PathBuf, Metadata, &'static str)> {
    if fs.precompressed.is_empty() {
        return None;
    }
    let accept = req.accept_encoding()?;
    let base_mtime = meta.modified().ok();
    // ★ 按**用戶端的**順序走，不是按設定的順序 ——
    //   `Accept-Encoding` 裡的順序是用戶端的偏好，而那是它的事不是我們的。
    for token in accept.split(',') {
        let name = token
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        let Some((_, ext)) = SIDECAR_EXT.iter().find(|(algo, _)| *algo == name) else {
            continue;
        };
        if !fs.precompressed.iter().any(|p| p == &name) {
            continue;
        }
        let mut cand = full.as_os_str().to_os_string();
        cand.push(".");
        cand.push(ext);
        let cand = PathBuf::from(cand);
        let Some(m) = stat(&cand).await else { continue };
        if !m.is_file() {
            continue;
        }
        // ★ 陳舊就當不存在（理由見上）。
        if let (Some(bm), Ok(sm)) = (base_mtime, m.modified())
            && sm < bm
        {
            debug!(
                "file_server：旁文件 {} 比原文件旧 —— 当它不存在",
                cand.display()
            );
            continue;
        }
        let algo: &'static str = SIDECAR_EXT
            .iter()
            .find(|(a, _)| *a == name)
            .map(|(a, _)| *a)?;
        return Some((cand, m, algo));
    }
    None
}

/// 8–11 步：選表示 → 頭 → 條件請求 → Range → 傳送。
async fn send_file(
    session: &mut Downstream<'_>,
    req: &FileReq,
    fs: &FileServerRt,
    // ★ ★ 「哪個檔案」與「它的 metadata / 位元組」現在是**同一個值**帶來的
    //   ⇒ ⛔ 不再有一個能與 metadata 各說各話的獨立 `full` 參數。
    // ★ `&mut`：整段發時把位元組從它那裡拿走（10-14，見下面「11. 傳送」）。
    probed: &mut Probe,
    head_only: bool,
    encoder: Option<crate::encode::Encoder>,
) -> Result<(), u16> {
    let full = probed.path();
    let meta = probed.meta();
    let base_name = full.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let ctype = mime::for_name(base_name);
    // ★ ★ 預壓縮旁檔案優先（**G99**）：它既省 CPU，又把強 ETag / Range /
    //   `Content-Length` 全部保住 —— 而現壓那三樣都保不住。
    let side = pick_sidecar(fs, req, full, meta).await;
    let repr = match &side {
        Some((p, m, algo)) => Repr {
            path: p,
            meta: m,
            ctype,
            encoding: Some(algo),
        },
        None => Repr {
            path: full,
            meta,
            ctype,
            encoding: None,
        },
    };
    // ⚠ 已經是預壓縮的就不再現壓一遍。
    //   ★ 它不靠這一行也不會雙重壓縮（壓縮層看見 `Content-Encoding` 就放行），
    //   但顯式寫出來比靠另一層的行為可靠。
    let encoder = if side.is_some() { None } else { encoder };

    let len = repr.meta.len();
    let ctype = repr.ctype;
    let mtime = repr.meta.modified().ok();
    let etag = etag_of(repr.meta);
    // ★ 比較用的 `&str` 視圖：ETag 是自己寫出來的引號 + 十六進制，`to_str` 必過。
    let etag_str = etag.as_ref().and_then(|e| e.to_str().ok());
    let last_mod = mtime.and_then(last_modified_value);
    let full = repr.path;

    // ── 9. 條件請求 ───────────────────────────────────────────────────────
    //
    // ★ 次序照 RFC 9110 §13.2.2：`If-None-Match` **優先**，
    //   它在場時 `If-Modified-Since` 一個字都不看。
    let fresh = match req.if_none_match() {
        Some(inm) => etag_str.is_some_and(|e| if_none_match_hit(inm, e)),
        None => match (req.if_modified_since(), mtime) {
            (Some(ims), Some(mt)) => match (httpdate::parse(ims), unix_secs(mt)) {
                // ⚠ 比的是**秒**：`Last-Modified` 本來就只有秒精度，
                //   拿納秒去比會讓「同一秒內的檔案」永遠不新鮮。
                (Some(since), Some(m)) => m <= since,
                _ => false,
            },
            _ => false,
        },
    };
    if fresh {
        debug!("file_server：条件请求命中 → 304（{}）", full.display());
        write_head(session, 304, |head| push_validators(head, etag, last_mod)).await;
        return Ok(());
    }

    // ── 10. Range ─────────────────────────────────────────────────────────
    //
    // `If-Range` 不匹配 ⇒ **忽略 Range**、回 200 全量（RFC 9110 §13.1.5）。
    let range_ok = match req.if_range() {
        None => true,
        Some(ir) => if_range_matches(ir, etag_str, mtime),
    };
    let verdict = match (range_ok, req.range()) {
        (true, Some(r)) => range::parse(r, len),
        _ => range::RangeVerdict::Ignore,
    };

    let (status, start, end) = match verdict {
        range::RangeVerdict::Unsatisfiable => {
            debug!("file_server：Range 不可满足 → 416（{}）", full.display());
            write_head(session, 416, |head| {
                push_dynamic(head, "Content-Range", range::unsatisfiable_header(len));
                head.push(("Content-Length", HeaderValue::from_static("0")));
                head.push(("Accept-Ranges", HeaderValue::from_static("bytes")));
                push_validators(head, etag, last_mod);
            })
            .await;
            return Ok(());
        }
        range::RangeVerdict::Single { start, end } => (206, start, end),
        // 空檔案走這裡：`0..=-1` 表示不出來，所以用 `len == 0` 單獨判。
        range::RangeVerdict::Ignore if len == 0 => (200, 0, 0),
        range::RangeVerdict::Ignore => (200, 0, len - 1),
    };
    let body_len = if len == 0 { 0 } else { end - start + 1 };

    // ⚠ ⚠ **206 不現壓**：`Content-Range` 說的是**這個表示**的位元組區間，
    //   而現壓會把位元組整個換掉 —— 兩者放在同一個回應裡必然對不上。
    //   ★ 預壓縮那條路不受影響：旁檔案本身就是那個表示，區間是它自己的。
    let mut encoder = if status == 206 { None } else { encoder };
    // ★ 順序就是上線的順序，⛔ 別重排。
    write_head_encoded(session, status, &mut encoder, |head| {
        head.push(("Content-Type", HeaderValue::from_static(ctype)));
        head.push(("Content-Length", content_length_value(body_len)));
        head.push(("Accept-Ranges", HeaderValue::from_static("bytes")));
        push_validators(head, etag, last_mod);
        if status == 206 {
            push_dynamic(head, "Content-Range", range::content_range(start, end, len));
        }
        // ★ 預壓縮旁檔案：宣佈編碼，並按 RFC 9110 補 `Vary`。
        //   ⚠ 少了 `Vary`，下游任何一層快取都會把這份 br 的位元組發給不認 br 的用戶端。
        if let Some(algo) = repr.encoding {
            head.push(("Content-Encoding", HeaderValue::from_static(algo)));
            head.push(("Vary", HeaderValue::from_static("Accept-Encoding")));
        }
    })
    .await;

    // ── 11. 傳送 ──────────────────────────────────────────────────────────
    if head_only || body_len == 0 {
        let _ = session.write_response_body(Bytes::new(), true).await;
        return Ok(());
    }
    // ★ ★ ★ **按路徑取回**：要發的是 `full`（可能已經被換成預壓縮旁檔案），
    //   而 `bytes_for` 在路徑不符時回 `None` ⇒ 旁檔案**自動**落回分塊路徑。
    //   ⛔ 這裡不再有「表示是不是原檔案」這個需要人記得寫對的條件 ——
    //   它由 `Probe` 的類型面擋著（見 `files/probe.rs` 的不變式 ②）。
    // ★ 先按路徑要一張憑證、再憑它把位元組**拿走**（10-14）：`full` 藉著 `probed`，藉著它拿不到 `&mut`；
    //   拿走而不是 `clone`，整段發時就不必把 `Bytes` 升級成共享的那一塊（一次分配）。
    match probed.check_bytes_for(full) {
        // 整個檔案已經在記憶體裡 ⇒ **這一整趟請求只有那一次 `spawn_blocking`**。
        Some(token) => {
            let bytes = probed.take_bytes(token).unwrap_or_default();
            send_bytes(session, bytes, start, body_len, &mut encoder).await;
        }
        None => stream_body(session, full, start, body_len, &mut encoder).await,
    }
    Ok(())
}

/// 分塊把 `[start, start+count)` 發出去。
async fn stream_body(
    session: &mut Downstream<'_>,
    full: &Path,
    start: u64,
    count: u64,
    encoder: &mut Option<crate::encode::Encoder>,
) {
    let p = full.to_path_buf();
    let opened = tokio::task::spawn_blocking(move || {
        let mut f = std::fs::File::open(&p)?;
        f.seek(SeekFrom::Start(start))?;
        Ok::<_, std::io::Error>(f)
    })
    .await;
    let mut f = match opened {
        Ok(Ok(f)) => f,
        // ⚠ 頭已經發出去了 —— 這裡除了斷掉連接沒有別的誠實做法。
        //   ★ 用戶端會看到一個「Content-Length 說了 N、實際收到 0」的回應，
        //   那正是「檔案在兩步之間被換掉了」應該長的樣子。
        other => {
            warn!("file_server：开文件失败（头已发出）：{other:?}");
            let _ = session.write_response_body(Bytes::new(), true).await;
            return;
        }
    };

    let mut left = count;
    while left > 0 {
        let want = left.min(CHUNK as u64) as usize;
        let read = tokio::task::spawn_blocking(move || {
            let mut buf = vec![0u8; want];
            let mut got = 0;
            while got < want {
                match f.read(&mut buf[got..]) {
                    Ok(0) => break,
                    Ok(n) => got += n,
                    Err(e) => return (f, Err(e)),
                }
            }
            buf.truncate(got);
            (f, Ok(buf))
        })
        .await;
        let (back, buf) = match read {
            Ok((back, Ok(buf))) => (back, buf),
            Ok((_, Err(e))) => {
                warn!("file_server：读文件失败（头已发出）：{e}");
                let _ = session.write_response_body(Bytes::new(), true).await;
                return;
            }
            Err(e) => {
                warn!("file_server：读文件的任务挂了：{e}");
                let _ = session.write_response_body(Bytes::new(), true).await;
                return;
            }
        };
        f = back;
        if buf.is_empty() {
            // 檔案在讀的中途被截短了。
            warn!("file_server：文件比 Content-Length 说的短（头已发出）");
            break;
        }
        left -= buf.len() as u64;
        let last = left == 0;
        let raw = Bytes::from(buf);
        // ★ 壓縮在寫之前。⚠ 壓縮層會**攢**資料：一塊進去可能什麼都不出來，
        //   而**空塊不能帶 `last=false` 寫下去** —— 分塊編碼裡零長塊就是體結束。
        let out = match encoder.as_mut() {
            Some(e) => e.body_filter(Some(&raw), false),
            None => None,
        };
        let chunk = out.unwrap_or(raw);
        // ⚠ 壓縮時**最後一塊也不能標 `last`**：收尾（gzip 的 footer）還沒吐出來。
        let mark_last = last && encoder.is_none();
        if chunk.is_empty() && !mark_last {
            continue;
        }
        if let Err(e) = session.write_response_body(chunk, mark_last).await {
            debug!("file_server：写响应体失败：{e}");
            return;
        }
    }
    // ★ 壓縮的收尾：漏了它，用戶端拿到的是一個**少了尾巴**的壓縮流，
    //   而它只會在解壓到最後一刻報「意外的流結束」。
    if let Some(enc) = encoder.as_mut() {
        if let Some(tail) = enc.body_filter(None, true)
            && !tail.is_empty()
            && let Err(e) = session.write_response_body(tail, false).await
        {
            debug!("file_server：写压缩收尾失败：{e}");
            return;
        }
        let _ = session.write_response_body(Bytes::new(), true).await;
    } else if left > 0 {
        let _ = session.write_response_body(Bytes::new(), true).await;
    }
}

/// `browse`：目錄列表。
async fn browse(
    session: &mut Downstream<'_>,
    dir: &Path,
    url_path: &str,
    head_only: bool,
    encoder: Option<crate::encode::Encoder>,
) -> Result<(), u16> {
    let d = dir.to_path_buf();
    let entries = tokio::task::spawn_blocking(move || {
        let mut names: Vec<(String, bool)> = Vec::new();
        for e in std::fs::read_dir(&d).ok()?.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            names.push((name, is_dir));
        }
        // 目錄在前，同類按名字排。★ 有序才談得上「兩次請求給同一個答案」。
        names.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Some(names)
    })
    .await;
    let Ok(Some(entries)) = entries else {
        return Err(404);
    };
    let html = render_listing(url_path, &entries);
    let bytes = Bytes::from(html);
    // ★ 目錄列表是 HTML，正是最該壓的那一類。
    let mut encoder = encoder;
    write_head_encoded(session, 200, &mut encoder, |head| {
        head.push((
            "Content-Type",
            HeaderValue::from_static("text/html; charset=utf-8"),
        ));
        head.push(("Content-Length", content_length_value(bytes.len() as u64)));
    })
    .await;
    if head_only {
        let _ = session.write_response_body(Bytes::new(), true).await;
        return Ok(());
    }
    match encoder.as_mut() {
        None => {
            let _ = session.write_response_body(bytes, true).await;
        }
        Some(enc) => {
            // ⚠ 一次把整個體喂進去、再拿收尾 —— 兩步都可能吐東西，
            //   而**任何一步漏掉都會讓解壓在最後一刻失敗**。
            if let Some(part) = enc.body_filter(Some(&bytes), false)
                && !part.is_empty()
            {
                let _ = session.write_response_body(part, false).await;
            }
            if let Some(tail) = enc.body_filter(None, true)
                && !tail.is_empty()
            {
                let _ = session.write_response_body(tail, false).await;
            }
            let _ = session.write_response_body(Bytes::new(), true).await;
        }
    }
    Ok(())
}

/// 目錄列表的 HTML。★ 分出來是為了讓它能被單測直接喂輸入。
fn render_listing(url_path: &str, entries: &[(String, bool)]) -> String {
    let mut s = String::new();
    s.push_str("<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\n<title>");
    escape_into(&mut s, url_path);
    s.push_str("</title>\n</head><body>\n<h1>");
    escape_into(&mut s, url_path);
    s.push_str("</h1>\n<ul>\n");
    if url_path != "/" {
        s.push_str("<li><a href=\"../\">../</a></li>\n");
    }
    for (name, is_dir) in entries {
        s.push_str("<li><a href=\"");
        // ⚠ href 裡要的是**百分號編碼**，正文裡要的是 **HTML 轉義** ——
        //   兩種轉義解決的是兩件事，混用一種會各漏一半。
        url_escape_into(&mut s, name);
        if *is_dir {
            s.push('/');
        }
        s.push_str("\">");
        escape_into(&mut s, name);
        if *is_dir {
            s.push('/');
        }
        s.push_str("</a></li>\n");
    }
    s.push_str("</ul>\n</body></html>\n");
    s
}

fn escape_into(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

/// href 用的百分號編碼。**只放行 URL 裡無歧義的那幾類字元**，其餘一律編碼。
fn url_escape_into(out: &mut String, s: &str) {
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
}

// ── 檢核器 ────────────────────────────────────────────────────────────────

/// `"<mtime纳秒十六进制>-<size十六进制>"`。
///
/// ★ ★ 用**納秒**而不是秒（nginx 用秒）：秒級精度下「同一秒內改了、大小沒變」
/// 這個視窗裡，ETag 不變 ⇒ 用戶端永遠拿舊的。納秒把這個視窗關掉。
/// ⚠ 代價：檔案系統若只記到秒（有的 tmpfs / 網路盤如此），納秒位恆為 0，
/// 於是這條 ETag 退化成 nginx 那一檔 —— 不會更差，但也不會更好。
fn etag_of(meta: &Metadata) -> Option<HeaderValue> {
    let nanos = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    etag_value(nanos, meta.len())
}

/// `"<nanos 十六进制>-<len 十六进制>"`，寫在棧上、⛔ 不走 `fmt`（10-05）。
/// ★ 與舊寫法 `format!("\"{nanos:x}-{len:x}\"")` 逐位元組相同由單測釘著 —— ETag 一變，
///   每個用戶端與中間快取手上的副本就一次性全部失效。
/// u128 最多 32 位十六進制、u64 最多 16 位 ⇒ 加兩個引號一個連字元共 51 位元組。
fn etag_value(nanos: u128, len: u64) -> Option<HeaderValue> {
    let mut buf = [0u8; 51];
    buf[0] = b'"';
    let mut n = 1;
    n += put_hex(&mut buf[n..], nanos);
    buf[n] = b'-';
    n += 1;
    n += put_hex(&mut buf[n..], u128::from(len));
    buf[n] = b'"';
    n += 1;
    HeaderValue::from_bytes(&buf[..n]).ok()
}

/// `Content-Length` 的頭值：十進位寫在棧上，只為頭值本身分配一次（2026-10-15 · respond 剩的分配）。
/// ⛔ 別換回 `HeaderValue::from(u64)`：http 1.x 先按最長 20 位元組建 `BytesMut` 再 `freeze`，
///   長度對不上容量 ⇒ `freeze` 還要另分配一塊共享頭，每個回應兩次。判據：`tests/respond_alloc.rs`。
pub fn content_length_value(n: u64) -> HeaderValue {
    let mut buf = [0u8; 20]; // u64 最多 20 位十進位
    let mut at = buf.len();
    let mut x = n;
    loop {
        at -= 1;
        buf[at] = b'0' + (x % 10) as u8;
        x /= 10;
        if x == 0 {
            break;
        }
    }
    // 只有數字 ⇒ 走不到 `Err`；真走到也照舊寫對，⛔ 不在請求路徑上 panic。
    HeaderValue::from_bytes(&buf[at..]).unwrap_or_else(|_| HeaderValue::from(n))
}

/// 小寫十六進制、無前導零（`0` 寫成 `0`）—— 與 `{:x}` 同一個口徑。回寫了幾位。
fn put_hex(out: &mut [u8], v: u128) -> usize {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let n = (128 - v.leading_zeros() as usize).div_ceil(4).max(1);
    let mut x = v;
    for b in out[..n].iter_mut().rev() {
        *b = DIGITS[(x & 0xf) as usize];
        x >>= 4;
    }
    n
}

/// `Last-Modified`。早於 1970 的時刻回 `None`（同 [`httpdate::format_imf`]，不假裝它是 epoch）。
fn last_modified_value(mtime: SystemTime) -> Option<HeaderValue> {
    let secs = unix_secs(mtime)?;
    let mut buf = [0u8; httpdate::IMF_LEN];
    if httpdate::write_imf_secs(secs, &mut buf) {
        HeaderValue::from_bytes(&buf).ok()
    } else {
        HeaderValue::try_from(httpdate::format_imf_secs(secs)).ok()
    }
}

fn unix_secs(t: SystemTime) -> Option<i64> {
    t.duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

/// ★ 按值收：每條路徑只推一次、推完不再用 ⇒ ⛔ 不必 `clone`。
fn push_validators(head: &mut Head, etag: Option<HeaderValue>, last: Option<HeaderValue>) {
    // ⚠ 名照舊寫成 `("名", 值)` 的樣子 —— `发文件时插的名都照原样写得出` 那條測試按這個樣子從原始碼裡摳名。
    if let Some(e) = etag {
        head.push(("ETag", e));
    }
    if let Some(l) = last {
        head.push(("Last-Modified", l));
    }
}

/// `If-None-Match`：`*`、逗號列表、`W/` 前綴都要處理（RFC 9110 §13.1.2）。
///
/// ⚠ 這裡用的是**弱比較**：`W/"x"` 與 `"x"` 算同一個。
/// RFC 對 `If-None-Match` 明確要求弱比較（`If-Match` 才是強比較）。
pub fn if_none_match_hit(header_value: &str, etag: &str) -> bool {
    let h = header_value.trim();
    if h == "*" {
        return true;
    }
    let want = strip_weak(etag);
    h.split(',').any(|c| strip_weak(c.trim()) == want)
}

fn strip_weak(s: &str) -> &str {
    s.strip_prefix("W/").unwrap_or(s)
}

/// `If-Range`：值要麼是一個 entity-tag，要麼是一個 HTTP 日期。
///
/// ⚠ ⚠ 這裡必須用**強比較**（RFC 9110 §13.1.5 明說）：`W/"x"` 不匹配任何東西。
/// 弱 ETag 的含義是「語義等價但位元組可能不同」，而 Range 要的正是**位元組**。
/// ★ 拿弱 ETag 去放行一次 Range，用戶端會把兩個版本的位元組拼在一起 ——
/// 而拼出來的檔案既不回報錯誤也不是任何一個版本。
fn if_range_matches(value: &str, etag: Option<&str>, mtime: Option<SystemTime>) -> bool {
    let v = value.trim();
    if v.starts_with('"') {
        return matches!(etag, Some(e) if e == v);
    }
    if v.starts_with("W/") {
        return false;
    }
    match (httpdate::parse(v), mtime.and_then(unix_secs)) {
        (Some(d), Some(m)) => d == m,
        _ => false,
    }
}

/// 要發的回應頭：名是**字面值**，值已經是 `HeaderValue`（10-05 · 構回應頭不再每個頭分配幾次）。
///
/// ★ 回應頭**不建大小寫表**（`build_no_case`，G162）⇒ 這裡的每個名都必須照 pingora 的標題化表
/// （`pingora-http` 的 `titled_header_name_str`）寫出來才算數：`ETag` / `Last-Modified` / `Content-Range` / `Vary` /
/// `Location` / `Allow` 是 fork 改動 17 補進去的，補之前它們沒表就以**小寫**上線。
/// ⚠ 新加一個頭名時先看它在不在那張表裡、寫法是否一字不差；不在就得先補表。
/// 判據：本模組的 `发文件时插的名都照原样写得出`（從本檔案原始碼裡摳名）與 `tests/files/run.sh` 逐行釘頭名的那幾條。
///
/// ★ 就是那個 `ResponseHeader` 外面套一層、只開 `push` 一個口（10-14 · 檔案那條路剩下的分配）：
///   頭直接放進去，⛔ 不先攢進一個 `Vec` 再倒一遍。由 [`write_head_encoded`] 建好後交給呼叫方的閉包去填。
struct Head(ResponseHeader);

impl Head {
    /// 放一個頭。覆蓋語義（`insert_header`），與原來攢好再逐個 `insert_header` 逐字相同；順序就是上線的順序。
    fn push(&mut self, (name, value): (&'static str, HeaderValue)) {
        let _ = self.0.insert_header(name, value);
    }
}

/// 動態的值（來自請求、或格式化出來的）：轉不成合法頭值就**不發這個頭** ——
/// 與舊寫法 `let _ = resp.insert_header(..)` 失敗即跳過是同一個處置。
fn push_dynamic(head: &mut Head, name: &'static str, value: String) {
    if let Ok(v) = HeaderValue::try_from(value) {
        head.push((name, v));
    }
}

async fn write_head(session: &mut Downstream<'_>, status: u16, fill: impl FnOnce(&mut Head)) {
    write_head_encoded(session, status, &mut None, fill).await;
}

/// 同上，但讓壓縮層看一眼這個頭（**M2 批 I**）。
///
/// ★ ★ 壓縮層在這裡做三件會改頭的事：補 `Vary: Accept-Encoding`、
/// 把強 ETag 弱化成 `W/"…"`（**G102**）、去掉 `Content-Length` 與 `Accept-Ranges`
/// 並改成 `chunked`。⚠ 最後那一條意味著**被現壓的回應沒有 Range** ——
/// 這是正確的（區間說的是未壓縮的位元組），但使用者看得見，所以寫在文件裡。
async fn write_head_encoded(
    session: &mut Downstream<'_>,
    status: u16,
    encoder: &mut Option<crate::encode::Encoder>,
    fill: impl FnOnce(&mut Head),
) {
    // ★ 不建大小寫表（G162）：名全是本檔案的字面值，且都照標題化表寫得出（見 [`Head`]）。
    let resp = match ResponseHeader::build_no_case(status, None) {
        Ok(r) => r,
        Err(e) => {
            warn!("file_server：构不出响应头（status={status}）：{e}");
            return;
        }
    };
    let mut head = Head(resp);
    fill(&mut head);
    let Head(mut resp) = head;
    if let Some(enc) = encoder.as_mut() {
        enc.header_filter(&mut resp, crate::encode::status_has_no_body(status));
    }
    if let Err(e) = session.write_response_header(Box::new(resp)).await {
        debug!("file_server：写响应头失败：{e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本檔案非測試部分裡「像頭名、後面緊跟逗號」的字串字面值 —— 即 `("名", 值)` 與 `push_dynamic(head, "名", 值)`。
    /// 粗掃：先去掉 `//` 註解，跳過 `'x'` / `'\x'` 字元字面值（含 `'"'`），字串裡認 `\` 轉義。
    fn header_name_literals(src: &str) -> std::collections::BTreeSet<String> {
        let code: Vec<&str> = src
            .split("#[cfg(test)]")
            .next()
            .unwrap()
            .lines()
            .map(|l| l.find("//").map_or(l, |i| &l[..i]))
            .collect();
        let code = code.join("\n");
        let b = code.as_bytes();
        let mut out = std::collections::BTreeSet::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\'' && i + 2 < b.len() && b[i + 2] == b'\'' {
                i += 3;
            } else if b[i] == b'\'' && i + 3 < b.len() && b[i + 1] == b'\\' && b[i + 3] == b'\'' {
                i += 4;
            } else if b[i] == b'"' {
                let mut j = i + 1;
                while j < b.len() && b[j] != b'"' {
                    if b[j] == b'\\' {
                        j += 1;
                    }
                    j += 1;
                }
                let lit = &code[i + 1..j.min(b.len())];
                let shaped = lit.bytes().next().is_some_and(|c| c.is_ascii_alphabetic())
                    && lit.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-');
                if shaped
                    && code
                        .get(j + 1..)
                        .is_some_and(|r| r.trim_start().starts_with(','))
                {
                    out.insert(lit.to_string());
                }
                i = j + 1;
            } else {
                i += 1;
            }
        }
        out
    }

    /// ★ G162：`file_server` 的回應頭不建大小寫表 ⇒ 本檔案發的每個頭名都必須照原樣寫得出（[`Head`]）。
    ///   名從**本檔案原始碼**裡摳，新加一個頭名不用記得來改這裡 —— 它自己會被掃到。
    #[test]
    fn 发文件时插的名都照原样写得出() {
        let names = header_name_literals(include_str!("mod.rs"));
        // 自證掃得到：今天發的那十個一個不能少（掃描器壞成「什麼都沒掃到」時這裡紅，⛔ 不是空集合恆真）。
        for want in [
            "Accept-Ranges",
            "Allow",
            "Content-Encoding",
            "Content-Length",
            "Content-Range",
            "Content-Type",
            "ETag",
            "Last-Modified",
            "Location",
            "Vary",
        ] {
            assert!(names.contains(want), "扫描器没扫到 {want}：{names:?}");
        }
        let bad: Vec<_> = names.iter().filter(|n| !crate::h1_name_as_is(n)).collect();
        assert!(bad.is_empty(), "不建大小写表时这些名会变样：{bad:?}");
    }

    /// 上面那個掃描器自己的兩個方向：該掃到的掃到，註解、字元字面值、值都不算。
    #[test]
    fn 头名扫描器_认得两种写法_不认注释与值() {
        let src = "fn f() {\n    head.push((\"X-A\", v));\n    push_dynamic(\n        &mut head,\n        \"x-b\",\n        s,\n    );\n    // (\"X-Comment\", v)\n    let q = '\"'; let c = (\"X-C\", v);\n    HeaderValue::from_static(\"Value-Only\");\n    let e = \"has \\\" quote\", (\"X-D\", v);\n}\n#[cfg(test)]\nmod t { (\"X-Test\", v) }\n";
        let got: Vec<_> = header_name_literals(src).into_iter().collect();
        assert_eq!(got, ["X-A", "X-C", "X-D", "x-b"]);
    }

    /// ★ 正文不另拷一份（HTTP 層診斷 10-05）：交給 `write_response_body` 的那塊 `Bytes`
    ///   必須**指著 Probe 讀進來的那塊記憶體**。⚠ 只比內容判不出這件事 —— 拷一份內容也相同。
    #[test]
    fn body_range_指着探测读进来的那块内存_不另拷一份() {
        let d = std::env::temp_dir().join(format!("fulcrum-body-slice-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("f.bin");
        let body: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
        std::fs::write(&p, &body).unwrap();
        // 慢路徑在任何檔案系統上都帶位元組；快路徑帶回來的是同一個欄位。
        let probe = Probe::open_blocking(p.clone(), CHUNK as u64).unwrap();
        let held = probe.bytes_for(&p).expect("4 KiB 在内联界之内");
        for (start, count) in [(0u64, 4096u64), (100, 200), (4095, 1)] {
            let out = body_range(held.clone(), start, count).expect("区间在内");
            let (s, e) = (start as usize, (start + count) as usize);
            assert_eq!(&out[..], &body[s..e], "start={start} count={count}");
            assert_eq!(
                out.as_ptr(),
                held[s..].as_ptr(),
                "start={start} count={count}：另拷了一份"
            );
        }
        assert!(
            body_range(held.clone(), 4000, 97).is_none(),
            "越界必须回 None（头已发出，调用方只能断连）"
        );
        assert!(
            body_range(held.clone(), u64::MAX, 1).is_none(),
            "溢出也是越界"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// ★ ETag 手寫十六進制（10-05 · 構回應頭不走 `fmt`）：必須與舊寫法
    ///   `format!("\"{nanos:x}-{len:x}\"")` **逐位元組相同** —— ETag 變了，等於讓每個用戶端與
    ///   中間快取手上的副本一次性全部失效。
    #[test]
    fn etag_手写十六进制与_format_逐字节相同() {
        let samples: [(u128, u64); 9] = [
            (0, 0),
            (1, 1),
            (15, 16),
            (0x18db_824f_d09e_aa84, 0x1000),
            (1_759_632_556_123_456_789, 4096),
            (0xab_cdef, 0),
            (1 << 64, 255),
            (u128::from(u64::MAX), u64::MAX),
            (u128::MAX, u64::MAX),
        ];
        for (nanos, len) in samples {
            let want = format!("\"{nanos:x}-{len:x}\"");
            let got = etag_value(nanos, len).expect("只有引号、连字符与十六进制");
            assert_eq!(got.as_bytes(), want.as_bytes(), "nanos={nanos} len={len}");
        }
    }

    /// ★ `HeaderValue::from_static` 遇到非法字元會**在請求路徑上 panic**（以前的寫法只是靜默少一個頭）
    ///   ⇒ 餵給它的固定值逐個釘住。mime 表那一半在 `mime.rs` 的同名單測裡。
    #[test]
    fn 喂给_from_static_的固定值都是合法头值() {
        for (algo, _) in SIDECAR_EXT {
            assert!(HeaderValue::from_str(algo).is_ok(), "{algo}");
        }
        for v in [
            "GET, HEAD",
            "0",
            "bytes",
            "Accept-Encoding",
            "text/html; charset=utf-8",
        ] {
            assert!(HeaderValue::from_str(v).is_ok(), "{v}");
        }
    }

    /// ★ `FileReq` 的三條取值口徑（HTTP 層診斷發現 2 · 請求頭不整份克隆）：
    ///   同名頭取**第一個**、`to_str()` 不過的當作沒有、沒帶的就是沒有。
    #[test]
    fn file_req_取第一个同名头_坏值当作没有() {
        let mut req = RequestHeader::build("HEAD", b"/a.txt", None).unwrap();
        req.append_header("Range", "bytes=0-1").unwrap();
        req.append_header("Range", "bytes=5-9").unwrap();
        req.append_header(
            "If-None-Match",
            HeaderValue::from_bytes(b"\"\xff\"").unwrap(),
        )
        .unwrap();
        req.append_header("Accept-Encoding", "gzip, br").unwrap();
        let f = FileReq::from_header(&req);
        assert_eq!(f.method(), "HEAD");
        assert_eq!(f.range(), Some("bytes=0-1"));
        assert_eq!(f.if_none_match(), None);
        assert_eq!(f.accept_encoding(), Some("gzip, br"));
        assert_eq!(f.if_modified_since(), None);
        assert_eq!(f.if_range(), None);
    }

    // ── If-None-Match ────────────────────────────────────────────────────
    #[test]
    fn if_none_match_认星号_列表_与弱前缀() {
        assert!(if_none_match_hit("*", "\"abc\""));
        assert!(if_none_match_hit("\"abc\"", "\"abc\""));
        assert!(if_none_match_hit("\"x\", \"abc\", \"y\"", "\"abc\""));
        assert!(
            if_none_match_hit("W/\"abc\"", "\"abc\""),
            "弱比较：W/ 前缀要脱掉"
        );
        assert!(!if_none_match_hit("\"nope\"", "\"abc\""));
        assert!(!if_none_match_hit("", "\"abc\""));
    }

    // ★ ★ `If-Range` 用**強**比較 —— 與上面那條**有意不同**。
    //   ⚠ 少了這條判據，兩處很容易被"統一"成同一個 helper，
    //   而統一之後弱 ETag 會放行 Range，用戶端拼出一個兩個版本混在一起的檔案。
    #[test]
    fn if_range_用强比较_弱_etag_一律不匹配() {
        let e = Some("\"abc\"");
        assert!(if_range_matches("\"abc\"", e, None));
        assert!(
            !if_range_matches("W/\"abc\"", e, None),
            "弱 ETag 不该放行 Range"
        );
        assert!(!if_range_matches("\"other\"", e, None));
    }

    #[test]
    fn if_range_也认日期() {
        let t = UNIX_EPOCH + std::time::Duration::from_secs(784_111_777);
        assert!(if_range_matches(
            "Sun, 06 Nov 1994 08:49:37 GMT",
            None,
            Some(t)
        ));
        assert!(!if_range_matches(
            "Sun, 06 Nov 1994 08:49:38 GMT",
            None,
            Some(t)
        ));
        assert!(!if_range_matches("not a date", None, Some(t)));
    }

    // ── 目錄列表 ──────────────────────────────────────────────────────────
    //
    // ★ ★ 目錄列表把**檔案名**放進 HTML —— 而檔案名是檔案系統上的任意位元組。
    //   一個叫 `<script>x</script>` 的檔案在沒有轉義的列表裡就是一次儲存型 XSS。
    #[test]
    fn 目录列表把文件名转义掉() {
        let html = render_listing("/", &[("<script>alert(1)</script>".into(), false)]);
        assert!(
            !html.contains("<script>alert(1)"),
            "原样的 script 标签跑出去了：{html}"
        );
        assert!(html.contains("&lt;script&gt;"), "正文该 HTML 转义");
        assert!(html.contains("%3Cscript%3E"), "href 该百分号编码");
    }

    #[test]
    fn 目录列表里引号也转义() {
        let html = render_listing("/", &[("a\"b'c&d".into(), false)]);
        assert!(html.contains("&quot;") && html.contains("&#39;") && html.contains("&amp;"));
        assert!(!html.contains("a\"b"), "裸引号会把 href 提前收尾");
    }

    #[test]
    fn 目录列表给目录加尾斜杠_且根目录不给上级链接() {
        let html = render_listing("/x/", &[("sub".into(), true), ("f.txt".into(), false)]);
        assert!(html.contains("href=\"sub/\">sub/</a>"));
        assert!(html.contains("href=\"f.txt\">f.txt</a>"));
        assert!(html.contains("href=\"../\""), "非根目录要有上级链接");

        let root = render_listing("/", &[]);
        assert!(!root.contains("href=\"../\""), "根目录不该给上级链接");
    }

    // ⚠ 中文名在 href 裡要編碼、在正文裡原樣（HTML 轉義不動它）。
    #[test]
    fn 中文文件名两种转义各做各的() {
        let html = render_listing("/", &[("中文.txt".into(), false)]);
        assert!(html.contains(">中文.txt<"), "正文该是原样的中文");
        assert!(
            html.contains("href=\"%E4%B8%AD%E6%96%87.txt\""),
            "href 该百分号编码"
        );
    }
}
