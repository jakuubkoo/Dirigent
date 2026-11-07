# Third-party notices

Dirigent itself is MIT-licensed (see `LICENSE`). It bundles or links the components below, each under its own license.
Nothing in the shipped binary is GPL/AGPL. One crate is MPL-2.0 (`option-ext`, file-level copyleft, used unmodified via
`dirs`; its source is on crates.io). Crates offered under a choice of licenses (e.g. `self_cell`: Apache-2.0 OR
GPL-2.0) are used under the permissive option.

Regenerate with `scripts/third-party-notices.sh`.

## Bundled assets

| Component | License | Text |
|---|---|---|
| Inter 4.1 (font) | SIL Open Font License 1.1 | `crates/app/assets/fonts/Inter-LICENSE.txt` |
| JetBrains Mono 2.304 (font) | SIL Open Font License 1.1 | `crates/app/assets/fonts/JetBrainsMono-OFL.txt` |
| Lucide 1.68 (icons) | ISC | `crates/app/assets/icons/LICENSE.txt` |
| tree-sitter grammars and their highlight queries | MIT | `crates/syntax/licenses/` (one file per grammar, list in its README) |

## Frameworks

- **gpui** and the other Zed crates it needs (`gpui_*`, `collections`, `refineable`, `sum_tree`, `scheduler`,
  `http_client`, …) from https://github.com/zed-industries/zed: Apache-2.0. Only Apache-2.0 Zed crates are used;
  none of Zed's GPL-3.0 crates are, and no code was copied from them.
- **alacritty_terminal** (terminal emulation): Apache-2.0.

## Linked crates

437 crates linked into the Linux build (`cargo tree -e normal --target x86_64-unknown-linux-gnu`).

