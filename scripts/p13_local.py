# SPDX-License-Identifier: Apache-2.0
"""Owned, loopback-only PostgreSQL/Server rig for P13 qualification."""

import hashlib
import json
import os
import socket
import subprocess
import threading
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path


class CallError(RuntimeError):
    def __init__(self, status):
        super().__init__(f"http_status_{status}")
        self.status = status


class Client:
    def __init__(self, base, token, uid="p13-reader", limit=10000):
        self.base, self.token, self.uid = base, token, uid
        self.calls, self.limit = 0, limit
        self.lock = threading.Lock()
        self.deadline = float("inf")

    def call(self, method, path, body=None, *, token=None, headers=None, timeout=30):
        with self.lock:
            if self.calls >= self.limit:
                raise RuntimeError("request_limit_reached")
            if time.monotonic() >= self.deadline:
                raise RuntimeError("wall_time_limit_reached")
            self.calls += 1
        request_headers = {
            "Authorization": "Bearer " + (token or self.token),
            "X-Munarium-Uid": self.uid,
        }
        if method in ("POST", "PUT"):
            request_headers["Idempotency-Key"] = str(uuid.uuid4())
        if isinstance(body, str):
            data = body.encode()
            request_headers["Content-Type"] = "text/yaml"
        else:
            data = None if body is None else json.dumps(body).encode()
            request_headers["Content-Type"] = "application/json"
        request_headers.update(headers or {})
        request = urllib.request.Request(
            self.base + path, data=data, method=method, headers=request_headers
        )
        try:
            with urllib.request.urlopen(request, timeout=timeout) as response:
                raw = response.read()
                return json.loads(raw) if raw else {}
        except urllib.error.HTTPError as exc:
            raise CallError(exc.code) from None


def unused_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def binary_hash(binary):
    with Path(binary).open("rb") as stream:
        return "sha256:" + hashlib.file_digest(stream, "sha256").hexdigest()


