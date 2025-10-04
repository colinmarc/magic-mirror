## [mmserver-v0.8.5] - 2025-10-04

## [mmserver-v0.8.4] - 2025-05-21

### Bugfixes

- Make missing hardware encode support a hard error (b16dccb01902b854a2c345406f4df416d3024811)

## [mmserver-v0.8.3] - 2025-03-12

### Bugfixes

- Try to avoid colliding with the system x11 socket (3af95ba9ab012e723d21415baf0b6f4679ba1534)
- Follow symlinks when calling move_mount (a7505aed296cab4648e2f3752d5901e5d95ded45)

## [mmserver-v0.8.2] - 2025-02-20

### Bugfixes

- Drop application frames if the application is too slow (d73d78dfc37a17fd011c1d3ef1dbfe12c85ed856)

## [mmclient-v0.7.0] - 2025-02-12

### New Features

- Deprecate KeepAlive in favor of connection keepalives (ad3cdca8faf089b85902977e3e48b4a35d5f89e3)
- Send hierarchical_layer as video frame metadata (ddbe84346fa03f55ebed7289b005b2e36ec23d36)
- Expose hierarchical_layer (b4bd4c66b62439c71c6a3ed52c26046b3a2f0b6f)
- Allow clients to configure their connection timeout (452ef70eb118280df9170bed382b8539813e7802)

### Bugfixes

- Remove a useless warning (c98bbe5382914dec22c92f4f160b2f276fb811ef)

## [mmserver-v0.8.0] - 2025-02-12

### New Features

- Make the session timeout configurable (39fa20cadfe7a780088c86f78ef2eae87e0c1222)
- Send hierarchical_layer as video frame metadata (ddbe84346fa03f55ebed7289b005b2e36ec23d36)

### Bugfixes

- Increment stream_seq when a refresh packet is sent (0fe282ae0fb71b476929ddf61bf71e4041ac0323)
- Send headers with H265 keyframes (dc7084412c3c4eec661a9887f8c0d031f8dc8a19)
- Add a warning if users are about to hit #29 (d5591bb4d59a635ffb9796b9c1f1cba9eba22b36)

## [mmserver-v0.7.2] - 2025-02-05

### New Features

- Deprecate KeepAlive in favor of connection keepalives (ad3cdca8faf089b85902977e3e48b4a35d5f89e3)

### Bugfixes

- Remove noexec from /tmp in the container (0c534f6677e07cda77e0384854dded47dd8a949b)
- Support resampling app audio output (897a053abc568255040a66c356703e3e6c3c9070)
- Support downmixing by throwing away extra channels (17d81d866efc94ed2c2839589541362be3d5aae1)
- Allow subtitles in application names (2fcac04765ce4af02923314667289ed88094f824)
- Use aligned width and height for DPB images (d6f3bf713373bbadde0590f44659f8146e44c28d)
- Relax the app startup timeout (a840c2b27b7adda073820a62d72fd64dc90e752b)
- Use QUIC PING frames to keep clients alive (95ddb3d6bdc7e761ff596e249cc7be83b3d14cfb)
- Don't flood the client with pings (b3b3194c042b8d56fda1f8b08f230042bf4461f7)
- Turn down the heat on udevfs logging (53f448e45110ef722a60f927edd7c5fe58455a19)

## [mmserver-v0.7.1] - 2025-01-30

### New Features

- Let the encoder swap frames (b519680e3e8c552874f53cd88e98859e90698ac8)

### Bugfixes

