//! hyoui CLI placeholder.
//!
//! The full subcommand surface (`run`, `agent`, `observe`, ...) lands in
//! stage 3-5. For stage 2 the binary just confirms that the workspace links
//! against the `hyoui` library crate.

#![forbid(unsafe_code)]

fn main() {
    // Reference a public item from the library so cargo wires the dep.
    let _ = hyoui::VERSION;
    println!("hyoui (placeholder, sys layer only)");
}
