//! 建置身分：讓 `fulcrum_build_info{version}` 區分得出兩次建置（`G141`）。
//!
//! ⛔ ★ **這裡產出的是「跑的是哪一次建置」，不是「這個版本承諾了什麼」。**
//!   兩件事共用一個字串會讓人把前者讀成後者，所以取值用 semver 的**建置中繼資料**
//!   語法（`+` 之後那一段）—— 按 semver 自己的規矩它**不參與版本先後比較**，
//!   語法本身就在宣告「這是身分，不是承諾」。
//!   分類法與承諾的起點見 `docs/governance/compatibility.md`。
//!
//! 取值鏈，從左到右第一個取得到的贏：
//!
//!   ① `FULCRUM_BUILD_ID` 環境變數 —— 打包與發佈由建置環境喂真值；
//!   ② 直接讀 `.git` 推出提交號 —— 日常開發與門禁；
//!   ③ 顯式 `unknown`。
//!
//! ⛔ ★ ★ **第三檔必須顯式寫成 `unknown`，不許悄悄退回裸的 `0.0.0`。**
//!   「讀不到」與「讀到了 0.0.0」是兩件事，而後者會讓人以為自己看到了一個真實的
//!   版本號 —— 與本儲存庫「沒能檢查不算檢查通過」是同一條紀律。
//!
//! ⚠ ★ **② 這一檔有意不調 `git` 這個二進位，而是直接讀檔案**，兩個理由都是實測出來的形狀：
//!   · 容器裡 `git` 對 bind-mount 進來的樹常報 `dubious ownership` 而**整條失敗**，
//!     那會讓每一次門禁建置都靜默落到第 ③ 檔；
//!   · `docker/Dockerfile.musl-product` 的上下文是儲存庫根，而 `.dockerignore` 排掉了
//!     `.git/` ⇒ **那一趟根本沒有 `.git` 可讀**，只能靠第 ① 檔喂進來。
//!
//! ⚠ **本檔不判「工作樹髒不髒」**：同一個提交上帶著不同未提交改動的兩次建置，
//!   報出來的身分相同。判髒要麼調 `git`（見上，不可靠），要麼把整棵樹雜湊一遍（太貴）。
//!   ⇒ 代價寫在明處：**發佈物一律走第 ① 檔**，那一檔由建置環境負責說準。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    // ⚠ 一旦列印了任何一條 `rerun-if-changed`，cargo 就**不再**使用「包內任何檔案變了就重跑」
    //   那個預設值。這裡是有意的：這個指令碼的產物只依賴「HEAD 指向哪」與那個環境變數，
    //   原始碼改動本來就會走正常的重編譯路徑。
    println!("cargo:rerun-if-env-changed=FULCRUM_BUILD_ID");

    let pkg = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());

    let version = match env::var("FULCRUM_BUILD_ID") {
        Ok(id) if !id.trim().is_empty() => id.trim().to_string(),
        _ => match git_commit() {
            Some(sha) => format!("{pkg}+g{sha}"),
            None => format!("{pkg}+unknown"),
        },
    };

    println!("cargo:rustc-env=FULCRUM_BUILD_VERSION={version}");
}

/// 從 `.git` 裡推出當前提交號的前 12 位。取不到就 `None` —— ⛔ 不猜。
fn git_commit() -> Option<String> {
    let git_dir = find_git_dir()?;

    let head_path = git_dir.join("HEAD");
    println!("cargo:rerun-if-changed={}", head_path.display());
    let head = fs::read_to_string(&head_path).ok()?;
    let head = head.trim();

    // 分離頭指針：HEAD 裡直接就是提交號。
    let Some(reference) = head.strip_prefix("ref: ") else {
        return short(head);
    };

    // 常規情形：HEAD 指向一個 ref 檔案。
    let ref_path = git_dir.join(reference);
    if ref_path.is_file() {
        println!("cargo:rerun-if-changed={}", ref_path.display());
        return short(fs::read_to_string(&ref_path).ok()?.trim());
    }

    // ⚠ ref 檔案不存在**不等於**儲存庫壞了：`git gc` 之後它會被折進 `packed-refs`。
    //   不看這一檔的話，一個剛被 gc 過的儲存庫會靜默落到第 ③ 檔。
    let packed = git_dir.join("packed-refs");
    println!("cargo:rerun-if-changed={}", packed.display());
    let packed = fs::read_to_string(&packed).ok()?;
    packed
        .lines()
        .filter(|line| !line.starts_with('#') && !line.starts_with('^'))
        .find_map(|line| {
            let (sha, name) = line.split_once(' ')?;
            (name.trim() == reference).then_some(sha)
        })
        .and_then(short)
}

/// 從 crate 目錄往上找 `.git`。
///
/// ⚠ `.git` **可能是檔案而不是目錄** —— git worktree 與 submodule 都是那樣，
///   內容是一行 `gitdir: <路径>`。本儲存庫用 worktree（`tests/lib/vol-lock.sh` 就是按
///   一棵樹一個卷設計的）⇒ 這一檔不是假想情形。
fn find_git_dir() -> Option<PathBuf> {
    let manifest = env::var("CARGO_MANIFEST_DIR").ok()?;
    let mut dir: Option<&Path> = Some(Path::new(&manifest));

    while let Some(here) = dir {
        let candidate = here.join(".git");

        if candidate.is_dir() {
            return Some(candidate);
        }

        if candidate.is_file() {
            let pointer = fs::read_to_string(&candidate).ok()?;
            let target = pointer.trim().strip_prefix("gitdir: ")?.trim();
            let target = Path::new(target);
            // `gitdir:` 裡既可能是絕對路徑，也可能是相對於 `.git` 所在目錄的相對路徑。
            return Some(if target.is_absolute() {
                target.to_path_buf()
            } else {
                here.join(target)
            });
        }

        dir = here.parent();
    }

    None
}

/// 提交號截到 12 位；⚠ 只接受十六進制，⛔ 免得把「讀到了一行別的東西」當成提交號。
fn short(sha: &str) -> Option<String> {
    let sha = sha.trim();
    (sha.len() >= 12 && sha.chars().all(|c| c.is_ascii_hexdigit())).then(|| sha[..12].to_string())
}
