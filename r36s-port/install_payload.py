#!/usr/bin/env python3
"""Verify and atomically install a payload, offline or on the running console."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time
import re
import uuid
import zipfile
import fcntl


def digest(path):
    h = hashlib.sha256()
    with open(path, 'rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def atomic_copy(source, target, mode):
    target.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix='.' + target.name, dir=str(target.parent))
    try:
        with os.fdopen(fd, 'wb') as output, open(source, 'rb') as source_file:
            shutil.copyfileobj(source_file, output)
            os.fchmod(output.fileno(), mode)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, str(target))
        directory = os.open(str(target.parent), os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def verify(payload):
    manifest = json.loads((payload / 'manifest.json').read_text())
    if manifest['architecture'] != 'aarch64':
        raise ValueError('Arquitetura incompatível')
    for filename, key in [('r36s-agent', 'agent_sha256'), ('r36s-agent.service', 'service_sha256')]:
        if digest(payload / filename) != manifest[key]:
            raise ValueError('SHA256 inválido: ' + filename)
    if 'installer_sha256' in manifest:
        if digest(payload / 'install_payload.py') != manifest['installer_sha256']:
            raise ValueError('SHA256 inválido: instalador')
    return manifest


def healthy(expected_hash):
    try:
        pid = subprocess.check_output(['systemctl', 'show', 'r36s-agent', '-p', 'MainPID', '--value']).decode().strip()
        if not pid or pid == '0' or digest('/proc/' + pid + '/exe') != expected_hash:
            return False
        with socket.create_connection(('127.0.0.1', 7365), 0.5) as sock:
            sock.sendall(b'{"type":"Ping"}\n')
            return json.loads(sock.makefile('rb').readline())['type'] == 'Pong'
    except (OSError, ValueError, KeyError, subprocess.SubprocessError):
        return False


POWER_HELPER = r"""#!/bin/sh
set -eu
case "$1" in
    prepare)
        touch /run/devbridge-suspending
        systemctl stop r36s-agent.service
        # Legacy g_ether must unbind before ArkOS removes its UDC/controller.
        modprobe -r g_ether
        ;;
    resume)
        # ExecStopPost runs after systemd-sleep and all vendor hooks finish, even on failure.
        trap 'rm -f /run/devbridge-suspending; systemctl start r36s-agent.service' EXIT
        modprobe dwc2
        ready=0
        for attempt in 1 2 3 4 5 6 7 8 9 10; do
            modprobe g_ether || true
            if [ -d /sys/class/net/usb0 ]; then
                /sbin/ip link set dev usb0 up
                /sbin/ip -4 address replace 192.168.7.2/24 dev usb0
                if /sbin/ip -o -4 addr show dev usb0 | grep -q '192.168.7.2/24'; then
                    ready=1
                    break
                fi
            fi
            # A loaded legacy gadget may have survived removal of its UDC without rebinding.
            modprobe -r g_ether || true
            sleep 1
        done
        if [ "$ready" != 1 ]; then
            echo 'DevBridge: USB interface did not recover after resume' >&2
            exit 1
        fi
        ;;
    *) exit 2 ;;
