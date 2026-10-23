# 第三方依赖许可清单（THIRD-PARTY LICENSES）

本文件由 `tools/gen_third_party_licenses.py` 依据 `Cargo.lock`（733 个第三方 crate）
与 `web-ui/package-lock.json`（运行时 npm 依赖）生成，列出每个依赖声明的 SPDX 许可。
本项目以 **Apache-2.0** 发布（见 `LICENSE`）；UNKNOWN 表示上游元数据缺失，需人工核对上游仓库。

## Rust（Cargo.lock）

共 733 个包。按许可归组：

- **Apache-2.0（含 MIT/BSD/ISC 双许可）**：495
- **MIT**：191
- **Unicode-DFS**：18
- **BSD/ISC**：10
- **MPL-2.0（文件级弱 Copyleft，兼容，见个案说明）**：6
- **Copyleft 双许可（选 Apache/MIT 分支，见个案说明）**：3
- **Public domain（Unlicense/CC0）**：3
- **zlib**：3
- **Apache-2.0 双许可（含 BSL 分支，选 Apache，见个案说明）**：2
- **CDLA-Permissive（数据许可，兼容，见个案说明）**：2

<details><summary>逐包清单（名称 / 版本 / 声明许可）</summary>

| 包 | 版本 | 许可 |
|---|---|---|
| adler2 | 2.0.1 | 0BSD OR MIT OR Apache-2.0 |
| adobe-cmap-parser | 0.4.1 | MIT |
| aes | 0.8.4 | MIT OR Apache-2.0 |
| ahash | 0.8.12 | MIT OR Apache-2.0 |
| aho-corasick | 1.1.5 | Unlicense OR MIT |
| alloc-no-stdlib | 3.0.0 | BSD-3-Clause |
| alloc-stdlib | 0.3.0 | BSD-3-Clause |
| allocator-api2 | 0.2.21 | MIT OR Apache-2.0 |
| android_system_properties | 0.1.6 | MIT OR Apache-2.0 |
| anyhow | 1.0.104 | MIT OR Apache-2.0 |
| ar_archive_writer | 0.5.3 | Apache-2.0 WITH LLVM-exception |
| arc-swap | 1.9.2 | MIT OR Apache-2.0 |
| arrayvec | 0.7.8 | MIT OR Apache-2.0 |
| ascii | 1.1.0 | Apache-2.0 OR MIT |
| ast_node | 5.0.0 | Apache-2.0 |
| async-lock | 3.4.2 | Apache-2.0 OR MIT |
| async-trait | 0.1.92 | MIT OR Apache-2.0 |
| asyncband | 0.7.3 | Apache-2.0 |
| atk | 0.18.2 | MIT |
| atk-sys | 0.18.2 | MIT |
| atomic-waker | 1.1.2 | Apache-2.0 OR MIT |
| autocfg | 1.5.1 | Apache-2.0 OR MIT |
| aws-lc-rs | 1.18.1 | ISC AND (Apache-2.0 OR ISC) |
| aws-lc-sys | 0.45.0 | ISC AND (Apache-2.0 OR ISC) AND Apache-2.0 AND MIT AND BSD-3-Clause AND (Apache-2.0 OR ISC OR MIT) AND (Apache-2.0 OR ISC OR MIT-0) |
| axum | 0.7.9 | MIT |
| axum-core | 0.4.5 | MIT |
| base64 | 0.21.7 | MIT OR Apache-2.0 |
| base64 | 0.22.1 | MIT OR Apache-2.0 |
| base64 | 0.23.1 | MIT OR Apache-2.0 |
| base64-simd | 0.8.0 | MIT |
| base64ct | 1.8.3 | Apache-2.0 OR MIT |
| better_scoped_tls | 1.0.1 | Apache-2.0 |
| bindgen | 0.72.1 | BSD-3-Clause |
| bit-set | 0.8.0 | Apache-2.0 OR MIT |
| bit-vec | 0.8.0 | Apache-2.0 OR MIT |
| bitflags | 1.3.2 | MIT/Apache-2.0 |
| bitflags | 2.13.2 | MIT OR Apache-2.0 |
| bitvec | 1.1.1 | MIT |
| block-buffer | 0.10.4 | MIT OR Apache-2.0 |
| block-buffer | 0.12.1 | MIT OR Apache-2.0 |
| block-padding | 0.3.3 | MIT OR Apache-2.0 |
| block2 | 0.6.2 | MIT |
| borrow-or-share | 0.2.4 | MIT-0 |
| borsh | 1.8.1 | MIT OR Apache-2.0 |
| borsh-derive | 1.8.1 | Apache-2.0 |
| brotli | 9.0.0 | BSD-3-Clause AND MIT |
| brotli-decompressor | 6.0.1 | BSD-3-Clause/MIT |
| bs58 | 0.5.1 | MIT/Apache-2.0 |
| bumpalo | 3.20.3 | MIT OR Apache-2.0 |
| bytecount | 0.6.9 | Apache-2.0/MIT |
| bytemuck | 1.25.2 | Zlib OR Apache-2.0 OR MIT |
| byteorder | 1.5.0 | Unlicense OR MIT |
| bytes | 1.12.1 | MIT |
| bytes-str | 0.2.9 | Apache-2.0 |
| cairo-rs | 0.18.5 | MIT |
| cairo-sys-rs | 0.18.2 | MIT |
| camino | 1.2.6 | MIT OR Apache-2.0 |
| cargo-platform | 0.1.9 | MIT OR Apache-2.0 |
| cargo_metadata | 0.19.2 | MIT |
| cargo_toml | 1.0.1 | Apache-2.0 OR MIT |
| castaway | 0.2.4 | MIT |
| cbc | 0.1.2 | MIT OR Apache-2.0 |
| cc | 1.6.0 | MIT OR Apache-2.0 |
| cesu8 | 1.1.0 | Apache-2.0/MIT |
| cexpr | 0.6.0 | Apache-2.0/MIT |
| cfb | 0.14.0 | MIT |
| cff-parser | 0.1.0 | MIT OR Apache-2.0 |
| cfg-expr | 0.15.8 | MIT OR Apache-2.0 |
| cfg-if | 1.0.5 | MIT OR Apache-2.0 |
| cfg_aliases | 0.2.2 | MIT |
| chacha20 | 0.10.2 | MIT OR Apache-2.0 |
| chrono | 0.4.45 | MIT OR Apache-2.0 |
| chrono-tz | 0.10.4 | MIT OR Apache-2.0 |
| cipher | 0.4.4 | MIT OR Apache-2.0 |
| clang-sys | 1.9.1 | Apache-2.0 |
| cmake | 0.1.58 | MIT OR Apache-2.0 |
| cmov | 0.5.4 | Apache-2.0 OR MIT |
| combine | 4.6.8 | MIT |
| compact_str | 0.7.1 | MIT |
| const-oid | 0.10.2 | Apache-2.0 OR MIT |
| const-oid | 0.9.6 | Apache-2.0 OR MIT |
| const-random | 0.1.18 | MIT OR Apache-2.0 |
| const-random-macro | 0.1.16 | MIT OR Apache-2.0 |
| convert_case | 0.8.0 | MIT |
| cookie | 0.18.2 | MIT OR Apache-2.0 |
| core-foundation | 0.10.1 | MIT OR Apache-2.0 |
| core-foundation | 0.9.4 | MIT OR Apache-2.0 |
| core-foundation-sys | 0.8.7 | MIT OR Apache-2.0 |
| core-graphics | 0.25.0 | MIT OR Apache-2.0 |
| core-graphics-types | 0.2.0 | MIT OR Apache-2.0 |
| core_detect | 1.0.0 | MIT/Apache-2.0 |
| cpufeatures | 0.2.17 | MIT OR Apache-2.0 |
| cpufeatures | 0.3.1 | MIT OR Apache-2.0 |
| crc32fast | 1.5.2 | MIT OR Apache-2.0 |
| crossbeam-channel | 0.5.17 | MIT OR Apache-2.0 |
| crossbeam-utils | 0.8.23 | MIT OR Apache-2.0 |
| crunchy | 0.2.4 | MIT |
| crypto-common | 0.1.7 | MIT OR Apache-2.0 |
| crypto-common | 0.2.2 | MIT OR Apache-2.0 |
| cssparser | 0.37.0 | MPL-2.0 |
| cssparser-macros | 0.7.1 | MPL-2.0 |
| ctor | 1.0.13 | Apache-2.0 OR MIT |
| ctutils | 0.4.3 | Apache-2.0 OR MIT |
| darling | 0.24.1 | MIT |
| darling_core | 0.24.1 | MIT |
| darling_macro | 0.24.1 | MIT |
| data-encoding | 2.11.1 | MIT |
| dbus | 0.9.12 | Apache-2.0/MIT |
| debugid | 0.8.0 | Apache-2.0 |
| defmt | 1.1.1 | MIT OR Apache-2.0 |
| defmt-macros | 1.1.1 | MIT OR Apache-2.0 |
| defmt-parser | 1.0.0 | MIT OR Apache-2.0 |
| der | 0.7.10 | Apache-2.0 OR MIT |
| deranged | 0.5.8 | MIT OR Apache-2.0 |
| derive_more | 2.1.1 | MIT |
| derive_more-impl | 2.1.1 | MIT |
| digest | 0.10.7 | MIT OR Apache-2.0 |
| digest | 0.11.3 | MIT OR Apache-2.0 |
| dirs | 7.0.0 | MIT OR Apache-2.0 |
| dirs-sys | 0.5.0 | MIT OR Apache-2.0 |
| dispatch2 | 0.3.1 | Zlib OR Apache-2.0 OR MIT |
| displaydoc | 0.2.7 | MIT OR Apache-2.0 |
| dlopen2 | 0.8.2 | MIT |
| dlopen2_derive | 0.4.3 | MIT |
| dlv-list | 0.5.2 | MIT OR Apache-2.0 |
| dom_query | 0.28.0 | MIT |
| downcast-rs | 2.0.2 | MIT OR Apache-2.0 |
| dpi | 0.1.2 | Apache-2.0 AND MIT |
| dragonbox_ecma | 0.1.12 | Apache-2.0 WITH LLVM-exception OR BSL-1.0 |
| dtoa | 1.0.11 | MIT OR Apache-2.0 |
| dtoa-short | 0.3.5 | MPL-2.0 |
| dunce | 1.0.5 | CC0-1.0 OR MIT-0 OR Apache-2.0 |
| dyn-clone | 1.0.20 | MIT OR Apache-2.0 |
| either | 1.19.0 | MIT OR Apache-2.0 |
| email_address | 0.2.9 | MIT |
| embed-resource | 3.0.12 | MIT |
| embed_plist | 1.2.2 | MIT OR Apache-2.0 |
| encoding_rs | 0.8.42 | (Apache-2.0 OR MIT) AND BSD-3-Clause |
| equivalent | 1.0.2 | Apache-2.0 OR MIT |
| erased-serde | 0.4.10 | MIT OR Apache-2.0 |
| errno | 0.3.14 | MIT OR Apache-2.0 |
| euclid | 0.20.14 | MIT / Apache-2.0 |
| event-listener | 5.4.2 | Apache-2.0 OR MIT |
| event-listener-strategy | 0.5.4 | Apache-2.0 OR MIT |
| eventsource-stream | 0.2.3 | MIT OR Apache-2.0 |
| fallible-iterator | 0.3.0 | MIT/Apache-2.0 |
| fallible-streaming-iterator | 0.1.9 | MIT/Apache-2.0 |
| fancy-regex | 0.19.2 | MIT |
| fastrand | 2.5.0 | Apache-2.0 OR MIT |
| fdeflate | 0.3.7 | MIT OR Apache-2.0 |
| field-offset | 0.3.6 | MIT OR Apache-2.0 |
| find-msvc-tools | 0.1.14 | MIT OR Apache-2.0 |
| fixedbitset | 0.5.7 | MIT OR Apache-2.0 |
| flate2 | 1.1.10 | MIT OR Apache-2.0 |
| fluent-uri | 0.4.1 | MIT |
| fnv | 1.0.7 | Apache-2.0 / MIT |
| foldhash | 0.1.5 | Zlib |
| foldhash | 0.2.0 | Zlib |
| foreign-types | 0.3.2 | MIT/Apache-2.0 |
| foreign-types | 0.5.0 | MIT/Apache-2.0 |
| foreign-types-macros | 0.2.4 | MIT/Apache-2.0 |
| foreign-types-shared | 0.1.1 | MIT/Apache-2.0 |
| foreign-types-shared | 0.3.1 | MIT/Apache-2.0 |
| form_urlencoded | 1.2.2 | MIT OR Apache-2.0 |
| fraction | 0.15.4 | MIT OR Apache-2.0 |
| from_variant | 3.0.0 | Apache-2.0 |
| fs2 | 0.4.3 | MIT/Apache-2.0 |
| fs_extra | 1.3.0 | MIT |
| funty | 2.0.0 | MIT |
| futures | 0.3.34 | MIT OR Apache-2.0 |
| futures-channel | 0.3.34 | MIT OR Apache-2.0 |
| futures-core | 0.3.34 | MIT OR Apache-2.0 |
| futures-executor | 0.3.34 | MIT OR Apache-2.0 |
| futures-io | 0.3.34 | MIT OR Apache-2.0 |
| futures-macro | 0.3.34 | MIT OR Apache-2.0 |
| futures-sink | 0.3.34 | MIT OR Apache-2.0 |
| futures-task | 0.3.34 | MIT OR Apache-2.0 |
| futures-timer | 3.0.4 | MIT/Apache-2.0 |
| futures-util | 0.3.34 | MIT OR Apache-2.0 |
| gdk | 0.18.2 | MIT |
| gdk-pixbuf | 0.18.5 | MIT |
| gdk-pixbuf-sys | 0.18.0 | MIT |
| gdk-sys | 0.18.2 | MIT |
| gdkwayland-sys | 0.18.2 | MIT |
| gdkx11 | 0.18.2 | MIT |
| gdkx11-sys | 0.18.2 | MIT |
| generator | 0.8.10 | MIT/Apache-2.0 |
| generic-array | 0.14.7 | MIT |
| getrandom | 0.2.17 | MIT OR Apache-2.0 |
| getrandom | 0.3.4 | MIT OR Apache-2.0 |
| getrandom | 0.4.3 | MIT OR Apache-2.0 |
| gio | 0.18.4 | MIT |
| gio-sys | 0.18.1 | MIT |
| glib | 0.18.5 | MIT |
| glib-macros | 0.18.5 | MIT |
| glib-sys | 0.18.1 | MIT |
| glob | 0.3.4 | MIT OR Apache-2.0 |
| gobject-sys | 0.18.0 | MIT |
| gtk | 0.18.2 | MIT |
| gtk-sys | 0.18.2 | MIT |
| gtk3-macros | 0.18.2 | MIT |
| h2 | 0.4.20 | MIT |
| hashbrown | 0.12.3 | MIT OR Apache-2.0 |
| hashbrown | 0.14.5 | MIT OR Apache-2.0 |
| hashbrown | 0.15.5 | MIT OR Apache-2.0 |
| hashbrown | 0.16.1 | MIT OR Apache-2.0 |
| hashbrown | 0.17.1 | MIT OR Apache-2.0 |
| hashlink | 0.9.1 | MIT OR Apache-2.0 |
| heck | 0.4.1 | MIT OR Apache-2.0 |
| heck | 0.5.0 | MIT OR Apache-2.0 |
| hermit-abi | 0.5.3 | MIT OR Apache-2.0 |
| hex | 0.4.3 | MIT OR Apache-2.0 |
| hipstr | 0.8.0 | MIT OR Apache-2.0 |
| hmac | 0.12.1 | MIT OR Apache-2.0 |
| hmac | 0.13.0 | MIT OR Apache-2.0 |
| hstr | 3.0.6 | Apache-2.0 |
| html5ever | 0.39.0 | MIT OR Apache-2.0 |
| http | 1.5.0 | MIT OR Apache-2.0 |
| http-body | 1.1.0 | MIT |
| http-body-util | 0.1.5 | MIT |
| httparse | 1.10.1 | MIT OR Apache-2.0 |
| httpdate | 1.0.3 | MIT OR Apache-2.0 |
| humantime | 2.4.0 | MIT OR Apache-2.0 |
| hybrid-array | 0.4.15 | MIT OR Apache-2.0 |
| hyper | 1.12.0 | MIT |
| hyper-rustls | 0.27.10 | Apache-2.0 OR ISC OR MIT |
| hyper-tls | 0.6.0 | MIT/Apache-2.0 |
| hyper-util | 0.1.21 | MIT |
| iana-time-zone | 0.1.65 | MIT OR Apache-2.0 |
| iana-time-zone-haiku | 0.1.2 | MIT OR Apache-2.0 |
| ico | 0.5.0 | MIT |
| icu_collections | 2.3.0 | Unicode-3.0 |
| icu_locale_core | 2.3.0 | Unicode-3.0 |
| icu_normalizer | 2.3.0 | Unicode-3.0 |
| icu_normalizer_data | 2.3.0 | Unicode-3.0 |
| icu_properties | 2.3.0 | Unicode-3.0 |
| icu_properties_data | 2.3.0 | Unicode-3.0 |
| icu_provider | 2.3.1 | Unicode-3.0 |
| ident_case | 1.0.1 | MIT/Apache-2.0 |
| idna | 1.1.0 | MIT OR Apache-2.0 |
| idna_adapter | 1.2.2 | Apache-2.0 OR MIT |
| if_chain | 1.0.3 | MIT/Apache-2.0 |
| indexmap | 1.9.3 | Apache-2.0 OR MIT |
| indexmap | 2.14.2 | Apache-2.0 OR MIT |
| infer | 0.22.0 | MIT |
| inout | 0.1.4 | MIT OR Apache-2.0 |
| ipnet | 2.12.2 | MIT OR Apache-2.0 |
| is-macro | 0.3.8 | Apache-2.0 |
| itertools | 0.13.0 | MIT OR Apache-2.0 |
| itoa | 1.0.18 | MIT OR Apache-2.0 |
| javascriptcore-rs | 1.1.2 | MIT |
| javascriptcore-rs-sys | 1.1.1 | MIT |
| jiff | 0.2.38 | Unlicense OR MIT |
| jiff-core | 0.1.1 | Unlicense OR MIT |
| jiff-static | 0.2.38 | Unlicense OR MIT |
| jiff-tzdb | 0.1.9 | Unlicense OR MIT |
| jiff-tzdb-platform | 0.1.3 | Unlicense OR MIT |
| jni | 0.21.1 | MIT/Apache-2.0 |
| jni | 0.22.4 | MIT OR Apache-2.0 |
| jni-macros | 0.22.4 | MIT OR Apache-2.0 |
| jni-sys | 0.3.1 | MIT OR Apache-2.0 |
| jni-sys | 0.4.1 | MIT OR Apache-2.0 |
| jni-sys-macros | 0.4.1 | MIT OR Apache-2.0 |
| jobserver | 0.1.35 | MIT OR Apache-2.0 |
| js-sys | 0.3.106 | MIT OR Apache-2.0 |
| json-patch | 4.2.0 | MIT/Apache-2.0 |
| json_dotpath | 1.1.0 | MIT |
| jsonptr | 0.7.1 | MIT OR Apache-2.0 |
| jsonschema | 0.49.9 | MIT |
| jsonschema-regex | 0.49.9 | MIT |
| jsonschema-value | 0.49.9 | MIT |
| kameleoon-reqwest-eventsource | 0.6.2 | MIT OR Apache-2.0 |
| keyboard-types | 0.8.3 | MIT OR Apache-2.0 |
| lazy_static | 1.5.1 | MIT OR Apache-2.0 |
| libappindicator | 0.9.0 | Apache-2.0 OR MIT |
| libappindicator-sys | 0.9.0 | Apache-2.0 OR MIT |
| libc | 0.2.190 | MIT OR Apache-2.0 |
| libdbus-sys | 0.2.7 | Apache-2.0/MIT |
| libloading | 0.7.4 | ISC |
| libloading | 0.8.9 | ISC |
| libm | 0.2.16 | MIT |
| libredox | 0.1.25 | MIT |
| libsqlite3-sys | 0.28.0 | MIT |
| linux-raw-sys | 0.12.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| litemap | 0.8.3 | Unicode-3.0 |
| lock_api | 0.4.14 | MIT OR Apache-2.0 |
| log | 0.4.34 | MIT OR Apache-2.0 |
| loom | 0.7.2 | MIT |
| lopdf | 0.34.0 | MIT |
| lru-slab | 0.1.3 | MIT OR Apache-2.0 OR Zlib |
| markup5ever | 0.39.0 | MIT OR Apache-2.0 |
| matchers | 0.2.0 | MIT |
| matchit | 0.7.3 | MIT AND BSD-3-Clause |
| md-5 | 0.10.6 | MIT OR Apache-2.0 |
| memchr | 2.8.3 | Unlicense OR MIT |
| memoffset | 0.9.1 | MIT |
| micromap | 0.3.0 | MIT |
| mime | 0.3.17 | MIT OR Apache-2.0 |
| minimal-lexical | 0.2.1 | MIT/Apache-2.0 |
| miniz_oxide | 0.8.9 | MIT OR Zlib OR Apache-2.0 |
| miniz_oxide | 0.9.1 | MIT OR Zlib OR Apache-2.0 |
| mio | 1.2.4 | MIT |
| muda | 0.20.0 | Apache-2.0 OR MIT |
| multiversion_no_op | 1.0.0 | Apache-2.0 OR MIT |
| native-tls | 0.2.18 | MIT OR Apache-2.0 |
| ndk | 0.9.0 | MIT OR Apache-2.0 |
| ndk-context | 0.1.1 | MIT OR Apache-2.0 |
| ndk-sys | 0.6.0+11769913 | MIT OR Apache-2.0 |
| new_debug_unreachable | 1.0.6 | MIT |
| nohash-hasher | 0.2.0 | Apache-2.0 OR MIT |
| nom | 7.1.3 | MIT |
| nu-ansi-term | 0.50.3 | MIT |
| num | 0.4.3 | MIT OR Apache-2.0 |
| num-bigint | 0.4.8 | MIT OR Apache-2.0 |
| num-bigint-dig | 0.8.6 | MIT/Apache-2.0 |
| num-cmp | 0.1.0 | MIT/Apache-2.0 |
| num-complex | 0.4.6 | MIT OR Apache-2.0 |
| num-conv | 0.2.2 | MIT OR Apache-2.0 |
| num-integer | 0.1.47 | MIT OR Apache-2.0 |
| num-iter | 0.1.46 | MIT OR Apache-2.0 |
| num-rational | 0.4.2 | MIT OR Apache-2.0 |
| num-traits | 0.2.19 | MIT OR Apache-2.0 |
| num_cpus | 1.17.0 | MIT OR Apache-2.0 |
| num_enum | 0.7.6 | BSD-3-Clause OR MIT OR Apache-2.0 |
| num_enum_derive | 0.7.6 | BSD-3-Clause OR MIT OR Apache-2.0 |
| objc2 | 0.6.5 | MIT |
| objc2-app-kit | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-cloud-kit | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-core-data | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-core-foundation | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-core-graphics | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-core-image | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-core-location | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-core-text | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-encode | 4.1.0 | MIT |
| objc2-exception-helper | 0.1.1 | Zlib OR Apache-2.0 OR MIT |
| objc2-foundation | 0.3.2 | MIT |
| objc2-io-surface | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-quartz-core | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-ui-kit | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-user-notifications | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-web-kit | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| object | 0.39.1 | Apache-2.0 OR MIT |
| once_cell | 1.21.4 | MIT OR Apache-2.0 |
| openssl | 0.10.81 | Apache-2.0 |
| openssl-macros | 0.1.1 | MIT/Apache-2.0 |
| openssl-probe | 0.2.1 | MIT OR Apache-2.0 |
| openssl-sys | 0.9.117 | MIT |
| option-ext | 0.2.0 | MPL-2.0 |
| ordered-multimap | 0.7.3 | MIT |
| outref | 0.5.2 | MIT |
| pango | 0.18.3 | MIT |
| pango-sys | 0.18.0 | MIT |
| par-core | 2.0.0 | Apache-2.0 |
| parking_lot | 0.12.5 | MIT OR Apache-2.0 |
| parking_lot_core | 0.9.12 | MIT OR Apache-2.0 |
| pbkdf2 | 0.12.2 | MIT OR Apache-2.0 |
| pdf-extract | 0.8.2 | MIT |
| pem | 4.0.0 | MIT |
| pem-rfc7468 | 0.7.0 | Apache-2.0 OR MIT |
| percent-encoding | 2.3.2 | MIT OR Apache-2.0 |
| petgraph | 0.8.3 | MIT OR Apache-2.0 |
| phf | 0.11.3 | MIT |
| phf | 0.12.1 | MIT |
| phf | 0.13.1 | MIT |
| phf_codegen | 0.13.1 | MIT |
| phf_generator | 0.11.3 | MIT |
| phf_generator | 0.13.1 | MIT |
| phf_macros | 0.11.3 | MIT |
| phf_macros | 0.13.1 | MIT |
| phf_shared | 0.11.3 | MIT |
| phf_shared | 0.12.1 | MIT |
| phf_shared | 0.13.1 | MIT |
| pin-project-lite | 0.2.17 | Apache-2.0 OR MIT |
| pkcs1 | 0.7.5 | Apache-2.0 OR MIT |
| pkcs5 | 0.7.1 | Apache-2.0 OR MIT |
| pkcs8 | 0.10.2 | Apache-2.0 OR MIT |
| pkg-config | 0.3.34 | MIT OR Apache-2.0 |
| plist | 1.10.1 | MIT |
| png | 0.17.16 | MIT OR Apache-2.0 |
| png | 0.18.1 | MIT OR Apache-2.0 |
| pom | 1.1.0 | MIT |
| portable-atomic | 1.15.0 | Apache-2.0 OR MIT |
| portable-atomic-util | 0.2.8 | Apache-2.0 OR MIT |
| postscript | 0.14.1 | Apache-2.0/MIT |
| potential_utf | 0.1.6 | Unicode-3.0 |
| powerfmt | 0.2.1 | MIT OR Apache-2.0 |
| ppv-lite86 | 0.2.21 | MIT OR Apache-2.0 |
| precomputed-hash | 0.1.1 | MIT |
| prettyplease | 0.2.37 | MIT OR Apache-2.0 |
| proc-macro-crate | 1.3.1 | MIT OR Apache-2.0 |
| proc-macro-crate | 2.0.2 | MIT OR Apache-2.0 |
| proc-macro-crate | 3.5.0 | MIT OR Apache-2.0 |
| proc-macro-error | 1.0.4 | MIT OR Apache-2.0 |
| proc-macro-error-attr | 1.0.4 | MIT OR Apache-2.0 |
| proc-macro2 | 1.0.107 | MIT OR Apache-2.0 |
| psm | 0.1.32 | MIT OR Apache-2.0 |
| quick-xml | 0.42.0 | MIT |
| quinn | 0.11.12 | MIT OR Apache-2.0 |
| quinn-proto | 0.11.19 | MIT OR Apache-2.0 |
| quinn-udp | 0.5.16 | MIT OR Apache-2.0 |
| quote | 1.0.47 | MIT OR Apache-2.0 |
| r-efi | 5.3.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later |
| r-efi | 6.0.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later |
| radium | 0.7.0 | MIT |
| rand | 0.10.3 | MIT OR Apache-2.0 |
| rand | 0.8.8 | MIT OR Apache-2.0 |
| rand | 0.9.5 | MIT OR Apache-2.0 |
| rand_chacha | 0.3.1 | MIT OR Apache-2.0 |
| rand_chacha | 0.9.0 | MIT OR Apache-2.0 |
| rand_core | 0.10.1 | MIT OR Apache-2.0 |
| rand_core | 0.6.4 | MIT OR Apache-2.0 |
| rand_core | 0.9.5 | MIT OR Apache-2.0 |
| rand_pcg | 0.10.2 | MIT OR Apache-2.0 |
| rangemap | 1.8.0 | MIT/Apache-2.0 |
| raw-window-handle | 0.6.2 | MIT OR Apache-2.0 OR Zlib |
| recursive | 0.1.1 | MIT |
| recursive-proc-macro-impl | 0.1.1 | MIT |
| redox_syscall | 0.5.18 | MIT |
| redox_users | 0.5.3 | MIT |
| ref-cast | 1.0.27 | MIT OR Apache-2.0 |
| ref-cast-impl | 1.0.27 | MIT OR Apache-2.0 |
| referencing | 0.49.9 | MIT |
| regex | 1.13.1 | MIT OR Apache-2.0 |
| regex-automata | 0.4.18 | MIT OR Apache-2.0 |
| regex-syntax | 0.8.11 | MIT OR Apache-2.0 |
| relative-path | 2.0.2 | MIT OR Apache-2.0 |
| reqsign | 0.20.7 | Apache-2.0 |
| reqsign-aws-core | 3.2.0 | Apache-2.0 |
| reqsign-aws-v4 | 3.3.1 | Apache-2.0 |
| reqsign-azure-storage | 3.3.0 | Apache-2.0 |
| reqsign-command-execute-tokio | 3.0.7 | Apache-2.0 |
| reqsign-core | 3.3.2 | Apache-2.0 |
| reqsign-file-read-tokio | 3.0.7 | Apache-2.0 |
| reqsign-google | 3.2.0 | Apache-2.0 |
| reqsign-http-send-reqwest | 4.0.7 | Apache-2.0 |
| reqwest | 0.12.28 | MIT OR Apache-2.0 |
| reqwest | 0.13.5 | MIT OR Apache-2.0 |
| rfd | 0.16.0 | MIT |
| ring | 0.17.14 | Apache-2.0 AND ISC |
| rquickjs | 0.10.0 | MIT |
| rquickjs-core | 0.10.0 | MIT |
| rquickjs-macro | 0.10.0 | MIT |
| rquickjs-sys | 0.10.0 | MIT |
| rsa | 0.9.10 | MIT OR Apache-2.0 |
| rusqlite | 0.31.0 | MIT |
| rust-ini | 0.21.3 | MIT |
| rust_decimal | 1.43.0 | MIT |
| rust_decimal_macros | 1.40.0 | MIT |
| rustc-hash | 2.1.3 | Apache-2.0 OR MIT |
| rustc_version | 0.4.1 | MIT OR Apache-2.0 |
| rustix | 1.1.5 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| rustls | 0.23.45 | Apache-2.0 OR ISC OR MIT |
| rustls-native-certs | 0.8.4 | Apache-2.0 OR ISC OR MIT |
| rustls-pki-types | 1.15.1 | MIT OR Apache-2.0 |
| rustls-platform-verifier | 0.7.1 | MIT OR Apache-2.0 |
| rustls-platform-verifier-android | 0.2.0 | MIT OR Apache-2.0 |
| rustls-webpki | 0.103.15 | ISC |
| rustversion | 1.0.23 | MIT OR Apache-2.0 |
| ryu | 1.0.23 | Apache-2.0 OR BSL-1.0 |
| salsa20 | 0.10.2 | MIT OR Apache-2.0 |
| same-file | 1.0.6 | Unlicense/MIT |
| schannel | 0.1.29 | MIT |
| schemars | 0.8.22 | MIT |
| schemars | 0.9.0 | MIT |
| schemars | 1.2.2 | MIT |
| schemars_derive | 0.8.22 | MIT |
| scoped-tls | 1.0.1 | MIT/Apache-2.0 |
| scopeguard | 1.2.0 | MIT OR Apache-2.0 |
| scrypt | 0.11.0 | MIT OR Apache-2.0 |
| security-framework | 3.7.0 | MIT OR Apache-2.0 |
| security-framework-sys | 2.17.0 | MIT OR Apache-2.0 |
| selectors | 0.38.0 | MPL-2.0 |
| self_cell | 1.3.0 | Apache-2.0 OR GPL-2.0-only |
| semver | 1.0.28 | MIT OR Apache-2.0 |
| seq-macro | 0.3.6 | MIT OR Apache-2.0 |
| serde | 1.0.229 | MIT OR Apache-2.0 |
| serde-untagged | 0.1.9 | MIT OR Apache-2.0 |
| serde_core | 1.0.229 | MIT OR Apache-2.0 |
| serde_derive | 1.0.229 | MIT OR Apache-2.0 |
| serde_derive_internals | 0.29.1 | MIT OR Apache-2.0 |
| serde_json | 1.0.151 | MIT OR Apache-2.0 |
| serde_path_to_error | 0.1.20 | MIT OR Apache-2.0 |
| serde_repr | 0.1.21 | MIT OR Apache-2.0 |
| serde_spanned | 0.6.9 | MIT OR Apache-2.0 |
| serde_spanned | 1.1.2 | MIT OR Apache-2.0 |
| serde_urlencoded | 0.7.1 | MIT/Apache-2.0 |
| serde_with | 3.24.0 | MIT OR Apache-2.0 |
| serde_with_macros | 3.24.0 | MIT OR Apache-2.0 |
| serialize-to-javascript | 0.1.2 | MIT OR Apache-2.0 |
| serialize-to-javascript-impl | 0.1.2 | MIT OR Apache-2.0 |
| servo_arc | 0.4.3 | MIT OR Apache-2.0 |
| sha1 | 0.10.7 | MIT OR Apache-2.0 |
| sha1 | 0.11.0 | MIT OR Apache-2.0 |
| sha2 | 0.10.9 | MIT OR Apache-2.0 |
| sha2 | 0.11.0 | MIT OR Apache-2.0 |
| sharded-slab | 0.1.7 | MIT |
| shlex | 1.3.0 | MIT OR Apache-2.0 |
| shlex | 2.0.1 | MIT OR Apache-2.0 |
| signal-hook-registry | 1.4.8 | MIT OR Apache-2.0 |
| signature | 2.2.0 | Apache-2.0 OR MIT |
| simd-adler32 | 0.3.10 | MIT |
| simd_cesu8 | 1.2.0 | Apache-2.0 OR MIT |
| simdutf8 | 0.1.5 | MIT OR Apache-2.0 |
| siphasher | 0.3.11 | MIT/Apache-2.0 |
| siphasher | 1.0.4 | MIT OR Apache-2.0 |
| slab | 0.4.12 | MIT |
| smallvec | 1.16.2 | MIT OR Apache-2.0 |
| smartstring | 1.0.1 | MPL-2.0+ |
| socket2 | 0.6.5 | MIT OR Apache-2.0 |
| softbuffer | 0.4.8 | MIT OR Apache-2.0 |
| soup3 | 0.5.0 | MIT |
| soup3-sys | 0.5.0 | MIT |
| spin | 0.9.9 | MIT |
| spki | 0.7.3 | Apache-2.0 OR MIT |
| stable_deref_trait | 1.2.1 | MIT OR Apache-2.0 |
| stacker | 0.1.25 | MIT OR Apache-2.0 |
| static_assertions | 1.1.0 | MIT OR Apache-2.0 |
| string_cache | 0.9.0 | MIT OR Apache-2.0 |
| string_cache_codegen | 0.6.1 | MIT OR Apache-2.0 |
| string_enum | 1.0.2 | Apache-2.0 |
| strsim | 0.11.1 | MIT |
| strum | 0.27.2 | MIT |
| strum | 0.28.0 | MIT |
| strum_macros | 0.27.2 | MIT |
| strum_macros | 0.28.0 | MIT |
| subtle | 2.6.1 | BSD-3-Clause |
| swc_allocator | 4.0.1 | Apache-2.0 |
| swc_atoms | 9.0.3 | Apache-2.0 |
| swc_common | 23.0.2 | Apache-2.0 |
| swc_config | 5.0.0 | Apache-2.0 |
| swc_config_macro | 1.0.1 | Apache-2.0 |
| swc_ecma_ast | 25.0.0 | Apache-2.0 |
| swc_ecma_codegen | 28.0.2 | Apache-2.0 |
| swc_ecma_codegen_macros | 2.0.2 | Apache-2.0 |
| swc_ecma_hooks | 0.9.0 | Apache-2.0 |
| swc_ecma_parser | 41.1.2 | Apache-2.0 |
| swc_ecma_transforms_base | 44.0.4 | Apache-2.0 |
| swc_ecma_transforms_react | 50.0.0 | Apache-2.0 |
| swc_ecma_transforms_typescript | 50.0.0 | Apache-2.0 |
| swc_ecma_utils | 31.0.2 | Apache-2.0 |
| swc_ecma_visit | 25.0.0 | Apache-2.0 |
| swc_eq_ignore_macros | 1.0.1 | Apache-2.0 |
| swc_macros_common | 1.0.1 | Apache-2.0 |
| swc_sourcemap | 10.0.2 | BSD-3-Clause |
| swc_ts_fast_strip | 53.0.0 | Apache-2.0 |
| swc_visit | 2.0.2 | Apache-2.0 |
| swift-rs | 1.0.9 | MIT OR Apache-2.0 |
| syn | 1.0.109 | MIT OR Apache-2.0 |
| syn | 2.0.119 | MIT OR Apache-2.0 |
| syn | 3.0.6 | MIT OR Apache-2.0 |
| sync_wrapper | 1.0.2 | Apache-2.0 |
| synstructure | 0.14.0 | MIT |
| system-configuration | 0.7.0 | MIT OR Apache-2.0 |
| system-configuration-sys | 0.6.0 | MIT OR Apache-2.0 |
| system-deps | 6.2.2 | MIT OR Apache-2.0 |
| tao | 0.37.1 | Apache-2.0 |
| tao-macros | 0.1.4 | MIT OR Apache-2.0 |
| tap | 1.0.1 | MIT |
| target-lexicon | 0.12.16 | Apache-2.0 WITH LLVM-exception |
| tauri | 2.12.2 | Apache-2.0 OR MIT |
| tauri-build | 2.7.1 | Apache-2.0 OR MIT |
| tauri-codegen | 2.7.1 | Apache-2.0 OR MIT |
| tauri-macros | 2.7.1 | Apache-2.0 OR MIT |
| tauri-plugin | 2.7.2 | Apache-2.0 OR MIT |
| tauri-plugin-dialog | 2.8.3 | Apache-2.0 OR MIT |
| tauri-plugin-fs | 2.7.1 | Apache-2.0 OR MIT |
| tauri-runtime | 2.12.1 | Apache-2.0 OR MIT |
| tauri-runtime-wry | 2.12.1 | Apache-2.0 OR MIT |
| tauri-utils | 2.10.2 | Apache-2.0 OR MIT |
| tauri-winres | 0.3.6 | MIT |
| tempfile | 3.27.0 | MIT OR Apache-2.0 |
| tendril | 0.5.1 | MIT OR Apache-2.0 |
| thiserror | 1.0.69 | MIT OR Apache-2.0 |
| thiserror | 2.0.21 | MIT OR Apache-2.0 |
| thiserror-impl | 1.0.69 | MIT OR Apache-2.0 |
| thiserror-impl | 2.0.21 | MIT OR Apache-2.0 |
| thread_local | 1.1.10 | MIT OR Apache-2.0 |
| time | 0.3.55 | MIT OR Apache-2.0 |
| time-core | 0.1.9 | MIT OR Apache-2.0 |
| time-macros | 0.2.32 | MIT OR Apache-2.0 |
| tiny-keccak | 2.0.2 | CC0-1.0 |
| tinystr | 0.8.4 | Unicode-3.0 |
| tinyvec | 1.13.3 | Zlib OR Apache-2.0 OR MIT |
| tokio | 1.53.2 | MIT |
| tokio-macros | 2.7.2 | MIT |
| tokio-native-tls | 0.3.1 | MIT |
| tokio-rustls | 0.26.6 | MIT OR Apache-2.0 |
| tokio-stream | 0.1.19 | MIT |
| tokio-util | 0.7.19 | MIT |
| toml | 0.8.2 | MIT OR Apache-2.0 |
| toml | 1.1.8+spec-1.1.0 | MIT OR Apache-2.0 |
| toml_datetime | 0.6.3 | MIT OR Apache-2.0 |
| toml_datetime | 1.1.2+spec-1.1.0 | MIT OR Apache-2.0 |
| toml_edit | 0.19.15 | MIT OR Apache-2.0 |
| toml_edit | 0.20.2 | MIT OR Apache-2.0 |
| toml_edit | 0.25.17+spec-1.1.0 | MIT OR Apache-2.0 |
| toml_parser | 1.1.5+spec-1.1.0 | MIT OR Apache-2.0 |
| toml_writer | 1.1.3+spec-1.1.0 | MIT OR Apache-2.0 |
| tower | 0.5.3 | MIT |
| tower-http | 0.6.11 | MIT |
| tower-layer | 0.3.3 | MIT |
| tower-service | 0.3.3 | MIT |
| tracing | 0.1.44 | MIT |
| tracing-attributes | 0.1.31 | MIT |
| tracing-core | 0.1.36 | MIT |
| tracing-log | 0.2.0 | MIT |
| tracing-subscriber | 0.3.23 | MIT |
| tray-icon | 0.25.1 | MIT OR Apache-2.0 |
| triomphe | 0.1.16 | MIT OR Apache-2.0 |
| try-lock | 0.2.5 | MIT |
| type1-encoding-parser | 0.1.1 | MIT |
| typed-arena | 2.0.2 | MIT |
| typed-path | 0.12.3 | MIT OR Apache-2.0 |
| typeid | 1.0.3 | MIT OR Apache-2.0 |
| typenum | 1.20.1 | MIT OR Apache-2.0 |
| unicode-general-category | 1.1.0 | Apache-2.0 |
| unicode-id-start | 1.5.0 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| unicode-ident | 1.0.26 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| unicode-normalization | 0.1.26 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| unicode-segmentation | 1.13.3 | MIT OR Apache-2.0 |
| unicode-width | 0.2.2 | MIT OR Apache-2.0 |
| untrusted | 0.9.0 | ISC |
| url | 2.5.8 | MIT OR Apache-2.0 |
| urlpattern | 0.6.0 | MIT |
| utf8_iter | 1.0.4 | Apache-2.0 OR MIT |
| uuid | 1.27.0 | Apache-2.0 OR MIT |
| uuid-simd | 0.8.0 | MIT |
| valuable | 0.1.1 | MIT |
| vcpkg | 0.2.15 | MIT/Apache-2.0 |
| version-compare | 0.2.1 | MIT |
| version_check | 0.9.5 | MIT/Apache-2.0 |
| vsimd | 0.8.0 | MIT |
| vswhom | 0.1.0 | MIT |
| vswhom-sys | 0.1.3 | MIT |
| walkdir | 2.5.0 | Unlicense/MIT |
| want | 0.3.2 | MIT |
| wasi | 0.11.1+wasi-snapshot-preview1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| wasip2 | 1.0.4+wasi-0.2.12 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| wasm-bindgen | 0.2.129 | MIT OR Apache-2.0 |
| wasm-bindgen-futures | 0.4.79 | MIT OR Apache-2.0 |
| wasm-bindgen-macro | 0.2.129 | MIT OR Apache-2.0 |
| wasm-bindgen-macro-support | 0.2.129 | MIT OR Apache-2.0 |
| wasm-bindgen-shared | 0.2.129 | MIT OR Apache-2.0 |
| wasm-streams | 0.4.2 | MIT OR Apache-2.0 |
| wasm-streams | 0.5.0 | MIT OR Apache-2.0 |
| web-sys | 0.3.106 | MIT OR Apache-2.0 |
| web-time | 1.1.0 | MIT OR Apache-2.0 |
| web_atoms | 0.2.6 | MIT OR Apache-2.0 |
| webkit2gtk | 2.0.2 | MIT |
| webkit2gtk-sys | 2.0.2 | MIT |
| webpki-root-certs | 1.0.9 | CDLA-Permissive-2.0 |
| webpki-roots | 1.0.9 | CDLA-Permissive-2.0 |
| webview2-com | 0.39.1 | MIT |
| webview2-com-macros | 0.8.1 | MIT |
| webview2-com-sys | 0.39.1 | MIT |
| weezl | 0.1.12 | MIT OR Apache-2.0 |
| winapi | 0.3.9 | MIT/Apache-2.0 |
| winapi-i686-pc-windows-gnu | 0.4.0 | MIT/Apache-2.0 |
| winapi-util | 0.1.11 | Unlicense OR MIT |
| winapi-x86_64-pc-windows-gnu | 0.4.0 | MIT/Apache-2.0 |
| window-vibrancy | 0.8.1 | Apache-2.0 OR MIT |
| windows | 0.62.2 | MIT OR Apache-2.0 |
| windows-collections | 0.3.2 | MIT OR Apache-2.0 |
| windows-core | 0.62.2 | MIT OR Apache-2.0 |
| windows-future | 0.3.2 | MIT OR Apache-2.0 |
| windows-implement | 0.60.2 | MIT OR Apache-2.0 |
| windows-interface | 0.59.3 | MIT OR Apache-2.0 |
| windows-link | 0.2.1 | MIT OR Apache-2.0 |
| windows-numerics | 0.3.1 | MIT OR Apache-2.0 |
| windows-registry | 0.6.1 | MIT OR Apache-2.0 |
| windows-result | 0.4.1 | MIT OR Apache-2.0 |
| windows-strings | 0.5.1 | MIT OR Apache-2.0 |
| windows-sys | 0.45.0 | MIT OR Apache-2.0 |
| windows-sys | 0.52.0 | MIT OR Apache-2.0 |
| windows-sys | 0.60.2 | MIT OR Apache-2.0 |
| windows-sys | 0.61.2 | MIT OR Apache-2.0 |
| windows-targets | 0.42.2 | MIT OR Apache-2.0 |
| windows-targets | 0.52.6 | MIT OR Apache-2.0 |
| windows-targets | 0.53.5 | MIT OR Apache-2.0 |
| windows-threading | 0.2.1 | MIT OR Apache-2.0 |
| windows-version | 0.1.7 | MIT OR Apache-2.0 |
| windows_aarch64_gnullvm | 0.42.2 | MIT OR Apache-2.0 |
| windows_aarch64_gnullvm | 0.52.6 | MIT OR Apache-2.0 |
| windows_aarch64_gnullvm | 0.53.1 | MIT OR Apache-2.0 |
| windows_aarch64_msvc | 0.42.2 | MIT OR Apache-2.0 |
| windows_aarch64_msvc | 0.52.6 | MIT OR Apache-2.0 |
| windows_aarch64_msvc | 0.53.1 | MIT OR Apache-2.0 |
| windows_i686_gnu | 0.42.2 | MIT OR Apache-2.0 |
| windows_i686_gnu | 0.52.6 | MIT OR Apache-2.0 |
| windows_i686_gnu | 0.53.1 | MIT OR Apache-2.0 |
| windows_i686_gnullvm | 0.52.6 | MIT OR Apache-2.0 |
| windows_i686_gnullvm | 0.53.1 | MIT OR Apache-2.0 |
| windows_i686_msvc | 0.42.2 | MIT OR Apache-2.0 |
| windows_i686_msvc | 0.52.6 | MIT OR Apache-2.0 |
| windows_i686_msvc | 0.53.1 | MIT OR Apache-2.0 |
| windows_x86_64_gnu | 0.42.2 | MIT OR Apache-2.0 |
| windows_x86_64_gnu | 0.52.6 | MIT OR Apache-2.0 |
| windows_x86_64_gnu | 0.53.1 | MIT OR Apache-2.0 |
| windows_x86_64_gnullvm | 0.42.2 | MIT OR Apache-2.0 |
| windows_x86_64_gnullvm | 0.52.6 | MIT OR Apache-2.0 |
| windows_x86_64_gnullvm | 0.53.1 | MIT OR Apache-2.0 |
| windows_x86_64_msvc | 0.42.2 | MIT OR Apache-2.0 |
| windows_x86_64_msvc | 0.52.6 | MIT OR Apache-2.0 |
| windows_x86_64_msvc | 0.53.1 | MIT OR Apache-2.0 |
| winnow | 0.5.40 | MIT |
| winnow | 1.0.4 | MIT |
| winreg | 0.56.0 | MIT |
| wit-bindgen | 0.57.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| writeable | 0.6.4 | Unicode-3.0 |
| wry | 0.57.0 | Apache-2.0 OR MIT |
| wyz | 0.5.1 | MIT |
| x11 | 2.21.0 | MIT |
| x11-dl | 2.21.0 | MIT |
| yoagent | 0.25.4 | MIT |
| yoke | 0.8.3 | Unicode-3.0 |
| yoke-derive | 0.8.4 | Unicode-3.0 |
| zen-engine | 2.1.2 | MIT |
| zen-expression | 2.1.2 | MIT |
| zen-macros | 2.1.2 | MIT |
| zen-tmpl | 2.1.2 | MIT |
| zen-types | 2.1.2 | MIT |
| zerocopy | 0.8.62 | BSD-2-Clause OR Apache-2.0 OR MIT |
| zerocopy-derive | 0.8.62 | BSD-2-Clause OR Apache-2.0 OR MIT |
| zerofrom | 0.1.8 | Unicode-3.0 |
| zerofrom-derive | 0.1.8 | Unicode-3.0 |
| zeroize | 1.9.1 | Apache-2.0 OR MIT |
| zerotrie | 0.2.5 | Unicode-3.0 |
| zerovec | 0.11.8 | Unicode-3.0 |
| zerovec-derive | 0.11.6 | Unicode-3.0 |
| zip | 8.6.0 | MIT |
| zlib-rs | 0.6.8 | Zlib |
| zmij | 1.0.23 | MIT |

