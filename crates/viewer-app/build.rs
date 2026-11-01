//! Embeds the icon resource in the Windows executable. Explorer, shortcuts, the window and the taskbar use it.

fn main() {
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=assets/app.ico");
    embed_resource::compile("app.rc", embed_resource::NONE)
        .manifest_required()
        .expect("compile icon resource");
}
