#!/usr/bin/env python3
"""Inject deterministic BPv7 chaos into the RV32 QEMU UART TCP backend."""

import argparse
import select
import socket
import sys
import time


VALID_BUNDLE = bytes([
    0xA6, 0x01, 0x07, 0x02, 0x03,
    0x04, 0x64, ord("d"), ord("e"), ord("s"), ord("t"),
    0x05, 0x63, ord("s"), ord("r"), ord("c"),
    0x06, 0x82, 0x19, 0x04, 0xD2, 0x01,
    0x07, 0x19, 0x0E, 0x10,
    0x85, 0x01, 0x01, 0x00, 0x00, 0x45,
    ord("h"), ord("e"), ord("l"), ord("l"), ord("o"),
])


def expired_bundle():
    # rv32_sim validates with timestamp 0, so creation=0/lifetime=0 is expired.
    bundle = bytearray(VALID_BUNDLE)
    bundle[18:22] = b"\x00\x00"
    bundle[20:24] = b"\x07\x00"
    return bytes(bundle)


def missing_payload_bundle():
    return VALID_BUNDLE[:26]


def truncated_bundle():
    return VALID_BUNDLE[:-3]


def bitflip_bundle():
    bundle = bytearray(VALID_BUNDLE)
    bundle[0] ^= 0xFF
    return bytes(bundle)


def noise_bundle(seed):
    output = bytearray(32)
    value = seed ^ 0xA5A55A5A5A5A5A5A
    for index in range(len(output)):
        value ^= (value << 13) & ((1 << 64) - 1)
        value ^= value >> 7
        value ^= (value << 17) & ((1 << 64) - 1)
        output[index] = (value >> 8) & 0xFF
    return bytes(output)


def read_available(serial, timeout):
    output = bytearray()
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        remaining = max(0.0, deadline - time.monotonic())
        readable, _, _ = select.select([serial], [], [], remaining)
        if not readable:
            break
        chunk = serial.recv(4096)
        if not chunk:
            break
        output.extend(chunk)
    return bytes(output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", default="127.0.0.1:4567")
    parser.add_argument("--interval-ms", type=int, default=150)
    parser.add_argument("--count", type=int, default=0, help="0 means infinite")
    parser.add_argument("--once", action="store_true")
    parser.add_argument(
        "--pattern",
        choices=("valid", "expired", "missing-payload", "truncated", "bitflip", "noise"),
        help="send only one pattern",
    )
    args = parser.parse_args()

    host, port_text = args.target.rsplit(":", 1)
    port = int(port_text)
    patterns = [
        ("valid", lambda _: VALID_BUNDLE),
        ("expired", lambda _: expired_bundle()),
        ("missing-payload", lambda _: missing_payload_bundle()),
        ("truncated", lambda _: truncated_bundle()),
        ("bitflip", lambda _: bitflip_bundle()),
        ("noise", noise_bundle),
    ]
    if args.pattern is not None:
        patterns = [pattern for pattern in patterns if pattern[0] == args.pattern]

    try:
        with socket.create_connection((host, port), timeout=5) as serial:
            startup = read_available(serial, 1.0)
            if startup:
                sys.stdout.buffer.write(startup)
                sys.stdout.flush()

            sent = 0
            seed = 0xC0FFEE
            while not args.count or sent < args.count:
                name, build_payload = patterns[sent % len(patterns)]
                payload = build_payload(seed)
                # Reset an incomplete previous frame before each test case.
                serial.sendall(b"\xA6" + payload)
                print(f"sent #{sent}: pattern={name} len={len(payload)}", flush=True)

                response = read_available(serial, max(args.interval_ms, 25) / 1000.0)
                if response:
                    sys.stdout.buffer.write(response)
                    sys.stdout.flush()

                sent += 1
                seed = (seed + 0x9E3779B97F4A7C15) & ((1 << 64) - 1)
                if args.once:
                    break
                if args.interval_ms > 0:
                    time.sleep(args.interval_ms / 1000.0)
    except OSError as error:
        print(f"RV32 UART connection failed: {error}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
