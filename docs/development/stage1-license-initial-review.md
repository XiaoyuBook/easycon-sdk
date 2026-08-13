# Stage 1 GPL/source/dependency license 初审

- 状态：Initial review completed / Release qualification open
- 复核日期：2026-08-12
- production implementation input：`f99333f1e4d359af4588a659e2890bcfd58483de`
- production input parent：`be4c2cdfe9ffcdfe9de35cdda032502d2a63e89c`
- production input tree：`cc9a6040d000be9c070f586b04bb20d2be75a685`
- approved H validation input：`df13db4cb78c14602e05a31636a3e3a8f277f873`
- H input parent：`8edc700a91dc02fbe58833126954881a2ab0de22`
- H input tree：`a3819e469c9dc629876b84835fccbabcc73ccc8e`
- 2026-08-12 license audit environment fingerprint：`a3b01b70ef0aee4ac7258e2bbf40aa6e953c64d40078b2fabd4adfe420bff9be`
- vcpkg registry baseline：`cd61e1e26a038e82d6550a3ebbe0fbbfe7da78e3`

## 目的与边界

本记录满足 Stage 1 对 GPL 来源、版权和 dependency license 的初审门槛。它不是法律意见，也不是 release license scan、
SBOM、`THIRD_PARTY_NOTICES`、source archive、package contents audit、签名或发行批准。完整审计实际在 2026-08-12
的 `8edc700a91dc02fbe58833126954881a2ab0de22` 上完成，覆盖由 [Cargo.lock](../../Cargo.lock)、
[vcpkg manifest](../../vcpkg.json)、[Windows environment manifest](../../tools/windows_build_environment.json) 与
[OCR model manifest](../../spec/fixtures/vision/ocr-model.json) 固定的软件输入。上述 environment fingerprint 只标识
该次 license audit 使用的受控环境，不是 `df13db4c` H A 或本 R candidate A 的 credential。

`df13db4cb78c14602e05a31636a3e3a8f277f873` 相对 `8edc700a91dc02fbe58833126954881a2ab0de22`
只修改 validation/CI/guard/policy/runner 及其文档。2026-08-13 只读逐 blob 核验确认下列 19 个 fixed input 均相同；
因此 2026-08-12 实际形成的 41/17/2 清单、来源、版本和许可证选择继续适用于 approved H input。本文不声称在
`df13db4c` 上重新运行 Cargo metadata、prepared vendor/native closure 或 OCR materialized-artifact 审计。

| 固定输入 | 数量 | `8edc700a` -> `df13db4c` blob 核验 |
| --- | ---: | --- |
| `Cargo.lock`、`tests/hardware/Cargo.lock` | 2 | 全部相同 |
| root、8 个 `crates/*`、`tests/hardware`、`tests/support` 的 `Cargo.toml` | 11 | 全部相同 |
| `vcpkg.json`、`vcpkg-configuration.json` | 2 | 全部相同 |
| `tools/windows_build_environment.json` | 1 | 相同 |
| `spec/fixtures/vision/ocr-model.json`、`spec/fixtures/vision/ocr/manifest.json` | 2 | 全部相同 |
| `LICENSE` | 1 | 相同 |

仓库自有代码继续位于 `GPL-3.0-only` 边界，完整条款见 [LICENSE](../../LICENSE)。依据
[ADR-0001](../decisions/0001-source-boundary.md)，`EasyCon/` 是 ignored、只读第三方 reference source，不被修改、
tracked、加入 workspace、build、candidate 或 package；其中第三方内容保留自身许可证。本文没有改变 SDK 自有代码的
许可证边界。

## 2026-08-12 可复核方法与结果

初审只读取 tracked lock/manifests 和受控 Verify 已确认的 prepared environment metadata，不联网、不运行 Setup，且不在
本文记录机器绝对路径：

