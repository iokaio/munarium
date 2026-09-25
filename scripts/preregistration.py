# SPDX-License-Identifier: Apache-2.0
"""Git object inclusion proofs for frozen manifests in shallow/offline checkouts.

The proof establishes commit inclusion, not a trusted wall-clock timestamp.
"""

import base64
import hashlib
import json
import subprocess
from pathlib import Path


def object_id(kind, data):
    return hashlib.sha1(f"{kind} {len(data)}\0".encode() + data).hexdigest()


def child(tree, name, mode):
    entries = {}
    while tree:
        header, tree = tree.split(b"\0", 1)
        entry_mode, entry_name = header.split(b" ", 1)
        if len(tree) < 20 or entry_name in entries:
            raise ValueError("invalid_git_tree")
        entries[entry_name] = (entry_mode, tree[:20].hex())
        tree = tree[20:]
    if name.encode() not in entries or entries[name.encode()][0] != mode:
        raise ValueError("manifest_not_in_tree")
    return entries[name.encode()][1]


def verify(manifest, commit, proof):
    try:
        commit_data = base64.b64decode(proof["commit_object"], validate=True)
        if object_id("commit", commit_data) != commit:
            raise ValueError("commit_object_mismatch")
        header = commit_data.split(b"\n", 1)[0]
        if not header.startswith(b"tree "):
            raise ValueError("missing_commit_tree")
        expected = header[5:].decode("ascii")
        names = [
            "server",
            "conformance",
            "results",
            manifest["id"].replace(":", "-") + ".json",
        ]
        if len(proof["tree_objects"]) != len(names):
            raise ValueError("invalid_tree_chain")
        for index, name in enumerate(names):
            tree = base64.b64decode(proof["tree_objects"][index], validate=True)
            if object_id("tree", tree) != expected:
                raise ValueError("tree_object_mismatch")
            expected = child(tree, name, b"100644" if index == 3 else b"40000")
        blob = base64.b64decode(proof["manifest_blob"], validate=True)
        if object_id("blob", blob) != expected or json.loads(blob) != manifest:
            raise ValueError("committed_manifest_mismatch")
    except (KeyError, TypeError, UnicodeError) as exc:
        raise ValueError("malformed_preregistration_proof") from exc


def capture(root, manifest, commit):
    root = Path(root).resolve()

    def get(kind, identity):
        return subprocess.check_output(
            [
                "git",
                "-c",
                f"safe.directory={root.as_posix()}",
                "-C",
                str(root),
                "cat-file",
                kind,
                identity,
            ],
            stderr=subprocess.DEVNULL,
        )

    commit_data = get("commit", commit)
    identity = commit_data.split(b"\n", 1)[0][5:].decode()
    trees = []
    for index, name in enumerate(
        ["server", "conformance", "results", manifest["id"].replace(":", "-") + ".json"]
    ):
        tree = get("tree", identity)
        trees.append(base64.b64encode(tree).decode())
        identity = child(tree, name, b"100644" if index == 3 else b"40000")
    proof = {
        "commit_object": base64.b64encode(commit_data).decode(),
        "tree_objects": trees,
        "manifest_blob": base64.b64encode(get("blob", identity)).decode(),
    }
    verify(manifest, commit, proof)
    return proof
