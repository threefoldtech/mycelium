// TEMPORARY BUILD SCRIPT FOR SCTP LINKING
fn main() {
    println!("cargo::rustc-link-search=/usr/lib");
    println!("cargo::rustc-link-lib=sctp");
}
