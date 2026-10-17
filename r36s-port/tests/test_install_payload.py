import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('installer', Path(__file__).parents[1] / 'install_payload.py')
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.payload = Path(self.temp.name) / 'payload'
        self.root = Path(self.temp.name) / 'root'
        self.payload.mkdir()
        self.binary = self.root / 'usr/local/bin/r36s-agent'
        self.binary.parent.mkdir(parents=True)
        self.binary.write_bytes(b'previous executable')
        self.make_payload()

    def make_payload(self):
        (self.payload / 'r36s-agent').write_bytes(b'new executable')
        (self.payload / 'r36s-agent.service').write_bytes(b'new service')
        (self.payload / 'manifest.json').write_text(json.dumps({
            'version': 'test', 'architecture': 'aarch64',
            'agent_sha256': installer.digest(self.payload / 'r36s-agent'),
            'service_sha256': installer.digest(self.payload / 'r36s-agent.service'),
        }))

    def test_corruption_is_rejected_before_changes(self):
        (self.payload / 'r36s-agent').write_bytes(b'corrupt')
        with self.assertRaises(ValueError):
            installer.install(self.payload, self.root)
        self.assertEqual(self.binary.read_bytes(), b'previous executable')

    def test_atomic_replacement_preserves_open_executable_and_backup(self):
        with self.binary.open('rb') as running:
            installer.install(self.payload, self.root)
            self.assertEqual(running.read(), b'previous executable')
        self.assertEqual(self.binary.read_bytes(), b'new executable')
        self.assertEqual((self.root / 'usr/local/share/devbridge/previous/r36s-agent').read_bytes(), b'previous executable')
        self.assertEqual(self.binary.stat().st_mode & 0o777, 0o755)

    def test_failed_install_restores_previous_files(self):
        original = installer.atomic_copy
        def copy(source, target, mode):
            if source == self.payload / 'r36s-agent.service':
                raise OSError('simulated full filesystem')
            original(source, target, mode)
        with patch.object(installer, 'atomic_copy', side_effect=copy):
            with self.assertRaises(OSError):
                installer.install(self.payload, self.root)
        self.assertEqual(self.binary.read_bytes(), b'previous executable')
        self.assertFalse((self.root / 'usr/local/share/devbridge/manifest.json').exists())


if __name__ == '__main__':
    unittest.main()

class ConfigurationTransactionTests(InstallerTests):
    def test_resume_configuration_is_included_in_rollback(self):
        legacy = self.root / 'lib/systemd/system-sleep/99-r36s-devbridge.sh'
        legacy.parent.mkdir(parents=True)
        legacy.write_text('old hook')
        installer.install(self.payload, self.root)
        self.assertFalse(legacy.exists())
        dropin = self.root / 'etc/systemd/system/systemd-suspend.service.d/50-devbridge.conf'
        self.assertIn('ExecStopPost=', dropin.read_text())
        installer.rollback(self.root)
        self.assertEqual(legacy.read_text(), 'old hook')
        self.assertFalse(dropin.exists())
        self.assertEqual(self.binary.read_bytes(), b'previous executable')

    def test_configuration_failure_restores_every_changed_file(self):
        original = installer.atomic_copy
        def fail(source, target, mode):
            if target == self.root / 'usr/local/libexec/r36s-devbridge-power':
                raise OSError('configuration filesystem full')
            return original(source, target, mode)
        with patch.object(installer, 'atomic_copy', side_effect=fail):
            with self.assertRaises(OSError):
                installer.install(self.payload, self.root)
        self.assertEqual(self.binary.read_bytes(), b'previous executable')
        self.assertFalse((self.root / 'etc/modprobe.d/g_ether.conf').exists())

    def test_mac_is_persistent_and_not_shared_by_new_installations(self):
        installer.install(self.payload, self.root)
        mac = self.root / 'etc/modprobe.d/g_ether.conf'
        first = mac.read_text()
        installer.install(self.payload, self.root)
        self.assertEqual(mac.read_text(), first)
        other = Path(self.temp.name) / 'another-console'
        self.assertNotEqual(installer.generated_files(other)['etc/modprobe.d/g_ether.conf'][0], first)
