## Building `mmserver`

The following are required to build the server and its dependencies:

```
rust (MSRV 1.77.2)
nasm
cmake
protoc
libxkbcommon
```

Besides rust, the following command will install everything on ubuntu:

```
apt install nasm cmake protobuf-compiler libxkbcommon-dev
```

Then you should be good to go:

```
cd mm-server
cargo build --bin mmserver [--release]
```

### Feature flags

The following feature flags are available:

 - `vulkan_encode` (on by default) - enables hardware encode
 - `svt_encode` (on by default) - enables svt-av1 and svt-hevc for CPU encode
 - `ffmpeg_encode` - allows using system-installed ffmpeg to do CPU encode

Note that `ffmpeg_encode` takes precedence over `svt_encode` if enabled, but the server will always choose hardware encode if available on your platform.

## Building `mmclient`

The following are required to build the client and its dependencies:

```
rust (MSRV 1.77.2)
nasm
cmake
protoc
libxkbcommon (only linux)
libwayland-client (only linux)
alsa (only linux)
ffmpeg 6.x
```

Besides rust, the following command will install everything on ubuntu:

```
apt install \
    nasm cmake protobuf-compiler libxkbcommon-dev libwayland-dev libasound2-dev \
    ffmpeg libavutil-dev libavformat-dev libavdevice-dev libavfilter-dev
```

Or using homebrew on macOS:

```
brew install nasm cmake ffmpeg@6 protobuf
```