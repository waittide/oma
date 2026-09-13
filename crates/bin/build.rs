//! 构建准备：保证 `web/dist` 目录存在，使 `#[derive(Embed)]` 始终有合法目录可嵌入。
//!
//! 前端是独立工程（pnpm），本脚本**不会**替你构建前端：交叉编译或离线环境下
//! 拉 node 依赖既慢又易失败，因此只负责把目录补齐，缺资产时由运行时显式报错。
//!
//! 目录不存在时 `rust-embed` 会直接编译失败，且报错信息与前端毫无关联；
//! 这里建一个空目录让编译通过，把「缺资产」推迟到 `oma web` 启动时给出可操作的提示。

use std::{env, fs, path::PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set"));
    // crates/bin -> 仓库根 -> web/dist
    let web_dist = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|root| root.join("web").join("dist"))
        .expect("crates/bin always has a grandparent directory");

    if !web_dist.exists() {
        fs::create_dir_all(&web_dist).expect("failed to create web/dist");
        println!(
            "cargo:warning=web/dist 不存在，已创建空目录。请先在 web/ 下执行 `pnpm build`，\
             否则运行 `oma web` 会提示缺少前端资产。"
        );
    }

    // 通过环境变量把路径传给 rust-embed 的 `#[folder = "$OMA_WEB_DIST"]`
    println!("cargo:rustc-env=OMA_WEB_DIST={}", web_dist.display());
    // 资产变化后需要重新编译二进制：release 下内容会被真正内嵌
    println!("cargo:rerun-if-changed={}", web_dist.display());
    println!("cargo:rerun-if-changed=build.rs");
}
