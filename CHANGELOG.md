# Changelog

This document describes the changes to Minimq between releases.

# Unreleased

* Updating to `std-embedded-nal` v0.1 (dev dependency only; now again used for integration tests)
* Added support for PingReq/PingResp to handle broken TCP connections and configuration of the
keep-alive interval
* Updating tests to use `std-embedded-time`
* Fixing main docs.
* Refactoring network stack management into a separate container class

# Version 0.3.0
Version 0.3.0 was published on 2021-08-06

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

# Version 0.2.0
Version 0.2.0 was published on 2021-02-15

* Updating the `MqttClient::poll()` function to take a `FnMut` closure to allow internal state
  mutation.
* Use the `std-embedded-nal` crate as a dependency to provide a standard `NetworkStack` for
  integration tests.
* Updating `read()` to not block when no data is available. Updating `write()` to progate network
  stack errors out to the user.
* Updating library to re-export `generic-array` publicly.

## Version 0.1.0
Version 0.1.0 was published on 2020-08-27

* Initial library release and publish to crates.io