</details>

## Node.js（web-ui 运行时依赖）

共 83 个包。按许可归组：

- **MIT**：78
- **BSD/ISC**：5

<details><summary>逐包清单（名称 / 版本 / 声明许可）</summary>

| 包 | 版本 | 许可 |
|---|---|---|
| @babel/helper-string-parser | 7.29.7 | MIT |
| @babel/helper-validator-identifier | 7.29.7 | MIT |
| @babel/parser | 7.29.9 | MIT |
| @babel/types | 7.29.8 | MIT |
| @ctrl/tinycolor | 4.2.1 | MIT |
| @element-plus/icons-vue | 2.3.2 | MIT |
| @floating-ui/core | 1.8.0 | MIT |
| @floating-ui/dom | 1.8.0 | MIT |
| @floating-ui/utils | 0.2.12 | MIT |
| @jridgewell/sourcemap-codec | 1.6.0 | MIT |
| @popperjs/core | 2.11.8 | MIT |
| @types/lodash | 4.17.26 | MIT |
| @types/lodash-es | 4.17.12 | MIT |
| @types/web-bluetooth | 0.0.21 | MIT |
| @vue/compiler-core | 3.5.43 | MIT |
| @vue/compiler-dom | 3.5.43 | MIT |
| @vue/compiler-sfc | 3.5.43 | MIT |
| @vue/compiler-ssr | 3.5.43 | MIT |
| @vue/devtools-api | 7.7.10 | MIT |
| @vue/devtools-kit | 7.7.10 | MIT |
| @vue/devtools-shared | 7.7.10 | MIT |
| @vue/reactivity | 3.5.43 | MIT |
| @vue/runtime-core | 3.5.43 | MIT |
| @vue/runtime-dom | 3.5.43 | MIT |
| @vue/server-renderer | 3.5.43 | MIT |
| @vue/shared | 3.5.43 | MIT |
| @vueuse/core | 15.0.0 | MIT |
| @vueuse/metadata | 15.0.0 | MIT |
| @vueuse/shared | 15.0.0 | MIT |
| agent-base | 6.0.2 | MIT |
| async-validator | 4.2.5 | MIT |
| asynckit | 0.4.0 | MIT |
| axios | 1.20.0 | MIT |
| birpc | 2.9.0 | MIT |
| call-bind-apply-helpers | 1.0.2 | MIT |
| combined-stream | 1.0.8 | MIT |
| copy-anything | 4.1.5 | MIT |
| csstype | 3.2.3 | MIT |
| dayjs | 1.11.23 | MIT |
| debug | 4.4.3 | MIT |
| delayed-stream | 1.0.0 | MIT |
| dunder-proto | 1.0.1 | MIT |
| element-plus | 2.14.7 | MIT |
| entities | 7.0.1 | BSD-2-Clause |
| es-define-property | 1.0.1 | MIT |
| es-errors | 1.3.0 | MIT |
| es-object-atoms | 1.1.2 | MIT |
| es-set-tostringtag | 2.1.0 | MIT |
| estree-walker | 2.0.2 | MIT |
| follow-redirects | 1.16.1 | MIT |
| form-data | 4.0.6 | MIT |
| function-bind | 1.1.2 | MIT |
| get-intrinsic | 1.3.0 | MIT |
| get-proto | 1.0.1 | MIT |
| gopd | 1.2.0 | MIT |
| has-symbols | 1.1.0 | MIT |
| has-tostringtag | 1.0.2 | MIT |
| hasown | 2.0.4 | MIT |
| hookable | 5.5.3 | MIT |
| https-proxy-agent | 5.0.1 | MIT |
| lodash | 4.18.1 | MIT |
| lodash-es | 4.18.1 | MIT |
| lodash-unified | 1.0.3 | MIT |
| magic-string | 0.30.21 | MIT |
| math-intrinsics | 1.1.0 | MIT |
| memoize-one | 6.0.0 | MIT |
| mime-db | 1.52.0 | MIT |
| mime-types | 2.1.35 | MIT |
| mitt | 3.0.1 | MIT |
| ms | 2.1.3 | MIT |
| nanoid | 3.3.20 | MIT |
| normalize-wheel-es | 1.2.0 | BSD-3-Clause |
| perfect-debounce | 1.0.0 | MIT |
| picocolors | 1.1.1 | ISC |
| pinia | 3.0.4 | MIT |
| postcss | 8.5.29 | MIT |
| proxy-from-env | 2.1.0 | MIT |
| rfdc | 1.4.1 | MIT |
| source-map-js | 1.2.2 | BSD-3-Clause |
| speakingurl | 14.0.1 | BSD-3-Clause |
| superjson | 2.2.6 | MIT |
| vue | 3.5.43 | MIT |
| vue-component-type-helpers | 3.3.13 | MIT |

