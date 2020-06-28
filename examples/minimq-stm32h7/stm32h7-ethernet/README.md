# [Documentation](https://docs.rs/stm32h7-ethernet)

# stm32h7-ethernet

[![docs.rs](https://docs.rs/stm32h7-ethernet/badge.svg)](https://docs.rs/stm32h7-ethernet)
[![Travis](https://travis-ci.com/richardeoin/stm32h7-ethernet.svg?branch=master)](https://travis-ci.com/richardeoin/stm32h7-ethernet)
[![Crates.io](https://img.shields.io/crates/v/stm32h7-ethernet.svg)](https://crates.io/crates/stm32h7-ethernet)

This crate implements a [smoltcp][] device interface `phy::Device` for
the STM32H7 series of microcontrollers.

Multiple PHYs are supported:
- SMSC LAN8742a
- Micrel KSZ8081R

To build this crate, a device feature of [stm32h7xx-hal][] must be
selected. For example:

```
cargo build  --features stm32h743v
```

When using this crate as a dependency, it re-exports the device
features so you can specify them in Cargo.toml

```
stm32h7-ethernet = { version = 0.4.0, features = ["stm32h743v"] }
```

Specifing device features is not nessesary if an identical version of
stm32h7xx_hal is in use somewhere else in your dependency tree. In this
case cargo unions the feature flags.

## Prerequisites

```
rustup target add thumbv7em-none-eabihf
rustup component add llvm-tools-preview
cargo install cargo-binutils
```

## Hardware Examples

### STM32H747I-DISCO

Targeting the STM32H747I-DISCO evaluation board from ST.

*Note:* Close solder jumper SB8!

### NUCLEO-H743ZI2

Targeting the NUCLEO-H743ZI2 evaluation board from ST.

Quickstart:

```
# Build the .bin file
cargo objcopy --example nucleo-h743zi2 --release --features="phy_lan8742a stm32h7xx-hal/rt stm32h7xx-hal/revision_v stm32h743v" -- -O binary target/thumbv7em-none-eabihf/release/examples/nucleo-h743zi2.bin

# Copy it to your device (in this case located at path `D:`).
cp ./target/thumbv7em-none-eabihf/release/examples/nucleo-h743zi2.bin D:
```

### License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

[stm32h7xx-hal]: https://github.com/stm32-rs/stm32h7xx-hal
[smoltcp]: https://github.com/smoltcp-rs/smoltcp
