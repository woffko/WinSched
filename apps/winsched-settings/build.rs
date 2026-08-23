use std::error::Error;

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="requireAdministrator" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{4f476546-9373-4f25-84da-75e4c4896837}" />
    </application>
  </compatibility>
</assembly>"#;

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=../../assets/tray/winsched.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("../../assets/tray/winsched.ico")
            .set_manifest(MANIFEST)
            .set("ProductName", "WinSched Settings")
            .set("FileDescription", "WinSched configuration editor")
            .set("LegalCopyright", "Copyright (c) WinSched contributors");
        resource.compile()?;
    }
    Ok(())
}
