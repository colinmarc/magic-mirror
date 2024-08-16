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