- Update quiche (c3d1e0080c1040151ecdc08e85584ff267f6eed6)
- Remove an extra copy on the dgram path (0d204fa2549f4fd9abc804f4996f8fe11162e67b)
- Fix clippy warning (7b041bbff7c908e57928d621ad74751bb7b76355)
- Respect layer limits in VkVideoEncodeCapabilities (ce515b4d85b8af4da83a3fed0281f907e28253b1)
- Print child logs correctly (e8cd88fb344ec74398eae09e757589700de3bff3)
- Print an error when dmabuf importing fails (112d48d706fc19ce8882e2767c33672e7a044527)
- Change target for vulkan error logs (641f51675a572402710fe3ee2ff0721857228ab9)
- Add context (7572ac6a5131486f6d4cf6951742eea9c0f24d25)
- Use the default congestion control algorithm (7245e624f785b2b01e0b5da507380a88121de542)
- Get explicit sync working on NVIDIA (8d5786445e56629c67338258b1bc8cc7debb410e)
- Remove unused import (df03d5d38236b325e826b998cef26ea2d9008e75)
- Fix explicit sync on nvidia proprietary (8f806b233f537395d72de6e06d3861e73963bec2)
- Check for the right nvidia version (08b6462a320ee76eabaf4387c354a1a6634ec8df)
- Fix explicit sync on nvidia proprietary (3c70a79cda9cc545acf665ccacc495ed30f3440a)

## [mmclient-v0.6.0] - 2025-01-26

### New Features

- Support ffmpeg vulkan decode (5c76b29273d3c0b29edb9e34e33096af76814398)
- Explicit video refresh (60dffc04f4f338c3fce6d791211c12d7471a187a)
- Implement forward error correction (729e652a001d155345c80b7f5fef397a884a1a98)

### Bugfixes

- Enforce non-fractional scales from the client (2a25ca95db01ff8460328f8f258faadf55d948bb)
- Take application names with path included (100d51e8f44129a23b1df944a897a3123ef12d1c)

## [mmserver-v0.7.0] - 2025-01-26

### New Features

- Enable hierarchical coding (90d636ffba8379da420e09c6f228fb65c334a7f2)
- Explicit video refresh (60dffc04f4f338c3fce6d791211c12d7471a187a)
- Implement forward error correction (729e652a001d155345c80b7f5fef397a884a1a98)
- Remove support for CPU encoders (a5d069cb7bde15931748e41ae3d9e12a6f917445)
- Log basic attachment stats (b42cb40cb3d5fbedd2a17d37e09da8984029998c)

### Bugfixes

- Pass correct flags to move_mount (af519eebc5a8f251624b3d063a7241910cddf2cc)
- Pass correct flags to move_mount (take 2) (2e6053675a229dba4fc012b5de4afb723e9a0aca)
- Enforce non-fractional scales from the client (2a25ca95db01ff8460328f8f258faadf55d948bb)
- Disable explicit sync if the syncobj_surface is destroyed (e6017dec6bb9daadbbe50898f7cd9cf7c14b19aa)
- Reduce the verbosity of some logging (b0abe2a76466e98ba8d4f844e88fd4ad4ce6c7ee)
- Print frame duration from encoder (4a9af4f712ce723306364bece359c9bf18515554)
- Add overall encode timing to tracy (334d5b37fe394e652bd27224c1c8e905e9c8a794)

## [mmserver-v0.6.1] - 2024-12-17

### New Features

- Save vulkaninfo for --bug-report (6deae3feb5a72a7e0099edd4983814d7fc873f15)

### Bugfixes

- Avoid an endless loop when printing child output (7d700dfa4b9ef6d02e58c4a32151e69055fa3929)

## [mmserver-v0.6.0] - 2024-12-11

### New Features

- Support wp_linux_drm_syncobj_timeline (54f311653d800cf5a7aefe1b54edd27010f219ce)
- Officially support nvidia proprietary (204126cdfcce09f4971de2e1bb9c86a4adf04d97)

### Bugfixes

- Bind-mount nvidia devices, if present (4bb63d3c1e85f297c5d169219943694f133bbcfa)

## [mmclient-v0.5.0] - 2024-12-11

### New Features

- Add 'app_path' for organizing apps (b417559625c97e182dc074a5732ea35617332f36)
- Add header images to the application list (756bfa866020da57be18d383367e0a2b189051aa)

### Bugfixes