</details>

## 特殊许可个案（与 Apache-2.0 的兼容性说明）

| 依赖 | 许可 | 处理 |
|---|---|---|
| cssparser / cssparser-macros / dtoa-short / option-ext / selectors / smartstring | MPL-2.0(+) | 文件级弱 Copyleft：不修改其源码即无传染性，随作品分发保留其许可声明即可 |
| self_cell | Apache-2.0 OR GPL-2.0-only | 双许可，**选用 Apache-2.0 分支** |
| r-efi | MIT OR Apache-2.0 OR LGPL-2.1-or-later | 三许可，**选用 Apache-2.0 分支** |
| ryu / dragonbox_ecma | Apache-2.0 OR BSL-1.0（OR LLVM-exception） | 双许可，**选用 Apache-2.0 分支** |
| webpki-roots / webpki-root-certs | CDLA-Permissive-2.0 | 数据许可（CA 根证书集），宽松可再分发，保留声明即可 |

其余依赖均为 MIT / Apache-2.0 / BSD / ISC / Zlib / Unicode / 0BSD / Unlicense 等
宽松许可，与 Apache-2.0 完全兼容。

## 外部语料（datasets/）

| 数据集 | 原始许可 | 用途 |
|---|---|---|
| JFLEG（dev/test） | 研究用途（见上游 keirks/jfleg） | 英文 GEC 评测 |
| MuCGEC（dev 子集） | Apache-2.0 | 中文 GEC 评测 |
| CSCD / CEC dat 混淆集 | 见上游 ChineseErrorCorrector 声明（MIT/Apache-2.0） | 中文评测与词表 |
| anomaly_gold / cctc_gold（本仓库自标） | Apache-2.0 | 回归评测 |

注：语料仅用于本地评测，不随二进制产物分发；再分发前请核对各上游许可条款。
