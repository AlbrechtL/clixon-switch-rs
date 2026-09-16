// A backend plugin is dlopen()ed by clixon_backend, which has these libraries
// loaded already. Linking them anyway records them as NEEDED, so that the
// plugin's dependencies show up in readelf and in Yocto's shlib dependencies.
//
// Cargo finds them through the linker's default search path: the Yocto
// sysroot, /usr/local/lib in the dev container, or CLIXON_LIB_DIR.
fn main() {
    println!("cargo:rerun-if-env-changed=CLIXON_LIB_DIR");
    if let Ok(dir) = std::env::var("CLIXON_LIB_DIR") {
        println!("cargo:rustc-link-search=native={dir}");
    }
    for lib in ["clixon_backend", "clixon", "cligen"] {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
}