- Use Error::ServerError to communicate server errors (a857e0f186b9514cd3e1dc9b0f60df04b4abe3fe)
- Correctly represent cursor images as optional (b08c76c9c65441fa92156f5282e9b02e98fa3ed9)
- Be more resilient sending messages on closed streams (8e3eea65ccff2b6448dd9993b9afef9996c6650d)
- Ensure attachment_ended is called on delegate (fd4d1c41e7da5ec949e26c91cc6171db1a41b1ce)
- Always send pointer_left events (06010c9cf336d637526dcc308d1ee842e3a21cc1)
- Handle ipv6 addresses correctly (9d442d2c8ad4c8cbfef96cb378289e1699d17e02)
- Log connection errors (0ecc6ef05a5470991f1df8d0feaf18ace99b8de8)
- Remove zone identifiers from hostnames (f9cee190718dc71aad8e9a0372b581a611551289)

## [mmserver-v0.5.6] - 2024-12-08

### New Features

- Warn if the client is using shm buffers (461e8913d9645c240d30a2ce1d269f8ba8aa0e39)
- Support wp_fractional_scale (2a267e102add6fb72504652375d9ea48ec2c6484)

### Bugfixes

- Handle invalid executables more comprehensively (f51174eb1509cecc73c10ab57cf991ee12a5cce7)
- Throw an error if the app exe would be shadowed (cc6ee7e3df086bba443bb41471d671a2bd1b191b)
- Reset staging query state each frame (982afb811ec062ddb6cc498a9cb92e6a4b5472ef)
- Handle stride of shm buffers correctly (e8e1ee5eeba71d767c543ae83c4fa09b381beba1)
- Log when container setup fails (83ea7b46fb95e1f1811cf516c55343622f9d9d35)
- Put the XWayland socket in a canonical location (76056acbdc084307c6d71a66d2c7a343adea9b77)
- Never discard surface content (f28e947201bc53be91ed13a53ad0221c27f931fb)
- Handle xdg_popups more gracefully (03e392506a52349a4fdb075f4a4e53008a237958)
- Translate surface coordinates correctly (9107636d2cb835409df3f604c47eed2d7397e819)
- Shadow /run/user but not /run (c810f24305a169d896cbe92b57d53fd732bdef09)

## [mmserver-v0.5.5] - 2024-12-05

### New Features

- MDNS service discovery (152d82ca7595063aa77db7470e1dfdace9ae7ac2)
- Add 'app_path' for organizing apps (b417559625c97e182dc074a5732ea35617332f36)
- Make the mDNS instance name configurable (17e632ccbee15132e2420a5fc162c94171d4a34a)
- Add header images to the application list (756bfa866020da57be18d383367e0a2b189051aa)

### Bugfixes

- Align h265 bitstreams correctly (fc0543889b70eb0a151084d6a117e464cbeaaca0)
- Improve error message when using self-signed certs (211dbcded77dc6fd0d97f19a415ca4b286327fb9)
- Handle differing width/height in encode granularity (6b4b2dac3473d3631da6daa31fd09dc1bd3e2059)
- Update the maximum message size to reflect the protocol docs (c517624d3683b7ad1e37fc7ea6a18d86c09ccb75)
- Remove unecessary casts (d28b0b4335eb3e220b004421395e1f7d1d874939)
- Warn when no hardware encoder is available (bef772948bbb7ff04788016fe74a84eefa7dee8c)
- Bail early on mesa 24.2 (17758e3269ba661541ee2e94616606f2d935c626)

## [mmserver-v0.5.4] - 2024-11-18

### Bugfixes

- Handle missing /sys/devices/virtual/input (8f316fe41c41101ae18156a41abe2e9ba1e3497f)
- Lock pointer based on pointer focus (4ce202d3bd9cb764c0586cdc83e890843c3c04d7)
- Correctly handle an edge case with pointer locks (7c3428932651a372c69b25d1f77dc973746273a9)

## [mmserver-v0.5.3] - 2024-10-24

### Bugfixes

