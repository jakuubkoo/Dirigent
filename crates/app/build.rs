//! Windows: embeds the exe icon (`dirigent.rc`). Nothing on other targets.

fn main() {
    println!("cargo:rerun-if-changed=dirigent.rc");
    println!("cargo:rerun-if-changed=assets/icon/dirigent.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("dirigent.rc", embed_resource::NONE).manifest_optional().unwrap();
    }
}
