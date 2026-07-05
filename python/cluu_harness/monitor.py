"""QEMU monitor (unix-socket) client.

The bash harness talks to QEMU's HMP monitor via ``nc -U`` one-shot
invocations. This module keeps a persistent connection so batches of
sendkey events are pipelined efficiently — ``FAST_KEYSTROKES=1`` in the
bash harness exists for the same reason.
"""

from __future__ import annotations

import contextlib
import logging
import socket
import time
from pathlib import Path

log = logging.getLogger(__name__)


class MonitorError(RuntimeError):
    """Raised on monitor socket I/O failures."""


class QemuMonitor:
    """Persistent HMP monitor client over a unix socket."""

    def __init__(self, sock_path: Path, connect_timeout_s: float = 5.0) -> None:
        self.sock_path = sock_path
        self._connect_timeout_s = connect_timeout_s
        self._sock: socket.socket | None = None

    def connect(self) -> None:
        """Open the monitor connection. Retries briefly until the socket appears."""
        deadline = time.monotonic() + self._connect_timeout_s
        last_err: Exception | None = None
        while time.monotonic() < deadline:
            try:
                sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                sock.settimeout(2.0)
                sock.connect(str(self.sock_path))
                # Drain the banner QEMU prints on connect.
                self._sock = sock
                self._drain_banner()
                return
            except (FileNotFoundError, ConnectionRefusedError, OSError) as exc:
                last_err = exc
                time.sleep(0.1)
        raise MonitorError(
            f"could not connect to monitor socket {self.sock_path} "
            f"within {self._connect_timeout_s}s: {last_err}"
        )

    def _drain_banner(self) -> None:
        """Read and discard the HMP welcome banner."""
        with contextlib.suppress(TimeoutError):
            self._raw_recv(timeout_s=0.2)

    def _raw_recv(self, timeout_s: float = 1.0) -> bytes:
        if self._sock is None:
            raise MonitorError("monitor not connected")
        self._sock.settimeout(timeout_s)
        chunks: list[bytes] = []
        try:
            while True:
                chunk = self._sock.recv(4096)
                if not chunk:
                    break
                chunks.append(chunk)
        except TimeoutError:
            pass
        return b"".join(chunks)

    def send(self, line: str) -> str:
        """Send one HMP command, return the textual response."""
        if self._sock is None:
            raise MonitorError("monitor not connected")
        if not line.endswith("\n"):
            line += "\n"
        self._sock.sendall(line.encode("ascii", errors="replace"))
        return self._raw_recv(timeout_s=2.0).decode("ascii", errors="replace")

    def send_keys_batch(self, key_names: list[str], gap_s: float = 0.0) -> None:
        """Pipeline many sendkey commands in one socket write.

        Mirrors ``FAST_KEYSTROKES=1`` in the bash harness: QEMU parses
        them as fast as it can read from the socket, hitting saturation
        rates the per-key open/close path can't reach.
        """
        if not key_names:
            return
        if self._sock is None:
            raise MonitorError("monitor not connected")
        payload = "".join(f"sendkey {k}\n" for k in key_names)
        self._sock.sendall(payload.encode("ascii", errors="replace"))
        # Drain responses so they don't back up in the kernel buffer.
        with contextlib.suppress(TimeoutError):
            self._raw_recv(timeout_s=0.5 if gap_s == 0 else gap_s)

    def send_key(self, key_name: str, delay_s: float = 0.0) -> None:
        """Send one key, optionally sleeping afterwards (per-key rate limit)."""
        self.send(f"sendkey {key_name}")
        if delay_s > 0:
            time.sleep(delay_s)

    def mouse_move(self, dx: int, dy: int) -> None:
        self.send(f"mouse_move {dx} {dy}")

    def mouse_button(self, button: int) -> None:
        self.send(f"mouse_button {button}")

    def pmemsave(self, phys_addr: int, size: int, out_path: Path) -> str:
        """Save guest physical memory range to a file (used by FB dump)."""
        return self.send(f"pmemsave 0x{phys_addr:x} {size} {out_path}")

    def quit(self) -> None:
        """Send the ``quit`` HMP command and close the socket."""
        if self._sock is None:
            return
        with contextlib.suppress(OSError):
            self._sock.sendall(b"quit\n")
        self.close()

    def close(self) -> None:
        if self._sock is not None:
            try:
                self._sock.close()
            finally:
                self._sock = None

    def __enter__(self) -> QemuMonitor:
        self.connect()
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()


__all__ = ["MonitorError", "QemuMonitor"]
