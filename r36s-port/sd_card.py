#!/usr/bin/env python3
"""Select ArkOS partitions from one present block device; never infer identity from directories."""
import json
import os
import subprocess
import sys


def select_partitions(tree, requested=None):
    candidates = []
    for disk in tree['blockdevices']:
        if disk.get('type') != 'disk':
            continue
        children = disk.get('children', [])
        if any(p.get('mountpoint') in ('/', '/boot', '/boot/efi') for p in children):
            continue
        if requested and disk['name'] != requested:
            continue
        easy = [p for p in children if p.get('label') == 'EASYROMS']
        if easy:
            candidates.append((disk, children))
    if len(candidates) != 1:
        raise ValueError('Seleção de cartão ambígua ou ausente. Informe DEVBRIDGE_SD_DEVICE=/dev/sdX com o disco do cartão.')
    disk, children = candidates[0]
    result = []
    for label in ('EASYROMS', 'BOOT', 'root'):
        matches = [p for p in children if p.get('label') == label]
        if len(matches) != 1:
            raise ValueError('Partição {} ausente ou duplicada no cartão {}'.format(label, disk['name']))
        partition = matches[0]
        if disk.get('ro') or partition.get('ro'):
            raise ValueError('Cartão somente leitura: ' + disk['name'])
        result.append((label, partition['name']))
    return result


if __name__ == '__main__':
    try:
        data = subprocess.check_output(['lsblk', '--json', '--paths', '-o', 'NAME,TYPE,LABEL,RO,MOUNTPOINT'])
        for label, path in select_partitions(json.loads(data), os.environ.get('DEVBRIDGE_SD_DEVICE')):
            print(label + '\t' + path)
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        sys.exit(str(error))
