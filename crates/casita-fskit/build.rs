fn main() {
    // Make Cargo not re-run the build-script unnecessarily.
    println!("cargo:rerun-if-changed=build.rs");

    let extension_bin = "casita-native-fskit-extension";

    // Nix toolchains can add unused external dylibs (notably libiconv), which
    // hardened-runtime library validation rejects when the app launches.
    // Keep this in the build so packaging does not depend on caller RUSTFLAGS.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        for binary in ["casita-native-fskit", extension_bin] {
            println!("cargo:rustc-link-arg-bin={binary}=-Wl,-dead_strip_dylibs");
        }
    }

    // Pass `-e _NSExtensionMain` to change the entry point for the extension.
    //
    // Combined with `#![no_main]`, this makes our binary an app extension.
    println!("cargo:rustc-link-arg-bin={extension_bin}=-e");
    println!("cargo:rustc-link-arg-bin={extension_bin}=_NSExtensionMain");

    // Configure the linker to give earlier diagnostics if linking dylibs that
    // aren't supported in application extensions. There currently aren't any
    // public frameworks / libraries where this is the case, so this doesn't
    // matter that much, but there might be in the future.
    println!("cargo:rustc-link-arg-bin={extension_bin}=-Wl,-application_extension");
    // objc2 resolves framework classes dynamically. Keep FSKit loaded even
    // when dead-strip-dylibs removes libraries with no static symbol references.
    println!("cargo:rustc-link-arg-bin={extension_bin}=-Wl,-u,_OBJC_CLASS_$_FSUnaryFileSystem");
}
