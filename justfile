cross_toolchain := "/home/misha/coding/crosstools/arm-gnu-toolchain-14.3.rel1-x86_64-arm-none-linux-gnueabihf"
cross_sysroot := cross_toolchain / "arm-none-linux-gnueabihf"
cross_cc := cross_toolchain / "bin/arm-none-linux-gnueabihf-gcc"
cross_cxx := cross_toolchain / "bin/arm-none-linux-gnueabihf-g++"
cross_target := "armv7-unknown-linux-gnueabihf"
roc_prefix_armhf := "/home/misha/coding/roc-toolkit/install_v0.4.0_zero2w"
roc_prefix_x86 := "/home/misha/coding/roc-toolkit/install_v0.4.0"
cargo_home := f"{{ justfile_directory() }}/.cargo"

# Build for current host
run *args:
    PKG_CONFIG_PATH="{{justfile_directory()}}/build/install/lib/x86_64-linux-gnu/pkgconfig:{{ roc_prefix_x86 }}/lib/pkgconfig" \
    CXXFLAGS="-I{{justfile_directory()}}/build/install/include" \
    BINDGEN_EXTRA_CLANG_ARGS="-I{{justfile_directory()}}/build/install/include" \
    LD_LIBRARY_PATH="{{justfile_directory()}}/build/install/lib/x86_64-linux-gnu:/home/misha/coding/roc-toolkit/install_v0.4.0/lib" \
    CARGO_HOME="{{ cargo_home }}" \
    cargo run --bin baby_rocs -- {{args}}
  
build *args:
    PKG_CONFIG_PATH="{{justfile_directory()}}/build/install/lib/x86_64-linux-gnu/pkgconfig:{{ roc_prefix_x86 }}/lib/pkgconfig" \
    CXXFLAGS="-I{{justfile_directory()}}/build/install/include" \
    BINDGEN_EXTRA_CLANG_ARGS="-I{{justfile_directory()}}/build/install/include" \
    LD_LIBRARY_PATH="{{justfile_directory()}}/build/install/lib/x86_64-linux-gnu:/home/misha/coding/roc-toolkit/install_v0.4.0/lib" \
    CARGO_HOME="{{ cargo_home }}" \
    cargo build {{args}}


build-aarch64 *args:
    docker run --rm -it -v $(pwd):/build -v /home/misha/coding/baby_rocs/../roc-toolkit/install_v0.4.0_aarch64:/roc baby-rocs-aarch64 bash -c "\
    PKG_CONFIG_PATH=/build/build_docker_aarch64/install/lib/aarch64-linux-gnu/pkgconfig:/roc/lib/pkgconfig:/build/build_docker_aarch64/install/libwebrtc-audio-processing-2ib/arm-linux-gnueabihf/pkgconfig/ \
    CXXFLAGS='-I/build/build_docker_aarch64/install/include' \
    BINDGEN_EXTRA_CLANG_ARGS='-I/build/build_docker_aarch64/install/include' \
    CARGO_HOME=\".cargo\" \
    CARGO_TARGET_DIR=\"target_aarch64\" \
    RUSTFLAGS=\"-C target-cpu=cortex-a53 -C target-feature=+neon\" \
    cargo build {{args}}"

# Cross-build for armhf (Raspberry Pi Zero and similar devices)
build-armhf *args:
    PKG_CONFIG_ALLOW_CROSS=1 \
    PKG_CONFIG_PATH="{{justfile_directory()}}/build_armhf/install/lib/pkgconfig:{{ roc_prefix_armhf }}/lib/pkgconfig" \
    CC_armv7_unknown_linux_gnueabihf="{{cross_cc}}" \
    CXX_armv7_unknown_linux_gnueabihf="{{cross_cxx}}" \
    CFLAGS_armv7_unknown_linux_gnueabihf="--sysroot={{cross_sysroot}}/libc" \
    CXXFLAGS_armv7_unknown_linux_gnueabihf="-I{{justfile_directory()}}/build_armhf/install/include" \
    BINDGEN_EXTRA_CLANG_ARGS_armv7_unknown_linux_gnueabihf="--target={{cross_target}} --sysroot={{cross_sysroot}}/libc -isystem {{cross_sysroot}}/include/c++/14.3.1 -isystem {{cross_sysroot}}/include/c++/14.3.1/arm-none-linux-gnueabihf -I{{justfile_directory()}}/build_armhf/install/include" \
    cargo build --target {{cross_target}} {{args}}

