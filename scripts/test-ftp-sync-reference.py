# Local disposable FTP synchronization reference; no Arco GUI is opened.
from contextlib import ExitStack
import hashlib
import json
import logging
import os
import shlex
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
from pyftpdlib.ioloop import IOLoop
from pyftpdlib.log import config_logging
from pyftpdlib.servers import FTPServer

config_logging(level=logging.ERROR)

def run_fixture(hash_mode, tests):
    with tempfile.TemporaryDirectory(prefix='arco-area3-ftp-') as share:
        authorizer = DummyAuthorizer()
        authorizer.add_user('arco', 'test-only', share, perm='elradfmwMT')
        if not hash_mode and os.environ.get('ARCO_TEST_NET_EXE'):
            authorizer.add_anonymous(share, perm='elr')
        transfers = []
        link_folder = Path(share).resolve() / 'arco-link-fixture'
        link_reference = any(name == 'reference_ftp_site_symlink' for _, name in tests)
        if link_reference:
            link_folder.mkdir()
            (link_folder / 'Quell Datei.txt').write_bytes(b'preserve original')
        class Fixture(FTPHandler):
            proto_cmds = dict(FTPHandler.proto_cmds)
            proto_cmds['SITE SYMLINK'] = dict(perm=None, auth=True, arg=True, help='SITE SYMLINK source destination')
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
                    transfers.append(('get', self._current_type))
                    return super().ftp_RETR(file)
            def ftp_STOR(self, file, mode='w'):
                transfers.append(('put', self._current_type))
                return super().ftp_STOR(file, mode)
            def ftp_SITE_SYMLINK(self, line):
                try:
                    source, destination = shlex.split(line)
                    source_path = Path(self.fs.ftp2fs(source)).resolve()
                    link = Path(self.fs.ftp2fs(destination)).resolve()
                    if source_path.parent != link_folder or link.parent != link_folder or not source_path.is_file():
                        raise ValueError('invalid fixture paths')
                    with link.open('x', encoding='utf-8') as stream:
                        stream.write(source)
                except (OSError, ValueError):
                    self.respond('550 Invalid link or destination already exists.')
                else:
                    self.respond('200 SITE SYMLINK command successful.')
        Fixture.authorizer = authorizer
        server = FTPServer(('127.0.0.1', 0), Fixture)
        thread = threading.Thread(target=server.serve_forever, kwargs={'timeout': .1, 'handle_exit': False}, daemon=True)
        thread.start()
        environment = os.environ.copy()
        variable = 'ARCO_TEST_HASH_FTP' if hash_mode else 'ARCO_TEST_FTP'
        environment[variable] = f'ftp://127.0.0.1:{server.socket.getsockname()[1]}'
        environment['ARCO_TEST_FTP_ROOT'] = share
        try:
            for package, name in tests:
                print(f'Reference: {name}', flush=True)
                runner = environment.get('ARCO_TEST_RUNNER_WINDOWS' if package == 'arco-windows' else 'ARCO_TEST_RUNNER_APP')
                command = [runner,name,'--ignored','--test-threads=1'] if runner else CARGO + ['test','--workspace','--all-targets','--locked','--offline',name,'--','--ignored','--test-threads=1']
                result = subprocess.run(command,env=environment,cwd=REPOSITORY,stdin=subprocess.DEVNULL,creationflags=subprocess.CREATE_NO_WINDOW,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,encoding='utf-8',errors='replace')
                print(result.stdout,flush=True)
                result.check_returncode()
            if not hash_mode and not link_reference and (executable := environment.get('ARCO_TEST_NET_EXE')):
                reference_transfers(executable, Path(share), environment[variable], transfers)
        finally:
            server.close_all()
            thread.join(2)

