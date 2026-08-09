//! Embeds the application icon into the Windows .exe resource table.
//! On other platforms this build script does nothing.

fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/icon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        // A missing resource compiler must not stop the build. The program
        // still gets its window icon from assets/icon-64.rgba.
        if let Err(err) = res.compile() {
            println!("cargo:warning=could not embed the .exe icon: {err}");
        }
    }
}