class LocalRig:
    """Never adopts a preexisting process, database, port or Docker container."""

    def __init__(self, binary, directory):
        self.binary = Path(binary).resolve()
        self.directory = Path(directory).resolve()
        self.name = "munarium-p13-" + uuid.uuid4().hex[:12]
        self.pg_port, self.http_port, self.ops_port = [unused_port() for _ in range(3)]
        self.rw, self.mgmt, self.secret = [uuid.uuid4().hex for _ in range(3)]
        self.process = None
        self.container_created = False
        self.log = None
        self.starts = 0
        self.environment = {
            k: v for k, v in os.environ.items() if not k.startswith("MUNARIUM_")
        }
        self.environment.update(
            {
                "MUNARIUM_STORE": "postgres",
                "MUNARIUM_SOURCE_STORE": "pg",
                "MUNARIUM_DATABASE_URL": f"postgres://p13:p13-local-only@127.0.0.1:{self.pg_port}/p13",
                "MUNARIUM_HTTP_ADDR": f"127.0.0.1:{self.http_port}",
                "MUNARIUM_OPS_ADDR": f"127.0.0.1:{self.ops_port}",
                "MUNARIUM_GRPC_ADDR": "disabled",
                "MUNARIUM_AUTH_MODE": "static",
                "MUNARIUM_TOKEN_SECRET": self.secret,
                "MUNARIUM_STATIC_TOKENS": f"{self.rw}:p13:rw,{self.mgmt}:p13:mgmt",
                "MUNARIUM_REQUIRE_UID": "true",
                "MUNARIUM_LOG": "error",
                "MUNARIUM_DB_MAX_CONNS": "8",
                "MUNARIUM_RETRIEVAL_MODE": "postgres",
            }
        )
        self.client = Client(f"http://127.0.0.1:{self.http_port}", self.rw)

    def docker(self, *args):
        result = subprocess.run(
            ["docker", *args], capture_output=True, text=True, timeout=120, check=False
        )
        if result.returncode:
            # CLI output can contain environment values. Keep it out of public records.
            raise RuntimeError("docker_command_failed:" + args[0])
        return result.stdout.strip()

    def __enter__(self):
        self.directory.mkdir(parents=True, exist_ok=False)
        try:
            image = "pgvector/pgvector:pg16"
            self.image_id = self.docker(
                "image", "inspect", image, "--format", "{{.Id}}"
            )
            self.docker(
                "run",
                "--detach",
                "--name",
                self.name,
                "--label",
                "munarium.p13=owned",
                "--publish",
                f"127.0.0.1:{self.pg_port}:5432",
                "--env",
                "POSTGRES_USER=p13",
                "--env",
                "POSTGRES_PASSWORD=p13-local-only",
                "--env",
                "POSTGRES_DB=p13",
                self.image_id,
            )
            self.container_created = True
            deadline = time.monotonic() + 60
            while True:
                result = subprocess.run(
                    [
                        "docker",
                        "exec",
                        self.name,
                        "pg_isready",
                        "-h",
                        "127.0.0.1",
                        "-U",
                        "p13",
                    ],
                    capture_output=True,
                    timeout=10,
                    check=False,
                )
                if result.returncode == 0:
                    break
                if time.monotonic() > deadline:
                    raise RuntimeError("postgres_readiness_timeout")
                time.sleep(0.2)
            self.start()
            return self
        except BaseException:
            self.close()
            raise

    def start(self):
        if time.monotonic() >= self.client.deadline:
            raise RuntimeError("wall_time_limit_reached")
        if self.process is not None:
            raise RuntimeError("server_already_owned")
        self.starts += 1
        self.log = (self.directory / f"server-{self.starts}.log").open("xb")
        before = time.perf_counter()
        self.process = subprocess.Popen(
            [str(self.binary)],
            cwd=self.directory,
            env=self.environment,
            stdout=self.log,
            stderr=self.log,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        )
        deadline = time.monotonic() + 90
        polls = 0
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise RuntimeError("server_exited_before_readiness")
            try:
                polls += 1
                with urllib.request.urlopen(
                    f"http://127.0.0.1:{self.ops_port}/readyz", timeout=1
                ) as response:
                    if response.status == 200:
                        return {
                            "duration_ms": (time.perf_counter() - before) * 1000,
                            "polls": polls,
                            "poll_interval_ms": 20,
                        }
            except (OSError, urllib.error.URLError):
                time.sleep(0.02)
        raise RuntimeError("server_readiness_timeout")

    def stop(self):
        if self.process is not None:
            self.process.terminate()
            self.process.wait(timeout=20)
            self.process = None
        if self.log is not None:
            self.log.close()
            self.log = None

    def database_bytes(self):
        return int(
            self.docker(
                "exec",
                self.name,
                "psql",
                "-U",
                "p13",
                "-d",
                "p13",
                "-Atc",
                "SELECT pg_database_size(current_database())",
            )
        )

    def memory_bytes(self):
        if not self.process:
            return None
        if os.name == "nt":
            import ctypes
            from ctypes import wintypes

            class Counters(ctypes.Structure):
                _fields_ = [
                    ("cb", wintypes.DWORD),
                    ("PageFaultCount", wintypes.DWORD),
                ] + [
                    (name, ctypes.c_size_t)
                    for name in (
                        "PeakWorkingSetSize",
                        "WorkingSetSize",
                        "QuotaPeakPagedPoolUsage",
                        "QuotaPagedPoolUsage",
                        "QuotaPeakNonPagedPoolUsage",
                        "QuotaNonPagedPoolUsage",
                        "PagefileUsage",
                        "PeakPagefileUsage",
                    )
                ]

            counters = Counters()
            counters.cb = ctypes.sizeof(counters)
            function = ctypes.windll.psapi.GetProcessMemoryInfo
            function.argtypes = [
                wintypes.HANDLE,
                ctypes.POINTER(Counters),
                wintypes.DWORD,
            ]
            if function(
                wintypes.HANDLE(int(self.process._handle)),
                ctypes.byref(counters),
                counters.cb,
            ):
                return counters.WorkingSetSize
        return None

    def close(self):
        try:
            self.stop()
        finally:
            if self.container_created:
                self.docker("rm", "--force", "--volumes", self.name)
                self.container_created = False

    def __exit__(self, *unused):
        self.close()
