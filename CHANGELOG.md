# Changelog

This document describes the changes to Minimq between releases.

# Unrelease

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
