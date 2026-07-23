fn main() {
    println!("cargo:rerun-if-changed=ffi/minimp3.c");
    println!("cargo:rerun-if-changed=ffi/minimp3.h");
    cc::Build::new()
        .include("ffi")
        .define("MINIMP3_IMPLEMENTATION", None)
        .define("MINIMP3_ONLY_MP3", None)
        .flag("-fno-stack-protector")
        .flag("-U_FORTIFY_SOURCE")
        .flag("-ffreestanding")
        .file("ffi/minimp3.c")
        .compile("minimp3");
}
