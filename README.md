# Magic Mirror 🪞✨
[![GitHub Actions Workflow Status](https://img.shields.io/github/actions/workflow/status/colinmarc/magic-mirror/tests.yaml)](https://github.com/colinmarc/magic-mirror/actions/workflows/tests.yaml)
[![Discord](https://img.shields.io/discord/1284975819222945802?style=flat&label=discord&color=7289DA)](https://discord.gg/v22G644DzS)

This is a game streaming and remote desktop tool for Linux hosts, featuring:

 - **Headless multitenant rendering:** Streamed applications are run offscreen, isolated from the rest of the system and any display hardware.
 - **No system dependencies:** The server is a single static binary, and there's no dependency on docker, pipewire, or any other systemwide setup.
 - **Native linux containerization:** apps are isolated in rootless containers with the equivalent of unshare(1), using new Linux namespace features
 - **High quality, tunable, 4k streaming:** See the [list of supported codecs](https://colinmarc.github.io/magic-mirror/setup/server/#hardware-software-encoding). 10-bit HDR support is in progress.
 - **Very low latency:** No extra CPU-GPU copy when using hardware encode. Total latency is less than one frame.
 - **Local cursor rendering:** Use the client-side cursor for minimal input lag.
 - **Client support for macOS and Linux:** A [SwiftUI client](https://github.com/colinmarc/magic-mirror-swiftui/releases/latest) is available for macOS, with tvOS/iOS support coming soon.

> [!WARNING]
> Alpha software! Please submit any issues you encounter. Run the server with `--bug-report` to generate detailed logs and record videos to attach to your report.

### Quick Links

 - [Documentation Book](https://colinmarc.github.io/magic-mirror)
 - [Latest Server Release [mmserver-v0.8.3]](https://github.com/colinmarc/magic-mirror/releases/tag/mmserver-v0.8.3)
 - [Latest CLI Client Release [mmclient-v0.7.0]](https://github.com/colinmarc/magic-mirror/releases/tag/mmclient-v0.7.0)
 - [Latest macOS Client Release](https://github.com/colinmarc/magic-mirror-swiftui/releases/latest)
 - [Discord](https://discord.gg/v22G644DzS)
