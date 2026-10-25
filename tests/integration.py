#!/usr/bin/env python3
"""Local UDP Raw -> YAM runtime API test across ring sizes and optional logo."""
import http.server
import json
import os
import pathlib
import socket
import subprocess
import tempfile
import threading
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
API = "/api/runtime/ya-ambilight-universal"


def free_udp_port():
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def run_case(count, logo_feature, logo_endpoint):
    received = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def reply(self, status, body=b""):
            self.send_response(status)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            assert self.headers["X-YAM-Token"] == "test-token"
            assert self.path == API + "/device"
            features = ["led.ring.rgb", f"led.ring.{count}"]
            if logo_feature:
                features.append("led.logo.rgb")
            self.reply(200, json.dumps({"features": features}).encode())

        def do_POST(self):
            assert self.headers["X-YAM-Token"] == "test-token"
            body = self.rfile.read(int(self.headers["Content-Length"]))
            received.append((self.path.removeprefix(API), body))
            if self.path.startswith(API + "/logo") and not logo_endpoint:
                self.reply(404)
            else:
                self.reply(204)

    server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    port = free_udp_port()
    with tempfile.TemporaryDirectory() as directory:
        config = (ROOT / "package/config.toml").read_text()
        config = config.replace('bind = "0.0.0.0:5568"', f'bind = "127.0.0.1:{port}"')
        config = config.replace("stream_timeout_ms = 1500", "stream_timeout_ms = 300")
        pathlib.Path(directory, "config.toml").write_text(config)
        env = os.environ.copy()
        env["YAM_API_URL"] = f"http://127.0.0.1:{server.server_port}{API}"
        env["YAM_API_TOKEN"] = "test-token"
        process = subprocess.Popen([ROOT / "target/debug/ya-ambilight-universal"], cwd=directory, env=env)
        try:
            time.sleep(0.15)
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
                sock.sendto(bytes([255, 0, 0] * 24), ("127.0.0.1", port))
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline and not any(path == "/led" for path, _ in received):
                time.sleep(0.02)
            expected_logo = count == 24 and logo_feature
            if expected_logo:
                while time.monotonic() < deadline and not any(path == "/logo" for path, _ in received):
                    time.sleep(0.02)
            led = [json.loads(body)["colors"] for path, body in received if path == "/led"]
            assert led and len(led[0]) == count, received
            logo = [json.loads(body)["colors"] for path, body in received if path == "/logo"]
            assert bool(logo) == expected_logo, received
            if logo:
                assert len(logo[0]) == 6 and len(set(logo[0])) == 1, logo
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline and not any(path == "/led/off" for path, _ in received):
                time.sleep(0.02)
            assert any(path == "/led/off" for path, _ in received), received
            assert any(path == "/logo/off" for path, _ in received) == (expected_logo and logo_endpoint), received
            assert process.poll() is None, "ring service exited unexpectedly"
        finally:
            process.terminate()
            process.wait(timeout=2)
            server.shutdown()


def main():
    subprocess.run(["cargo", "build", "--locked"], cwd=ROOT, check=True)
    run_case(3, False, False)
    run_case(6, False, False)
    run_case(3, True, True)
    run_case(24, False, False)
    run_case(24, True, True)
    run_case(24, True, False)
    print("3/6/24-zone rings, optional Midi logo, and missing logo endpoint: OK")


if __name__ == "__main__":
    main()