- Be consistent in xwayland socket naming (f6f6db3ab8b61e7af7684f14202d2b203b7e7760)
- Never use a 0 audio stream_seq (632bcb1f7c79d35701f31a29d2dbe659ab411e3c)
- Use the attachment coordinate space (57a59f478a6e4e248490b04b8c1ab42d2b1ae115)
- Don't close streams while partial writes are pending (0add85078734a27e121dda97293f0e48d8ebd214)

## [mmclient-v0.4.1] - 2024-10-24

### Bugfixes

- Handle video/audio stream seq more intelligently (4bab3902d1e7d88c7222ed6ef404190c512b1940)
- Make the overlay work again (0b1579bf68b2cd31611ca10a735061ef58e64604)
- Use the attachment coordinate space (57a59f478a6e4e248490b04b8c1ab42d2b1ae115)
- Don't close streams while partial writes are pending (0add85078734a27e121dda97293f0e48d8ebd214)
- Send relative pointer motion again (7fced702ebe37de5b2f96e46091c6b862806f757)

## [mmserver-v0.5.2] - 2024-10-19

### Bugfixes

- Use getgid if we want the group ID (6a9c71d25d58ff6b5bc4564b99230d76a6599f0e)
- Use _exit instead of exit or abort (c33a7b8989121706e0286af5efcdd8b5cf1291f1)
- Pass locale environment variables through to child apps (8022fd1bdb8e64918e15f38b2b4197361841f9d5)

## [mmserver-v0.5.1] - 2024-10-18

### Bugfixes

- Correctly emulate input nodes in udevfs (3fec928dcb5d7d5054d6ca7821864bae74559b9b)
- Increase the ready timeout (df5ba10642c5ec18064a67f8279d40d3b12baa76)
- Stub wl_data_device_manager (af1853aaf34c373617b78ddbfbde2d37a977d3df)
- Don't discard buffers when resending frame callbacks (3b9ce4164bb617ce7e0fd0840bad74fd281fda99)
- Organize bug report files slightly better (1806d3eea0e33c124f58d413fc3843e288cc0b0e)

## [mmclient-v0.4.0] - 2024-10-18

### New Features

- Plumb controller input through to the server (990f48cdac4181e69ac3cb5dd1473fe16fca3390)
- Allow specifying 'permanent' gamepads for a session (1d5b7f0a38017e0589c928a9acb6a10075bfac52)
- Refactor out most of mmclient into a UniFFI rust lib (e8097e594b72a336ace6ef5fe7247304a18dd364)
- List applications the server can launch (5d042be0f51095e06bbf68cdc3d3e40523c3e5ad)
- Add a logging interface (b961041ce28b7da961f193b17cd03f4e36c14ea7)

### Bugfixes

- Remove unecessary clone (87c95e63f6c6ce2f63207f96da839408f4617785)
- Rename Gamepad* enums to reduce the possibility of collision (5fd2241beff203c5c09089456e9326102213c2c2)
- Prevent a reattaching doom loop (dfa5d75e8daefa3dc15468145f55a5d06e7cd6e1)
- Correctly invert joystick direction (a60eb398b5f1dd13e1ac660f856a03857decad5b)
- Round off window height (d4227e772a7d6c8d30919b1e08876ee4a2e55802)
- Handle gamepad connected events correctly (aed00821a8ce3add26ef3ff2226b26e0752c1971)
- Increase the ready timeout (df5ba10642c5ec18064a67f8279d40d3b12baa76)

## [mmserver-v0.5.0] - 2024-10-15

### New Features

- Plumb controller input through to the server (990f48cdac4181e69ac3cb5dd1473fe16fca3390)
- Allow specifying 'permanent' gamepads for a session (1d5b7f0a38017e0589c928a9acb6a10075bfac52)
- Add support for native linux containerization (a37b0db8c5006e4c7b02cc98e506cd68a6ac2aa1)
- Basic gamepad support (f0eceab777fd38cb085e0f5120fe54ab2a71d362)
- List applications the server can launch (5d042be0f51095e06bbf68cdc3d3e40523c3e5ad)

### Bugfixes