| Crate | Version | License | Source |
|---|---|---|---|
| accesskit | 0.24.1 | MIT OR Apache-2.0 | https://github.com/AccessKit/accesskit |
| accesskit_atspi_common | 0.19.1 | MIT OR Apache-2.0 | https://github.com/AccessKit/accesskit |
| accesskit_consumer | 0.38.0 | MIT OR Apache-2.0 | https://github.com/AccessKit/accesskit |
| accesskit_unix | 0.22.1 | MIT OR Apache-2.0 | https://github.com/AccessKit/accesskit |
| addr2line | 0.27.1 | Apache-2.0 OR MIT | https://github.com/gimli-rs/addr2line |
| adler2 | 2.0.1 | 0BSD OR MIT OR Apache-2.0 | https://github.com/oyvindln/adler2 |
| aes | 0.8.4 | MIT OR Apache-2.0 | https://github.com/RustCrypto/block-ciphers |
| ahash | 0.8.12 | MIT OR Apache-2.0 | https://github.com/tkaitchuck/ahash |
| aho-corasick | 1.1.5 | Unlicense OR MIT | https://github.com/BurntSushi/aho-corasick |
| alacritty_terminal | 0.26.0 | Apache-2.0 | https://github.com/alacritty/alacritty |
| allocator-api2 | 0.2.21 | MIT OR Apache-2.0 | https://github.com/zakarumych/allocator-api2 |
| anyhow | 1.0.104 | MIT OR Apache-2.0 | https://github.com/dtolnay/anyhow |
| arrayref | 0.3.9 | BSD-2-Clause | https://github.com/droundy/arrayref |
| arrayvec | 0.7.8 | MIT OR Apache-2.0 | https://github.com/bluss/arrayvec |
| as-raw-xcb-connection | 1.0.1 | MIT OR Apache-2.0 | https://github.com/psychon/as-raw-xcb-connection |
| ash | 0.38.0+1.3.281 | MIT OR Apache-2.0 | https://github.com/ash-rs/ash |
| ashpd | 0.13.13 | MIT | https://github.com/bilelmoussaoui/ashpd |
| async-broadcast | 0.7.2 | MIT OR Apache-2.0 | https://github.com/smol-rs/async-broadcast |
| async-channel | 2.5.0 | Apache-2.0 OR MIT | https://github.com/smol-rs/async-channel |
| async-executor | 1.14.1 | Apache-2.0 OR MIT | https://github.com/smol-rs/async-executor |
| async-fs | 2.2.0 | Apache-2.0 OR MIT | https://github.com/smol-rs/async-fs |
| async-io | 2.6.0 | Apache-2.0 OR MIT | https://github.com/smol-rs/async-io |
| async-lock | 3.5.0 | Apache-2.0 OR MIT | https://github.com/smol-rs/async-lock |
| async-net | 2.0.0 | Apache-2.0 OR MIT | https://github.com/smol-rs/async-net |
| async-process | 2.5.0 | Apache-2.0 OR MIT | https://github.com/smol-rs/async-process |
| async-signal | 0.2.14 | Apache-2.0 OR MIT | https://github.com/smol-rs/async-signal |
| async-task | 4.7.1 | Apache-2.0 OR MIT | https://github.com/smol-rs/async-task |
| atomic-waker | 1.1.2 | Apache-2.0 OR MIT | https://github.com/smol-rs/atomic-waker |
| atspi | 0.29.0 | Apache-2.0 OR MIT | https://github.com/odilia-app/atspi |
| atspi-common | 0.13.0 | Apache-2.0 OR MIT | https://github.com/odilia-app/atspi |
| atspi-proxies | 0.13.0 | Apache-2.0 OR MIT | https://github.com/odilia-app/atspi |
| backtrace | 0.3.77 | MIT OR Apache-2.0 | https://github.com/rust-lang/backtrace-rs |
| base64 | 0.22.1 | MIT OR Apache-2.0 | https://github.com/marshallpierce/rust-base64 |
| bit-set | 0.11.1 | Apache-2.0 OR MIT | https://github.com/contain-rs/bit-vec |
| bit-vec | 0.10.1 | Apache-2.0 OR MIT | https://github.com/contain-rs/bit-vec |
| bit_field | 0.10.3 | Apache-2.0/MIT | https://github.com/phil-opp/rust-bit-field |
| bitflags | 1.3.2 | MIT/Apache-2.0 | https://github.com/bitflags/bitflags |
| bitflags | 2.13.2 | MIT OR Apache-2.0 | https://github.com/bitflags/bitflags |
| block-buffer | 0.10.4 | MIT OR Apache-2.0 | https://github.com/RustCrypto/utils |
| block-padding | 0.3.3 | MIT OR Apache-2.0 | https://github.com/RustCrypto/utils |
| blocking | 1.7.0 | Apache-2.0 OR MIT | https://github.com/smol-rs/blocking |
| bstr | 1.13.1 | MIT OR Apache-2.0 | https://github.com/BurntSushi/bstr |
| bytemuck | 1.25.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/Lokathor/bytemuck |
| byteorder | 1.5.0 | Unlicense OR MIT | https://github.com/BurntSushi/byteorder |
| byteorder-lite | 0.1.0 | Unlicense OR MIT | https://github.com/image-rs/byteorder-lite |
| bytes | 1.12.1 | MIT | https://github.com/tokio-rs/bytes |
| calloop | 0.14.3 | MIT | https://github.com/Smithay/calloop |
| calloop-wayland-source | 0.4.1 | MIT | https://github.com/smithay/calloop-wayland-source |
| cbc | 0.1.2 | MIT OR Apache-2.0 | https://github.com/RustCrypto/block-modes |
| cfg-if | 1.0.5 | MIT OR Apache-2.0 | https://github.com/rust-lang/cfg-if |
| chrono | 0.4.45 | MIT OR Apache-2.0 | https://github.com/chronotope/chrono |
| cipher | 0.4.4 | MIT OR Apache-2.0 | https://github.com/RustCrypto/traits |
| codespan-reporting | 0.13.1 | Apache-2.0 | https://github.com/brendanzab/codespan |
| collections | 0.1.0 | Apache-2.0 |  |
| color_quant | 1.1.0 | MIT | https://github.com/image-rs/color_quant.git |
| concurrent-queue | 2.5.0 | Apache-2.0 OR MIT | https://github.com/smol-rs/concurrent-queue |
| const-random | 0.1.18 | MIT OR Apache-2.0 | https://github.com/tkaitchuck/constrandom |
| convert_case | 0.10.0 | MIT | https://github.com/rutrum/convert-case |
| core_detect | 1.0.0 | MIT/Apache-2.0 | https://github.com/thomcc/core_detect |
| core_maths | 0.1.1 | MIT | https://github.com/robertbastian/core_maths |
| cosmic-text | 0.19.0 | MIT OR Apache-2.0 | https://github.com/pop-os/cosmic-text |
| cpufeatures | 0.2.17 | MIT OR Apache-2.0 | https://github.com/RustCrypto/utils |
| crc32fast | 1.5.4 | MIT OR Apache-2.0 | https://github.com/srijs/rust-crc32fast |
| crossbeam-deque | 0.8.8 | MIT OR Apache-2.0 | https://github.com/crossbeam-rs/crossbeam |
| crossbeam-epoch | 0.9.21 | MIT OR Apache-2.0 | https://github.com/crossbeam-rs/crossbeam |
| crossbeam-utils | 0.8.23 | MIT OR Apache-2.0 | https://github.com/crossbeam-rs/crossbeam |
| crunchy | 0.2.4 | MIT | https://github.com/eira-fransham/crunchy |
| crypto-common | 0.1.7 | MIT OR Apache-2.0 | https://github.com/RustCrypto/traits |
| cursor-icon | 1.2.0 | MIT OR Apache-2.0 OR Zlib | https://github.com/rust-windowing/cursor-icon |
| data-url | 0.3.2 | MIT OR Apache-2.0 | https://github.com/servo/rust-url |
| derive_more | 2.1.1 | MIT | https://github.com/JelteF/derive_more |
| digest | 0.10.7 | MIT OR Apache-2.0 | https://github.com/RustCrypto/traits |
| dirs | 6.0.0 | MIT OR Apache-2.0 | https://github.com/soc/dirs-rs |
| dirs-sys | 0.5.0 | MIT OR Apache-2.0 | https://github.com/dirs-dev/dirs-sys-rs |
| dlib | 0.5.3 | MIT | https://github.com/elinorbgr/dlib |
| downcast-rs | 1.2.1 | MIT/Apache-2.0 | https://github.com/marcianx/downcast-rs |
| dyn-clone | 1.0.20 | MIT OR Apache-2.0 | https://github.com/dtolnay/dyn-clone |
| either | 1.19.0 | MIT OR Apache-2.0 | https://github.com/rayon-rs/either |
| encoding_rs | 0.8.44 | (Apache-2.0 OR MIT) AND BSD-3-Clause | https://github.com/hsivonen/encoding_rs |
| endi | 1.1.1 | MIT | https://github.com/zeenix/endi |
| enumflags2 | 0.7.12 | MIT OR Apache-2.0 | https://github.com/meithecatte/enumflags2 |
| equivalent | 1.0.3 | Apache-2.0 OR MIT | https://github.com/indexmap-rs/equivalent |
| errno | 0.3.14 | MIT OR Apache-2.0 | https://github.com/lambda-fairy/rust-errno |
| etagere | 0.2.15 | MIT/Apache-2.0 | https://github.com/nical/etagere |
| euclid | 0.22.14 | MIT OR Apache-2.0 | https://github.com/servo/euclid |
| event-listener | 5.4.2 | Apache-2.0 OR MIT | https://github.com/smol-rs/event-listener |
| event-listener-strategy | 0.5.4 | Apache-2.0 OR MIT | https://github.com/smol-rs/event-listener-strategy |
| exr | 1.74.2 | BSD-3-Clause | https://github.com/johannesvollmer/exrs |
| fastrand | 2.5.0 | Apache-2.0 OR MIT | https://github.com/smol-rs/fastrand |
| fax | 0.2.7 | MIT | https://github.com/pdf-rs/fax |
| fdeflate | 0.3.7 | MIT OR Apache-2.0 | https://github.com/image-rs/fdeflate |
| filedescriptor | 0.8.3 | MIT | https://github.com/wezterm/wezterm |
| flate2 | 1.1.10 | MIT OR Apache-2.0 | https://github.com/rust-lang/flate2-rs |
| float-cmp | 0.9.0 | MIT | https://github.com/mikedilger/float-cmp |
| float-ord | 0.3.2 | MIT / Apache-2.0 | https://github.com/notriddle/rust-float-ord |
| float_next_after | 1.0.0 | MIT | https://gitlab.com/bronsonbdevost/next_afterf |
| flume | 0.12.0 | Apache-2.0/MIT | https://github.com/zesterer/flume |
| foldhash | 0.1.5 | Zlib | https://github.com/orlp/foldhash |
| foldhash | 0.2.0 | Zlib | https://github.com/orlp/foldhash |
| font-types | 0.11.3 | MIT OR Apache-2.0 | https://github.com/googlefonts/fontations |
| font-types | 0.12.7 | MIT OR Apache-2.0 | https://github.com/googlefonts/fontations |
| fontconfig-parser | 0.5.8 | MIT | https://github.com/Riey/fontconfig-parser |
| fontdb | 0.23.0 | MIT | https://github.com/RazrFalcon/fontdb |
| form_urlencoded | 1.2.2 | MIT OR Apache-2.0 | https://github.com/servo/rust-url |
| freetype-sys | 0.20.1 | MIT | https://github.com/PistonDevelopers/freetype-sys.git |
| futures | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-channel | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-core | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-executor | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-io | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-lite | 2.6.1 | Apache-2.0 OR MIT | https://github.com/smol-rs/futures-lite |
| futures-sink | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-task | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-util | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| generic-array | 0.14.7 | MIT | https://github.com/fizyk20/generic-array.git |
| gethostname | 1.1.0 | Apache-2.0 | https://codeberg.org/swsnr/gethostname.rs.git |
| getrandom | 0.2.17 | MIT OR Apache-2.0 | https://github.com/rust-random/getrandom |
| getrandom | 0.3.4 | MIT OR Apache-2.0 | https://github.com/rust-random/getrandom |
| getrandom | 0.4.3 | MIT OR Apache-2.0 | https://github.com/rust-random/getrandom |
| gif | 0.14.2 | MIT OR Apache-2.0 | https://github.com/image-rs/image-gif |
| gimli | 0.34.0 | MIT OR Apache-2.0 | https://github.com/gimli-rs/gimli |
| globset | 0.4.21 | Unlicense OR MIT | https://github.com/BurntSushi/ripgrep/tree/master/crates/globset |
| glow | 0.18.0 | MIT OR Apache-2.0 OR Zlib | https://github.com/grovesNL/glow |
| gpu-allocator | 0.28.0 | MIT OR Apache-2.0 | https://github.com/Traverse-Research/gpu-allocator |
| gpui | 0.2.2 | Apache-2.0 | https://github.com/zed-industries/zed |
| gpui_linux | 0.1.0 | Apache-2.0 |  |
| gpui_platform | 0.1.0 | Apache-2.0 |  |
| gpui_shared_string | 0.1.0 | Apache-2.0 |  |
| gpui_util | 0.1.0 | Apache-2.0 |  |
| gpui_wgpu | 0.1.0 | Apache-2.0 |  |
| half | 2.7.1 | MIT OR Apache-2.0 | https://github.com/VoidStarKat/half-rs |
| harfrust | 0.5.2 | MIT | https://github.com/harfbuzz/harfrust |
| hash32 | 0.3.1 | MIT OR Apache-2.0 | https://github.com/japaric/hash32 |
| hashbrown | 0.14.5 | MIT OR Apache-2.0 | https://github.com/rust-lang/hashbrown |
| hashbrown | 0.15.5 | MIT OR Apache-2.0 | https://github.com/rust-lang/hashbrown |
| hashbrown | 0.16.1 | MIT OR Apache-2.0 | https://github.com/rust-lang/hashbrown |
| hashbrown | 0.17.1 | MIT OR Apache-2.0 | https://github.com/rust-lang/hashbrown |
| heapless | 0.9.3 | MIT OR Apache-2.0 | https://github.com/rust-embedded/heapless |
| heck | 0.5.0 | MIT OR Apache-2.0 | https://github.com/withoutboats/heck |
| hex | 0.4.3 | MIT OR Apache-2.0 | https://github.com/KokaKiwi/rust-hex |
| hkdf | 0.12.4 | MIT OR Apache-2.0 | https://github.com/RustCrypto/KDFs/ |
| hmac | 0.12.1 | MIT OR Apache-2.0 | https://github.com/RustCrypto/MACs |
| home | 0.5.12 | MIT OR Apache-2.0 | https://github.com/rust-lang/cargo |
| http | 1.5.0 | MIT OR Apache-2.0 | https://github.com/hyperium/http |
| http-body | 1.1.0 | MIT | https://github.com/hyperium/http-body |
| http_client | 0.1.0 | Apache-2.0 |  |
| iana-time-zone | 0.1.65 | MIT OR Apache-2.0 | https://github.com/strawlab/iana-time-zone |
| icu_collections | 2.4.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| icu_locale_core | 2.4.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| icu_locale_fallback | 2.4.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| icu_locale_fallback_data | 2.4.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| icu_normalizer | 2.4.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| icu_normalizer_data | 2.4.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| icu_properties | 2.4.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| icu_properties_data | 2.4.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| icu_provider | 2.4.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| icu_segmenter | 2.4.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| icu_segmenter_data | 2.4.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| idna | 1.1.0 | MIT OR Apache-2.0 | https://github.com/servo/rust-url/ |
| idna_adapter | 1.2.2 | Apache-2.0 OR MIT | https://github.com/hsivonen/idna_adapter |
| ignore | 0.4.34 | Unlicense OR MIT | https://github.com/BurntSushi/ripgrep/tree/master/crates/ignore |
| image | 0.25.10 | MIT OR Apache-2.0 | https://github.com/image-rs/image |
| image-webp | 0.2.4 | MIT OR Apache-2.0 | https://github.com/image-rs/image-webp |
| imagesize | 0.14.0 | MIT | https://github.com/Roughsketch/imagesize |
| imara-diff | 0.2.0 | Apache-2.0 | https://github.com/pascalkuthe/imara-diff |
| indexmap | 2.14.2 | Apache-2.0 OR MIT | https://github.com/indexmap-rs/indexmap |
| inotify | 0.11.5 | ISC | https://github.com/hannobraun/inotify-rs |
| inotify-sys | 0.1.8 | ISC | https://github.com/hannobraun/inotify-sys |
| inout | 0.1.4 | MIT OR Apache-2.0 | https://github.com/RustCrypto/utils |
| inventory | 0.3.25 | MIT OR Apache-2.0 | https://github.com/dtolnay/inventory |
| is-docker | 0.2.0 | MIT | https://github.com/TheLarkInn/is-docker |
| is-wsl | 0.4.0 | MIT | https://github.com/TheLarkInn/is-wsl |
| itertools | 0.14.0 | MIT OR Apache-2.0 | https://github.com/rust-itertools/itertools |
| itoa | 1.0.18 | MIT OR Apache-2.0 | https://github.com/dtolnay/itoa |
| khronos-egl | 6.0.0 | MIT/Apache-2.0 | https://github.com/timothee-haudebourg/khronos-egl |
| kurbo | 0.13.1 | Apache-2.0 OR MIT | https://github.com/linebender/kurbo |
| lazy_static | 1.5.1 | MIT OR Apache-2.0 | https://github.com/rust-lang-nursery/lazy-static.rs |
| lebe | 0.5.3 | BSD-3-Clause | https://github.com/johannesvollmer/lebe |
| libc | 0.2.191 | MIT OR Apache-2.0 | https://github.com/rust-lang/libc |
| libloading | 0.8.9 | ISC | https://github.com/nagisa/rust_libloading/ |
| libm | 0.2.16 | MIT | https://github.com/rust-lang/compiler-builtins |
| linebender_resource_handle | 0.1.1 | Apache-2.0 OR MIT | https://github.com/linebender/raw_resource_handle |
| linux-raw-sys | 0.12.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | https://github.com/sunfishcode/linux-raw-sys |
| litemap | 0.8.3 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| litrs | 1.0.1 | MIT OR Apache-2.0 | https://github.com/LukasKalbertodt/litrs |
| lock_api | 0.4.14 | MIT OR Apache-2.0 | https://github.com/Amanieu/parking_lot |
| log | 0.4.35 | MIT OR Apache-2.0 | https://github.com/rust-lang/log |
| lyon | 1.0.19 | MIT OR Apache-2.0 | https://github.com/nical/lyon |
| lyon_algorithms | 1.0.22 | MIT OR Apache-2.0 | https://github.com/nical/lyon |
| lyon_geom | 1.0.20 | MIT OR Apache-2.0 | https://github.com/nical/lyon |
| lyon_path | 1.0.19 | MIT OR Apache-2.0 | https://github.com/nical/lyon |
| lyon_tessellation | 1.0.23 | MIT OR Apache-2.0 | https://github.com/nical/lyon |
| macro_rules_attribute | 0.2.3 | Apache-2.0 OR MIT OR Zlib | https://github.com/danielhenrymantilla/macro_rules_attribute-rs |
| md-5 | 0.10.6 | MIT OR Apache-2.0 | https://github.com/RustCrypto/hashes |
| memchr | 2.8.3 | Unlicense OR MIT | https://github.com/BurntSushi/memchr |
| memmap2 | 0.9.12 | MIT OR Apache-2.0 | https://github.com/RazrFalcon/memmap2-rs |
| miniz_oxide | 0.8.9 | MIT OR Zlib OR Apache-2.0 | https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide |
| miniz_oxide | 0.9.1 | MIT OR Zlib OR Apache-2.0 | https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide |
| mio | 1.2.4 | MIT | https://github.com/tokio-rs/mio |
| moxcms | 0.8.1 | BSD-3-Clause OR Apache-2.0 | https://github.com/awxkee/moxcms.git |
| naga | 30.0.0 | MIT OR Apache-2.0 | https://github.com/gfx-rs/wgpu |
| naga-types | 30.0.0 | MIT OR Apache-2.0 | https://github.com/gfx-rs/wgpu |
| nix | 0.28.0 | MIT | https://github.com/nix-rust/nix |
| notify | 9.0.0-rc.4 | CC0-1.0 | https://github.com/notify-rs/notify.git |
| notify-rust | 4.18.2 | MIT OR Apache-2.0 | https://github.com/hoodie/notify-rust |
| notify-types | 2.1.0 | MIT OR Apache-2.0 | https://github.com/notify-rs/notify.git |
| num | 0.4.3 | MIT OR Apache-2.0 | https://github.com/rust-num/num |
| num-bigint | 0.4.8 | MIT OR Apache-2.0 | https://github.com/rust-num/num-bigint |
| num-bigint-dig | 0.9.1 | MIT/Apache-2.0 | https://github.com/dignifiedquire/num-bigint |
| num-complex | 0.4.6 | MIT OR Apache-2.0 | https://github.com/rust-num/num-complex |
| num-integer | 0.1.47 | MIT OR Apache-2.0 | https://github.com/rust-num/num-integer |
| num-iter | 0.1.46 | MIT OR Apache-2.0 | https://github.com/rust-num/num-iter |
| num-rational | 0.4.2 | MIT OR Apache-2.0 | https://github.com/rust-num/num-rational |
| num-traits | 0.2.19 | MIT OR Apache-2.0 | https://github.com/rust-num/num-traits |
| num_cpus | 1.17.0 | MIT OR Apache-2.0 | https://github.com/seanmonstar/num_cpus |
| object | 0.40.0 | Apache-2.0 OR MIT | https://github.com/gimli-rs/object |
| once_cell | 1.21.4 | MIT OR Apache-2.0 | https://github.com/matklad/once_cell |
| oo7 | 0.6.0 | MIT | https://github.com/linux-credentials/oo7 |
| open | 5.4.5 | MIT | https://github.com/Byron/open-rs |
| option-ext | 0.2.0 | MPL-2.0 | https://github.com/soc/option-ext.git |
| ordered-float | 5.5.1 | MIT | https://github.com/reem/rust-ordered-float |
| ordered-stream | 0.2.0 | MIT OR Apache-2.0 | https://github.com/danieldg/ordered-stream |
| parking | 2.2.1 | Apache-2.0 OR MIT | https://github.com/smol-rs/parking |
| parking_lot | 0.12.5 | MIT OR Apache-2.0 | https://github.com/Amanieu/parking_lot |
| parking_lot_core | 0.9.12 | MIT OR Apache-2.0 | https://github.com/Amanieu/parking_lot |
| pathfinder_geometry | 0.5.1 | MIT/Apache-2.0 | https://github.com/servo/pathfinder |
| pathfinder_simd | 0.5.6 | MIT OR Apache-2.0 | https://github.com/servo/pathfinder |
| pbkdf2 | 0.12.2 | MIT OR Apache-2.0 | https://github.com/RustCrypto/password-hashes/tree/master/pbkdf2 |
| percent-encoding | 2.3.2 | MIT OR Apache-2.0 | https://github.com/servo/rust-url/ |
| phf | 0.13.1 | MIT | https://github.com/rust-phf/rust-phf |
| phf_generator | 0.13.1 | MIT | https://github.com/rust-phf/rust-phf |
| phf_shared | 0.13.1 | MIT | https://github.com/rust-phf/rust-phf |
| pico-args | 0.5.0 | MIT | https://github.com/RazrFalcon/pico-args |
| pin-project | 1.1.13 | Apache-2.0 OR MIT | https://github.com/taiki-e/pin-project |
| pin-project-lite | 0.2.17 | Apache-2.0 OR MIT | https://github.com/taiki-e/pin-project-lite |
| piper | 0.2.5 | MIT OR Apache-2.0 | https://github.com/smol-rs/piper |
| png | 0.17.16 | MIT OR Apache-2.0 | https://github.com/image-rs/image-png |
| png | 0.18.1 | MIT OR Apache-2.0 | https://github.com/image-rs/image-png |
| polling | 3.11.0 | Apache-2.0 OR MIT | https://github.com/smol-rs/polling |
| pollster | 0.4.0 | Apache-2.0/MIT | https://github.com/zesterer/pollster |
| polycool | 0.4.0 | MIT OR Apache-2.0 | https://github.com/linebender/kurbo |
| portable-pty | 0.9.0 | MIT | https://github.com/wezterm/wezterm |
| potential_utf | 0.1.6 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| ppv-lite86 | 0.2.21 | MIT OR Apache-2.0 | https://github.com/cryptocorrosion/cryptocorrosion |
| presser | 0.3.1 | MIT OR Apache-2.0 | https://github.com/EmbarkStudios/presser |
| proc-macro-crate | 3.5.0 | MIT OR Apache-2.0 | https://github.com/bkchr/proc-macro-crate |
| proc-macro2 | 1.0.107 | MIT OR Apache-2.0 | https://github.com/dtolnay/proc-macro2 |
| profiling | 1.0.18 | MIT OR Apache-2.0 | https://github.com/aclysma/profiling |
| pulp | 0.22.3 | MIT | https://github.com/sarah-quinones/pulp/ |
| pulp-wasm-simd-flag | 0.1.1 | MIT | https://github.com/sarah-quinones/pulp/ |
| pxfm | 0.1.30 | BSD-3-Clause OR Apache-2.0 | https://github.com/awxkee/pxfm |
| qoi | 0.4.1 | MIT/Apache-2.0 | https://github.com/aldanor/qoi-rust |
| quick-error | 2.0.1 | MIT/Apache-2.0 | http://github.com/tailhook/quick-error |
| quick-xml | 0.41.0 | MIT | https://github.com/tafia/quick-xml |
| quote | 1.0.47 | MIT OR Apache-2.0 | https://github.com/dtolnay/quote |
| rand | 0.9.5 | MIT OR Apache-2.0 | https://github.com/rust-random/rand |
| rand_chacha | 0.9.0 | MIT OR Apache-2.0 | https://github.com/rust-random/rand |
| rand_core | 0.9.5 | MIT OR Apache-2.0 | https://github.com/rust-random/rand |
| rangemap | 1.8.0 | MIT/Apache-2.0 | https://github.com/jeffparsons/rangemap |
| raw-cpuid | 11.6.0 | MIT | https://github.com/gz/rust-cpuid |
| raw-window-handle | 0.6.2 | MIT OR Apache-2.0 OR Zlib | https://github.com/rust-windowing/raw-window-handle |
| rayon | 1.12.0 | MIT OR Apache-2.0 | https://github.com/rayon-rs/rayon |
| rayon-core | 1.13.0 | MIT OR Apache-2.0 | https://github.com/rayon-rs/rayon |
| read-fonts | 0.37.0 | MIT OR Apache-2.0 | https://github.com/googlefonts/fontations |
| read-fonts | 0.41.0 | MIT OR Apache-2.0 | https://github.com/googlefonts/fontations |
| reborrow | 0.5.5 | MIT | https://github.com/sarah-ek/reborrow/ |
| ref-cast | 1.0.27 | MIT OR Apache-2.0 | https://github.com/dtolnay/ref-cast |
| refineable | 0.1.0 | Apache-2.0 |  |
| regex | 1.13.1 | MIT OR Apache-2.0 | https://github.com/rust-lang/regex |
| regex-automata | 0.4.18 | MIT OR Apache-2.0 | https://github.com/rust-lang/regex |
| regex-syntax | 0.8.11 | MIT OR Apache-2.0 | https://github.com/rust-lang/regex |
| renderdoc-sys | 1.1.0 | MIT OR Apache-2.0 | https://github.com/ebkalderon/renderdoc-rs |
| resvg | 0.46.0 | Apache-2.0 OR MIT | https://github.com/linebender/resvg |
| rgb | 0.8.53 | MIT | https://github.com/kornelski/rust-rgb |
| ropey | 1.6.1 | MIT | https://github.com/cessen/ropey |
| roxmltree | 0.20.0 | MIT OR Apache-2.0 | https://github.com/RazrFalcon/roxmltree |
| roxmltree | 0.21.2 | MIT OR Apache-2.0 | https://github.com/RazrFalcon/roxmltree |
| rustc-demangle | 0.1.28 | MIT/Apache-2.0 | https://github.com/rust-lang/rustc-demangle |
| rustc-hash | 1.1.0 | Apache-2.0/MIT | https://github.com/rust-lang-nursery/rustc-hash |
| rustc-hash | 2.1.3 | Apache-2.0 OR MIT | https://github.com/rust-lang/rustc-hash |
| rustix | 1.1.6 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | https://github.com/bytecodealliance/rustix |
| rustix-openpty | 0.2.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | https://github.com/sunfishcode/rustix-openpty |
| rustybuzz | 0.20.1 | MIT | https://github.com/harfbuzz/rustybuzz |
| ryu | 1.0.23 | Apache-2.0 OR BSL-1.0 | https://github.com/dtolnay/ryu |
| same-file | 1.0.6 | Unlicense/MIT | https://github.com/BurntSushi/same-file |
| scheduler | 0.1.0 | Apache-2.0 |  |
| schemars | 1.2.3 | MIT | https://github.com/GREsau/schemars |
| scoped-tls | 1.0.1 | MIT/Apache-2.0 | https://github.com/alexcrichton/scoped-tls |
| scopeguard | 1.2.0 | MIT OR Apache-2.0 | https://github.com/bluss/scopeguard |
| seahash | 4.1.0 | MIT | https://gitlab.redox-os.org/redox-os/seahash |
| self_cell | 1.3.0 | Apache-2.0 OR GPL-2.0-only | https://github.com/Voultapher/self_cell |
| serde | 1.0.229 | MIT OR Apache-2.0 | https://github.com/serde-rs/serde |
| serde_bytes | 0.11.19 | MIT OR Apache-2.0 | https://github.com/serde-rs/bytes |
| serde_core | 1.0.229 | MIT OR Apache-2.0 | https://github.com/serde-rs/serde |
| serde_derive_internals | 0.30.0 | MIT OR Apache-2.0 | https://github.com/serde-rs/serde |
| serde_json | 1.0.154 | MIT OR Apache-2.0 | https://github.com/serde-rs/json |
| serde_urlencoded | 0.7.1 | MIT/Apache-2.0 | https://github.com/nox/serde_urlencoded |
| serial2 | 0.2.39 | BSD-2-Clause OR Apache-2.0 | https://github.com/de-vri-es/serial2-rs |
| sha1_smol | 1.0.1 | BSD-3-Clause | https://github.com/mitsuhiko/sha1-smol |
| sha2 | 0.10.9 | MIT OR Apache-2.0 | https://github.com/RustCrypto/hashes |
| shell-words | 1.1.1 | MIT/Apache-2.0 | https://github.com/tmiasko/shell-words |
| signal-hook | 0.4.6 | MIT OR Apache-2.0 | https://github.com/vorner/signal-hook |
| signal-hook-registry | 1.4.8 | MIT OR Apache-2.0 | https://github.com/vorner/signal-hook |
| simd-adler32 | 0.3.10 | MIT | https://github.com/mcountryman/simd-adler32 |
| simdutf8 | 0.1.5 | MIT OR Apache-2.0 | https://github.com/rusticstuff/simdutf8 |
| simplecss | 0.2.2 | Apache-2.0 OR MIT | https://github.com/linebender/simplecss |
| siphasher | 1.0.4 | MIT OR Apache-2.0 | https://github.com/jedisct1/rust-siphash |
| skrifa | 0.40.0 | MIT OR Apache-2.0 | https://github.com/googlefonts/fontations |
| skrifa | 0.44.0 | MIT OR Apache-2.0 | https://github.com/googlefonts/fontations |
| slab | 0.4.12 | MIT | https://github.com/tokio-rs/slab |
| slotmap | 1.1.1 | Zlib | https://github.com/orlp/slotmap |
| smallvec | 1.16.4 | MIT OR Apache-2.0 | https://github.com/servo/rust-smallvec |
| smol | 2.0.2 | Apache-2.0 OR MIT | https://github.com/smol-rs/smol |
| smol_str | 0.3.7 | MIT OR Apache-2.0 | https://github.com/rust-lang/rust-analyzer/tree/master/lib/smol_str |
| spin | 0.10.1 | MIT | https://github.com/mvdnes/spin-rs.git |
| spin | 0.9.9 | MIT | https://github.com/mvdnes/spin-rs.git |
| spirv | 0.4.0+sdk-1.4.341.0 | Apache-2.0 | https://github.com/gfx-rs/rspirv |
| stable_deref_trait | 1.2.1 | MIT OR Apache-2.0 | https://github.com/storyyeller/stable_deref_trait |
| static_assertions | 1.1.0 | MIT OR Apache-2.0 | https://github.com/nvzqz/static-assertions-rs |
| str_indices | 0.4.4 | MIT OR Apache-2.0 | https://github.com/cessen/str_indices |
| streaming-iterator | 0.1.9 | MIT OR Apache-2.0 | https://github.com/sfackler/streaming-iterator |
| strict-num | 0.1.1 | MIT | https://github.com/RazrFalcon/strict-num |
| strum | 0.28.0 | MIT | https://github.com/Peternator7/strum |
| subtle | 2.6.1 | BSD-3-Clause | https://github.com/dalek-cryptography/subtle |
| sum_tree | 0.1.0 | Apache-2.0 |  |
| svg_fmt | 0.4.5 | MIT/Apache-2.0 | https://github.com/nical/rust_debug |
| svgtypes | 0.16.2 | Apache-2.0 OR MIT | https://github.com/linebender/svgtypes |
| swash | 0.2.10 | Apache-2.0 OR MIT | https://github.com/dfrg/swash |
| syn | 2.0.119 | MIT OR Apache-2.0 | https://github.com/dtolnay/syn |
| syn | 3.0.9 | MIT OR Apache-2.0 | https://github.com/dtolnay/syn |
| synstructure | 0.14.0 | MIT | https://github.com/mystor/synstructure |
| sys-locale | 0.3.2 | MIT OR Apache-2.0 | https://github.com/1Password/sys-locale |
| taffy | 0.15.0 | MIT | https://github.com/DioxusLabs/taffy |
| thiserror | 1.0.69 | MIT OR Apache-2.0 | https://github.com/dtolnay/thiserror |
| thiserror | 2.0.21 | MIT OR Apache-2.0 | https://github.com/dtolnay/thiserror |
| tiff | 0.11.3 | MIT | https://github.com/image-rs/image-tiff |
| tiny-keccak | 2.0.2 | CC0-1.0 |  |
| tiny-skia | 0.11.4 | BSD-3-Clause | https://github.com/RazrFalcon/tiny-skia |
| tiny-skia-path | 0.11.4 | BSD-3-Clause | https://github.com/RazrFalcon/tiny-skia/tree/master/path |
| tinystr | 0.8.5 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| tinyvec | 1.13.3 | Zlib OR Apache-2.0 OR MIT | https://github.com/Lokathor/tinyvec |
| toml_datetime | 1.1.2+spec-1.1.0 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_edit | 0.25.18+spec-1.1.0 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_parser | 1.1.5+spec-1.1.0 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| tracing | 0.1.44 | MIT | https://github.com/tokio-rs/tracing |
| tracing-core | 0.1.36 | MIT | https://github.com/tokio-rs/tracing |
| tree-sitter | 0.27.1 | MIT | https://github.com/tree-sitter/tree-sitter |
| tree-sitter-bash | 0.25.1 | MIT | https://github.com/tree-sitter/tree-sitter-bash |
| tree-sitter-c | 0.24.2 | MIT | https://github.com/tree-sitter/tree-sitter-c |
| tree-sitter-containerfile | 0.9.2 | MIT | https://github.com/wharflab/tree-sitter-containerfile |
| tree-sitter-cpp | 0.23.4 | MIT | https://github.com/tree-sitter/tree-sitter-cpp |
| tree-sitter-css | 0.25.0 | MIT | https://github.com/tree-sitter/tree-sitter-css |
| tree-sitter-highlight | 0.27.1 | MIT | https://github.com/tree-sitter/tree-sitter |
| tree-sitter-html | 0.23.2 | MIT | https://github.com/tree-sitter/tree-sitter-html |
| tree-sitter-javascript | 0.25.0 | MIT | https://github.com/tree-sitter/tree-sitter-javascript |
| tree-sitter-json | 0.24.8 | MIT | https://github.com/tree-sitter/tree-sitter-json |
| tree-sitter-language | 0.1.9 | MIT | https://github.com/tree-sitter/tree-sitter |
| tree-sitter-md | 0.5.4 | MIT | https://github.com/tree-sitter-grammars/tree-sitter-markdown |
| tree-sitter-php | 0.25.1 | MIT | https://github.com/tree-sitter/tree-sitter-php |
| tree-sitter-python | 0.25.0 | MIT | https://github.com/tree-sitter/tree-sitter-python |
| tree-sitter-rust | 0.24.2 | MIT | https://github.com/tree-sitter/tree-sitter-rust |
| tree-sitter-scss | 1.0.0 | MIT | https://github.com/tree-sitter-grammars/tree-sitter-scss |
| tree-sitter-sequel | 0.3.11 | MIT | https://github.com/derekstride/tree-sitter-sql.git |
| tree-sitter-toml-ng | 0.7.0 | MIT | https://github.com/tree-sitter-grammars/tree-sitter-toml |
| tree-sitter-twig | 1.1.0 | MIT | https://github.com/gbprod/tree-sitter-twig |
| tree-sitter-typescript | 0.23.2 | MIT | https://github.com/tree-sitter/tree-sitter-typescript |
| tree-sitter-yaml | 0.7.2 | MIT | https://github.com/tree-sitter-grammars/tree-sitter-yaml |
| ttf-parser | 0.25.1 | MIT OR Apache-2.0 | https://github.com/harfbuzz/ttf-parser |
| typenum | 1.20.1 | MIT OR Apache-2.0 | https://github.com/paholg/typenum |
| unicode-bidi | 0.3.18 | MIT OR Apache-2.0 | https://github.com/servo/unicode-bidi |
| unicode-bidi-mirroring | 0.4.0 | MIT/Apache-2.0 | https://github.com/RazrFalcon/unicode-bidi-mirroring |
| unicode-ccc | 0.4.0 | MIT/Apache-2.0 | https://github.com/RazrFalcon/unicode-ccc |
| unicode-ident | 1.0.27 | (MIT OR Apache-2.0) AND Unicode-3.0 | https://github.com/dtolnay/unicode-ident |
| unicode-linebreak | 0.1.5 | Apache-2.0 | https://github.com/axelf4/unicode-linebreak |
| unicode-properties | 0.1.3 | MIT/Apache-2.0 | https://github.com/unicode-rs/unicode-properties |
| unicode-script | 0.5.9 | MIT OR Apache-2.0 | https://github.com/unicode-rs/unicode-script |
| unicode-segmentation | 1.13.3 | MIT OR Apache-2.0 | https://github.com/unicode-rs/unicode-segmentation |
| unicode-vo | 0.1.0 | MIT/Apache-2.0 | https://github.com/RazrFalcon/unicode-vo |
| unicode-width | 0.2.3 | MIT OR Apache-2.0 | https://github.com/unicode-rs/unicode-width |
| url | 2.5.8 | MIT OR Apache-2.0 | https://github.com/servo/rust-url |
| usvg | 0.46.0 | Apache-2.0 OR MIT | https://github.com/linebender/resvg |
| utf8_iter | 1.1.0 | Apache-2.0 OR MIT | https://github.com/hsivonen/utf8_iter |
| utf_types | 1.0.0 | Apache-2.0 OR MIT | https://github.com/hsivonen/utf_types |
| uuid | 1.29.0 | Apache-2.0 OR MIT | https://github.com/uuid-rs/uuid |
| vte | 0.15.0 | Apache-2.0 OR MIT | https://github.com/alacritty/vte |
| waker-fn | 1.2.0 | Apache-2.0 OR MIT | https://github.com/smol-rs/waker-fn |
| walkdir | 2.5.0 | Unlicense/MIT | https://github.com/BurntSushi/walkdir |
| wayland-backend | 0.3.17 | MIT | https://github.com/smithay/wayland-rs |
| wayland-client | 0.31.15 | MIT | https://github.com/smithay/wayland-rs |
| wayland-cursor | 0.31.14 | MIT | https://github.com/smithay/wayland-rs |
| wayland-protocols | 0.32.13 | MIT | https://github.com/smithay/wayland-rs |
| wayland-protocols-plasma | 0.3.12 | MIT | https://github.com/smithay/wayland-rs |
| wayland-protocols-wlr | 0.3.12 | MIT | https://github.com/smithay/wayland-rs |
| wayland-sys | 0.31.11 | MIT | https://github.com/smithay/wayland-rs |
| web-time | 1.1.0 | MIT OR Apache-2.0 | https://github.com/daxpedda/web-time |
| weezl | 0.1.12 | MIT OR Apache-2.0 | https://github.com/image-rs/weezl |
| wgpu | 30.0.0 | MIT OR Apache-2.0 | https://github.com/gfx-rs/wgpu |
| wgpu-core | 30.0.0 | MIT OR Apache-2.0 | https://github.com/gfx-rs/wgpu |
| wgpu-core-deps-linux-android-bsd | 30.0.0 | MIT OR Apache-2.0 | https://github.com/gfx-rs/wgpu |
| wgpu-hal | 30.0.0 | MIT OR Apache-2.0 | https://github.com/gfx-rs/wgpu |
| wgpu-naga-bridge | 30.0.0 | MIT OR Apache-2.0 | https://github.com/gfx-rs/wgpu |
| wgpu-sync | 30.0.0 | MIT OR Apache-2.0 | https://github.com/gfx-rs/wgpu |
| wgpu-types | 30.0.0 | MIT OR Apache-2.0 | https://github.com/gfx-rs/wgpu |
| winnow | 1.0.4 | MIT | https://github.com/winnow-rs/winnow |
| writeable | 0.6.4 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| x11rb | 0.13.2 | MIT OR Apache-2.0 | https://github.com/psychon/x11rb |
| x11rb-protocol | 0.13.2 | MIT OR Apache-2.0 | https://github.com/psychon/x11rb |
| xcursor | 0.3.11 | MIT | https://github.com/esposm03/xcursor-rs |
| xim-ctext | 0.3.0 | MIT | https://github.com/Riey/xim-rs |
| xim-parser | 0.2.1 | MIT | https://github.com/Riey/xim-rs |
| xkbcommon | 0.8.0 | MIT | https://github.com/rust-x-bindings/xkbcommon-rs |
| xkeysym | 0.2.1 | MIT OR Apache-2.0 OR Zlib | https://github.com/notgull/xkeysym |
| xmlwriter | 0.1.0 | MIT | https://github.com/RazrFalcon/xmlwriter |
| xxhash-rust | 0.8.19 | BSL-1.0 | https://github.com/DoumanAsh/xxhash-rust |
| yazi | 0.2.2 | Apache-2.0 OR MIT | https://github.com/dfrg/yazi |
| yeslogic-fontconfig-sys | 6.0.1 | MIT | https://github.com/yeslogic/fontconfig-rs |
| yoke | 0.8.4 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| zbus | 5.19.0 | MIT | https://github.com/z-galaxy/zbus/ |
| zbus-lockstep | 0.5.2 | MIT | https://github.com/luukvanderduim/zbus-lockstep |
| zbus_names | 4.3.5 | MIT | https://github.com/z-galaxy/zbus/ |
| zbus_xml | 5.2.1 | MIT | https://github.com/z-galaxy/zbus/ |
| zcheapstr | 1.2.0 | MIT | https://github.com/z-galaxy/zcheapstr/ |
| zed-font-kit | 0.14.1-zed | MIT OR Apache-2.0 | https://github.com/servo/font-kit |
| zed-xim | 0.4.0-zed | MIT | https://github.com/Riey/xim-rs |
| zeno | 0.3.3 | Apache-2.0 OR MIT | https://github.com/dfrg/zeno |
| zerocopy | 0.8.63 | BSD-2-Clause OR Apache-2.0 OR MIT | https://github.com/google/zerocopy |
| zerofrom | 0.1.8 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| zeroize | 1.9.1 | Apache-2.0 OR MIT | https://github.com/RustCrypto/utils |
| zerotrie | 0.2.5 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| zerovec | 0.11.9 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| zmij | 1.0.23 | MIT | https://github.com/dtolnay/zmij |
| ztracing | 0.1.0 | Apache-2.0 |  |
| zune-core | 0.5.3 | MIT OR Apache-2.0 OR Zlib | https://github.com/etemesi254/zune-image |
| zune-inflate | 0.2.54 | MIT OR Apache-2.0 OR Zlib |  |
| zune-jpeg | 0.5.15 | MIT OR Apache-2.0 OR Zlib | https://github.com/etemesi254/zune-image/tree/dev/crates/zune-jpeg |
| zvariant | 5.15.1 | MIT | https://github.com/z-galaxy/zbus/ |
| zvariant_utils | 4.2.0 | MIT | https://github.com/z-galaxy/zbus/ |
