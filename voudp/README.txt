Here is going to be a shortened version of the specification of VoUDP:
Note: all u32 are BIG ENDIAN. () represents a byte for reference. ... means variable in size

Join packet:
    C2S: 5 bytes
        [0x01 ()] + [channel_id ()()()()]

Audio packet: (C2S + S2C)
    C2S + S2C: > 1 byte
        [0x02 ()] + [opus frame (default: 960 samples per 20ms) ...]

Leave packet: (AKA: EOF packet)
    C2S: 1 byte
        [0x03 ()]

Mask packet: (AKA: Nick packet)
    C2S: > 1 byte
        [0x04 ()] + [UTF-8 encoded string (nickname) ...]

List Packet:
    S2C: > 9 bytes
        [0x05 ()] + { [UTF-8 encoded string ...] + [0x01 delimiter ()] + [0x000000md (m: mute, d: deaf) flags ()]} + [0x01 delimiter ()] + {another block of this per client}

Chat Packet:
    C2S: > 1 byte
        [0x06 ()] + [UTF-8 encoded string (chat message) ...]
    S2C: > 1 byte
        [0x06 ()] + [UTF-8 encoded string (sender name) ...] + [0x01 delimiter ()] + [UTF-8 encoded string (chat message) ...]

Nick error packet:
    S2C: 1 byte
        [0x07 ()]

Control packet:
    C2S: > 2 bytes
        [0x08 ()] + [control option ()] + [more bytes if packet needs more context...]



--- Control options ---
    0x01: set deafened
    0x02: set undeafened
    0x03: set muted
    0x04: set unmuted
    0x05: set volume + u8 representing volume (0x0 lowest, 0x11111111 highest)