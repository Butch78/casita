import socket
import socketserver
import threading
import unittest
from unittest import mock

from benchmarks.lib import tcp_latency_proxy
from benchmarks.lib.tcp_latency_proxy import TcpLatencyProxy


class _EchoHandler(socketserver.BaseRequestHandler):
    def handle(self) -> None:
        while payload := self.request.recv(1024 * 1024):
            self.request.sendall(payload)


class TcpLatencyProxyTests(unittest.TestCase):
    def test_large_buffer_preserves_large_payload_and_half_close(self) -> None:
        server = socketserver.ThreadingTCPServer(("127.0.0.1", 0), _EchoHandler)
        server.daemon_threads = True
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        payload = bytes(range(256)) * 8192
        try:
            with TcpLatencyProxy("127.0.0.1", server.server_address[1], 20, buffer_bytes=4 * 2**20) as proxy:
                with socket.create_connection((proxy.host, proxy.port), timeout=5) as client:
                    def send():
                        client.sendall(payload)
                        client.shutdown(socket.SHUT_WR)
                    writer = threading.Thread(target=send)
                    writer.start()
                    received = bytearray()
                    while chunk := client.recv(65536):
                        received.extend(chunk)
                    writer.join(timeout=5)
                    self.assertFalse(writer.is_alive())
                    self.assertEqual(received, payload)
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)

    def test_invalid_buffer_is_rejected(self) -> None:
        for size in (0, -1, 65535, 65537):
            with self.assertRaises(ValueError):
                TcpLatencyProxy("127.0.0.1", 9000, 0, buffer_bytes=size)

    def test_proxy_forwards_bytes(self) -> None:
        server = socketserver.ThreadingTCPServer(("127.0.0.1", 0), _EchoHandler)
        server.daemon_threads = True
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with TcpLatencyProxy("127.0.0.1", server.server_address[1], 0) as proxy:
                with socket.create_connection((proxy.host, proxy.port), timeout=2) as client:
                    client.sendall(b"path-proof")
                    self.assertEqual(client.recv(64), b"path-proof")
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)

    def test_negative_rtt_is_rejected(self) -> None:
        with self.assertRaises(ValueError):
            TcpLatencyProxy("127.0.0.1", 9000, -1)
        with TcpLatencyProxy("127.0.0.1", 9000, 0) as proxy:
            proxy.set_rtt_ms(30)
            self.assertEqual(proxy.rtt_ms, 30)
            with self.assertRaises(ValueError):
                proxy.set_rtt_ms(-1)

    def test_relay_schedules_each_chunk_with_one_way_delay(self) -> None:
        source, sender = socket.socketpair()
        incoming = tcp_latency_proxy.queue.Queue()
        try:
            sender.sendall(b"scheduled")
            sender.shutdown(socket.SHUT_WR)
            with mock.patch.object(tcp_latency_proxy.time, "monotonic", return_value=100.0):
                tcp_latency_proxy._read_chunks(source, incoming, 0.01)
            chunk = incoming.get_nowait()
            self.assertEqual(chunk.payload, b"scheduled")
            self.assertEqual(chunk.ready_at, 100.01)
        finally:
            source.close()
            sender.close()

        destination, receiver = socket.socketpair()
        outgoing = tcp_latency_proxy.queue.Queue()
        outgoing.put(chunk)
        outgoing.put(tcp_latency_proxy._EOF)
        try:
            with (
                mock.patch.object(tcp_latency_proxy.time, "monotonic", return_value=100.0),
                mock.patch.object(tcp_latency_proxy.time, "sleep") as sleep,
            ):
                tcp_latency_proxy._write_chunks(destination, outgoing)
            self.assertEqual(receiver.recv(64), b"scheduled")
            sleep.assert_called_once_with(mock.ANY)
            self.assertAlmostEqual(sleep.call_args.args[0], 0.01)
        finally:
            destination.close()
            receiver.close()
