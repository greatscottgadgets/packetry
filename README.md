# luna-analyzer-rust

WIP USB analysis frontend for LUNA

## Development

### Prereqs

 * Rust - https://www.rust-lang.org/tools/install
 * GTK4 - This is still relatively new, but is included in Ubuntu 21.04+ and Debian unstable

### Build/run

```
# Build. (Cargo will create a debug build by default but these can be particularly slow, so make sure to specify a release build)
cargo build --release

# Run
cargo run --release <path/to/capture.pcap>
```