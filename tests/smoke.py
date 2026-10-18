"""Exercise ngxd against a tiny API stub; no Paperless installation or real account."""
import email.parser
import email.policy
import http.server
import json
import pathlib
import signal
import subprocess
import tempfile
import threading
import time
import urllib.parse
import uuid

BINARY = str(pathlib.Path('target/debug/ngxd').resolve())
lock = threading.Lock()
tasks = {}
uploads = []
errors = []


def wait_for(condition, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if condition():
            return
        time.sleep(0.1)
    raise AssertionError('Timed out waiting for expected state')


class Handler(http.server.BaseHTTPRequestHandler):
    def reply(self, value, code=200):
        body = json.dumps(value).encode()
        self.send_response(code)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        try:
            assert self.path == '/paperless/api/documents/post_document/'
            assert self.headers['Authorization'] == 'Token test-token'
            assert self.headers['Accept'] == 'application/json; version=9'
            raw = self.rfile.read(int(self.headers['Content-Length']))
            message = email.parser.BytesParser(policy=email.policy.default).parsebytes(
                b'Content-Type: ' + self.headers['Content-Type'].encode() + b'\r\n\r\n' + raw)
            fields = list(message.iter_parts())
            tags = [p.get_content() for p in fields
                    if p.get_param('name', header='content-disposition') == 'tags']
            assert tags == ['1', '7']
            part = next(p for p in fields if p.get_filename())
            task_id = str(uuid.uuid4())
            with lock:
                uploads.append((part.get_filename(), part.get_payload(decode=True), task_id))
                tasks[task_id] = 'STARTED'
            self.reply(task_id)
        except Exception as error:
            errors.append(repr(error))
            self.reply('bad upload', 400)

    def do_GET(self):
        task_id = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)['task_id'][0]
        assert self.headers['Authorization'] == 'Token test-token'
        with lock:
            status = tasks[task_id]
        self.reply([{'task_id': task_id, 'status': status}])

    def log_message(self, *args):
        pass


with tempfile.TemporaryDirectory(prefix='ngxd-test-') as tmp:
    root = pathlib.Path(tmp)
    folder = root / 'inbox'
    nested = folder / 'nested'
    nested.mkdir(parents=True)
    existing = nested / 'existing.pdf'
    existing.write_bytes(b'%PDF test document')
    config = root / 'config.json'
    args = [BINARY, '--config', str(config)]
    subprocess.run(args + ['init'], check=True, capture_output=True)
    original = config.read_bytes()
    subprocess.run(args + ['init'], check=True, capture_output=True)
    assert original == config.read_bytes()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    config.write_text(json.dumps({'folder': str(folder), 'api_key': 'test-token',
                                 'endpoint_url': f'http://127.0.0.1:{server.server_port}/paperless',
                                 'tag_ids': [1, 7], 'recursive': True}))
    log = (root / 'agent.log').open('w+')
    proc = None
    journal_path = root / 'config.json.queue' / 'jobs.json'

    def journal():
        return json.loads(journal_path.read_text())['jobs']

    def start():
        return subprocess.Popen(args + ['run'], stdout=log, stderr=log)

    def stop():
        proc.send_signal(signal.SIGTERM)
        assert proc.wait(timeout=5) == 0

    try:
        proc = start()
        wait_for(lambda: len(uploads) == 1 and any(j['phase']['state'] == 'polling' for j in journal()))
        assert existing.exists(), 'upload acceptance must not delete the file'
        second = subprocess.run(args + ['run'], capture_output=True, timeout=5)
        assert second.returncode != 0 and b'Another ngxd process' in second.stderr
        task_id = uploads[0][2]
        # Restart must resume polling the saved UUID rather than uploading again.
        stop()
        proc = start()
        time.sleep(0.5)
        assert len(uploads) == 1
        with lock:
            tasks[task_id] = 'SUCCESS'
        wait_for(lambda: not existing.exists())
        assert uploads[0][:2] == ('existing.pdf', b'%PDF test document')
        # A changed source must survive success for the old snapshot.
        changed = nested / 'changed.pdf'
        changed.write_bytes(b'old content')
        wait_for(lambda: len(uploads) == 2)
        changed.write_bytes(b'new content')
        with lock:
            tasks[uploads[1][2]] = 'SUCCESS'
        wait_for(lambda: len(uploads) == 3)
        assert uploads[2][1] == b'new content'
        assert changed.read_bytes() == b'new content'
        with lock:
            tasks[uploads[2][2]] = 'FAILURE'
        wait_for(lambda: any(j['phase']['state'] == 'failed' for j in journal()))
        assert changed.exists()
        stop()
        assert not errors, errors
        print('PASS: JSON config, multipart/tags/auth, startup discovery, queue lock, UUID restart recovery, success cleanup, changed-file preservation, failure retention')
    finally:
        if proc is not None and proc.poll() is None:
            proc.kill()
            proc.wait()
        server.shutdown()
        server.server_close()
        log.seek(0)
        print(log.read())
        log.close()
