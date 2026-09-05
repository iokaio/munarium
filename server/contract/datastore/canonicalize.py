# SPDX-License-Identifier: Apache-2.0
"""artifact@1 canonicalization, in Python.

The reference implementation of the rules in `canonicalization.schema.json`,
used to generate and check the identity vectors. The Rust implementation in
`munarium-datastore` must agree with this on every vector; where they disagree,
the VECTORS are the contract and both implementations are suspect until one is
shown to violate them.

RFC 8785 (JCS), restricted: floating-point numbers are refused rather than
formatted. See the schema's `why_no_floats` for the reasoning -- ES6
Number::toString is the part JCS implementations get wrong, and nothing in these
documents needs a float.
"""

import hashlib
import json
import sys

_SHORT = {0x08: "\\b", 0x09: "\\t", 0x0A: "\\n", 0x0C: "\\f", 0x0D: "\\r"}


def _string(s):
    out = ['"']
    for ch in s:
        o = ord(ch)
        if ch == '"':
            out.append('\\"')
        elif ch == "\\":
            out.append("\\\\")
        elif o in _SHORT:
            out.append(_SHORT[o])
        elif o < 0x20:
            out.append("\\u%04x" % o)
        else:
            out.append(ch)
    out.append('"')
    return "".join(out)


def _key(s):
    # JCS sorts object members by UTF-16 code unit. Comparing big-endian UTF-16
    # bytes is exactly that order, and differs from Python's native code-point
    # ordering above the BMP -- which is the whole reason this is not `sorted()`.
    return s.encode("utf-16-be")


def serialize(value, path="$"):
    if value is None:
        return "null"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, int):
        if not (-(2**63) <= value < 2**63):
            raise ValueError(f"{path}: integer outside signed 64-bit range")
        return str(value)
    if isinstance(value, float):
        raise ValueError(
            f"{path}: floating-point numbers are forbidden under artifact@1 "
            f"(got {value!r}); carry a ratio as a decimal STRING at a declared scale"
        )
    if isinstance(value, str):
        return _string(value)
    if isinstance(value, list):
        return "[" + ",".join(serialize(v, f"{path}[{i}]") for i, v in enumerate(value)) + "]"
    if isinstance(value, dict):
        for k in value:
            if not isinstance(k, str):
                raise ValueError(f"{path}: non-string object key {k!r}")
        items = sorted(value.items(), key=lambda kv: _key(kv[0]))
        return "{" + ",".join(_string(k) + ":" + serialize(v, f"{path}.{k}") for k, v in items) + "}"
    raise ValueError(f"{path}: unsupported type {type(value).__name__}")


def canonical_bytes(value):
    return serialize(value).encode("utf-8")


def sha256_hex(value):
    return hashlib.sha256(canonical_bytes(value)).hexdigest()


def index_version_id(build_spec):
    """'idx2-' + the full SHA-256 of the canonical BuildSpec."""
    return "idx2-" + sha256_hex(build_spec)


def artifact_id(manifest):
    """The full SHA-256 of the canonical manifest. No prefix, one identifier."""
    return sha256_hex(manifest)


def _load(p):
    with open(p, encoding="utf-8") as fh:
        return json.load(fh)


if __name__ == "__main__":
    # `python canonicalize.py doc.json` prints the canonical bytes and the hash,
    # so a divergence can be diffed rather than argued about.
    doc = _load(sys.argv[1])
    body = canonical_bytes(doc)
    sys.stdout.buffer.write(body + b"\n")
    print(hashlib.sha256(body).hexdigest())
