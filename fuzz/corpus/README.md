# Corpus

The checked-in corpus is small and curated on purpose.
This file is the source of truth for what the checked-in seed files mean and
how each target interprets their bytes.

## Target Formats

- `fuzz_received_packet`: raw MQTT packet bytes.
- `fuzz_packet_reader`: first byte is `PacketReader` storage size, remaining bytes are the inbound byte stream.
- `fuzz_serializer`:
  - byte 0: output buffer length
  - byte 1: packet family tag
  - byte 2: QoS selector
  - byte 3: retain flag
  - byte 4: aux selector for properties/reason variants
  - byte 5+: split marker, then topic bytes and payload bytes

The seed files themselves are intentionally raw binary blobs. They are small,
stable, and named for the packet shape they represent. If a seed changes, keep
the filename meaningful and update this document.

## Seeds

### `fuzz_received_packet`

- `seed_connack`: minimal successful `CONNACK`
- `seed_connack_max_packet_size`: `CONNACK` with `MaximumPacketSize`
- `seed_publish_qos0`: minimal QoS 0 `PUBLISH`
- `seed_publish_response_topic`: `PUBLISH` with `ResponseTopic`
- `seed_publish_user_property`: `PUBLISH` with `UserProperty`
- `seed_publish_correlation_data`: `PUBLISH` with `CorrelationData`
- `seed_puback`: minimal `PUBACK`
- `seed_puback_reason_string`: `PUBACK` with `ReasonString`
- `seed_suback`: minimal `SUBACK`
- `seed_suback_user_property`: `SUBACK` with `UserProperty`
- `seed_unsuback`: minimal `UNSUBACK`
- `seed_pingresp`: minimal `PINGRESP`
- `seed_pubrec`: minimal `PUBREC`
- `seed_pubrec_reason_string`: `PUBREC` with `ReasonString`
- `seed_pubrel`: minimal `PUBREL`
- `seed_pubrel_reason_string`: `PUBREL` with `ReasonString`
- `seed_pubcomp`: minimal `PUBCOMP`
- `seed_pubcomp_reason_string`: `PUBCOMP` with `ReasonString`
- `seed_disconnect`: `DISCONNECT` with reason only
- `seed_disconnect_reason_string`: `DISCONNECT` with `ReasonString`

### `fuzz_packet_reader`

- `seed_connack`: fragmented `CONNACK`
- `seed_connack_max_packet_size`: fragmented `CONNACK` with `MaximumPacketSize`
- `seed_publish_qos0`: fragmented QoS 0 `PUBLISH`
- `seed_publish_response_topic`: fragmented `PUBLISH` with `ResponseTopic`
- `seed_publish_user_property`: fragmented `PUBLISH` with `UserProperty`
- `seed_publish_correlation_data`: fragmented `PUBLISH` with `CorrelationData`
- `seed_pubrel`: fragmented `PUBREL`
- `seed_pubrec_reason_string`: fragmented `PUBREC` with `ReasonString`
- `seed_disconnect`: fragmented `DISCONNECT`
- `seed_disconnect_reason_string`: fragmented `DISCONNECT` with `ReasonString`

### `fuzz_serializer`

- `seed_pingreq`: fixed-header-only path
- `tiny_buf`: undersized buffer path
- `seed_publish`: plain `PUBLISH`
- `seed_publish_with_props`: `PUBLISH` with properties selected
- `seed_publish_correlated`: `PUBLISH` with correlation data selected
- `seed_subscribe`: plain `SUBSCRIBE`
- `seed_subscribe_with_props`: `SUBSCRIBE` with property selected
- `seed_unsubscribe`: plain `UNSUBSCRIBE`
- `seed_connect`: plain `CONNECT`
- `seed_connect_with_props`: `CONNECT` with properties selected
- `seed_pubrel`: plain `PUBREL`
- `seed_puback`: plain `PUBACK`
- `seed_puback_reason_props`: `PUBACK` with non-empty reason properties
- `seed_pubrec`: plain `PUBREC`
- `seed_pubrec_reason_props`: `PUBREC` with non-empty reason properties
- `seed_pubcomp`: plain `PUBCOMP`
- `seed_disconnect_req`: `DISCONNECT`
