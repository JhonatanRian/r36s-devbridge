#!/usr/bin/env python3
"""Read one DevBridge menu choice from the R36S gamepad or a keyboard."""

import glob
import os
import select
import struct
import sys
import termios
import time
import tty
import xml.etree.ElementTree as ET


DEFAULT_BUTTONS = {1: "2", 0: "4", 2: "1", 3: "3"}  # A, B, X, Y
DEFAULT_TIMEOUT = 30.0


def load_button_map(config_path):
    try:
        root = ET.parse(config_path).getroot()
        for controller in root.findall("inputConfig"):
            if controller.get("type") != "joystick":
                continue
            buttons = {
                entry.get("name"): int(entry.get("id"))
                for entry in controller.findall("input")
                if entry.get("type") == "button" and entry.get("name") in {"a", "b", "x", "y"}
            }
            if len(buttons) == 4:
                return {
                    buttons["a"]: "2",
                    buttons["b"]: "4",
                    buttons["x"]: "1",
                    buttons["y"]: "3",
                }, controller.get("deviceName")
    except (OSError, ValueError, ET.ParseError):
        pass
    return DEFAULT_BUTTONS, None


def find_joystick(device_name):
    override = os.environ.get("DEVBRIDGE_JS_DEVICE")
    if override:
        return override
    candidates = sorted(glob.glob("/dev/input/js*"))
    if device_name:
        for path in candidates:
            name_path = "/sys/class/input/{}/device/name".format(os.path.basename(path))
            try:
                with open(name_path, encoding="utf-8") as name_file:
                    if name_file.read().strip() == device_name:
                        return path
            except OSError:
                pass
    return candidates[0] if len(candidates) == 1 else None


def read_choice(timeout):
    button_map, device_name = load_button_map(
        os.environ.get("DEVBRIDGE_INPUT_CONFIG", "/etc/emulationstation/es_input.cfg")
    )
    joystick_path = find_joystick(device_name)
    joystick_fd = None
    tty_fd = None
    old_tty = None
    try:
        if joystick_path:
            try:
                joystick_fd = os.open(joystick_path, os.O_RDONLY | os.O_NONBLOCK)
            except OSError:
                pass
        try:
            tty_fd = os.open(os.environ.get("DEVBRIDGE_TTY", "/dev/tty1"), os.O_RDONLY | os.O_NONBLOCK)
            old_tty = termios.tcgetattr(tty_fd)
            tty.setcbreak(tty_fd)
        except (OSError, termios.error):
            if tty_fd is not None:
                os.close(tty_fd)
                tty_fd = None

        deadline = time.monotonic() + timeout
        uninstall_until = 0.0
        while time.monotonic() < deadline:
            fds = [fd for fd in (joystick_fd, tty_fd) if fd is not None]
            if not fds:
                break
            ready, _, _ = select.select(fds, [], [], min(0.5, max(0.0, deadline - time.monotonic())))
            for fd in ready:
                if fd == tty_fd:
                    try:
                        key = os.read(fd, 1)
                    except BlockingIOError:
                        continue
                    if key in (b"1", b"2", b"4"):
                        return key.decode("ascii")
                    if key == b"3":
                        if time.monotonic() <= uninstall_until:
                            return "3"
                        uninstall_until = time.monotonic() + 5
                        print("Pressione Y/3 novamente para confirmar a desinstalacao.", file=sys.stderr)
                else:
                    try:
                        data = os.read(fd, 8)
                    except BlockingIOError:
                        continue
                    if len(data) != 8:
                        continue
                    _, value, event_type, button = struct.unpack("<IhBB", data)
                    if event_type != 0x01 or value != 1:  # ignore axis, release, initial state
                        continue
                    choice = button_map.get(button)
                    if choice == "3":
                        if time.monotonic() <= uninstall_until:
                            return "3"
                        uninstall_until = time.monotonic() + 5
                        print("Pressione Y novamente para confirmar a desinstalacao.", file=sys.stderr)
                    elif choice:
                        return choice
        return "4"  # Timeout exits without changing the service.
    finally:
        if old_tty is not None and tty_fd is not None:
            termios.tcsetattr(tty_fd, termios.TCSANOW, old_tty)
        for fd in (joystick_fd, tty_fd):
            if fd is not None:
                os.close(fd)


if __name__ == "__main__":
    seconds = float(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_TIMEOUT
    print(read_choice(seconds))
