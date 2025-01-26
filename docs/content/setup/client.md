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
it [here](https://github.com/colinmarc/magic-mirror/releases/tag/mmclient-v0.6.0).

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

Besides Rust itself, the following command will install everything on ubuntu:

```
apt install \
    nasm cmake protobuf-compiler libxkbcommon-dev libwayland-dev libasound2-dev \
    ffmpeg libavutil-dev libavformat-dev libavdevice-dev libavfilter-dev
```

Or using homebrew on macOS:

```
brew install nasm cmake ffmpeg@6 protobuf
```