1. `cargo metadata --locked --offline --format-version 1` 解析全部 50 个 workspace/registry package；9 个本地
   workspace package 的 manifest license 均精确为 `GPL-3.0-only`。筛选 41 个 registry package，将唯一
   `name@version` 与 41 个 prepared `cargo-vendor` directory 双向比较，并逐项核对 `Cargo.lock` checksum、vendor
   `.cargo-checksum.json` package SHA-256、manifest license、registry source 和 `.cargo_vcs_info.json` VCS SHA。
2. 解析 prepared vcpkg `installed/vcpkg/status` 的 44 个 record；筛选 `x64-windows-static-md` 的 40 个 base/feature
   record，从 3 个 direct pin 沿 base 与 feature `Depends` 双向遍历，得到与 17 个 installed runtime base package
   完全相等的 closure。逐项解析 `share/<port>/vcpkg.spdx.json`，并核对非空 `copyright` text 与下表 SHA-256。
3. 将 tracked OCR manifest 的 path、URL、bytes、SHA-256 与 prepared model/license directory 双向比较；目录恰好包含
   1 个 model 和 1 个 license text。

2026-08-12 的结果为 workspace license 9/9、Rust registry 41/41 metadata/vendor/lock identities、native 17/17
closure/SPDX/copyright entries 和 OCR 2/2 manifest/materialized artifacts；没有漏项、孤立项、重复项或 hash mismatch。

## Rust registry packages（41）

下表所有 source 均为 `registry+https://github.com/rust-lang/crates.io-index`。crate SHA-256 来自 vendor
`.cargo-checksum.json`，VCS SHA 来自 `.cargo_vcs_info.json`；二者连同 `name@version` 可唯一回溯当前 vendored source。

