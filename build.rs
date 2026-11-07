fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/swiftsim.ico");
    // Embeds the icon and version info in swiftsim.exe. Needs a Windows host (the SDK's rc.exe),
    // so Linux cross-checks skip it; winresource is a Windows-only build-dependency.
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        // FileDescription is the name Windows shows in "Open with" and Task Manager.
        res.set_icon("assets/swiftsim.ico").set("FileDescription", "SwiftSim").set("ProductName", "SwiftSim");
        res.compile().expect("embedding Windows resources");
    }
}
