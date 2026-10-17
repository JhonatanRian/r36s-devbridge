import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('sd_card', Path(__file__).parents[1] / 'sd_card.py')
sd = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sd)


def card(name='/dev/sdb'):
    return {'name': name, 'type': 'disk', 'ro': False, 'children': [
        {'name': name + str(n), 'label': label, 'ro': False, 'mountpoint': None}
        for n, label in enumerate(('BOOT', 'root', 'EASYROMS'), 1)]}


class CardIdentityTests(unittest.TestCase):
    def test_all_partitions_must_belong_to_selected_disk(self):
        broken = card()
        broken['children'].pop(0)
        with self.assertRaises(ValueError):
            sd.select_partitions({'blockdevices': [broken]})

    def test_multiple_cards_require_explicit_disk(self):
        tree = {'blockdevices': [card(), card('/dev/sdc')]}
        with self.assertRaises(ValueError):
            sd.select_partitions(tree)
        self.assertEqual(sd.select_partitions(tree, '/dev/sdc')[0], ('EASYROMS', '/dev/sdc3'))

    def test_system_disk_and_readonly_card_are_rejected(self):
        unsafe = card()
        unsafe['children'][0]['mountpoint'] = '/boot/efi'
        with self.assertRaises(ValueError):
            sd.select_partitions({'blockdevices': [unsafe]})
        unsafe = card()
        unsafe['ro'] = True
        with self.assertRaises(ValueError):
            sd.select_partitions({'blockdevices': [unsafe]})
