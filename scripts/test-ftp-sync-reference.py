# Local disposable FTP synchronization reference; no Arco GUI is opened.
import hashlib
import logging
import os
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile
import threading

if os.name != 'nt':
    raise SystemExit('This reference runner requires Windows.')
if os.environ.get('ARCO_LOCAL_FTP_PACKAGES'):
    sys.path.insert(0, os.environ['ARCO_LOCAL_FTP_PACKAGES'])
REPOSITORY = next(folder for folder in Path(__file__).resolve().parents if (folder / 'Cargo.toml').is_file())
CARGO = ['rtk', 'cargo'] if shutil.which('rtk') else ['cargo']
from pyftpdlib.authorizers import DummyAuthorizer
from pyftpdlib.handlers import FTPHandler
from pyftpdlib.log import config_logging
from pyftpdlib.servers import FTPServer

config_logging(level=logging.ERROR)

def run_fixture(hash_mode, tests):
    with tempfile.TemporaryDirectory(prefix='arco-area3-ftp-') as share:
        authorizer = DummyAuthorizer()
        authorizer.add_user('arco', 'test-only', share, perm='elradfmwMT')
        class Fixture(FTPHandler):
            proto_cmds = dict(FTPHandler.proto_cmds)
            if hash_mode:
                proto_cmds['HASH'] = dict(perm='r', auth=True, arg=True, help='HASH <path>')
            def on_connect(self):
                if hash_mode:
                    self._extra_feats.append('HASH SHA-256*;')
            def ftp_OPTS(self, line):
                if hash_mode and line.upper().startswith('HASH '):
                    self.respond('200 SHA-256 selected.')
                else:
                    super().ftp_OPTS(line)
            def ftp_HASH(self, path):
                if Path(path).name == 'fallback.txt':
                    self.respond('550 HASH disabled for fallback fixture.')
                else:
                    digest = hashlib.sha256(Path(path).read_bytes()).hexdigest()
                    size = Path(path).stat().st_size
                    self.respond(f'213 SHA-256 0-{max(0,size-1)} {digest} {Path(path).name}')
            def ftp_RETR(self, file):
                if hash_mode and Path(file).name != 'fallback.txt':
                    self.respond('550 RETR disabled for HASH fixture.')
                else:
                    return super().ftp_RETR(file)
        Fixture.authorizer = authorizer
        server = FTPServer(('127.0.0.1', 0), Fixture)
        thread = threading.Thread(target=server.serve_forever, kwargs={'timeout': .1, 'handle_exit': False}, daemon=True)
        thread.start()
        environment = os.environ.copy()
        variable = 'ARCO_TEST_HASH_FTP' if hash_mode else 'ARCO_TEST_FTP'
        environment[variable] = f'ftp://127.0.0.1:{server.socket.getsockname()[1]}'
        try:
            for package, name in tests:
                print(f'Reference: {name}', flush=True)
                runner = environment.get('ARCO_TEST_RUNNER_WINDOWS' if package == 'arco-windows' else 'ARCO_TEST_RUNNER_APP')
                command = [runner,name,'--ignored','--test-threads=1'] if runner else CARGO + ['test','--workspace','--all-targets','--locked','--offline',name,'--','--ignored','--test-threads=1']
                result = subprocess.run(command,env=environment,cwd=REPOSITORY,stdin=subprocess.DEVNULL,creationflags=subprocess.CREATE_NO_WINDOW,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,encoding='utf-8',errors='replace')
                print(result.stdout,flush=True)
                result.check_returncode()
        finally:
            server.close_all()
            thread.join(2)

run_fixture(False, [
    ('arco-windows','synchronized_upload_is_published_in_the_selected_directory_and_checks_snapshots'),
    ('arco-app','ftp_plan_compares_content_and_synchronizes_both_directions'),
    ('arco-app','ftp_mirror_checks_absence_and_keeps_deletions_out_of_failed_copy_runs'),
    ('arco-app','ftp_mirror_protects_directories_containing_ignored_remote_files'),
])
run_fixture(True,[('arco-app','ftp_content_comparison_uses_advertised_hash_without_retr')])
