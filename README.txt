** VoUDP Specification **

Here is going to be a shortened version of the specification of VoUDP for future reference:
Note: all u32 are BIG ENDIAN. () represents a byte for reference. ... means variable in size. {} means repeated per client

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
        [0x05 ()] + [unmasked_count ()()()()] + [masked_count ()()()()] { [UTF-8 encoded string (mask)...] + [0x01 delimiter ()] + [0x000000md (m: mute, d: deaf) flags ()]} + [0x01 delimiter ()]

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

Flow Join packet (user joined channel):
    S2C: > 1 byte
        [0x0a ()] + [UTF-8 encoded string (username) ...]

Flow Leave packet (user left channel):
    S2C: > 1 byte
        [0x0b ()] + [UTF-8 encoded string (username) ...]

Sync Commands packet:
    C2S: 1 byte
        [0x0c ()]

Console Command packet:
    C2S: > 1 byte
        [0x0d ()] + [UTF-8 encoded string (command) ...]
    S2C: > 1 byte
        [0x0d ()] + [UTF-8 encoded string (response) ...]

Flow Renick packet (user changed nickname):
    S2C: > 2 bytes
        [0x10 ()] + [old_mask_len ()] + [old_mask ...] + [new_mask_len ()] + [new_mask ...]

DM packet (direct message / broadcast):
    S2C: > 1 byte
        [0x11 ()] + [UTF-8 encoded string (message) ...]

Register Console packet:
    C2S: > 1 byte
        [0xff ()] + [UTF-8 encoded string (console name) ...]

--- Console packet types (for already registered consoles) ---
    0x03: console EOF
    0x04: console keepalive
    0x0d: console command

--- Control options ---
    0x01: set deafened
    0x02: set undeafened
    0x03: set muted
    0x04: set unmuted

--- Internal flags for packet processing (non-standard, for reliable transport) ---
    0x80: reliable flag
    0x81: ack flag