# mqtt-staging

`mqtt-staging` moves one bounded byte object into application-owned storage over
MQTT 5. It provides a `no_std` Rust device state machine and one Python host
command. The device requests sequential, write-aligned chunks, so an embedded
application can stage directly into flash without buffering the whole object.

Firmware is the prime use case (notably Stabilizer with Embassy flash), but OTA
policy is deliberately outside the protocol. The application still owns MQTT
I/O, storage, allocation, timeouts, checksum verification, activation, and
reboot. FNV-1a detects staging errors; it does not authenticate firmware.

## Host

```console
pipx install ./py
mqtt-staging --prefix dt/sinara/stabilizer/ota --file firmware.bin
```

`--broker` defaults to `$BROKER`, then `localhost:1883`. From a checkout,
`python py/mqtt_staging.py ...` runs the same command. The prefix is the exact root
chosen by the application; the command appends only `/manifest`, `/status`, and
`/chunk`.

## Device

```toml
[dependencies]
mqtt-staging = "0.1"
```

Create a service with the staging capacity and direct-write limits:

```rust,ignore
let mut staging = mqtt_staging::Service::new(
    "dt/sinara/stabilizer/ota",
    mqtt_staging::Config {
        capacity: firmware.capacity(),
        max_chunk_size: 4096,
        write_size: firmware.write_size(),
    },
)?;
```

An Embassy flash worker can handle the emitted requests directly:

```rust,ignore
let ok = match request {
    mqtt_staging::StagingRequest::Prepare { size } =>
        firmware.prepare(size).await.is_ok(),
    mqtt_staging::StagingRequest::Write(write) => {
        let mut ok = firmware.write(write.offset, write.payload).await.is_ok();
        if ok && let Some(expected) = write.fnv1a64 {
            ok = firmware.finish(write.size, expected).await.is_ok();
        }
        ok
    }
};
staging.complete_request(ok);
```

Here `firmware.write()` can be a thin call to Embassy
`FirmwareUpdater::write_firmware()`. `finish()` verifies the staged bytes; the
application decides whether to call `mark_updated()` and reboot.

Call `begin_startup()` after each MQTT connection, `step()` until it is
quiescent, and route inbound publishes through `handle()`. `StagingWrite` borrows
Minimq's current RX packet. A concurrent worker must copy that payload once
before the next connection operation; a synchronous worker may consume it in
place.

## Protocol

| Topic | Publisher | Payload |
| --- | --- | --- |
| `<prefix>/manifest` | host | JSON manifest |
| `<prefix>/status` | device | JSON state and next-chunk properties |
| `<prefix>/chunk` | host | object bytes |

```json
{"id":"85944171f73967e8","size":6,"fnv1a64":9625390261332436968}
```

`id` is a 1–48 byte ASCII token using letters, digits, `-`, `_`, or `.`. The
device echoes it in status and as MQTT `CorrelationData`. Status also contains
`state`, `code`, `next_offset`, `size`, `mtu`, and `write_size`, with:

- `ResponseTopic`: `<prefix>/chunk`
- `UserProperty("offset", decimal)`: requested byte offset

The state path is `idle -> preparing -> ready <-> writing -> complete`; an
active transfer may instead end in `error`. QoS 1 manifest and chunk duplicates
are idempotent. Chunks are stop-and-wait and sequential; future offsets fail,
and final alignment padding must be `0xff`. A new transfer requires a new
service instance after `complete` or `error`.

## Tests

```console
python -m unittest discover -s py/tests -p 'test_*.py'
cargo test --all-targets
BROKER=localhost:1883 cargo test --test end_to_end -- --ignored
```

The ignored test crosses a real broker from the Python command through Minimq
to mock storage. Set `MQTT_STAGING_FEEDER` to override the command.

## License

Licensed under either Apache-2.0 or MIT.
