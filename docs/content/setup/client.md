+++
title = "Client Setup"

[extra]
toc = true
+++

## macOS GUI Client

The native macOS client can be downloaded from [the releases page](https://github.com/colinmarc/magic-mirror-swiftui/releases/latest).

It should work out of the box on ARM and Intel Macs running macOS 10.14 or
later.

## Installing the commandline client

There is also a cross-platform commandline client, `mmclient`. You can download
it [here](https://github.com/colinmarc/magic-mirror/releases/tag/mmclient-v0.7.0).

The commandline client requires `ffmpeg` 6.0 or later to be installed on the
system. It also requires up-to-date Vulkan drivers.

## Building mmclient

The following are required to build the client and its dependencies:

```
rust (MSRV 1.77.2)
nasm
cmake
protoc
libxkbcommon (linux only)
libwayland-client (linux only)
alsa (linux only)
ffmpeg 6.x
```

Besides Rust itself, the following command will install everything on Ubuntu:

```
apt install \
    nasm cmake protobuf-compiler libxkbcommon-dev libwayland-dev libasound2-dev \
    ffmpeg libavutil-dev libavformat-dev libavdevice-dev libavfilter-dev
```

Or using homebrew on macOS:

```
brew install nasm cmake ffmpeg@6 protobuf
```

Note that `mmclient` doesn't compile with the latest versions of clang. It's tested against clang 18 (which is part of the Ubuntu 24.04 repositories). If you have multiple clang versions installed and version 18 is among them, you'll probably want to set some environment variables to point to the clang 18 library paths as described in the [clang-sys docs](https://github.com/KyleMayes/clang-sys?tab=readme-ov-file#environment-variables).

For building `mmclient` you'll also need to have [slang v2025.5](https://github.com/shader-slang/slang/releases/tag/v2025.5) available. Since this isn't packaged on most distributions, you'll likely need to install the release assets yourself and specify the following environment variables at build time:

```bash
# Create temporary directory to download slang into.
mkdir -p /tmp/slang
# Download linux release asset for slang v2025.5 into temporary directory.
curl -o /tmp/slang/slang.tar.gz https://github.com/shader-slang/slang/releases/download/v2025.5/slang-2025.5-linux-x86_64.tar.gz
# Extract assets from archive.
tar -xf /tmp/slang/slang.tar.gz
# Set required environment variables for building `mmclient`.
export SLANG_DIR=/tmp/slang
export DYLD_LIBRARY_PATH=/tmp/slang/lib
export LD_LIBRARY_PATH=/tmp/slang/lib
```