- Remove a bunch of dead code (b5e88bbe9e472866d9ddd5316a7a8187d7676778)
- Add description field to application configs (d786828a87ce2c5ed18f373e3be06a1808ad5c42)
- Include more context when reading config files (d39aaf46c09d2c6d4525dfb3b452374cd1476b9d)
- Require app names to start with a letter (4182a506ea3a15809c42010ef88da1aeac12278d)
- Handle unknown message types more gracefully (2978f9b2d41e4916f7a18905586466bb66e92c35)
- Add application name to session spans (eccca93fd50530d7d658e8a69bb22ef1b689b5a4)
- Sleep the compositor if no client is attached (e03d8f2914867cc733fa4b44f78f00f7f89ea361)
- Make reattaching slightly more robust (10cfede5b4ef625f9961b3582ac7dab33cba6dd7)
- If using layers > 0, pass that many rate control layers (3a201510794deaebf262a81e8b02e8a3d9359cfd)
- Get hierarchical coding working on H265 (7b63cc694b28eb7fd1e9155a182e5446b80ef998)
- Add some preflight checks at startup (91e00002073a1c07af73fb5a7f1e27a5779d66b3)
- Improve shutdown behavior (5e77d7719313c2c6d53fa3335aec06840a9fe92a)
- Use putenv instead of Command::env (0a832c0f606a9d130eeca0bcb334dc6c5d65e169)
- Remove unshare as a dependency (e5c4575e3cacc9d00656cda7af114a0eb471777c)

## [mmserver-v0.4.1] - 2024-08-16

### Bugfixes

- Time out if the compositor doesn't accept an attachment in a reasonable timeframe (c1d6c6ca82fe3ff5ffcbf204c7f90e149b82f0ae)
- Explicitly close QUIC streams when a worker finishes (a4b0c18e4af7455dcde689b241e4fe2737e50f57)
- Never use 0 as a stream_seq (8fc95e4ef0d4a01d9c1809860a633c7417913115)
- Raise the attachment timeout to account for slow clients (6b60df3e7625da72157b5a6ae8479e9e05469c71)
- Set a default for video_profile (b4f2e01548ad0d374b4fc816f6a2a5c7c11f1751)
- Correctly send vertical scroll events (6a25863b00f049d354dda5f598a3f507db653285)
- Change order of press/release when simulating repeat (6df3f5cea5f8e6b2e2634f1307b2c4ee054ed638)

## [mmserver-v0.4.0] - 2024-08-02

### New Features

- Rewrite compositor from scratch (945a7793abbbc377f8c9ad1a852715203a16b097)
- Allow attachments to be configured for HDR10 output (0c4b85af422378881f550f61882439b1a4abade1)
- Support streaming in HDR (713dbbdce931e0ba98cc51bf144a2fe26dd9e2a1)

### Bugfixes

- Improve compositor error messages with s/client/app (e5b24afe2ccd8ce77f74a5732a2e02f723256cda)

## [mmclient-v0.3.0] - 2024-08-02

### New Features

- Allow attachments to be configured for HDR10 output (0c4b85af422378881f550f61882439b1a4abade1)
- Support playing HDR streams (12ef76930f729af0331bb83c3ceadb110bf22a6f)

### Bugfixes

- Make --detach the default (7ca5ee3ea03bcc19f754c1542675be360e3216af)
- Take name or id for --kill (7a1f8c1483bd43c292e5ec8189535b0e59fc453c)
- Move the cursor before locking it (2a5cc571f868c7ade0c9798b41e96ee21209de4d)
- Calculate RTT correctly (4762c1ab0594897949e4ce81a7897fab30d9c7fe)
- Make sure session width/height are even (5a344ade0e3cd62c1c8e0f4b99d6be8dee7b513f)
- Handle ConnectionClosed (953b9d4398ccca75b4108da0c31589c56747ff70)
- Ensure --ui-scale overrides environment scaling (776b4dc2c5462a05c8520e769361f3136d5bcc6a)
- Swap order of lock/warp when locking cursor on not-mac (525622b29d46fc8e659d0e3c37cf920faf587866)

