# Baby Rocs

This is a baby monitor. It runs on a computer in child's bedroom, captures sound from a microphone, detects voice activity, and streams it towards a roc-toolkit endpoint.

It uses WebRTC audio processing subpart for voice activity detection (VAD) and basic noise subtraction.

There is no video planned. Potentially some more advanced denoising could be embedded.

## Cross building for armhf

Prerequisite: 
You need to build roc-toolkit for armhf first and have somewhere. 

Then on the host:

1. Add .cargo and .cargo/config.toml
```
$ mkdir -p .cargo
$ cat > .cargo/config.toml << 'EOF'
[env]

[source.crates-io]
replace-with = "vendored-sources"

[source."git+https://codeberg.org/Misha-Baranov/roc-rs.git?branch=dev%2Fmisha"]
git = "https://codeberg.org/Misha-Baranov/roc-rs.git"
branch = "dev/misha"
replace-with = "vendored-sources"

[source.vendored-sources]
directory = ".cargo/vendor"
EOF

$ CARGO_HOME=./.cargo cargo vendor .cargo/vendor
```


2. Start a container

```
$ docker build -t baby-rocs-armhf -f ci/dockerfiles/Dockerfile.armhf .
$ docker run -it --rm -v $(pwd):/build  -v $(realpath ../roc-toolkit/install_v0.4.0_zero2w):/roc baby-rocs-armhf bash

# In the container:
$ meson setup build_docker 3rdparty/webrtc-audio-processing --prefix=/build/build_docker/install
$ meson compile -C build_docker
$ meson install -C build_docker
$ PKG_CONFIG_PATH=/build/build_docker/install/lib/arm-linux-gnueabihf/pkgconfig:/roc/lib/pkgconfig   CXXFLAGS='-I/build/build_docker/install/include'   BINDGEN_EXTRA_CLANG_ARGS='-I/build/build_docker/install/include' CARGO_HOME=./.cargo   cargo build --frozen
```



