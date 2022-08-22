# Changelog

This document describes the changes to Minimq between releases.

# [Unreleased]

## Added
* Allow configuration of non-default broker port numbers

## Fixed
* All unacknowledged messages will be guaranteed to be retransmitted upon connection with the
broker.
* The `ReceiveMaximum` property is now sent in the connection request to the broker

# [0.5.3] - 2022-02-14

## Added
* Property comparison now implements PartialEq

# [0.5.2] - 2021-12-14

## Fixed
* Made `mqtt_client` module public to correct documentation
* Partial packet writes no longer cause the connection to the broker to break down.
  [#74](https://github.com/quartiq/minimq/issues/74)

# [0.5.1] - 2021-12-07

## Fixed
* Fixed an issue where the keepalive interval could not be set properly. See
  [#69](https://github.com/quartiq/minimq/issues/69).
* Fixed an issue where the keepalive interval was not set properly. See
  [#70](https://github.com/quartiq/minimq/issues/70).

# [0.5.0] - 2021-12-06

## Added
* Support for the `Will` message specification.
* [breaking] Adding `retained` flag to `publish()` to allow messages to be published in a retained
  manner.

# [0.4.0] - 2021-10-08

* Updating to `std-embedded-nal` v0.1 (dev dependency only; now again used for integration tests)
* Added support for PingReq/PingResp to handle broken TCP connections and configuration of the
keep-alive interval
* Updating tests to use `std-embedded-time`
* Fixing main docs.
* Added support for publishing with QoS 1
* Refactoring network stack management into a separate container class
* Keep-alive settings now take a u16 integer number of seconds

# [0.3.0] - 2021-08-06

* Client ID may now be unspecified to allow the broker to automatically assign an ID.
* Strict client ID check removed to allow broker-validated IDs.
* Updated `generic-array` dependencies to address security vulnerability.
* Updating `new()` to allow the network stack to be non-functional during initialization.
* `Property` updated to implement `Copy`/`Clone`.
* Session state is now maintained
* `Error::SessionReset` can be used to detect need to resubscribe to topics.
* Refactoring client into `Minimq` and `MqttClient` to solve internal mutability issues.
* Updating to `embedded-nal` v0.6
* Removing using of `generic-array` in favor of const-generics.
* Correcting an issue where the client would not reconnect if the broker was restarted.

# [0.2.0] - 2021-02-15

* Updating the `MqttClient::poll()` function to take a `FnMut` closure to allow internal state
  mutation.
* Use the `std-embedded-nal` crate as a dependency to provide a standard `NetworkStack` for
  integration tests.
* Updating `read()` to not block when no data is available. Updating `write()` to progate network
  stack errors out to the user.
* Updating library to re-export `generic-array` publicly.

## [0.1.0] - 2020-08-27

* Initial library release and publish to crates.io

[Unreleased]: https://github.com/quartiq/minimq/compare/0.5.3...HEAD
[0.5.2]: https://github.com/quartiq/minimq/releases/tag/0.5.3
[0.5.2]: https://github.com/quartiq/minimq/releases/tag/0.5.2
[0.5.1]: https://github.com/quartiq/minimq/releases/tag/0.5.1
[0.5.0]: https://github.com/quartiq/minimq/releases/tag/0.5.0
[0.4.0]: https://github.com/quartiq/minimq/releases/tag/0.4.0
[0.3.0]: https://github.com/quartiq/minimq/releases/tag/0.3.0
[0.2.0]: https://github.com/quartiq/minimq/releases/tag/0.2.0
[0.1.0]: https://github.com/quartiq/minimq/releases/tag/0.1.0
