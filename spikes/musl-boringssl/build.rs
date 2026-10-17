//! 只做一件事：把**編譯時**的目標三元組燒進二進位。
//!
//! ★ 探針會把它印出來，而這不是裝飾：交叉編譯時「我以為在編 aarch64」與
//!   「實際編的是 x86_64」在 `cargo build` 的輸出裡長得一樣，而產物跑起來
//!   也一樣對 —— 因為跑的根本是另一個架構的那份。
//!   ⇒ 讓二進位自己說出它是給誰編的，`probe.sh` 再拿它跟 `file(1)` 讀出來的對一遍。
fn main() {
    let target = std::env::var("TARGET").expect("cargo 一定会给 TARGET");
    println!("cargo:rustc-env=PROBE_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
