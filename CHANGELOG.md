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

## [mmclient-v0.2.0] - 2024-05-05

### Bugfixes

- Actually sync video and audio (4822bda39b4a5f07ed74e4fd76d5b080ea1c2078)
- Tune verbosity of conn message (e9f0d18da517e1c7f1ab34d9c154b8ba70573f2e)
- Fix typo in conn init (d8dd70b25952e1d1155bf8e6930d2304ca51c79e)

## [mmserver-v0.3.0] - 2024-05-05

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

## [mmclient-v0.2.0] - 2024-05-05

### Bugfixes

  - Increase the default timeout when waiting for frames (a8aefcb295803d087349625a37e1fdef3f2ec9d7)
  - Handle video frames sent over the attachment stream (c0ecfba8fd5f06a64ab2e3c5d02731938a41170b)
  - Handle VideoChunk messages on the attachment stream (75f409d1b2c0685bf6e4413a44535798a7a53a71)
  - Handle AudioChunk messages on the attachment stream (3a63b07149fd36308d72378c66b53c41574abb1e)
  - Be more robust in the face of bad stream data (7c920b66451e615205cea7a8d229c068c340324c)
  - Respect hidden cursors (003fe97034cbbd71a8845841cf9d26e592c27696)

