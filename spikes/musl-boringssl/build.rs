//! 只做一件事：把**编译时**的目标三元组烧进二进制。
//!
//! ★ 探针会把它印出来，而这不是装饰：交叉编译时「我以为在编 aarch64」与
//!   「实际编的是 x86_64」在 `cargo build` 的输出里长得一样，而产物跑起来
//!   也一样对 —— 因为跑的根本是另一个架构的那份。
//!   ⇒ 让二进制自己说出它是给谁编的，`probe.sh` 再拿它跟 `file(1)` 读出来的对一遍。
fn main() {
    let target = std::env::var("TARGET").expect("cargo 一定会给 TARGET");
    println!("cargo:rustc-env=PROBE_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