def reference_transfers(executable, root, base, transfers):
    executable = str(Path(executable).resolve())
    with tempfile.TemporaryDirectory(prefix='arco-ftp-presets-') as work:
        work = Path(work)
        profile = work / 'profile.json'
        folder = root / 'Nested Folder'
        folder.mkdir()
        data = b'one\r\ntwo\r\n'
        source = work / 'test.txt'
        source.write_bytes(data)
        stamp = 1_700_000_031
        os.utime(source, (stamp, stamp))
        address = base + '/Nested%20Folder/'
        env = dict(os.environ, ARCO_FTP_FIXTURE_PASSWORD='test-only',
                   ARCO_FTP_FIXTURE_PASSPHRASE='reference-only-passphrase')

        def run(profiles, script, *arguments, expected=0):
            profile.write_text(json.dumps({'ftp_profiles': profiles}), encoding='utf-8')
            result = subprocess.run(
                [executable, '--script', '-', '--profile', str(profile), *arguments],
                cwd=work, env=env, input=script, capture_output=True,
                encoding='utf-8', timeout=60, creationflags=subprocess.CREATE_NO_WINDOW,
            )
            assert result.returncode == expected, result.stderr
            if expected:
                return json.loads(result.stderr)['error']
            return [json.loads(line)['event'] for line in result.stdout.splitlines()]

        transfers.clear()
        run({'Preset': {
            'address': address, 'security': 'plain', 'user': 'arco',
            'transfer': {'mode': 'automatic', 'speed_kib': 32,
                         'preserve_time': True, 'masks': '*.txt'},
        }}, 'open Preset\nput test.txt\nget test.txt copy.txt\n',
            '--password-env', 'ARCO_FTP_FIXTURE_PASSWORD')
        assert transfers == [('put', 'a'), ('get', 'a')], transfers
        assert (work / 'copy.txt').read_bytes().splitlines() == data.splitlines()
        assert int((folder / 'test.txt').stat().st_mtime) == stamp
        assert int((work / 'copy.txt').stat().st_mtime) == stamp
        endpoint = {'Preset': {'address': address, 'security': 'plain', 'user': 'arco',
                              'transfer': {'mode': 'automatic'}}}
        password_option = ('--password-env', 'ARCO_FTP_FIXTURE_PASSWORD')
        transfers.clear()
        run(endpoint, 'open Preset\nget test.txt once.txt -transfer=binary -speed=0 -preservetime\n'
                      'get test.txt default.txt -filemask=*.txt\n', *password_option)
        assert transfers == [('get', 'i'), ('get', 'a')], transfers
        assert int((work / 'once.txt').stat().st_mtime) == stamp
        assert '-filemask' in run(endpoint, 'open Preset\nget test.txt denied.txt -filemask=*.csv\n',
                                  *password_option, expected=1)
        assert not (work / 'denied.txt').exists()

        # Keep the size equal and the content distinct. The command must use MFMT alone.
        pair_left, pair_right = work / 'stamp.txt', folder / 'stamp.txt'
        pair_left.write_bytes(b'left')
        pair_right.write_bytes(b'rght')
        os.utime(pair_left, (stamp + 60, stamp + 60))
        os.utime(pair_right, (stamp, stamp))
        transfers.clear()
        preview = run(endpoint, 'open Preset\nsynchronize both -timestamps -filemask=stamp.txt -preview\n',
                      *password_option)[-1]
        assert preview['changes'] == [{'path': 'stamp.txt', 'action': 'timestamp-remote'}], preview
        run(endpoint, 'open Preset\nsynchronize both -timestamps -filemask=stamp.txt\n', *password_option)
        assert transfers == [], transfers
        assert int(pair_right.stat().st_mtime) == stamp + 60 and pair_right.read_bytes() == b'rght'
        print('FTP timestamp-only synchronization and per-command transfer options passed.', flush=True)

        source_batch, target_batch = work / 'batch input', work / 'batch output'
        source_batch.mkdir()
        target_batch.mkdir()
        (source_batch / 'report.txt').write_bytes(b'first line\r\n')
        (source_batch / 'summary.txt').write_bytes(b'second line\r\n')
        (source_batch / 'ignored.csv').write_bytes(b'exclude')
        (folder / 'empty inbox').mkdir()
        transfers.clear()
        batch = run(endpoint, 'open Preset\nput "batch input/*" "empty inbox/" -filemask=*.txt\n'
                              'get "empty inbox/*.txt" "batch output/"\n', *password_option)
        assert batch[-2] == {'uploaded': ['report.txt', 'summary.txt'], 'completed': 2}
        assert batch[-1]['completed'] == 2
        assert sorted(path.name for path in target_batch.iterdir()) == ['report.txt', 'summary.txt']
        assert (target_batch / 'report.txt').read_bytes().splitlines() == [b'first line']
        assert transfers == [('put', 'a'), ('put', 'a'), ('get', 'a'), ('get', 'a')], transfers
        print('FTP batch transfer observed masks and FTP text mode.', flush=True)

        resume_data = bytes(range(255)) * 60
        (source_batch / 'partial.txt').write_bytes(resume_data)
        (folder / 'partial.txt').write_bytes(resume_data[:512])
        run(endpoint, 'open Preset\nput -resume "batch input/partial.txt"\nget -resume partial.txt resumed.txt\n',
            *password_option)
        assert (folder / 'partial.txt').read_bytes() == resume_data
        assert (work / 'resumed.txt').read_bytes() == resume_data
        print('FTP script resumed a validated partial upload and completed a binary download.', flush=True)

        binary_tree = work / 'binary tree' / 'deep'
        (binary_tree / 'empty').mkdir(parents=True)
        binary_data = bytes(range(256)) * 20
        (binary_tree / 'payload.txt').write_bytes(binary_data)
        (work / 'tree result').mkdir()
        transfers.clear()
        result = run(endpoint, 'open Preset\nput "binary tree" ./\nget "binary tree" "tree result/"\n',
                     *password_option)
        assert [event['completed'] for event in result[-2:]] == [4, 4]
        assert (work / 'tree result/binary tree/deep/empty').is_dir()
        assert (work / 'tree result/binary tree/deep/payload.txt').read_bytes() == binary_data
        assert transfers == [('put', 'i'), ('get', 'i')], transfers
        print('Recursive FTP transfer preserved binary contents and empty directories.', flush=True)

        (folder / 'nested bin test').mkdir()
        recycled_payload = folder / 'nested bin test' / 'restore.txt'
        recycled_payload.write_bytes(b'FTP recycle reference')
        endpoint['Preset']['recycle_remote'] = True
        removed = run(endpoint, 'open Preset\ncd "nested bin test"\nrm restore.txt\n', *password_option)[-1]
        slot = removed['slot']
        saved = folder / '.arco-trash' / slot / 'restore.txt'
        assert not recycled_payload.exists() and saved.read_bytes() == b'FTP recycle reference'
        run(endpoint, f'open Preset\ntrash restore {slot} "nested bin test"\n', *password_option)
        assert recycled_payload.read_bytes() == b'FTP recycle reference' and not saved.exists()
        print('FTP remote recycle restored a file across directories.', flush=True)

        encrypted_source = work / 'Mehrzeiliger Bericht.txt'
        encrypted_source.write_bytes(b'embedded NUL\x00 and different separators\r\nline two\n' * 3000)
        decoded = work / 'decoded'
        decoded.mkdir()
        endpoint['Preset']['transfer'] = {'mode': 'text', 'masks': '*.txt'}
        transfers.clear()
        package = run(
            endpoint, 'open Preset\nput-encrypted "Mehrzeiliger Bericht.txt" ARCO_FTP_FIXTURE_PASSPHRASE\n',
            *password_option,
        )[-1]['uploaded']
        assert (folder / package).read_bytes().startswith(b'age-encryption.org/v1\n')
        run(endpoint, f'open Preset\nget-encrypted {package} ARCO_FTP_FIXTURE_PASSPHRASE decoded\n',
            *password_option)
        assert (decoded / encrypted_source.name).read_bytes() == encrypted_source.read_bytes()
        assert transfers == [('put', 'i'), ('get', 'i')], transfers
        print('Encrypted FTP transfers stayed binary with ASCII selected in the profile.', flush=True)

        rows = run({
            'First': {'address': base + '/', 'security': 'plain'},
            'Second': {'address': address, 'security': 'plain'},
        }, 'open First\nopen Second\nsession\nsession 1\npwd\nclose 1\npwd\nclose 2\nsession\n')
        assert [entry['id'] for entry in rows[2]['sessions']] == [1, 2]
        assert rows[2]['sessions'][1]['active']
        assert rows[4]['remote'] == '/'
        assert rows[6]['remote'] == '/Nested Folder/'
        assert rows[8]['sessions'] == []
        print('FTP transfer presets and multiple script sessions passed.', flush=True)

