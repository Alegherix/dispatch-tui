fn main() {
    // Any file added or changed under plugin/ triggers a recompile.
    println!("cargo:rerun-if-changed=plugin");
}