| Package | License expression | Crate SHA-256 | VCS SHA |
| --- | --- | --- | --- |
| `aho-corasick@1.1.4` | `Unlicense OR MIT` | `ddd31a130427c27518df266943a5308ed92d4b226cc639f5a8f1002816174301` | `17f8b32e3b7c845ef3c5429b823804f552f14ec9` |
| `base64@0.22.1` | `MIT OR Apache-2.0` | `72b3254f16251a8381aa12e40e3c4d2f0199f8c6508fbecb9d91f575e0fbb8c6` | `e14400697453bcc85997119b874bc03d9601d0af` |
| `cc@1.2.67` | `MIT OR Apache-2.0` | `e17dd265a7d0f31ef544e1b20e03add05d3b45b491b633b10d67145d2acc1a38` | `fa031a077aca2b19a31de895eafb17e965f35c89` |
| `cfg-if@1.0.4` | `MIT OR Apache-2.0` | `9330f8b2ff13f34540b44e946ef35111825727b38d33286ef986142615121801` | `3510ca6abea34cbbc702509a4e50ea9709925eda` |
| `find-msvc-tools@0.1.9` | `MIT OR Apache-2.0` | `5baebc0774151f905a1a2cc41989300b1e6fbb29aff0ceffa1064fdd3088d582` | `0767349e1d1253e6849b4c2af2059db661f54343` |
| `generator@0.8.9` | `MIT/Apache-2.0` | `b3b854b0e584ead1a33f18b2fcad7cf7be18b3875c78816b753639aa501513ae` | `94b35ac4e39ee8b8d3b580a5044278b24b0648da` |
| `itoa@1.0.18` | `MIT OR Apache-2.0` | `8f42a60cbdf9a97f5d2305f08a87dc4e09308d1276d28c869c684d7777685682` | `af77385d0daf4d0e949e81f2588be2e44f69f086` |
| `lazy_static@1.5.0` | `MIT OR Apache-2.0` | `bbd2bcb4c963f2ddae06a2efc7e9f3591312473c50c6685e1f298068316e66fe` | `be7c1c43f264699f956b70ce8e29941bd1e61bde` |
| `libc@0.2.186` | `MIT OR Apache-2.0` | `68ab91017fe16c622486840e4c83c9a37afeff978bd239b5293d61ece587de66` | `42620ffc4109dc32e02f1cae9e63a3f4311b4b71` |
| `log@0.4.33` | `MIT OR Apache-2.0` | `0ceec5bc11778974d1bcb055b18002eba7f4b3518b6a0081b3af5f21666da9ad` | `f405739f3a15a3f00680c793e1e1fa7e57d26ba4` |
| `loom@0.7.2` | `MIT` | `419e0dc8046cb947daa77eb95ae174acfbddb7673b4151f56d1eed8e93fbfaca` | `a7033ee06a97c52eb2f8131c095fbea6c6eecba3` |
| `matchers@0.2.0` | `MIT` | `d1525a2a28c7f4fa0fc98bb91ae755d1e2d1505079e05539e35bc876b5d65ae9` | `a73b203f95b61113a2012ecada9ee287c1d8abee` |
| `memchr@2.8.3` | `Unlicense OR MIT` | `cf8baf1c55e62ffcace7a9f06f4bd9cd3f0c4beb022d3b367256b91b87513d98` | `5fdb40c054e1fff359a2f7bdf7f87a13b34b465d` |
| `nu-ansi-term@0.50.3` | `MIT` | `7957b9740744892f114936ab4a57b3f487491bbeafaf8083688b16841a4240e5` | `a23b71dae8efc17ca89d6bfe86134e15c3b157f5` |
| `once_cell@1.21.4` | `MIT OR Apache-2.0` | `9f7c3e4beb33f85d45ae3e3a1792185706c8e16d043238c593331cc7cd313b50` | `80fe900b21f6d76c1a2ed74d3343e8a3a88c46d0` |
| `pin-project-lite@0.2.17` | `Apache-2.0 OR MIT` | `a89322df9ebe1c1578d689c92318e070967d1042b512afbe49518723f4e6d5cd` | `3bdf763446aa78f90e3bdac1ef583e014832ab4c` |
| `proc-macro2@1.0.106` | `MIT OR Apache-2.0` | `8fd00f0bb2e90d81d1044c2b32617f68fcb9fa3bb7640c23e9c748e53fb30934` | `58ab776b95a4c2865554badbb6629c50971a9118` |
| `quote@1.0.46` | `MIT OR Apache-2.0` | `dfbc457d0c7a0759a614551b11a6409e5951f6c7537be1f1b7682b9ae9230368` | `bc4caf255fa9e58e025e5ff5a11ca948442c0f7a` |
| `regex-automata@0.4.16` | `MIT OR Apache-2.0` | `8fcfdb36bda0c880c5931cdc7a2bcdc8ba4556847b9d912bca70bc94708711ad` | `40e98238fff903f3e1ec95bbdb487185dd60504a` |
| `regex-syntax@0.8.11` | `MIT OR Apache-2.0` | `d6f6ff9a378485b298a5286656da665ba74413d36db0979633275d2e708145d4` | `140167995737fa11dfe11b8af8b9aa143b790b4e` |
| `rustversion@1.0.23` | `MIT OR Apache-2.0` | `cf54715a573b99ac80df0bc206da022bcd442c974952c7b9720069370852e21f` | `3a7c76605450b9a7299c6502a421909de9126a59` |
| `scoped-tls@1.0.1` | `MIT/Apache-2.0` | `e1cf6437eb19a8f4a6cc0f7dca544973b0b78843adbfeb3683d1a94a0024a294` | `c0ff7bf6d33e568353ed863d90f893e7e80a0ed1` |
| `serde@1.0.228` | `MIT OR Apache-2.0` | `9a8e94ea7f378bd32cbbd37198a4a91436180c5bb472411e48b5ec2e2124ae9e` | `a866b336f14aa57a07f0d0be9f8762746e64ecb4` |
| `serde_core@1.0.228` | `MIT OR Apache-2.0` | `41d385c7d4ca58e59fc732af25c3983b67ac852c1a25000afe1175de458b67ad` | `a866b336f14aa57a07f0d0be9f8762746e64ecb4` |
| `serde_derive@1.0.228` | `MIT OR Apache-2.0` | `d540f220d3187173da220f885ab66608367b6574e925011a9353e4badda91d79` | `a866b336f14aa57a07f0d0be9f8762746e64ecb4` |
| `serde_json@1.0.150` | `MIT OR Apache-2.0` | `e8014e44b4736ed0538adeecded0fce2a272f22dc9578a7eb6b2d9993c74cfb9` | `a1ae73ac6a6940a4a57c673aebaa13ed4dfe3e8c` |
| `sharded-slab@0.1.7` | `MIT` | `f40ca3c46823713e0d4209592e8d6e826aa57e928f09752619fc696c499637f6` | `40579b92debe2ef283a455eb379945e023080ff3` |
| `shlex@2.0.1` | `MIT OR Apache-2.0` | `f8fadd59c855ef2080decdef8ff161eb6661b86933c9d82e5ba29dc602a55aba` | `e82b1411beb7c92871c2c078c9ab415bbcf207ef` |
| `smallvec@1.15.2` | `MIT OR Apache-2.0` | `8ed6a63f02c8539c91a8685a86f4099661ba3da017932f6ebbea6de3f0fa7c90` | `c469051a1ba05ef1a03dd69e14b4a5aa329e6f10` |
| `syn@2.0.119` | `MIT OR Apache-2.0` | `872831b642d1a07999a962a351ed35b955ea2cfc8f3862091e2a240a84f17297` | `3295f9e9841785ac88a5e558c884854d5fb7d67f` |
| `thread_local@1.1.10` | `MIT OR Apache-2.0` | `1ad99c4c6d32803332c548b1af0540b357b3f5fc0be8f6c6bfe8b2e6ae784070` | `4724199250d3307824cefe8a0fa5b232d95edae3` |
| `tracing@0.1.44` | `MIT` | `63e71662fa4b2a2c3a26f570f037eb95bb1f85397f3cd8076caed2f026a6d100` | `2d55f6faf9be83e7e4634129fb96813241aac2b8` |
| `tracing-core@0.1.36` | `MIT` | `db97caf9d906fbde555dd62fa95ddba9eecfd14cb388e4f491a66d74cd5fb79a` | `10a9e838a35e6ded79d66af246be2ee05417136d` |
| `tracing-log@0.2.0` | `MIT` | `ee855f1f400bd0e5c02d150ae5de3840039a3f54b025156404e34c23c03f47c3` | `4161d8137d4f6117f17b110b0ec022d9350bf8e6` |
| `tracing-subscriber@0.3.23` | `MIT` | `cb7f578e5945fb242538965c2d0b04418d38ec25c79d160cd279bf0731c8d319` | `54ede4d5d85a536aed5485c5213011d9ec961935` |
| `unicode-ident@1.0.24` | `(MIT OR Apache-2.0) AND Unicode-3.0` | `e6e4313cd5fcd3dad5cafa179702e2b244f760991f45397d14d4ebf38247da75` | `5b54a632702b5744a1c40ea01c127c0ac0498172` |
| `valuable@0.1.1` | `MIT` | `ba73ea9cf16a25df0c8caa16c51acb937d5712a8429db78a3ee29d5dcacd3a65` | `9efc29b6e58cef28f6566a47aa7e142a55fead77` |
| `windows-link@0.2.1` | `MIT OR Apache-2.0` | `f0805222e57f7521d6a62e36fa9163bc891acd422f971defe97d64e70d0a4fe5` | `d468916ac27a36fb8a12bafc1bf5c0ec2fe92238` |
| `windows-result@0.4.1` | `MIT OR Apache-2.0` | `7781fa89eaf60850ac3d2da7af8e5242a5ea78d1a11c49bf2910bb5a73853eb5` | `32c3144490c016fe496a0aed769bce60987a2e9d` |
| `windows-sys@0.61.2` | `MIT OR Apache-2.0` | `ae137229bcbd6cdf0f7b80a31df61766145077ddf49416a728b02cb3921ff3fc` | `32c3144490c016fe496a0aed769bce60987a2e9d` |
| `zmij@1.0.23` | `MIT` | `29666d0abbfad1e3dc4dcf6144730dd3a3ab225bbbdac83319345b1b44ccfc1b` | `7b7cc48b58028e8af7be87e94c0c1c8936f1a57c` |

