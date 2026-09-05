# `mqtt-staging` Python sender

Python 3.11+ sender for the MQTT Staging protocol implemented by the sibling
Rust crate.

```sh
python -m pip install mqtt-staging
mqtt-staging --prefix dt/device/staging --file object.bin
```

The command sends one bounded object. The device owns storage preparation,
validation, activation, and reboot policy.
