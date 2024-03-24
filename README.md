
## Magic Mirror

This is a game streaming or remote desktop tool for Linux, featuring:

 - Up to 4k streaming (support for 10-bit HDR in progress)
 - Local cursor rendering
 - Hardware encoding on discrete GPUs
 - Client support for macOS

> [!WARNING]  
> Alpha software! Please submit any issues you encounter. Run the server with `--bug-report` to generate detailed logs and record videos to attach to your report.

### Quickstart

```shell
$ cd mm-server
$ cargo build --bin mmserver --release --features vulkan-encode
$ cat >> steam-bigpicture.toml <<EOF
command = ["steam", "-gamepadui"]
xwayland = true
EOF
$ target/release/mmserver --bind "<your local ip>:9599" -i steam-bigpicture.toml
```

And then on the client:

```shell
$ cd mm-client
$ cargo build --bin mmclient --release
$ target/release/mmclient "<ip>:9599" steam-bigpicture --codec h264 --resolution 1080
```

This will work over the local network or a private IP space like Tailscale. To serve over the public internet, TLS is required. See [mmserver.default.toml](mmserver.default.toml) for more detail on that and other configuration options.

### Encoder support

Hardware encoding, based on Vulkan Video, is still a work in progress. You will need very, very new drivers. CPU-based encode is available as a fallback, but has a few more frames of latency.

To see if your GPU supports video encoding, see the following matrix for your vendor: [AMD](https://en.wikipedia.org/wiki/Unified_Video_Decoder#Format_support) | [NVIDIA](https://developer.nvidia.com/video-encode-and-decode-gpu-support-matrix-new)

Note that with the `ffmpeg` feature, linking against a system-installed `ffmpeg` is supported, which may allow you to use specific CPU-based codecs not considered in this table.

| Codec | CPU |  AMD  | NVIDIA | Intel |
| ----- | :-: | :---: | :----: | :---: |
| H.264 |  ❌  | ✔️[^1]|   🚧    | ❔[^2] |
| H.265 |  ✔️ |  ✔️[^1]|   🚧    |   ❌   |
|  AV1  |  ✔️ |   ❌   |   ❌    |   ❌   |

[^1]: Requires [this draft MR](https://gitlab.freedesktop.org/mesa/mesa/-/merge_requests/25900).

[^2]: I don't have a card available to test, and it's difficult to find information online about driver/card support for hardware encode. Please let me know how it goes!