fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let name = std::env::var("CARGO_PKG_NAME").expect("cargo sets the package name");
        winresource::WindowsResource::new()
            .set_icon("../../assets/icon.ico")
            .set("ProductName", &name)
            .set("FileDescription", "agent rules, intent, skills and memory")
            .set("OriginalFilename", &format!("{name}.exe"))
            .compile()?;
    }
    Ok(())
}