def run_fxp_copy():
    stamp = 1_700_000_031
    for host in ('127.0.0.1', '::1'):
        with ExitStack() as resources:
            source, target = [Path(resources.enter_context(tempfile.TemporaryDirectory(prefix='arco-f5-fxp-')))
                              for _ in range(2)]
            (source / 'batch/nested').mkdir(parents=True)
            (source / 'batch/empty').mkdir()
            (target / 'batch').mkdir()
            (target / 'batch/rename.txt').write_bytes(b'keep this original')
            (target / 'batch/replace.txt').write_bytes(b'old target data')
            payloads = {name: f'payload for {name}'.encode() for name in
                        ('nested/fresh.txt', 'rename.txt', 'replace.txt', 'race.txt', 'ignored.txt', 'omit.bin')}
            for name, payload in payloads.items():
                path = source / 'batch' / name
                path.write_bytes(payload)
                os.utime(path, (stamp, stamp))
            channels, prevented = [], []

            class DirectSource(FTPHandler):
                direct = False

                def ftp_PORT(self, line):
                    self.direct = True
                    return super().ftp_PORT(line)

                def ftp_EPRT(self, line):
                    self.direct = True
                    return super().ftp_EPRT(line)

                def ftp_RETR(self, path):
                    name = Path(path).name
                    if not self.direct:
                        prevented.append(name)
                        self.respond('550 Data must go directly to the target FTP server.')
                        return
                    channels.append(name)
                    if name == 'race.txt':
                        (target / 'batch/race.txt').write_bytes(b'created during direct transfer')
                    return super().ftp_RETR(path)

            def start(folder, base):
                authorizer = DummyAuthorizer()
                authorizer.add_user('arco', 'test-only', str(folder), perm='elradfmwMT')
                handler = type('FxpFixture', (base,), {'authorizer': authorizer})
                server = FTPServer((host, 0), handler, ioloop=IOLoop())
                thread = threading.Thread(target=server.serve_forever, kwargs={'timeout': .05, 'handle_exit': False}, daemon=True)
                thread.start()

                def stop():
                    server.close_all()
                    thread.join(2)

                resources.callback(stop)
                address = f'[{host}]' if ':' in host else host
                return f'ftp://{address}:{server.address[1]}/'

            environment = dict(os.environ, ARCO_TEST_FXP_COPY_SOURCE=start(source, DirectSource),
                               ARCO_TEST_FXP_COPY_TARGET=start(target, FTPHandler))
            name = 'app::ftp_sync::tests::direct_fxp_copy_reviews_and_applies_a_directory'
            runner = environment.get('ARCO_TEST_RUNNER_APP')
            command = [runner, name] if runner else CARGO + ['test', '-p', 'arco-app', '--bin', 'arco-commander', '--locked', name, '--']
            result = subprocess.run([*command, '--exact', '--ignored', '--nocapture'], env=environment,
                                    cwd=REPOSITORY, timeout=120 if runner else None, creationflags=subprocess.CREATE_NO_WINDOW,
                                    capture_output=True, encoding='utf-8', errors='replace')
            print(result.stdout, end='', flush=True)
            if result.returncode:
                print(result.stderr, file=sys.stderr, flush=True)
            result.check_returncode()
            assert '1 passed' in result.stdout
            assert sorted(channels) == ['fresh.txt', 'race.txt', 'rename.txt', 'replace.txt'] and not prevented
            assert (target / 'batch/empty').is_dir()
            assert (target / 'batch/rename.txt').read_bytes() == b'keep this original'
            assert (target / 'batch/race.txt').read_bytes() == b'created during direct transfer'
            assert not (target / 'batch/ignored.txt').exists() and not (target / 'batch/omit.bin').exists()
            for destination, original in [('nested/fresh.txt', 'nested/fresh.txt'), ('gewählt.txt', 'rename.txt'), ('replace.txt', 'replace.txt')]:
                file = target / 'batch' / destination
                assert file.read_bytes() == payloads[original]
                assert int(file.stat().st_mtime) == stamp
            staged, = (target / 'batch').glob('.arco-fxp-*')
            assert staged.read_bytes() == payloads['race.txt']
            print(f'F5 FXP applied the reviewed directory on {host} with direct data, conflicts, masks, and timestamps.', flush=True)


if os.environ.get('ARCO_TEST_FXP_COPY') == '1':
    run_fxp_copy()
    raise SystemExit(0)

run_fixture(False, [
    ('arco-app','ftp_edits_can_be_published_twice_and_detect_content_conflicts'),
    ('arco-windows','synchronized_upload_is_published_in_the_selected_directory_and_checks_snapshots'),
    ('arco-app','ftp_plan_compares_content_and_synchronizes_both_directions'),
    ('arco-app','ftp_mirror_checks_absence_and_keeps_deletions_out_of_failed_copy_runs'),
    ('arco-app','ftp_mirror_protects_directories_containing_ignored_remote_files'),
])
run_fixture(False, [('arco-windows','reference_ftp_site_symlink')])
run_fixture(True,[('arco-app','ftp_content_comparison_uses_advertised_hash_without_retr')])
