//! 构建身份：让 `fulcrum_build_info{version}` 区分得出两次构建（`G141`）。
//!
//! ⛔ ★ **这里产出的是「跑的是哪一次构建」，不是「这个版本承诺了什么」。**
//!   两件事共用一个字符串会让人把前者读成后者，所以取值用 semver 的**构建元数据**
//!   语法（`+` 之后那一段）—— 按 semver 自己的规矩它**不参与版本先后比较**，
//!   语法本身就在声明「这是身份，不是承诺」。
//!   分类法与承诺的起点见 `docs/governance/compatibility.md`。
//!
//! 取值链，从左到右第一个取得到的赢：
//!
//!   ① `FULCRUM_BUILD_ID` 环境变量 —— 打包与发布由构建环境喂真值；
//!   ② 直接读 `.git` 推出提交号 —— 日常开发与门禁；
//!   ③ 显式 `unknown`。
//!
//! ⛔ ★ ★ **第三档必须显式写成 `unknown`，不许悄悄退回裸的 `0.0.0`。**
//!   「读不到」与「读到了 0.0.0」是两件事，而后者会让人以为自己看到了一个真实的
//!   版本号 —— 与本仓「没能检查不算检查通过」是同一条纪律。
//!
//! ⚠ ★ **② 这一档有意不调 `git` 这个二进制，而是直接读文件**，两个理由都是实测出来的形状：
//!   · 容器里 `git` 对 bind-mount 进来的树常报 `dubious ownership` 而**整条失败**，
//!     那会让每一次门禁构建都静默落到第 ③ 档；
//!   · `docker/Dockerfile.musl-product` 的上下文是仓库根，而 `.dockerignore` 排掉了
//!     `.git/` ⇒ **那一趟根本没有 `.git` 可读**，只能靠第 ① 档喂进来。
//!
//! ⚠ **本档不判「工作树脏不脏」**：同一个提交上带着不同未提交改动的两次构建，
//!   报出来的身份相同。判脏要么调 `git`（见上，不可靠），要么把整棵树哈希一遍（太贵）。
//!   ⇒ 代价写在明处：**发布物一律走第 ① 档**，那一档由构建环境负责说准。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    // ⚠ 一旦打印了任何一条 `rerun-if-changed`，cargo 就**不再**使用「包内任何文件变了就重跑」
    //   那个默认值。这里是有意的：这个脚本的产物只依赖「HEAD 指向哪」与那个环境变量，
    //   源码改动本来就会走正常的重编译路径。
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

/// 从 `.git` 里推出当前提交号的前 12 位。取不到就 `None` —— ⛔ 不猜。
fn git_commit() -> Option<String> {
    let git_dir = find_git_dir()?;

    let head_path = git_dir.join("HEAD");
    println!("cargo:rerun-if-changed={}", head_path.display());
    let head = fs::read_to_string(&head_path).ok()?;
    let head = head.trim();

    // 分离头指针：HEAD 里直接就是提交号。
    let Some(reference) = head.strip_prefix("ref: ") else {
        return short(head);
    };

    // 常规情形：HEAD 指向一个 ref 文件。
    let ref_path = git_dir.join(reference);
    if ref_path.is_file() {
        println!("cargo:rerun-if-changed={}", ref_path.display());
        return short(fs::read_to_string(&ref_path).ok()?.trim());
    }

    // ⚠ ref 文件不存在**不等于**仓库坏了：`git gc` 之后它会被折进 `packed-refs`。
    //   不看这一档的话，一个刚被 gc 过的仓库会静默落到第 ③ 档。
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

/// 从 crate 目录往上找 `.git`。
///
/// ⚠ `.git` **可能是文件而不是目录** —— git worktree 与 submodule 都是那样，
///   内容是一行 `gitdir: <路径>`。本仓用 worktree（`tests/lib/vol-lock.sh` 就是按
///   一棵树一个卷设计的）⇒ 这一档不是假想情形。
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
            // `gitdir:` 里既可能是绝对路径，也可能是相对于 `.git` 所在目录的相对路径。
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

/// 提交号截到 12 位；⚠ 只接受十六进制，⛔ 免得把「读到了一行别的东西」当成提交号。
fn short(sha: &str) -> Option<String> {
    let sha = sha.trim();
    (sha.len() >= 12 && sha.chars().all(|c| c.is_ascii_hexdigit())).then(|| sha[..12].to_string())
}
