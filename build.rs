//! Compiles the window's Slint markup (`src/gui/ui/`) into Rust when the `gui` feature is on.

fn main() {
    #[cfg(feature = "gui")]
    gui();
}

#[cfg(feature = "gui")]
fn gui() {
    // Debug info lets tests find elements by accessible label and read their positions, in debug and release builds
    // alike; it adds element names to the binary, nothing else.
    let config = slint_build::CompilerConfiguration::new().with_debug_info(true);
    if let Err(e) = slint_build::compile_with_config("src/gui/ui/app.slint", config) {
        println!("cargo::error=could not compile the window's Slint markup: {e}");
    }
}