`valuable@0.1.1` 的 manifest 明确声明 `MIT`，registry checksum 和 VCS source 完整，README 指向上游
LICENSE；该 crate tarball/vendored tree 没有独立 `LICENSE` file。Stage 1 initial review 据 manifest 声明接纳它，
但 release notices/source archive 检查必须取得并固定权威 MIT text，不能把本记录描述为该发行义务已经完成。

## Native runtime dependencies（17）

17 项来自同一 `x64-windows-static-md` installed status closure。版本由 status 固定，source URL/SHA-512 由每项
`vcpkg.spdx.json` 记录，port recipe 由 registry baseline 固定；下表 hash 对应实际 installed `copyright` text。

| Package | Version | 初审许可证/选择 | Primary source ref | Copyright SHA-256 |
| --- | --- | --- | --- | --- |
| `zlib` | `1.3.2#1` | `Zlib` | `madler/zlib@v1.3.2` | `e32ff4e00d9d94930537635291da39e7e612703334bf6fde8c7f1686fe8a45a2` |
| `libpng` | `1.6.58` | `libpng-2.0` | `pnggroup/libpng@v1.6.58` | `bdb0a645ea18c60507d0368379b1ac5474b92255fcc2d115e07486a7672ba526` |
| `libjpeg-turbo` | `3.1.4.1` | `BSD-3-Clause` | `libjpeg-turbo/libjpeg-turbo@3.1.4.1` | `e10114e6e40f3d0311c401ca25245ac5ef459a43c20f976fd63f03e816f5741f` |
| `opencv4` | `4.12.0#5` | port 无 SPDX；installed text 为 Apache-2.0 主体并包含 SoftFloat/chi_table 3-clause BSD notices | `opencv/opencv@4.12.0` + `opencv_contrib@4.12.0` | `e5313db48cd388bb81b5f748aa9ba8b979e592e49ca7cc612ad8c4b01168f6f2` |
| `zstd` | `1.5.7` | `(BSD-3-Clause OR GPL-2.0-only)` 中明确选择 `BSD-3-Clause` branch | `facebook/zstd@v1.5.7` | `434dca949c6da7c500413aef694539fe37f867dd1a94d83d4ed1d260194e2660` |
| `liblzma` | `5.8.3` | port 无 SPDX；installed licensing 明确 runtime `liblzma` 为 `0BSD` | `tukaani-project/xz@v5.8.3` | `616a3ad264ce29b8f1cb97e53037b139d406899ca8d1f799651e17bfa09830b8` |
| `lz4` | `1.10.0` | `BSD-2-Clause` | `lz4/lz4@v1.10.0` | `8b58c446121a109ccf32edc094bba3010a3d85e4ee3702950db55e4d3e87736c` |
| `openssl` | `3.6.3` | `Apache-2.0` | `openssl/openssl@openssl-3.6.3` | `7d5450cb2d142651b8afa315b5f238efc805dad827d91ba367d8516bc9d49e7a` |
| `bzip2` | `1.0.8#6` | `bzip2-1.0.6` | `bzip2-1.0.8.tar.gz` | `c6dbbf828498be844a89eaa3b84adbab3199e342eb5cb2ed2f0d4ba7ec0f38a3` |
| `libarchive` | `3.8.7` | port 无 SPDX；installed text 为主 2-clause BSD-style、UC 3-clause portions、public-domain file，并对三许可文件选择 Apache-2.0 | `libarchive/libarchive@v3.8.7` | `30e556b3959e3985d66efefec5eaac51d4995053caa1d3cffe6eb916f146f229` |
| `tiff` | `4.7.1` | `libtiff` | `libtiff/libtiff@v4.7.1` | `0e27c2382d7b8147972bbb746e04059a1152c8d0fda9d03ef1399d1a433c4ade` |
| `openjpeg` | `2.5.4` | `BSD-2-Clause` | `uclouvain/openjpeg@v2.5.4` | `a6af136f3e15038a666b61f376612a07d9a4e48cb7c01adbf3e33b3f14ab49b6` |
| `libwebp` | `1.6.0#2` | `BSD-3-Clause` | `webmproject/libwebp@v1.6.0` | `050b5ba2c8eb0bd3b996e12ef79312a1c62237f2de1b3c609843f00bb74744f3` |
| `giflib` | `6.1.3` | `MIT` | `giflib-6.1.3.tar.gz` | `ed5d90cb4a041bddad679470a071302ab05ae5d0ec2cf8f9c97ad7b2708751e6` |
| `leptonica` | `1.87.0` | port 无 SPDX；installed text 为 2-clause BSD-style | `DanBloomberg/leptonica@1.87.0` | `87829abb5bbb00b55a107365da89e9a33f86c4250169e5a1e5588505be7d5806` |
| `curl` | `8.21.0` | `curl AND ISC AND BSD-3-Clause` | `curl/curl@curl-8_21_0`；SPDX source checksum 固定 | `543457a53893d439ac029f115c15940c0921ce1b919db501bbad266e2a4d1059` |
| `tesseract` | `5.5.2` | `Apache-2.0` | `tesseract-ocr/tesseract@5.5.2` | `cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30` |