## [mmclient-v0.2.0] - 2024-05-08

### New Features

- Cursor locking and relative motion (e11dfec7e42802a528ac8c8b4629044e6d6b1c3f)
- Add --preset, for setting quality/bandwidth usage dynamically (6c590efaab02e31aae8413b683e8f8d228256b3b)

### Bugfixes

- Don't sync every frame (5a7f1cfe11e6684e11bd618e2f1adf4d043640f5)

## [mmserver-v0.3.0] - 2024-05-08

### New Features

- Cursor locking and relative motion (e11dfec7e42802a528ac8c8b4629044e6d6b1c3f)
- Add --preset, for setting quality/bandwidth usage dynamically (6c590efaab02e31aae8413b683e8f8d228256b3b)

### Bugfixes

- Remove debugging code (152a1714ca950256f136757f47b7b2cf587d6880)
- Un-transpose min and max QP (0570a6470b934e62dd4c9dcc42467a6db1a311e4)
- Correctly set max QP on lower presets (b3f73533bb896c93d4a1d4e5c8efc336e329042c)
- Prevent a segfault on nvidia (8b331b5de98a50dd3c59671a2dbfe37b966b95b9)
- Re-send cursor status when reattaching (eba4a368c33a5bcd1cdf27a8b791f31ff466bb29)

## [mmclient-v0.1.2] - 2024-05-05

### Bugfixes

- Actually sync video and audio (4822bda39b4a5f07ed74e4fd76d5b080ea1c2078)
- Tune verbosity of conn message (e9f0d18da517e1c7f1ab34d9c154b8ba70573f2e)
- Fix typo in conn init (d8dd70b25952e1d1155bf8e6930d2304ca51c79e)

## [mmserver-v0.2.0] - 2024-05-05

### New Features

- Add enable_datagrams, off by default (e1dc976ee3228b006b874e077cd2c6cf7f784927)
- Add glxinfo and eglinfo output to --bug-report (696464d9b980f1664e2b9dcce9e6f6dde83407f2)

### Bugfixes

- Don't panic on dmabuf cursors (9f87ce7d99289ba31ad11b5d1796b992fd21c796)
- Print version after initializing logging (f708ad2d8e5ddc9fb17ac023fef8f81706c31be7)
- Handle full send queues more gracefully (face8776acea8c22e4d83b62c54ece5682f95cee)
- Manually enable radv encode (26ba3f93f3da29921f9754181738f2087284a164)
- Correctly expose a vulkan fn (2c627c94569050d0b53429204e8153119d268560)
- Write xwayland logs to the bug report dir (0ba97f5f3bd72caf7df815e341c4c4f0a807b094)
- Support older versions of xwayland with wl_drm (54c9724a476d023547fb1c2ccc5d74bc6eadc6a3)
- Kill hung clients (5179e6688a2bc8fcceded03c0d92e2a00c38fb99)
- Implement basic rate control (781c97e3efde247ef437ad2e19e8cdf57b6d216e)
- Log entire config (b588f198d13122869936b52c0690e980586a7f88)
- Garbage-collect partial writes (a095994de28ec31bd49a54c2d757493f41fc0c06)

## [mmclient-v0.1.1] - 2024-05-05

### Bugfixes

  - Increase the default timeout when waiting for frames (a8aefcb295803d087349625a37e1fdef3f2ec9d7)
  - Handle video frames sent over the attachment stream (c0ecfba8fd5f06a64ab2e3c5d02731938a41170b)
  - Handle VideoChunk messages on the attachment stream (75f409d1b2c0685bf6e4413a44535798a7a53a71)
  - Handle AudioChunk messages on the attachment stream (3a63b07149fd36308d72378c66b53c41574abb1e)
  - Be more robust in the face of bad stream data (7c920b66451e615205cea7a8d229c068c340324c)
  - Respect hidden cursors (003fe97034cbbd71a8845841cf9d26e592c27696)

