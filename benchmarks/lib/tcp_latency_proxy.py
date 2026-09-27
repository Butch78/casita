"""Rootless TCP proxy with a fixed propagation delay in each direction."""

from __future__ import annotations

import queue
import socket
import socketserver
import threading
import time
from dataclasses import dataclass


_EOF = object()


@dataclass(frozen=True)
class _DelayedChunk:
    ready_at: float
    payload: bytes


def _read_chunks(source: socket.socket, output: queue.Queue[object], delay_seconds: float, stopped: threading.Event | None = None) -> None:
    stopped = stopped or threading.Event()
    source.settimeout(0.5)
    try:
        while not stopped.is_set():
            try:
                payload = source.recv(64 * 1024)
            except socket.timeout:
                continue
            if not payload:
                break
            item = _DelayedChunk(time.monotonic() + delay_seconds, payload)
            while not stopped.is_set():
                try:
                    output.put(item, timeout=0.5)
                    break
                except queue.Full:
                    continue
    except OSError:
        pass
    finally:
        while not stopped.is_set():
            try:
                output.put(_EOF, timeout=0.5)
                break
            except queue.Full:
                continue


def _write_chunks(destination: socket.socket, incoming: queue.Queue[object], bytes_per_second: int = 0, stopped: threading.Event | None = None) -> None:
    stopped = stopped or threading.Event()
    next_send = time.monotonic()
    try:
        while True:
            item = incoming.get()
            if item is _EOF:
                break
            assert isinstance(item, _DelayedChunk)
            ready_at = item.ready_at
            if bytes_per_second:
                next_send = max(next_send, ready_at) + len(item.payload) / bytes_per_second
                ready_at = next_send
            remaining = ready_at - time.monotonic()
            if remaining > 0:
                time.sleep(remaining)
            destination.sendall(item.payload)
    except OSError:
        pass
    finally:
        stopped.set()
        try:
            destination.shutdown(socket.SHUT_WR)
        except OSError:
            pass


class _ProxyServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

    def __init__(
        self,
        server_address: tuple[str, int],
        target: tuple[str, int],
        one_way_delay_seconds: float,
        buffer_chunks: int,
    ):
        self.target = target
        self.one_way_delay_seconds = one_way_delay_seconds
        self.bytes_per_second = 0
        self.buffer_chunks = buffer_chunks
        super().__init__(server_address, _ProxyHandler)


class _ProxyHandler(socketserver.BaseRequestHandler):
    def handle(self) -> None:
        server = self.server
        assert isinstance(server, _ProxyServer)
        try:
            upstream = socket.create_connection(server.target, timeout=10)
        except OSError:
            return
        with upstream:
            self.request.settimeout(None)
            upstream.settimeout(None)
            self.request.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            upstream.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            client_to_upstream: queue.Queue[object] = queue.Queue(maxsize=server.buffer_chunks)
            upstream_to_client: queue.Queue[object] = queue.Queue(maxsize=server.buffer_chunks)
            outbound_stopped = threading.Event()
            inbound_stopped = threading.Event()
            threads = [
                threading.Thread(
                    target=_read_chunks,
                    args=(self.request, client_to_upstream, server.one_way_delay_seconds, outbound_stopped),
                    daemon=True,
                ),
                threading.Thread(
                    target=_write_chunks,
                    args=(upstream, client_to_upstream, server.bytes_per_second, outbound_stopped),
                    daemon=True,
                ),
                threading.Thread(
                    target=_read_chunks,
                    args=(upstream, upstream_to_client, server.one_way_delay_seconds, inbound_stopped),
                    daemon=True,
                ),
                threading.Thread(
                    target=_write_chunks,
                    args=(self.request, upstream_to_client, server.bytes_per_second, inbound_stopped),
                    daemon=True,
                ),
            ]
            for thread in threads:
                thread.start()
            for thread in threads:
                thread.join()


class TcpLatencyProxy:
    """Forward TCP connections with a fixed configured round-trip delay."""

    def __init__(
        self,
        target_host: str,
        target_port: int,
        rtt_ms: int,
        listen_host: str = "127.0.0.1",
        listen_port: int = 0,
        buffer_bytes: int = 512 * 1024,
    ):
        if rtt_ms < 0:
            raise ValueError("RTT must be non-negative")
        if buffer_bytes < 65536 or buffer_bytes % 65536:
            raise ValueError("buffer must be a positive multiple of 64 KiB")
        self.rtt_ms = rtt_ms
        self._server = _ProxyServer(
            (listen_host, listen_port),
            (target_host, target_port),
            rtt_ms / 2 / 1000,
            buffer_bytes // 65536,
        )
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._thread.start()

    def set_rtt_ms(self, rtt_ms: int) -> None:
        if rtt_ms < 0:
            raise ValueError("RTT must be non-negative")
        self.rtt_ms = rtt_ms
        self._server.one_way_delay_seconds = rtt_ms / 2 / 1000

    def set_bandwidth(self, bytes_per_second: int) -> None:
        """Set each connection's full-duplex rate; zero leaves it unlimited."""
        if bytes_per_second < 0:
            raise ValueError("bandwidth must be non-negative")
        self._server.bytes_per_second = bytes_per_second

    @property
    def host(self) -> str:
        return str(self._server.server_address[0])

    @property
    def port(self) -> int:
        return int(self._server.server_address[1])

    @property
    def endpoint(self) -> str:
        return f"http://{self.host}:{self.port}"

    def close(self) -> None:
        self._server.shutdown()
        self._server.server_close()
        self._thread.join(timeout=5)

    def __enter__(self) -> "TcpLatencyProxy":
        return self

    def __exit__(self, *_: object) -> None:
        self.close()
