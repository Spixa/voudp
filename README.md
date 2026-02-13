# VoUDP Protocol 0.2 specification
---

## Client → Server Packets (C2S)

| Packet | Layout (inside encrypted payload) | Reliable? | Notes |
|--------|---------------------------------|------------|------|
| **Join** | `[0x01 ()] + [channel_id ()()()()]` | Yes | Client requests to join a channel |
| **Audio** | `[0x02 ()] + [Opus frame ...]` | Optional | Only reliable if needed for certain control frames |
| **Leave / EOF** | `[0x03 ()]` | No | Signals leaving channel |
| **Mask / Nick** | `[0x04 ()] + [UTF-8 nickname ...]` | Yes | Nickname change |
| **Sync Commands** | `[0x0c ()]` | Yes | Client requests server to sync commands |
| **Console Register** | `[0xff ()] + [UTF-8 server password ...]` | Yes | Only needed when registering console |
| **Control** | `[0x08 ()] + [control option ()] + [extra bytes if needed]` | Yes | Options: 0x01=deaf, 0x02=undeaf, 0x03=mute, 0x04=unmute |
| **Chat** | `[0x06 ()] + [UTF-8 message ...]` | Optional | Sent as reliable only if ordering matters |
| **Console Command** | `[0x0d ()] + [UTF-8 command ...]` | Yes | Requires ACK from server |

---

## Server → Client Packets (S2C)

| Packet | Layout (inside encrypted payload) | Reliable? | Notes |
|--------|---------------------------------|------------|------|
| **Audio** | `[0x02 ()] + [Opus frame ...]` | No | Low latency; reliability optional per client |
| **List** | `[0x05 ()] + [unmasked_count ()()()()] + [masked_count ()()()()] { [UTF-8 string ...] + [0x01 delimiter ()] + [u8 flags (mute/deaf) ()] } + [0x01 delimiter ()]` | No | Client roster info |
| **Flow Join** | `[0x0a ()] + [UTF-8 username ...]` | No | Indicates a user joined channel |
| **Flow Leave** | `[0x0b ()] + [UTF-8 username ...]` | No | Indicates a user left channel |
| **Flow Renick** | `[0x10 ()] + [old_mask_len ()] + [old_mask ...] + [new_mask_len ()] + [new_mask ...]` | No | Nickname change |
| **DM / Broadcast** | `[0x11 ()] + [UTF-8 message ...]` | Optional | Only reliable if ordering matters |
| **Chat** | `[0x06 ()] + [UTF-8 sender ...] + [0x01 delimiter ()] + [sender team ()] + [UTF-8 message ...]` | Optional | Displayed in chat UI |
| **Nick error** | `[0x07 ()]` | Yes | Reliable |
| **Console Command Response** | `[0x0d ()] + [UTF-8 response ...]` | Yes | Reliable ACK from server |
| **Console EOF / Keepalive** | `[0x03 ()]` / `[0x04 ()]` | No | Sent to registered consoles |

---

## Reliable Transport & ACKs

- Reliable packets: `0x80 + seq + payload`.  
- ACK packets: `0x81 + seq`.  
- Client retransmits if no ACK after timeout (default: 100ms).  
- Server ignores sequence enforcement; it only replies with ACK when it receives reliable packets.  

---

## Encryption / Nonce

- Every packet (C2S or S2C) is **encrypted with ChaCha20Poly1305**.  
- Nonce: `12 bytes` → `[4-byte session random prefix || 8-byte monotonic counter]`.  
- The counter **increments per packet**, shared across clones / threads using `AtomicU64`.  
- Session prefix ensures **different ciphertexts for identical payloads in different sessions**.  

---

## Control Options (inside Control packets)

| Option | Byte | Description |
|--------|------|-------------|
| Set Deafened | 0x01 | Client cannot hear audio |
| Set Undeafened | 0x02 | Client can hear audio |
| Set Muted | 0x03 | Client cannot send audio |
| Set Unmuted | 0x04 | Client can send audio |

---

## Internal Flags (for reliable transport, non-standard)

| Flag | Byte | Purpose |
|------|------|--------|
| RELIABLE | 0x80 | Indicates payload must be ACKed |
| ACK | 0x81 | Acknowledges a reliable packet |

---

**End of VoUDP v0.2 Specification**
