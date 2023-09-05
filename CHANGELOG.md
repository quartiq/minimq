# Changelog

This document describes the changes to Minimq between releases.

# [Unreleased]

## Changed
* [breaking] Const generics for message size and allowable in-flight messages have been removed.
  Instead, the user now supplies an RX buffer, a TX buffer, and a session state buffer.
 * Setup-only configuration APIs such as `set_will()` and `set_keepalive_interval()` have been moved
 to a new `Config` structure that is supplied to the `Minimq::new()` constructor to simplify the
 client.
* Added a new `correlate()` API to publication builder to easily add correlation data.


## Added
* Support for subscribing at `QoS::ExactlyOnce`
* Support for downgrading the `QoS` to the maximum permitted by the server
* Brokers may now be provided using domain-name syntax or static IP addresses.

## Fixed
* Fixed an issue where PubComp was serialized with an incorrect control code
* Fixed an issue where some response control packets would be improperly serialized
* The client now respects the server max packet reception


# [0.7.0] - 2023-06-22

## Fixed
* [breaking] Embedded-nal version updated to 0.7
* Fixed an issue where the MQTT client would become permanently inoperable when the broker
  disconnected under certain conditions.

# [0.6.2] - 2023-04-05

## Fixed
* `UserProperty` now properly serializes key-then-value. Serialization order was previously
  unintentionally inverted.

# [0.6.1] - 2022-11-03

## Fixed
* `PubAck` can now be deserialized when no properties are present, but a reason code is specified.

# [0.6.0] - 2022-11-03

## Added
* Allow configuration of non-default broker port numbers
* Support added for QoS::ExactlyOnce transmission
* `poll()` now supports returning from the closure. An `Option::Some()` will be generated whenever
  the `poll()` closure executes on an inbound `Publish` message.
* `subscribe()` now supports subscription configuration, such as retain configuration, QoS
  specification, and no-local publications.
* `subscribe()` modified to take a list of topic filters.
* Subscriptions at QoS::AtLeastOnce are now supported.
* Added a new `Publication` builder API to easily construct new messages or reply to received
  messages.

## Changed
* [breaking] The client is no longer publicly exposed, and is instead accessible via `Minimq::client()`
* Single MQTT packets are now processed per `Minimq::poll()` execution, reducing stack usage.
* [breaking] External crate is now used for `varint` encoding. Varints changed to u32 format.
* Deserialization and serialization is now handled directly by `serde`.
* [breaking] Properties are now wrapped in MQTT-specific data types.
* [breaking] Packets now support reason codes. Unused error codes were removed.
* `poll()` updated such that the user should call it repeatedly until it returns `Ok(None)`.
* [breaking] Property handling has been changed such that an arbitrary number can be received.
  Properties are deserialized as needed by the application.
* [breaking] `publish` now accepts a single `Pub` message type, which can be easily created using
  the `Publication` builder utility.

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

[Unreleased]: https://github.com/quartiq/minimq/compare/0.7.0...HEAD
[0.7.0]: https://github.com/quartiq/minimq/releases/tag/0.7.0
[0.6.2]: https://github.com/quartiq/minimq/releases/tag/0.6.2
[0.6.1]: https://github.com/quartiq/minimq/releases/tag/0.6.1
[0.6.0]: https://github.com/quartiq/minimq/releases/tag/0.6.0
[0.5.3]: https://github.com/quartiq/minimq/releases/tag/0.5.3
[0.5.2]: https://github.com/quartiq/minimq/releases/tag/0.5.2
[0.5.1]: https://github.com/quartiq/minimq/releases/tag/0.5.1
[0.5.0]: https://github.com/quartiq/minimq/releases/tag/0.5.0
[0.4.0]: https://github.com/quartiq/minimq/releases/tag/0.4.0
[0.3.0]: https://github.com/quartiq/minimq/releases/tag/0.3.0
[0.2.0]: https://github.com/quartiq/minimq/releases/tag/0.2.0
[0.1.0]: https://github.com/quartiq/minimq/releases/tag/0.1.0