vendor:
    CARGO_HOME="{{ cargo_home }}" \
    cargo vendor "{{ cargo_home }}/vendor"


    #docker run --rm -v $(pwd):/build  -v $(pwd)/../roc-toolkit/install_v0.4.0_zero2w:/roc baby-rocs-armhf bash -c "\
    #cd /build && \
    #meson setup build_docker 3rdparty/webrtc-audio-processing --prefix=/build/build_docker/install && \
    #meson compile -C build_docker && \
    #meson install -C build_docker && \
    #PKG_CONFIG_PATH=/build/build_docker/install/lib/pkgconfig:/roc/lib/pkgconfig:/build/build_docker/install/lib/arm-linux-gnueabihf/pkgconfig/ \
    #CXXFLAGS='-I/build/build_docker/install/include' \
    #BINDGEN_EXTRA_CLANG_ARGS='-I/build/build_docker/install/include' \
    #cargo build"CARGO_HOME="{{ cargo_home }}" \

    # docker run --rm -v $(pwd):/build  -v $(pwd)/../roc-toolkit/install_v0.4.0_aarch64:/roc baby-rocs-aarch64 bash -c "\
    #cd /build && \
    #meson setup build_docker_aarch64 3rdparty/webrtc-audio-processing --prefix=/build/build_docker_aarch64/install && \
    #meson compile -C build_docker_aarch64 && \
    #meson install -C build_docker_aarch64  && \
    #PKG_CONFIG_PATH=/build/build_docker_aarch64/install/lib/aarch64-linux-gnu/pkgconfig:/roc/lib/pkgconfig:/build/build_docker_aarch64/install/lib/arm-linux-gnueabihf/pkgconfig/ \
    #CXXFLAGS='-I/build/build_docker_aarch64/install/include' \
    #BINDGEN_EXTRA_CLANG_ARGS='-I/build/build_docker_aarch64/install/include' \
    #CARGO_HOME=".cargo" \
    #cargo build --profile release

# Build webrtc-audio-processing for armhf via meson
build-webrtc-armhf:
    pushd 3rdparty/webrtc-audio-processing && \
    meson setup ../../build_armhf --wipe --reconfigure --cross-file=../crossbuild_armhf.ini --prefix={{justfile_directory()}}/build_armhf/install && \
    popd
    meson compile -C build_armhf
    meson install -C build_armhf

build-webrtc-aarch64:
    docker run --rm -it -v $(pwd):/build -v /home/misha/coding/baby_rocs/../roc-toolkit/install_v0.4.0_aarch64:/roc baby-rocs-aarch64 bash -c "\
      cd /build && \
      meson setup build_docker_aarch64 3rdparty/webrtc-audio-processing --prefix=/build/build_docker_aarch64/install && \
      meson compile -C build_docker_aarch64 && \
      meson install -C build_docker_aarch64"

build-webrtc:
    pushd 3rdparty/webrtc-audio-processing && \
    meson setup ../../build --wipe --reconfigure --prefix={{justfile_directory()}}/build/install && \
    popd
    meson compile -C build
    meson install -C build

# Clean build artifacts
clean:
    cargo clean

# Cross-build release for armhf
release-armhf: (build-armhf "--release")

deploy_gateway := "pi@10.10.10.3"
deploy_host := "pi@192.168.1.211"
deploy_dir := "/home/pi/baby-rocs"

# Deploy binary, models and webrtc lib to the target host.
# If `amihome` succeeds, connect directly; otherwise tunnel through the gateway.
deploy_aarch64:
    #!/usr/bin/env bash
    set -euo pipefail
    ssid=$(nmcli -t -f name connection show --active | head -1) 
    amihome=$([[ $ssid == "mkh" ]] || [[ $ssid == "mkh5G" ]])
    if [[ $ssid == "mkh" ]] || [[ $ssid == "mkh5G" ]]; then
        jump=()
    else
        jump=(-J {{deploy_gateway}})
    fi
    ssh "${jump[@]}" {{deploy_host}} "mkdir -p {{deploy_dir}}"
    scp "${jump[@]}" config.json {{deploy_host}}:{{deploy_dir}}/
    scp "${jump[@]}" target/release/baby_rocs {{deploy_host}}:{{deploy_dir}}/
    scp "${jump[@]}" -r 3rdparty/DeepFilterNer/models {{deploy_host}}:{{deploy_dir}}/
    scp "${jump[@]}" build_docker_aarch64/install/lib/aarch64-linux-gnu/libwebrtc-audio-processing-2.so* {{deploy_host}}:{{deploy_dir}}/
