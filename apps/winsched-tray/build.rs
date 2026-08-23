use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=../../assets/tray/winsched.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("../../assets/tray/winsched.ico")
            .set("ProductName", "WinSched Tray")
            .set("FileDescription", "WinSched service tray controller")
            .set("LegalCopyright", "Copyright (c) WinSched contributors");
        resource.compile()?;
    }
    Ok(())
}
