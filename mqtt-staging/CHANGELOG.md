<!-- markdownlint-disable MD024 -->
# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [UNRELEASED](https://github.com/quartiq/minimq/compare/mqtt-staging-v0.1.3...HEAD) - DATE

## Fixed

* Preserve queued MQTT work through cancellation and temporary capacity limits.
* Preserve subscription startup across status updates and session resume.
* Ignore retained staging commands.

## [0.1.3](https://github.com/quartiq/minimq/compare/mqtt-staging-v0.1.2...mqtt-staging-v0.1.3) - 2026-10-09

## [0.1.2](https://github.com/quartiq/minimq/compare/mqtt-staging-v0.1.1...mqtt-staging-v0.1.2) - 2026-09-15

## Fixed

* Handle MQTT publish-buffer exhaustion without panicking.

## [0.1.1](https://github.com/quartiq/minimq/compare/0ae37e5...mqtt-staging-v0.1.1) - 2026-09-08

## Fixed

* Refresh the MQTT receive limit for reconstructed services on resumed sessions.

## [0.1.0](https://github.com/quartiq/mqtt-staging/releases/tag/v0.1.0) - 2026-09-01

* Initial release
