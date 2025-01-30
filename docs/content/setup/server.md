+++
title = "Server Setup"

[extra]
toc = true
+++

## Quickstart

First, grab [the latest server release](https://github.com/colinmarc/magic-mirror/releases/tag/mmserver-v0.7.2) and untar it somewhere:

```sh
curl -fsSL "https://github.com/colinmarc/magic-mirror/releases/download/mmserver-v0.7.2/mmserver-v0.7.2-linux-amd64.tar.gz" \
    | tar zxv
cd mmserver-v0.7.2
```

Then, create a [configuration file](@/reference/config.md) with at least one application definition:

```toml
# mmserver.toml
[apps.steam-gamepadui]
command = ["steam", "-gamepadui"]
xwayland = true
```

Then you can start the server like so:

```
$ ./mmserver -C config.toml
2024-12-09T16:57:30.989261Z  INFO mmserver: listening on [::1]:9599
```

You can also create a configuration directory, and add a file (json or toml) for each application:

```sh
mkdir apps.d
echo 'command = ["steam", "-gamepadui"]' > apps.d/steam.toml
./mmserver -i apps.d
```

## Connectivity

By default, mmserver only listens on `localhost`, which is not terribly
useful. There are a few different options to configure which socket address the
server listens for connections on.

The easiest is to bind to a local IP, or use a VPN like wireguard or tailscale:

```toml
# config.toml
[server]
bind = "192.168.1.37"
```

Or from the command line:

```sh
mmserver --bind $(tailscale ip -4):9599
```

If you'd like to stream on a public IP, or on all interfaces (with `0.0.0.0`),
mmserver requires that you set up a TLS certificate and key:

```toml
# config.toml
[server]
tls_cert = "/path/to/tls.key"
tls_key = "/path/to/tls.cert"
```

Generating such certificates and adding them to the client is out of scope for
this guide. Note that while all Magic Mirror traffic is encrypted with TLS
(whether you supply certificates or not), no _authentication_ is performed on
incoming connections.

Finally, you can also use `--bind-systemd` or `bind_systemd = true` to bind to a
[systemd socket](https://www.freedesktop.org/software/systemd/man/latest/systemd.socket.html).

## System Requirements

The following is required to run the server:

 - Linux 6.x (for Ubuntu, this means Mantic or Noble)
 - (For AMD/Intel cards) Mesa 24.3.x or later
 - (For NVIDIA cards) [Vulkan drivers](https://developer.nvidia.com/vulkan-driver) version 550 or later
 - XWayland (for X11 apps)

## Hardware/software encoding

Magic Mirror uses hardware-based video compression codecs to stream the game over the wire.

To see if your GPU supports video encoding, see the following matrix for your vendor:
 - [AMD](https://en.wikipedia.org/wiki/Unified_Video_Decoder#Format_support)
 - [NVIDIA](https://developer.nvidia.com/video-encode-and-decode-gpu-support-matrix-new)

| Codec | AMD | NVIDIA | Intel |
| ----- | :-: | :----: | :---: |
| H.264 |  ✅ |   ✅   |   ❔  |
| H.265 |  ✅ |   ✅   |   ❔  |
|  AV1  |  ❌ |   ❌   |   ❌  |

## Building `mmserver` from source

The following are required to build the server and its dependencies:

```
rust (MSRV 1.77.2)
nasm
cmake
protoc
libxkbcommon
```

Besides Rust itself, the following command will install everything on ubuntu:

```
apt install nasm cmake protobuf-compiler libxkbcommon-dev
```

Then you should be good to go:

```
cd mm-server
cargo build --bin mmserver [--release]
```