esac
"""


def generated_files(root):
    # Preserve a persisted MAC pair. A new installation receives its own local identity.
    mac_file = root / 'etc/modprobe.d/g_ether.conf'
    existing = mac_file.read_text() if mac_file.exists() else ''
    if not (re.search(r'host_addr=[0-9a-fA-F:]{17}', existing)
            and re.search(r'dev_addr=[0-9a-fA-F:]{17}', existing)):
        identity = bytearray(uuid.uuid4().bytes[:6])
        identity[0] = 2
        host = ':'.join('{:02x}'.format(b) for b in identity)
        identity[-1] ^= 1
        device = ':'.join('{:02x}'.format(b) for b in identity)
        existing += '\noptions g_ether host_addr={} dev_addr={}\n'.format(host, device)
    files = {
        'etc/modprobe.d/g_ether.conf': (existing, 0o644),
        'etc/modules-load.d/r36s-devbridge.conf': ('dwc2\ng_ether\n', 0o644),
        'usr/local/libexec/r36s-devbridge-power': (POWER_HELPER, 0o755),
        'etc/udev/rules.d/99-r36s-devbridge-usb0.rules': (
            'SUBSYSTEM=="net", ACTION=="add", KERNEL=="usb0", RUN+="/sbin/ip link set dev usb0 up", RUN+="/sbin/ip -4 address replace 192.168.7.2/24 dev usb0"\n', 0o644),
        'etc/NetworkManager/conf.d/99-r36s-devbridge-usb0.conf': (
            '[keyfile]\nunmanaged-devices=interface-name:usb0\n', 0o644),
    }
    dropin = ('[Service]\nExecStartPre=+/usr/local/libexec/r36s-devbridge-power prepare\n'
              'ExecStopPost=+/usr/local/libexec/r36s-devbridge-power resume\n')
    for service in ('suspend', 'hibernate', 'hybrid-sleep', 'suspend-then-hibernate'):
        files['etc/systemd/system/systemd-{}.service.d/50-devbridge.conf'.format(service)] = (dropin, 0o644)
    return files


def install(payload, root, activate=False):
    manifest = verify(payload)
    if activate and root.resolve() != Path('/'):
        raise ValueError('--activate exige root /')
    with tempfile.TemporaryDirectory(prefix='devbridge-transaction-') as work:
        work = Path(work)
        targets = [
            (payload / 'r36s-agent', root / 'usr/local/bin/r36s-agent', 0o755),
            (payload / 'r36s-agent.service', root / 'etc/systemd/system/r36s-agent.service', 0o644),
            (payload / 'manifest.json', root / 'usr/local/share/devbridge/manifest.json', 0o644),
            (Path(__file__), root / 'usr/local/share/devbridge/install_payload.py', 0o755),
        ]
        for index, (relative, (content, mode)) in enumerate(generated_files(root).items()):
            source = work / str(index)
            source.write_text(content)
            targets.append((source, root / relative, mode))
        obsolete = root / 'lib/systemd/system-sleep/99-r36s-devbridge.sh'
        mutations = [(source, target, mode) for source, target, mode in targets
                     if not target.exists() or digest(source) != digest(target)]
        if obsolete.exists():
            mutations.append((None, obsolete, obsolete.stat().st_mode & 0o777))
        # Capture every changed file before the first installed file is replaced.
        snapshots = []
        backup = root / 'usr/local/share/devbridge/previous'
        for index, (source, target, mode) in enumerate(mutations):
            old = work / ('previous-' + str(index))
            if target.exists():
                old_mode = target.stat().st_mode & 0o777
                atomic_copy(target, old, old_mode)
                snapshots.append((old, target, old_mode))
                relative = target.relative_to(root)
                atomic_copy(target, backup / 'files' / relative, old_mode)
                if target.name in ('r36s-agent', 'r36s-agent.service', 'manifest.json'):
                    atomic_copy(target, backup / target.name, old_mode)
            else:
                snapshots.append((None, target, mode))
        (backup / 'transaction.json').parent.mkdir(parents=True, exist_ok=True)
        record = work / 'transaction.json'
        record.write_text(json.dumps([{'path': str(t.relative_to(root)), 'existed': old is not None, 'mode': mode}
                                      for old, t, mode in snapshots]))
        if mutations:
            atomic_copy(record, backup / 'transaction.json', 0o644)
        try:
            for source, target, mode in mutations:
                if source is None:
                    target.unlink()
                else:
                    atomic_copy(source, target, mode)
            if activate:
                subprocess.check_call(['systemctl', 'daemon-reload'])
                subprocess.check_call(['udevadm', 'control', '--reload-rules'])
                subprocess.check_call(['systemctl', 'restart', 'r36s-agent'])
                deadline = time.monotonic() + 15
                while time.monotonic() < deadline:
                    if healthy(manifest['agent_sha256']):
                        print('Agente v{} ativo; hash e protocolo conferidos.'.format(manifest['version']))
                        return
                    time.sleep(0.5)
                raise RuntimeError('Novo agente não ficou saudável em 15 segundos')
        except Exception:
            for old, target, mode in reversed(snapshots):
                if old is None:
                    if target.exists():
                        target.unlink()
                else:
                    atomic_copy(old, target, mode)
            if activate:
                subprocess.call(['systemctl', 'daemon-reload'])
                subprocess.call(['udevadm', 'control', '--reload-rules'])
                subprocess.call(['systemctl', 'restart', 'r36s-agent'])
            print('Arquivos anteriores restaurados.')
            raise
    print('Agente v{} instalado offline; validar conexão após o boot.'.format(manifest['version']))


def rollback(root, activate=False):
    backup = root / 'usr/local/share/devbridge/previous'
    records = json.loads((backup / 'transaction.json').read_text())
    # Verify the complete backup before restoring any target.
    for record in records:
        relative = Path(record['path'])
        if relative.is_absolute() or '..' in relative.parts:
            raise ValueError('Caminho inválido no backup')
        if record['existed'] and not (backup / 'files' / relative).is_file():
            raise ValueError('Backup incompleto: ' + str(relative))
    for record in reversed(records):
        relative = Path(record['path'])
        target = root / relative
        if record['existed']:
            atomic_copy(backup / 'files' / relative, target, record['mode'])
        elif target.exists():
            target.unlink()
    if activate:
        subprocess.check_call(['systemctl', 'daemon-reload'])
        subprocess.check_call(['udevadm', 'control', '--reload-rules'])
        subprocess.check_call(['systemctl', 'restart', 'r36s-agent'])


def uninstall(root):
    for relative in generated_files(root):
        if relative == 'etc/modprobe.d/g_ether.conf':
            # This may predate DevBridge or be shared with a USB SSH setup.
            continue
        target = root / relative
        if target.exists():
            target.unlink()
    for relative in ('lib/systemd/system-sleep/99-r36s-devbridge.sh',
                     'etc/systemd/system/r36s-agent.service',
                     'etc/systemd/system/multi-user.target.wants/r36s-agent.service',
                     'usr/local/bin/r36s-agent'):
        target = root / relative
        if target.exists() or target.is_symlink():
            target.unlink()


def install_archive(archive, root, activate):
    with tempfile.TemporaryDirectory(prefix='devbridge-payload-') as directory:
        directory = Path(directory)
        with zipfile.ZipFile(archive) as package:
            required = {'r36s-agent', 'r36s-agent.service', 'manifest.json', 'install_payload.py'}
            names = package.namelist()
            if set(names) != required or len(names) != len(required):
                raise ValueError('Conteúdo do payload inválido')
            for info in package.infolist():
                if info.file_size > 16 * 1024 * 1024:
                    raise ValueError('Arquivo do payload excede o limite')
                (directory / info.filename).write_bytes(package.read(info))
        manifest = verify(directory)
        if 'installer_sha256' not in manifest:
            raise ValueError('Payload sem hash do instalador')
        # Execute the verified new installer, so an update also migrates its configuration logic.
        subprocess.check_call(['python3', str(directory / 'install_payload.py'), str(directory),
                               '--root', str(root)] + (['--activate'] if activate else []))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('payload', type=Path, nargs='?')
    parser.add_argument('--archive', type=Path)
    parser.add_argument('--rollback', action='store_true')
    parser.add_argument('--uninstall', action='store_true')
    parser.add_argument('--root', type=Path, default=Path('/'))
    parser.add_argument('--activate', action='store_true')
    args = parser.parse_args()
    if sum(map(bool, (args.archive, args.payload, args.rollback, args.uninstall))) != 1:
        parser.error('Informe payload, --archive, --rollback ou --uninstall')
    if args.activate and args.root.resolve() != Path('/'):
        parser.error('--activate exige root /')
    # Serialize installs on the same target filesystem.
    lock_dir = args.root / 'usr/local/share/devbridge'
    lock_dir.mkdir(parents=True, exist_ok=True)
    if args.archive:
        install_archive(args.archive, args.root, args.activate)
    else:
        with (lock_dir / '.install.lock').open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            if args.rollback:
                rollback(args.root, args.activate)
            elif args.uninstall:
                uninstall(args.root)
            else:
                install(args.payload, args.root, args.activate)
