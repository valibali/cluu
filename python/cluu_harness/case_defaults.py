"""Per-case defaults: TEST_COMMAND, SENDKEY_SEQUENCE, RUN_WAIT, etc.

Ports ``scripts/harness_case_defaults.sh``. The bash version is a giant
case statement over ``$MARKER_MODE``; this module exposes the same data
as a dict so new cases are declarative.

Knowledge-vault conventions applied here:

* ``cluu-sendkey-nowait-for-login-cases`` — every case that injects
  credentials sets ``sendkey_sequence_nowait=True``.
* ``cluu-harness-sendkey-sleep-must-match-boot`` — the credential
  sequence sleeps 12s before keys, not 5s, so kbd IRQ handler is
  attached when keys arrive.
"""

from __future__ import annotations

from dataclasses import dataclass, field


@dataclass
class CaseDefaults:
    """Derived defaults for one MARKER_MODE."""

    test_command: str | None = None
    post_sendkey: str | None = None
    sendkey_sequence: list[str] = field(default_factory=list)
    sendkey_sequence_nowait: bool = False
    run_wait_s: int | None = None
    # Extra keystroke commands to inject (KEYSTROKE_COMMANDS in bash).
    keystroke_commands: list[str] = field(default_factory=list)


# Standard root/root credentials sendkey sequence. Per
# cluu-harness-sendkey-sleep-must-match-boot, the prefix sleep is 12s
# (kbd attaches at ~9.4s, login window at ~9.8s).
_CREDS_SENDKEY_ROOT: list[str] = [
    "sleep 12",
    "sendkey r", "sendkey o", "sendkey o", "sendkey t", "sendkey ret",
    "sleep 2",
    "sendkey r", "sendkey o", "sendkey o", "sendkey t", "sendkey ret",
]


def _creds() -> list[str]:
    """Return a fresh copy of the credential sequence (mutable)."""
    return list(_CREDS_SENDKEY_ROOT)


# MARKER_MODE → defaults. Keep alphabetical for grep-ability.
# When adding an entry, set sendkey_sequence_nowait=True if the case
# injects credentials (login modal spawns before any shell — see
# cluu-sendkey-nowait-for-login-cases).
_DEFAULTS: dict[str, CaseDefaults] = {
    "legacy_p1": CaseDefaults(
        test_command="minimal",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "m1_recv": CaseDefaults(test_command="hello"),
    "m3_mapfail": CaseDefaults(test_command="mapfail 12 4"),
    "m3_mapcopyfail": CaseDefaults(test_command="mapcopyfail 4"),
    "m3_maperror": CaseDefaults(test_command="maperror 3"),
    "m4_sender_auth": CaseDefaults(test_command="hello"),
    "m4_deny_paths": CaseDefaults(
        test_command="killdeny 2 9",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "m5_fairness": CaseDefaults(test_command="repeat 8 hello"),
    "b_spawn_warm": CaseDefaults(
        test_command="benchprobe spawnonly",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "c_futex": CaseDefaults(
        test_command="futexprobe",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "c_futex_race": CaseDefaults(
        test_command="futexrace",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_ext2write": CaseDefaults(
        test_command="ext2io write",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_ext2unlink": CaseDefaults(
        test_command="ext2io unlink",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_cd": CaseDefaults(
        test_command="cd /; cd etc; pwd",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_ls": CaseDefaults(
        test_command="ls /etc",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_mkdir": CaseDefaults(
        test_command="mkdir /tmp/a; mkdir -p /tmp/b/c/d",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_login": CaseDefaults(
        test_command=None,
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_cluuterm_login": CaseDefaults(
        test_command=None,
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_cluuterm_exit": CaseDefaults(
        test_command=None,
        sendkey_sequence=_creds()
        + ["sleep 3", "sendkey e", "sendkey x", "sendkey i", "sendkey t", "sendkey ret"],
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_vt4_default": CaseDefaults(
        test_command=None,
        # Pure boot-time marker — no keyboard input needed.
    ),
    "none": CaseDefaults(test_command="hello"),
}


def get_defaults(marker_mode: str) -> CaseDefaults:
    """Return the defaults for a MARKER_MODE.

    Unknown modes fall back to ``test_command="hello"`` (matching the
    bash harness's ``*) TEST_COMMAND="hello"`` wildcard).
    """
    d = _DEFAULTS.get(marker_mode)
    if d is not None:
        return d
    return CaseDefaults(test_command="hello")


__all__ = ["CaseDefaults", "get_defaults"]
