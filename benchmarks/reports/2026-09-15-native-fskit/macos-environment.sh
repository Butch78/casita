export PATH=/nix/store/lr0yffvmpdb5j9nd2v8cq5n73414nshs-rust-stable-1.96.0-1.96.0/bin:/nix/store/qndx79izh6lanm535xj9pbayy6j8fz5n-clang-wrapper-21.1.8/bin:/nix/store/5pbjszdbx14nwsssfjq95sn52djgjg61-python3-3.14.7/bin:/usr/bin:/bin:/usr/sbin:/sbin
export SDKROOT=/nix/store/ljwd86bmp2yacq14lyr36k016nr20nik-apple-sdk-26.4/Platforms/MacOSX.platform/Developer/SDKs/MacOSX26.4.sdk
export CC=/nix/store/qndx79izh6lanm535xj9pbayy6j8fz5n-clang-wrapper-21.1.8/bin/clang
export CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/nix/store/qndx79izh6lanm535xj9pbayy6j8fz5n-clang-wrapper-21.1.8/bin/clang
export CARGO_BUILD_JOBS=4

export RUSTFLAGS="-C link-arg=-isysroot -C link-arg=$SDKROOT"

export RUSTFLAGS="-C link-arg=-isysroot -C link-arg=$SDKROOT -L native=/nix/store/0iavzb323r97k7sps762r5kj1f2bny0b-libiconv-115.100.1/lib"

export RUSTFLAGS="$RUSTFLAGS -C link-arg=-Wl,-dead_strip_dylibs"
