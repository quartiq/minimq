# Changelog

This document describes the changes to Minimq between releases.

# Unpublished
* Updating the `MqttClient::poll()` function to take a `FnMut` closure to allow internal state
  mutation.
* Use the `std-embedded-nal` crate as a dependency to provide a standard `NetworkStack` for
  integration tests.
* Updating `read()` to not block when no data is available. Updating `write()` to progate network
  stack errors out to the user.

## Version 0.1.0
Version 0.1.0 was published on 2020-08-27

* Initial library release and publish to crates.io
