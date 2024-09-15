
<p align="center" style="font-size:0.9em">
<i>server download:</i> <a href="https://github.com/colinmarc/magic-mirror/releases/download/mmserver-v0.4.1/mmserver-v0.4.1-linux-amd64.tar.gz">linux-amd64</a>
<i>client download:</i> <a href="https://github.com/colinmarc/magic-mirror/releases/download/mmclient-v0.3.0/mmclient-v0.3.0-linux-amd64.tar.gz">linux-amd64</a> • <a href="https://github.com/colinmarc/magic-mirror/releases/download/mmclient-v0.3.0/mmclient-v0.3.0-darwin-arm64.tar.gz">darwin-arm64</a>
</p>

# Magic Mirror 🪞✨
![GitHub Actions Workflow Status](https://img.shields.io/github/actions/workflow/status/colinmarc/magic-mirror/tests.yaml)
![Discord](https://img.shields.io/discord/1284975819222945802?style=flat&label=discord&color=7289DA)

This is a game streaming and remote desktop tool for Linux, featuring:

 - 100% headless, offscreen, multitenant rendering - no dummy plug required
 - Up to 4k streaming (with support for 10-bit HDR in progress)
 - Very low latency (about 1 frame, plus network)
 - Local cursor rendering
 - Client support for macOS

> [!WARNING]  
> Alpha software! Please submit any issues you encounter. Run the server with `--bug-report` to generate detailed logs and record videos to attach to your report.

### Quickstart

Grab the latest release (link above), and run it on a server with a GPU:

```shell
$ cat > steam-bigpicture.toml <<EOF
command = ["steam", "-steamos", "-gamepadui"]
xwayland = true
EOF
$ ./mmserver --bind "<your local ip>:9599" -i steam-bigpicture.toml
```

You can replace steam with your app of choice, or even a full nested desktop environment like [Sway](https://swaywm.org/).

And then on the client (after installing `ffmpeg`):

```shell
$ ./mmclient "<ip>:9599" steam-bigpicture --codec h265 --resolution 1080
```

This will work over the local network, or a private IP space like Tailscale. To serve over the public internet, TLS is required. See [mmserver.default.toml](mmserver.default.toml) for more detail on that and other configuration options.

For instructions on building the server and/or client yourself, see [BUILD.md](BUILD.md).


### System Requirements

The following is required to run the server:

 - Linux 6.x (for Ubuntu, this means Mantic or Noble)
 - (For AMD/Intel cards) Mesa 24.1.x or later (not yet released, so use `mesa-git` on arch or build from source)
 - (For NVIDIA cards) [Vulkan drivers](https://developer.nvidia.com/vulkan-driver) version 550 or later
 - XWayland (for X11 apps)

The following is required to run the client:

 - Ffmpeg 6.x

### Encoder support

Hardware encoding, based on Vulkan Video, is needed to get the best performance and latency. CPU-based encode is available as a fallback, but it's much slower.

To see if your GPU supports video encoding, see the following matrix for your vendor: [AMD](https://en.wikipedia.org/wiki/Unified_Video_Decoder#Format_support) • [NVIDIA](https://developer.nvidia.com/video-encode-and-decode-gpu-support-matrix-new)

Note that with the `ffmpeg` feature, linking against a system-installed `ffmpeg` is supported, which may allow you to use specific CPU-based codecs not considered in this table.

| Codec | CPU | AMD | NVIDIA | Intel[^1] |
| ----- | :-: | :-: | :----: | :---: |
| H.264 |  ❌ |  ✔️  |   ✔️    |   ❔  |
| H.265 |  ✔️  |  ✔️  |   ✔️    |   ❔  |
|  AV1  |  ✔️  |  ❌ |   ❌   |   ❌  |

[^1]: I don't have an Intel GPU available to test, and it's difficult to find information online about driver/card support for hardware encode. Please let me know how it goes!
