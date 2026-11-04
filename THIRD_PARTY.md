# Third-party components

Exact dependency versions and registry checksums are recorded in `Cargo.lock`. Use `cargo metadata --locked --format-version 1` for dependency package metadata, including license identifiers and source locations.

The main components include eframe/egui, CPAL, Symphonia, realfft, rfd, hound, flacenc, image, mp3lame-encoder, and vorbis_rs. Their licenses are independent of any license later chosen for this application's source.

MP3 encoding compiles the LAME sources bundled by `mp3lame-sys` 0.2.0. LAME is LGPL licensed. Vorbis and Ogg native sources are bundled through `aotuv_lancer_vorbis_sys` and `ogg_next_sys`, respectively. Distributors must preserve applicable notices and comply with each component's redistribution terms, including any source/relinking requirements for static linking.

Upstream sources:

- https://crates.io/crates/mp3lame-sys/0.2.0
- https://lame.sourceforge.io/
- https://github.com/ComunidadAylas/vorbis-rs
- https://github.com/xiph/vorbis
- https://github.com/xiph/ogg

This app does not distribute Supertone software, proprietary model files, or the original project's plugin/reference binaries.