direct pins 为 `opencv4@4.12.0#5`（port tree `0e28cd8713d7b810bef28ed0d7859dd761215854`）、
`tesseract@5.5.2`（port tree `ed497e10dafba7808ea1f33bd10954d3923396a8`）和 `leptonica@1.87.0`
（port tree `68a12d4a7e128ae8deed60ec8758db92b02f4a08`）；其余 14 项是该配置的实际传递 runtime closure。
vcpkg host helper packages 和 feature records 不计入 17 项 runtime base package。

## OCR 测试模型（2 项：1 个 model + 1 个 license text）

该输入只供 Vision/Stage 1 OCR component tests 使用，不 tracked、不 packaged，物化到 ignored controlled storage。

| Artifact | Source | Bytes | SHA-256 | License |
| --- | --- | ---: | --- | --- |
| `eng.traineddata` | `tesseract-ocr/tessdata_fast` tag `4.1.0` 的 tracked manifest URL | 4,113,088 | `7d4322bd2a7749724879683fc3912cb542f19906c83bcc1a52132556427170b2` | 由下一行 Apache-2.0 text 覆盖 |
| `LICENSE` | 同一 tag 的 tracked manifest URL | 11,358 | `cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30` | `Apache-2.0` |

这不是 O-03 的 official package model qualification。默认接口语言、未来 `chi_sim` 或其他 model 的来源、质量、版权、
大小和 package strategy 仍须在 release 前独立关闭。

