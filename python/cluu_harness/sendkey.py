"""ASCII → QEMU ``sendkey`` translation.

CLUU's guest keyboard layout is Hungarian (QWERTZ). The bash harness's
``type_ascii_command`` maps each ASCII character to the QEMU monitor
key name that produces the intended glyph under that layout. This
module ports that mapping verbatim — see ``cluu-hu-keyboard-layout-
mangles-escapes`` in the knowledge vault for why direct US scancodes
produce wrong characters.

For sequences containing layout-sensitive characters (``\\``, ``'``,
``/``, ``0``, ``y``, ``z``), prefer raw ``SENDKEY_SEQUENCE`` entries
with explicit QEMU key names — they bypass this translator entirely.
"""

from __future__ import annotations

# Single-character → QEMU sendkey name. Verbatim from
# scripts/harness_run.sh:type_ascii_command, including the HU QWERTZ
# swaps documented inline there.
_HU_MAP: dict[str, str] = {
    " ": "spc",
    "\t": "tab",
    # HU: '-' on slash key (scancode 0x35, base_symbol → b'-')
    "-": "slash",
    # HU: '_' on shift-slash
    "_": "shift-slash",
    # HU: '=' on shift-7
    "=": "shift-7",
    # HU: '+' on shift-3
    "+": "shift-3",
    ".": "dot",
    ",": "comma",
    # HU: '/' on shift-6, '?' on shift-comma
    "/": "shift-6",
    "?": "shift-comma",
    # HU: ';' AltGr-comma
    ";": "alt_r-comma",
    # HU: ':' shift-dot
    ":": "shift-dot",
    # HU: '\'' shift-1, '"' shift-2
    "'": "shift-1",
    '"': "shift-2",
    # HU: parens on shift-8 / shift-9
    "(": "shift-8",
    ")": "shift-9",
    # HU: brackets/braces/backslash/pipe are AltGr combos
    "[": "alt_r-f",
    "]": "alt_r-g",
    "{": "alt_r-b",
    "}": "alt_r-n",
    "\\": "alt_r-q",
    "|": "alt_r-w",
    # HU: '!' shift-4
    "!": "shift-4",
    "@": "alt_r-v",
    "#": "alt_r-x",
    "$": "alt_r-semicolon",
    "%": "shift-5",
    "^": "alt_r-3",
    "&": "alt_r-c",
    "*": "alt_r-slash",
    "<": "alt_r-m",
    ">": "alt_r-dot",
    "`": "alt_r-7",
    "~": "alt_r-1",
    # HU QWERTZ swaps y↔z
    "y": "z",
    "z": "y",
    "Y": "shift-z",
    "Z": "shift-y",
    # HU: '0' lives on grave_accent scancode (0x29)
    "0": "grave_accent",
}

# Letters (lowercase) and digits 1-9 use their own key name directly.
for _c in "abcdefghijklmnopqrstuvwx123456789":  # 'y','z','0' handled above
    _HU_MAP.setdefault(_c, _c)

# Uppercase letters → shift-<lowercase>
for _c in "ABCDEFGHIJKLMNOPQRSTUVWXY":  # 'Z' handled above
    _HU_MAP.setdefault(_c, f"shift-{_c.lower()}")

# Digits 1-9 already covered by the loop above; digit 0 is special-cased.


def char_to_sendkey(ch: str) -> str | None:
    """Map one ASCII char to a QEMU sendkey name.

    Returns ``None`` for unsupported characters (caller should warn).
    """
    return _HU_MAP.get(ch)


def command_to_sendkeys(cmd: str) -> list[str]:
    """Translate a full command string into a list of sendkey names.

    ``ret`` is appended as the final key, mirroring the bash harness.
    Unsupported characters are skipped (a warning is the caller's job).
    """
    keys: list[str] = []
    for ch in cmd:
        k = char_to_sendkey(ch)
        if k is not None:
            keys.append(k)
    keys.append("ret")
    return keys


def unsupported_chars(cmd: str) -> list[str]:
    """Return characters in ``cmd`` that have no sendkey mapping."""
    return [ch for ch in cmd if ch not in _HU_MAP]


__all__ = ["char_to_sendkey", "command_to_sendkeys", "unsupported_chars"]
