fn main() {
    // Debug info lets tests find elements by accessible label; `POSTVAK_PROTO_NO_DEBUG_INFO=1` leaves it out, for
    // measuring the binary as it would ship.
    println!("cargo:rerun-if-env-changed=POSTVAK_PROTO_NO_DEBUG_INFO");
    let debug_info = std::env::var_os("POSTVAK_PROTO_NO_DEBUG_INFO").is_none();
    let config = slint_build::CompilerConfiguration::new().with_debug_info(debug_info);
    if let Err(e) = slint_build::compile_with_config("ui/mail.slint", config) {
        eprintln!("could not compile ui/mail.slint: {e}");
        std::process::exit(1);
    }
}
