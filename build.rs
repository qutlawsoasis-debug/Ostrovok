//! Вшивает в программу сведения о ней: название, версию, значок и манифест.
//! Без них Windows и антивирусы видят безымянный файл и относятся к нему с подозрением.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/ostrovok.ico")
        .set_language(0x0419) // русский
        .set("ProductName", "Островок")
        .set("FileDescription", "Островок — панель-остров для Windows")
        .set("CompanyName", "qutlawsoasis-debug")
        .set("LegalCopyright", "© 2026 qutlawsoasis-debug. Лицензия MIT")
        .set("OriginalFilename", "Ostrovok.exe")
        .set("InternalName", "ostrovok")
        .set("Comments", "Открытый исходный код: https://github.com/qutlawsoasis-debug/Ostrovok")
        // манифест — только латиницей: с кириллицей внутри Windows отказывается запускать программу (ошибка 14001)
        .set_manifest(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="qutlawsoasis-debug.Ostrovok" version="1.0.0.0" processorArchitecture="amd64"/>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
    </windowsSettings>
  </application>
</assembly>"#,
        );
    if let Err(e) = res.compile() {
        // без сведений программа всё равно соберётся — просто сообщим
        println!("cargo:warning=сведения о программе не вшиты: {e}");
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/ostrovok.ico");
}