## 初审结论与未来门槛

在 2026-08-12 实际复核的 workspace 9/9 和 dependency 41/17/2 closure 中，没有发现缺失 license declaration、
unknown source 或已知与 `GPL-3.0-only` incompatible 的 input；19 个固定 tracked blob 在 approved H input 上连续，
因此 Stage 1 initial-review gate 继续适用。该结论只表示没有触发本阶段的 fail-closed blocker，不替代逐文件、逐 package
和 release legal review，也不构成本 H/R 的 A evidence。

future release 必须重新从最终 package closure 生成并复核 per-package notices、SBOM、corresponding source/source
archive、model manifest、checksum 与 signing；核对所有实际 shipped binaries/data，而不是复用本 initial-review count。
`valuable` authoritative MIT text、OpenCV bundled notices、libarchive per-file combination、official OCR model 以及任何
新增 dependency 均是 release review explicit gate。若后续发现 missing license、unknown source 或 known incompatibility，
candidate 必须立即 fail closed，不得用本记录继续通过。

## 关联

- [ADR-0026：Controller D1 settlement 实现重新冻结候选](../decisions/0026-controller-d1-settlement-refreeze.md)
- [ADR-0027：Stage 1 Runtime/Controller/Vision 软件核心收口候选](../decisions/0027-stage-1-software-core-closeout.md)
- [构建、发布与合规](../architecture/build-release.md)
